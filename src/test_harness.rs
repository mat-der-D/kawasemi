//! DB-integration test harness (TestHarness boundary, Requirements 8.1-8.5,
//! design.md's "Test / テスト層" -> "TestHarness" component).
//!
//! Scope: this module owns [`spawn_test_app`] — a way for any spec's own
//! integration tests to boot a real, running instance of this application
//! (a bound TCP listener serving the real foundation router, an
//! already-migrated database, and a deterministic [`RuntimeContext`]) — and
//! [`TestApp::cleanup`], the mandatory, explicit async release path for the
//! resources that instance acquired (Requirement 8.5).
//!
//! `spawn_test_app` deliberately recomposes the same building blocks
//! [`crate::bootstrap::bootstrap`] does — [`crate::db::establish_pool`],
//! [`crate::migrate::apply_migrations`],
//! [`crate::runtime::RuntimeContext::deterministic`],
//! [`crate::state::AppState::new`], and [`crate::server::build_router`] —
//! rather than reimplementing any of them (design.md: "Outbound: Bootstrap
//! 構成要素... を再利用"). It does not reuse
//! [`crate::bootstrap::bootstrap`]/[`crate::server::serve_with_shutdown_and_signal`]
//! directly, because both bind a caller-supplied, fixed [`std::net::SocketAddr`]
//! internally without handing the actually-bound address back — this module
//! instead binds an ephemeral `127.0.0.1:0` listener itself so that many
//! `#[tokio::test]` functions (which `cargo test` runs concurrently by
//! default) can each get their own real, non-colliding port (Requirement
//! 8.1), and so [`TestApp::address`] can report the real bound
//! [`std::net::SocketAddr`] back to the caller.
//!
//! ## Isolation strategy (Requirement 8.4)
//! Each call to [`spawn_test_app`] creates its own throwaway PostgreSQL
//! *schema* inside the same shared `kawasemi_test` database (mirroring
//! `src/migrate/tests.rs`'s established per-test-schema pattern; the
//! `kawasemi_test` role has no `CREATEDB` privilege, so a fresh throwaway
//! *database* per test is not available here), and connects
//! [`crate::db::establish_pool`]'s pool to that schema by encoding a
//! Postgres `options=-c search_path=<schema>` startup parameter into the
//! connection URL's query string (`options[search_path]=<schema>`, which
//! `sqlx_postgres`'s connection-string parser turns into exactly that
//! startup option — see `sqlx-postgres-0.9.0/src/options/parse.rs`'s
//! `"options["`-prefixed branch). Every unqualified table reference
//! [`crate::migrate::apply_migrations`] issues (including sqlx's own
//! `_sqlx_migrations` bookkeeping table) therefore lands in that schema,
//! isolated from every other concurrently-running instance, while still
//! running the exact same production `establish_pool`/`apply_migrations`
//! code paths (not a test-only substitute).
//!
//! ## Deterministic injection (Requirement 8.3)
//! [`spawn_test_app`] always builds its [`RuntimeContext`] via
//! [`RuntimeContext::deterministic`] with a fixed constant seed (see this
//! module's private `default_test_seed`), never [`RuntimeContext::production`]. The seed is
//! fixed (not derived per-instance) because Requirement 8.3 only asks for
//! non-determinism to be replaced with a deterministic implementation, not
//! for uniqueness across concurrently-running instances; a fixed seed also
//! means two separately-spawned `TestApp`s in the same test binary observe
//! the identical clock/id/rng/key sequence, which is the more useful
//! property for a caller asserting on those values.
//!
//! ## Release path: `cleanup()` vs `Drop` (Requirement 8.5)
//! [`TestApp::cleanup`] is the one documented, mandatory release path: it
//! signals the listener's graceful shutdown and awaits it actually stopping,
//! closes the shared pool, and then drops the isolated schema — in that
//! order, so the schema is only dropped once nothing still holds a
//! connection pinned to it. `Drop for TestApp` never does any of this
//! synchronously: it only (a) fires the same shutdown signal `cleanup` would
//! (sending on a `oneshot::Sender` is synchronous and non-blocking, unlike
//! awaiting the listener task) and (b) spawns the isolated-schema teardown
//! as a *detached* background task via `tokio::runtime::Handle::try_current`
//! when a Tokio runtime happens to be available (never `block_on`, which
//! would risk a panic inside a sync `Drop::drop` running on a tokio
//! runtime thread) — falling back to leaving the orphaned schema in place
//! (for a future startup sweep to reclaim) when no runtime handle is
//! reachable at all. Correctness is never assumed to come from `Drop`; only
//! `cleanup()` is guaranteed to have released everything by the time it
//! returns.

#[cfg(test)]
mod tests;

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sqlx::Executor;
use sqlx::postgres::PgPool;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::actor::keys::cipher::{ChaCha20Poly1305KeyCipher, KeyCipher};
use crate::actor::keys::provider::DbSigningKeyProvider;
use crate::actor::{self, ActorModule};
use crate::config::{
    ActorConfig, AppConfig, DatabaseConfig, LogConfig, LogLevel, OauthConfig, OwnerConfig, Secret,
    ServerConfig,
};
use crate::db;
use crate::migrate;
use crate::oauth::OauthModule;
use crate::runtime::{DeterministicSeed, RuntimeContext};
use crate::server;
use crate::state::AppState;

/// Environment variable overriding the shared test database's connection
/// URL, mirroring `src/db/tests.rs`'/`src/migrate/tests.rs`'s own convention.
const TEST_DB_URL_ENV: &str = "KAWASEMI_TEST_DATABASE_URL";

/// Default shared test database connection URL: the same fixed local-only,
/// non-production `kawasemi_test` role/database `src/db/tests.rs` and
/// `src/migrate/tests.rs` already rely on.
const DEFAULT_TEST_DB_URL: &str =
    "postgres://kawasemi_test:kawasemi_test_pw@127.0.0.1:5432/kawasemi_test";

/// Fixed numeric seed every [`spawn_test_app`] call builds its deterministic
/// [`RuntimeContext`] from (via [`default_test_seed`]). See this module's
/// doc comment ("Deterministic injection") for why a fixed constant, rather
/// than a per-call value, is the right choice here.
const DEFAULT_TEST_SEED_VALUE: u64 = 424_242;

/// Returns the fixed [`DeterministicSeed`] every [`spawn_test_app`] call
/// uses. A plain function rather than a `const` value because
/// [`DeterministicSeed::new`] is not `const fn`. Not part of design.md's
/// documented Service Interface; kept private (reachable by this module's
/// own tests via `super::*`) so callers assert on `TestApp`'s deterministic
/// behavior rather than depending on the concrete seed value directly.
fn default_test_seed() -> DeterministicSeed {
    DeterministicSeed::new(DEFAULT_TEST_SEED_VALUE)
}

/// Fixed, non-production Key-Encryption-Key every [`spawn_test_app`] call
/// uses to build its `ChaCha20Poly1305KeyCipher` (task 6.1, Requirement
/// 6.1). Fixed rather than per-call (mirroring [`DEFAULT_TEST_SEED_VALUE`]'s
/// own "why fixed" reasoning) since no test needs KEK uniqueness across
/// concurrently-running `TestApp`s, only a valid one.
const TEST_KEK: [u8; 32] = [0x42; 32];

/// Fixed, non-production owner passphrase every [`spawn_test_app`] call uses
/// (api-foundation task 1.2, Requirement 2.2). Fixed rather than per-call,
/// mirroring [`TEST_KEK`]'s own "why fixed" reasoning.
const TEST_OWNER_PASSWORD: &str = "test-harness-owner-passphrase";

/// Fixed, non-production OAuth token-hashing key every [`spawn_test_app`]
/// call uses (api-foundation task 1.2, Requirement 3.6). Fixed rather than
/// per-call, mirroring [`TEST_KEK`]'s own "why fixed" reasoning.
const TEST_TOKEN_HASH_KEY: [u8; 32] = [0x24; 32];

/// Resolves the shared test database's connection URL: an explicit
/// `KAWASEMI_TEST_DATABASE_URL` override if set, otherwise
/// [`DEFAULT_TEST_DB_URL`].
fn base_test_db_url() -> String {
    std::env::var(TEST_DB_URL_ENV).unwrap_or_else(|_| DEFAULT_TEST_DB_URL.to_string())
}

/// Generates a schema name unique to this process/run: a monotonic counter
/// plus wall-clock nanoseconds, so concurrently-running `#[tokio::test]`
/// functions (and repeated `cargo test` invocations) never collide. Mirrors
/// `src/migrate/tests.rs`'s `unique_schema_name` convention.
fn unique_schema_name() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("kawasemi_test_harness_{nanos}_{seq}")
}

/// Builds a `DatabaseConfig` pointed at the shared test database, with no
/// per-schema `search_path` pinning applied. Used for the throwaway
/// bootstrap/admin connections this module opens to create and drop
/// isolated schemas (never for a `TestApp`'s own pool).
fn admin_db_config() -> DatabaseConfig {
    DatabaseConfig {
        url: Secret::new(base_test_db_url()),
        max_connections: 1,
        acquire_timeout: Duration::from_secs(5),
    }
}

/// Builds the connection URL a `TestApp`'s own pool uses: the shared test
/// database's URL, with a `search_path`-pinning startup option appended so
/// every connection this pool opens defaults to `schema` (see this module's
/// doc comment, "Isolation strategy"). `schema` is always this module's own
/// [`unique_schema_name`] output (fixed prefix plus numeric timestamp/
/// counter), never untrusted input, so no additional escaping is applied.
fn schema_scoped_url(base_url: &str, schema: &str) -> String {
    let separator = if base_url.contains('?') { '&' } else { '?' };
    format!("{base_url}{separator}options[search_path]={schema}")
}

/// Creates `schema` in the shared test database via a throwaway admin
/// connection (reusing [`crate::db::establish_pool`], never a bespoke
/// `PgPoolOptions` call). Panics on failure: an inability to even create the
/// isolated schema means the environment this harness needs is not
/// available, which every caller relying on this harness needs to know
/// immediately rather than receiving a partially-initialized `TestApp`.
async fn create_schema(schema: &str) {
    let admin_pool = db::establish_pool(&admin_db_config())
        .await
        .expect("establishing an admin connection to the shared test database must succeed");
    admin_pool
        .execute(sqlx::query(sqlx::AssertSqlSafe(format!(
            r#"CREATE SCHEMA "{schema}""#
        ))))
        .await
        .expect("creating the isolated per-test-instance schema must succeed");
    admin_pool.close().await;
}

/// Best-effort teardown of `schema` (and everything in it, including its
/// private `_sqlx_migrations` table): drops it via a fresh admin connection.
/// Failures here are logged, never panicked on — by the time this runs
/// (either from [`TestApp::cleanup`] or `Drop`'s detached fallback), the
/// schema is disposable test scaffolding, not something whose loss should
/// fail a caller that already got everything it asked for.
async fn drop_schema(schema: &str) {
    match db::establish_pool(&admin_db_config()).await {
        Ok(admin_pool) => {
            if let Err(err) = admin_pool
                .execute(sqlx::query(sqlx::AssertSqlSafe(format!(
                    r#"DROP SCHEMA IF EXISTS "{schema}" CASCADE"#
                ))))
                .await
            {
                eprintln!("test_harness: failed to drop isolated schema {schema}: {err}");
            }
            admin_pool.close().await;
        }
        Err(err) => {
            eprintln!(
                "test_harness: failed to open an admin connection while dropping isolated \
                 schema {schema}: {err}"
            );
        }
    }
}

/// A running test instance of the application (design.md's "TestHarness"
/// Service Interface): a real, connectable [`address`](Self::address), a
/// [`pool`](Self::pool) pinned to a schema isolated from every other
/// `TestApp`, and a [`runtime`](Self::runtime) built from
/// [`RuntimeContext::deterministic`].
///
/// Callers must call [`TestApp::cleanup`] when done with it (Requirement
/// 8.5) — see this module's doc comment for why `Drop` alone is not a
/// substitute.
pub struct TestApp {
    /// The real, bound socket address the foundation router
    /// ([`crate::server::build_router`]) is being served on. Connectable
    /// over real TCP for HTTP-level integration tests.
    pub address: SocketAddr,
    /// The connection pool for this instance's isolated schema, established
    /// via [`crate::db::establish_pool`] with the embedded migrations
    /// already applied.
    pub pool: PgPool,
    /// The deterministic non-determinism injection boundaries this instance
    /// was booted with (Requirement 8.3). `runtime.keys` is a deliberate,
    /// documented exception to "everything here is deterministic": it is
    /// always the real, DB-backed `DbSigningKeyProvider` (task 6.1,
    /// Requirement 6.1) built the same way `bootstrap()`'s own production
    /// path builds one, from a `KeyCache` scoped to this instance's isolated
    /// schema — not [`crate::runtime::signing_key::FixedSigningKeyProvider`]
    /// (the placeholder [`RuntimeContext::deterministic`] would otherwise
    /// use). This is what lets an integration test built on `spawn_test_app`
    /// prove actor creation -> key supply -> rotation end to end through the
    /// real supply boundary, not a fixed stand-in.
    pub runtime: RuntimeContext,
    /// The actor-model service bundle this instance was booted with,
    /// wired the same way `bootstrap()`'s own production path wires one
    /// (`crate::actor::build_actor_module`, task 6.1): the same `KeyCache`
    /// instance backs both this field's `SigningKeyService` and
    /// `runtime.keys`'s `DbSigningKeyProvider`, so writes made through
    /// `actor.signing_key_service()`/`actor.actor_service()` are
    /// immediately observable via `runtime.keys.signing_key(..)`.
    pub actor: ActorModule,
    /// The fully-assembled `AppState` this instance is serving (task 7.1):
    /// the exact same value passed to [`crate::server::build_router`] below,
    /// exposed so a caller's own integration test can build additional
    /// `AppState`-compatible test-only routers (mirroring
    /// `src/server/tests.rs`'s established "merge a test-only route onto
    /// `router()`, then `.with_state(state)`" technique) against the real,
    /// running instance's exact composition-root wiring — e.g. to prove the
    /// Bearer auth middleware's `AuthState: FromRef<AppState>` bridge
    /// (`src/server.rs`) works, without needing a second, separately-wired
    /// `AppState` reconstructed field-by-field.
    pub state: AppState,
    /// Name of this instance's isolated PostgreSQL schema (Requirement 8.4).
    /// `Some` until whichever of [`TestApp::cleanup`] or `Drop` runs first
    /// takes it, so the schema is torn down exactly once even though `Drop`
    /// always runs (including immediately after a successful `cleanup()`
    /// call, since `cleanup` takes `self` by value). Not part of
    /// design.md's documented Service Interface; kept private to this
    /// module's own [`create_schema`]/[`drop_schema`] plumbing (and this
    /// module's own tests, which may reach it via `super::*`).
    schema: Option<String>,
    /// Fires the injected shutdown signal the serving task is racing
    /// against. `Some` until either [`TestApp::cleanup`] or `Drop` consumes
    /// it; sending on a `oneshot::Sender` is synchronous and non-blocking,
    /// so `Drop` can safely fire it too.
    shutdown_tx: Option<oneshot::Sender<()>>,
    /// The spawned task serving `address` until `shutdown_tx` fires.
    /// `Some` until [`TestApp::cleanup`] awaits it; `Drop` never awaits this
    /// (see module doc comment) and simply lets it finish running detached
    /// in the background once the shutdown signal has been sent.
    server_task: Option<JoinHandle<std::io::Result<()>>>,
}

impl TestApp {
    /// The mandatory, explicit async release path (Requirement 8.5): signals
    /// the serving task to shut down and awaits it actually stopping, closes
    /// the connection pool, and drops this instance's isolated schema — in
    /// that order, so the schema is only torn down once nothing still holds
    /// a connection pinned to it. Test code must call this when finished
    /// with a `TestApp`, regardless of whether the test body itself
    /// succeeded or failed.
    pub async fn cleanup(mut self) {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            // The receiving end lives inside the spawned server task's
            // `with_graceful_shutdown` future; a `Err` here only means that
            // task has already exited on its own, which is harmless.
            let _ = shutdown_tx.send(());
        }
        if let Some(server_task) = self.server_task.take() {
            match server_task.await {
                Ok(Ok(())) => {}
                Ok(Err(io_err)) => {
                    eprintln!(
                        "test_harness: TestApp's listener task exited with an I/O error during \
                         cleanup: {io_err}"
                    );
                }
                Err(join_err) => {
                    eprintln!(
                        "test_harness: TestApp's listener task panicked during cleanup: \
                         {join_err}"
                    );
                }
            }
        }

        self.pool.close().await;
        if let Some(schema) = self.schema.take() {
            drop_schema(&schema).await;
        }
    }
}

impl Drop for TestApp {
    /// Best-effort only (design.md: "`Drop` はベストエフォートのみに留める"):
    /// covers the case where a test panics or otherwise returns without
    /// calling [`TestApp::cleanup`], without ever blocking or panicking
    /// inside this synchronous `drop`. See this module's doc comment
    /// ("Release path") for the full reasoning.
    fn drop(&mut self) {
        // Synchronous and non-blocking: at most wakes up a task that may
        // already be gone.
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }
        // The server task itself is deliberately left to run to completion
        // on its own (it will stop once/if the signal above is observed);
        // this destructor never awaits or aborts it.

        let Some(schema) = self.schema.take() else {
            // Already torn down by `cleanup()` (or by an earlier `Drop`
            // call, though `drop` only ever runs once per value) — nothing
            // left to do.
            return;
        };
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                // Detached: this destructor does not await the spawned
                // task, so dropping a `TestApp` outside `cleanup()` never
                // blocks the current thread or risks panicking from a
                // `block_on` inside `Drop::drop`.
                handle.spawn(async move {
                    drop_schema(&schema).await;
                });
            }
            Err(_) => {
                // No Tokio runtime reachable from this thread: fall back to
                // leaving the orphaned schema in place for a future
                // startup-time sweep to reclaim, rather than risking a panic
                // by trying to drive async cleanup here.
                eprintln!(
                    "test_harness: TestApp dropped without calling cleanup() and outside a \
                     Tokio runtime; leaving orphaned schema {schema} for a future startup sweep"
                );
            }
        }
    }
}

/// Boots a test instance of the application (design.md's "TestHarness"
/// Service Interface): creates a fresh isolated PostgreSQL schema
/// (Requirement 8.4), establishes a pool pinned to it and applies the
/// embedded migrations against it (Requirement 8.2), builds a deterministic
/// [`RuntimeContext`] (Requirement 8.3) whose `keys` boundary is nonetheless
/// the real, DB-backed `DbSigningKeyProvider` (task 6.1, Requirement 6.1 —
/// see [`TestApp::runtime`]'s own doc comment), assembles the actor-model
/// service bundle the same way `bootstrap()`'s production path does
/// (`crate::actor::build_actor_module`), and serves the real foundation
/// router ([`crate::server::build_router`]) on a freshly bound ephemeral TCP
/// listener (Requirement 8.1) — reusing the same Bootstrap building blocks
/// [`crate::bootstrap::bootstrap`] itself composes, rather than
/// reimplementing them (see this module's doc comment).
///
/// Panics if the shared test database (see this module's private
/// `base_test_db_url`, overridable via `KAWASEMI_TEST_DATABASE_URL`) is not
/// reachable, or if any
/// other setup step fails: callers that need to skip in environments with no
/// local PostgreSQL should check reachability themselves before calling this
/// (mirroring `src/db/tests.rs`'s/`src/migrate/tests.rs`'s own
/// `should_run_against_real_database` convention), since this function's
/// design.md-specified signature returns `TestApp` directly, not a `Result`.
pub async fn spawn_test_app() -> TestApp {
    let schema = unique_schema_name();
    create_schema(&schema).await;

    let db_config = DatabaseConfig {
        url: Secret::new(schema_scoped_url(&base_test_db_url(), &schema)),
        max_connections: 5,
        acquire_timeout: Duration::from_secs(5),
    };
    let pool = db::establish_pool(&db_config)
        .await
        .expect("establishing the isolated per-test-instance connection pool must succeed");

    migrate::apply_migrations(&pool)
        .await
        .expect("applying embedded migrations to the isolated test schema must succeed");

    // `clock`/`ids`/`rng` stay deterministic (Requirement 8.3); `keys` is
    // swapped for the real, DB-backed `DbSigningKeyProvider` (task 6.1)
    // instead of `RuntimeContext::deterministic`'s own seed-derived
    // `FixedSigningKeyProvider` placeholder, so a `TestApp`'s
    // `RuntimeContext.keys` exercises the exact same supply path
    // `bootstrap()` wires in production (Requirements 6.1, 6.4) rather than
    // a fixed stand-in. The freshly migrated schema starts with no signing
    // keys at all, so the cache warm below is expected to load zero entries
    // — `ActorService::create_actor`/`SigningKeyService::provision_key`
    // populate it afterward through the same `KeyCache` handle.
    let deterministic = RuntimeContext::deterministic(default_test_seed());
    let cipher: Arc<dyn KeyCipher> =
        Arc::new(ChaCha20Poly1305KeyCipher::new(Secret::new(TEST_KEK)));
    let cache = actor::load_key_cache(&pool, cipher.as_ref())
        .await
        .expect("loading the freshly migrated (empty) signing key cache must succeed");
    let runtime = RuntimeContext {
        clock: deterministic.clock,
        ids: deterministic.ids,
        rng: deterministic.rng,
        keys: Arc::new(DbSigningKeyProvider::new(cache.clone())),
    };
    let actor_module: ActorModule =
        actor::build_actor_module(pool.clone(), runtime.clone(), cipher, cache);

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("binding an ephemeral test-harness listener must succeed");
    let address = listener
        .local_addr()
        .expect("a just-bound listener must have a local address");

    let config = AppConfig {
        server: ServerConfig {
            domain: "test-harness.kawasemi.internal".to_string(),
            bind_addr: address,
            shutdown_grace: Duration::from_secs(1),
        },
        database: db_config,
        log: LogConfig {
            level: LogLevel::Error,
            sql_diagnostic: false,
        },
        actor: ActorConfig {
            kek: Secret::new(TEST_KEK),
        },
        owner: OwnerConfig {
            password: Secret::new(TEST_OWNER_PASSWORD.to_string()),
        },
        oauth: OauthConfig {
            token_hash_key: Secret::new(TEST_TOKEN_HASH_KEY),
        },
    };

    // Assembles the OAuth service bundle (task 7.1) the same way
    // `bootstrap()`'s production path does (`OauthModule::new`), from this
    // instance's own fixed, non-production `oauth.token_hash_key`/
    // `owner.password` (see `config` above). `cookie_secure: false` mirrors
    // `bootstrap.rs`'s own production default (this test harness never
    // serves over TLS either).
    let oauth_module = OauthModule::new(
        pool.clone(),
        runtime.clone(),
        config.oauth.token_hash_key.clone(),
        config.owner.clone(),
        false,
    );

    let state = AppState::new(
        pool.clone(),
        runtime.clone(),
        config,
        actor_module.clone(),
        oauth_module,
    );
    let router = server::build_router(state.clone());

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let server_task = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await
    });

    TestApp {
        address,
        pool,
        runtime,
        actor: actor_module,
        state,
        schema: Some(schema),
        shutdown_tx: Some(shutdown_tx),
        server_task: Some(server_task),
    }
}
