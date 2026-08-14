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
//! `spawn_test_app` deliberately reuses the same building blocks
//! [`crate::bootstrap::bootstrap`] does — [`crate::db::establish_pool`],
//! [`crate::migrate::apply_migrations`],
//! [`crate::runtime::RuntimeContext::deterministic`],
//! [`crate::bootstrap::wiring::compose_modules`],
//! [`crate::state::AppState::new`], and [`crate::server::build_router`] —
//! rather than reimplementing any of them (design.md: "Outbound: Bootstrap
//! 構成要素... を再利用"). In particular the nine-module wiring sequence is *not*
//! spelled out here at all: this module's former stage-by-stage copy of it
//! was replaced with a single `compose_modules` call, so production, this
//! harness, and [`crate::federation::test_harness`] all run one shared
//! implementation. It does not reuse
//! [`crate::bootstrap::bootstrap`]/[`crate::server::serve_with_shutdown_and_signal`]
//! directly, because both bind a caller-supplied, fixed
//! [`std::net::SocketAddr`] internally without handing the actually-bound
//! address back — this module instead binds an ephemeral `127.0.0.1:0`
//! listener itself so that many `#[tokio::test]` functions (which `cargo
//! test` runs concurrently by default) can each get their own real,
//! non-colliding port (Requirement 8.1), and so [`TestApp::address`] can
//! report the real bound [`std::net::SocketAddr`] back to the caller.
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
//! ## Release path: `cleanup()` vs `Drop`
//! [`TestApp::cleanup`] is the explicit, synchronous release path: it signals
//! the listener's graceful shutdown and awaits it actually stopping, closes
//! the shared pool, and then drops the isolated schema — in that order, so
//! the schema is only dropped once nothing still holds a connection pinned to
//! it. It remains the only path that has provably finished releasing
//! everything by the time it returns, and every existing caller keeps using
//! it unchanged.
//!
//! `Drop for TestApp` covers the case where a test panics, returns early, or
//! simply never calls `cleanup()` — which 131 call sites in this repository
//! do. It cannot do the work itself: `drop` is synchronous, and the release
//! steps are `async`. It used to detach them onto
//! `tokio::runtime::Handle::try_current`, but a `#[tokio::test]`'s runtime is
//! destroyed the moment the test function returns, so a task spawned from a
//! destructor running on that runtime never completes — and the pool was
//! never closed at all, only the schema drop was attempted. The leak was
//! measured directly: 8 dropped instances still holding 40 connections three
//! seconds later.
//!
//! `Drop` now (a) fires the same shutdown signal `cleanup` would (sending on
//! a `oneshot::Sender` is synchronous, non-blocking and infallible here) and
//! (b) hands a *clone* of the pool plus the isolated schema name to
//! [`reaper::HarnessReaper`], the process-resident executor that owns its own
//! runtime on its own thread and therefore outlives every per-test runtime.
//! Submitting is a lock-free channel push: it never blocks and never panics,
//! which is what makes it legal inside a destructor that may be running
//! during an unwind. The reclaim itself (close the pool, then drop the
//! schema) happens on the reaper's runtime, after the caller's runtime is
//! gone. `Drop` still never blocks, never awaits, and never reports failure.
//!
//! That delegation is asynchronous, and therefore best-effort at process
//! exit: the reaper is deliberately never shut down, so requests still queued
//! when the test binary exits are simply lost together with it (see
//! [`reaper::HarnessReaper::global`]). Their schemas stay on the server as
//! residue for the startup sweep ([`sweep`]) to reclaim on a later run. What
//! `Drop` guarantees is that a leaked fixture is *queued* for release, not
//! that it has been released by any particular moment — only `cleanup()`
//! guarantees that.
//!
//! The two paths meet on every `cleanup()` call site, since `cleanup` takes
//! `self` by value and `Drop` runs immediately afterwards. They do not
//! collide: `cleanup` takes the `schema` out of the `Option`, and `Drop`
//! submits nothing when it finds `None`, so a cleaned-up instance generates
//! no redundant reclaim at all. (Were one ever generated, the reaper's own
//! contract makes it harmless — `Pool::close` is idempotent and the schema
//! drop is `IF EXISTS` — but not generating it is cheaper and keeps the
//! reaper's queue proportional to the fixtures that actually leaked.)

#[cfg(test)]
mod tests;

/// The resident reclaim executor every `TestApp`/`TestDb` destructor hands
/// its pool and isolated schema to.
/// Not `#[cfg(test)]`: `tests/*.rs` integration binaries drop harness
/// fixtures too, and their destructors need the same release path this
/// crate's own unit tests get.
pub(crate) mod reaper;

/// Startup reclaim of schemas earlier runs left behind.
/// Not `#[cfg(test)]` for the same reason
/// [`reaper`] is not: `tests/*.rs` integration binaries spawn fixtures too,
/// and the safety net has to cover the residue they leave.
///
/// `pub` rather than `pub(crate)` (unlike [`reaper`]) only because
/// `tests/harness_sweep_it.rs` has to make a sweep happen on demand and count
/// the ones that happened by themselves; both entry points it needs are
/// documented as the harness's own test surface. The module's internals —
/// [`sweep::is_reclaimable`], the prefix, the threshold — stay crate-private,
/// and the whole of `test_harness` leaves the shipped library together in a
/// build without the `test-harness` feature.
pub mod sweep;

/// The lightweight fixture tier: an isolated schema and a migrated pool with
/// no running instance around them. Not `#[cfg(test)]`, for the same reason
/// [`reaper`] is not:
/// `tests/*.rs` integration binaries are a separate crate and can only see
/// `pub` items.
pub mod db_fixture;

/// SQL-statement counting for the tests that pin what a code path costs in
/// queries.
///
/// Gated exactly like the rest of this module rather than at `#[cfg(test)]`,
/// for the same reason [`reaper`] is not `#[cfg(test)]`: the paths whose query
/// counts are worth pinning are exercised from `tests/*.rs` integration
/// binaries as well as from this crate's own unit tests, and the narrower gate
/// would let the measurement decide where such a test is allowed to live —
/// a placement constraint, not a property of the measurement.
///
/// Widening the gate does not widen the shipped library. `src/lib.rs` declares
/// `test_harness` itself under this very `cfg`, so a build without the
/// `test-harness` feature drops the module declaration and the whole subtree
/// under it, this one included; the counting machinery leaves the distributed
/// artifact together with the fixed credentials it sits beside, exactly as
/// before. What the change moves is the boundary between "compiled for the
/// lib's own tests" and "compiled for every test build", never the boundary
/// between test builds and shipped ones.
///
/// `pub` rather than `pub(crate)` (like [`sweep`] and [`db_fixture`], unlike
/// [`reaper`]) because `tests/*.rs` is a separate crate and can only see `pub`
/// items — and no re-export can stand in for that, since `pub use` cannot
/// widen the visibility an item declares for itself.
#[cfg(any(test, feature = "test-harness"))]
pub mod query_log;

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
use crate::bootstrap::wiring::{
    ComposedModules, FederationPollCadence, ModuleWiringInput, compose_modules,
};
use crate::config::{
    ActorConfig, AppConfig, DatabaseConfig, FederationConfig, LogConfig, LogLevel, MediaConfig,
    OauthConfig, OwnerConfig, Secret, ServerConfig, StatusesConfig,
};
use crate::db;
use crate::federation::signatures::ReqwestFederationHttpClient;
use crate::migrate;
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

/// Every [`spawn_test_app`] call's delivery-worker poll interval (task 5.4):
/// far shorter than [`crate::federation::module::DEFAULT_DELIVERY_POLL_INTERVAL`]'s
/// production default so an integration test proving delivery completion
/// (e.g. observing a `delivery_jobs` row transition after enqueuing) does
/// not need to wait several seconds per assertion.
const TEST_DELIVERY_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Every [`spawn_test_app`] call's delivery-worker batch size. Identical to
/// [`crate::federation::module::DEFAULT_DELIVERY_POLL_BATCH_SIZE`]'s
/// production default — only the two *intervals* around it are shortened for
/// tests. Named (rather than left as a literal inside the
/// [`FederationPollCadence`] construction) so this harness's whole
/// background-loop cadence reads as one group.
const TEST_DELIVERY_POLL_BATCH_SIZE: i64 = 20;

/// Every [`spawn_test_app`] call's received-Activity pruning-loop interval
/// (task 5.4): short for the same reason as [`TEST_DELIVERY_POLL_INTERVAL`],
/// though no test in this crate currently depends on pruning actually
/// running within a test's lifetime (task 3.1's own unit tests already
/// cover `prune_expired`'s correctness directly).
const TEST_PRUNING_INTERVAL: Duration = Duration::from_secs(5);

/// Resolves the shared test database's connection URL: an explicit
/// `KAWASEMI_TEST_DATABASE_URL` override if set, otherwise
/// [`DEFAULT_TEST_DB_URL`].
fn base_test_db_url() -> String {
    std::env::var(TEST_DB_URL_ENV).unwrap_or_else(|_| DEFAULT_TEST_DB_URL.to_string())
}

/// Generates a `LocalFsStore` root unique to this call, under the OS temp
/// directory rather than a path relative to the process's current working
/// directory (task 5.2). `crate::config::MediaConfig::storage_root`'s own
/// production default (`"media_storage"`, resolved against the process's
/// cwd) is safe for a real single long-running deployment, but every prior
/// task's own `MediaConfig` fixture in this file used that same literal
/// relative default without consequence — nothing exercised the real,
/// composition-root-wired `LocalFsStore` end to end until this task's own
/// `ProcessingWorker`/mounted endpoints made it actually write bytes.
/// Reusing that fixed relative path here would mean every `spawn_test_app`
/// call (and every `cargo test` run) accumulates ever-growing, never-cleaned
/// files directly inside this repository's own working directory. Mirrors
/// `src/media/local_fs.rs`'s/`tests/media_endpoints_it.rs`'s own established
/// `unique_temp_root` convention (counter + wall-clock nanoseconds under
/// `std::env::temp_dir()`) — left for the OS's own temp-directory lifecycle
/// to reclaim, exactly like every other test-only temp root in this crate,
/// rather than adding a bespoke `Drop`-based cleanup path to `TestApp` for
/// this one config value.
fn unique_media_storage_root() -> std::path::PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("kawasemi_test_harness_media_storage_{nanos}_{seq}"))
}

/// Generates a schema name unique to this process/run: a monotonic counter
/// plus wall-clock nanoseconds, so concurrently-running `#[tokio::test]`
/// functions (and repeated `cargo test` invocations) never collide. Mirrors
/// `src/migrate/tests.rs`'s `unique_schema_name` convention.
///
/// The embedded nanosecond timestamp is not decoration: it is the only
/// evidence [`sweep::is_reclaimable`] has for deciding whether an abandoned
/// schema is stale residue or a live process's workspace. The prefix is taken
/// from [`sweep::HARNESS_SCHEMA_PREFIX`] rather than spelled here so that the
/// two sides of that convention cannot drift apart silently.
fn unique_schema_name() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{}{nanos}_{seq}", sweep::HARNESS_SCHEMA_PREFIX)
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
/// (either from [`TestApp::cleanup`] or from [`reaper::HarnessReaper`]), the
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

/// The isolated database a fixture is built on: a freshly created schema, a
/// pool pinned to it with the embedded migrations already applied, and the
/// `DatabaseConfig` that pool was established from.
///
/// Exists so every fixture in this crate — [`spawn_test_app`],
/// [`db_fixture::spawn_test_db`] and
/// [`crate::federation::test_harness::spawn_federation_pair`]'s own paired
/// instances — shares one implementation of the schema/pool/migrate sequence
/// rather than each carrying its own copy. That sequence is exactly the part
/// they have in common, and the part whose details (the `search_path` pinning
/// convention, the pool size, the schema-name prefix, the startup sweep) must
/// not drift between them: a second copy would mean a fixture isolated by
/// different rules than a `TestApp`, and — because both the startup sweep and
/// [`reaper::HarnessReaper`] recognize a reclaimable schema by that one
/// prefix — one whose abandoned schemas no reclamation path would ever find.
pub(crate) struct IsolatedDb {
    pub(crate) pool: PgPool,
    pub(crate) schema: String,
    /// Retained because [`spawn_test_app`] needs the very same
    /// `DatabaseConfig` inside the `AppConfig` it synthesizes, and rebuilding
    /// it there would reintroduce the schema-scoped URL construction this
    /// helper exists to own.
    pub(crate) db_config: DatabaseConfig,
}

/// Creates a fresh isolated schema and returns a migrated pool pinned to it
/// running the once-per-process startup sweep first.
/// Panics on any failure, for the reason [`create_schema`] documents: a
/// caller cannot do anything useful with a partially-initialized fixture.
pub(crate) async fn establish_isolated_db() -> IsolatedDb {
    // Before anything else in the process's first fixture: reclaim what
    // earlier runs abandoned, so a suite never has to be preceded by a manual
    // cleanup step. Subsequent calls return immediately —
    // the once-per-process guard lives inside
    // `sweep_orphans`, so every fixture built on this helper inherits it by
    // calling the same function rather than by repeating the trigger.
    sweep::sweep_orphans().await;

    let schema = unique_schema_name();
    create_schema(&schema).await;

    // `max_connections: 2` rather than 5: every connection here is opened
    // eagerly, so a pool's full size is the unit in which a fixture costs
    // the shared server. This started as a mitigation for `Drop` releasing
    // nothing at all (at 5, the server's ~97 usable slots were exhausted
    // after ~19 uncleaned instances, which is what made a single-process
    // `cargo test --lib` run fail en masse with `PoolTimedOut`); `Drop` now
    // hands the pool to `reaper::HarnessReaper`, so uncleaned instances are
    // reclaimed rather than accumulated. The reduced size is kept because it
    // still bounds what is held while a reclaim is in flight, and because
    // no test needs more.
    //
    // Not 1: at a single connection
    // `tests/federation_outbound_worker_it.rs`'s
    // `run_once_marks_a_job_failed_immediately_when_sender_no_longer_resolves`
    // deterministically claims
    // zero jobs. That is not a `claim_due` defect: this fixture also starts
    // the real delivery-worker loop (`federation_background.spawn()` below,
    // polling every `TEST_DELIVERY_POLL_INTERVAL`), so two workers compete
    // for the same `delivery_jobs` rows and `FOR UPDATE SKIP LOCKED` hands
    // the row to whichever claims first — exactly its contract. Pool size
    // only decides who wins: at 2 the background loop's first poll gets its
    // own connection immediately and runs against a still-empty table,
    // whereas at 1 its already-queued `acquire` is served the moment the
    // test's `enqueue` releases the sole connection, i.e. always just
    // before the test's own claim. The race exists at 2 as well, only far
    // more rarely (measured 1 failure in 34 runs). Pool size only decides who
    // wins the race, never whether the row is processed exactly once.
    let db_config = DatabaseConfig {
        url: Secret::new(schema_scoped_url(&base_test_db_url(), &schema)),
        max_connections: 2,
        acquire_timeout: Duration::from_secs(5),
    };
    let pool = db::establish_pool(&db_config)
        .await
        .expect("establishing the isolated per-test-instance connection pool must succeed");

    migrate::apply_migrations(&pool)
        .await
        .expect("applying embedded migrations to the isolated test schema must succeed");

    IsolatedDb {
        pool,
        schema,
        db_config,
    }
}

/// A running test instance of the application (design.md's "TestHarness"
/// Service Interface): a real, connectable [`address`](Self::address), a
/// [`pool`](Self::pool) pinned to a schema isolated from every other
/// `TestApp`, and a [`runtime`](Self::runtime) built from
/// [`RuntimeContext::deterministic`].
///
/// [`TestApp::cleanup`] remains the explicit release path callers should
/// prefer (Requirement 8.5): it is the only one that has provably finished by
/// the time it returns. Omitting it no longer leaks for the rest of the
/// process, though — `Drop` hands the pool and schema to
/// [`reaper::HarnessReaper`] instead. That hand-off is best-effort at process
/// exit: a request still queued
/// when the test binary exits is lost with the reaper, leaving a stale schema
/// for the startup sweep to reclaim on a later run. See this module's doc
/// comment ("Release path") for how the two differ.
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
    /// takes it, so the release is requested exactly once even though `Drop`
    /// always runs (including immediately after a successful `cleanup()`
    /// call, since `cleanup` takes `self` by value — that is the case in
    /// which `Drop` finds `None` and submits nothing to the reaper). Not part of
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

/// The already-built parts [`TestApp::from_parts`] bundles into a
/// [`TestApp`] (task 6.4, `Boundary: FederationTestHarness,
/// federation_pair_it`) — a plain data bundle (mirroring this crate's own
/// `FederationWiringConfig`-style "group a constructor's inputs into a named
/// struct" convention) rather than a long positional parameter list.
pub(crate) struct TestAppParts {
    pub(crate) address: SocketAddr,
    pub(crate) pool: PgPool,
    pub(crate) runtime: RuntimeContext,
    pub(crate) actor: ActorModule,
    pub(crate) state: AppState,
    pub(crate) schema: String,
    pub(crate) shutdown_tx: oneshot::Sender<()>,
    pub(crate) server_task: JoinHandle<std::io::Result<()>>,
}

impl TestApp {
    /// `pub(crate)` constructor from already-built parts (task 6.4,
    /// `Boundary: FederationTestHarness, federation_pair_it`): lets
    /// [`crate::federation::test_harness::spawn_federation_pair`] reuse
    /// `TestApp`'s own release lifecycle (`cleanup()`/`Drop`) for the paired
    /// instances it composes itself — with a different `domain`
    /// (`cfg.server.domain`/`FederationWiringConfig.domain`, set to that
    /// instance's own bound address rather than [`spawn_test_app`]'s fixed
    /// `"test-harness.kawasemi.internal"`) and a different
    /// [`crate::federation::signatures::ReqwestFederationHttpClient`]
    /// (`insecure_loopback()` rather than `new()`) — without duplicating
    /// [`TestApp`]'s own struct fields or [`Drop`] logic in that module.
    /// `pub(crate)` (not `pub`): [`TestApp`]'s external contract stays
    /// exactly "boot a full app via [`spawn_test_app`]/`spawn_federation_pair`;
    /// call [`Self::cleanup`] when done" — no `tests/*.rs` integration test
    /// (a separate crate, seeing only `pub` items) can reach this
    /// constructor to build a `TestApp` from arbitrary parts of its own.
    pub(crate) fn from_parts(parts: TestAppParts) -> Self {
        Self {
            address: parts.address,
            pool: parts.pool,
            runtime: parts.runtime,
            actor: parts.actor,
            state: parts.state,
            schema: Some(parts.schema),
            shutdown_tx: Some(parts.shutdown_tx),
            server_task: Some(parts.server_task),
        }
    }

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
    /// Delegates release to the process-resident [`reaper::HarnessReaper`]
    /// covering the case where a
    /// test panics or otherwise returns without calling [`TestApp::cleanup`].
    /// Every step here is synchronous, non-blocking and infallible, because a
    /// panic in a destructor running during an unwind aborts the process. See
    /// this module's doc comment ("Release path") for the full reasoning.
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
            // Already released by `cleanup()`, which closed the pool and
            // dropped the schema synchronously. Submitting anyway would be
            // harmless (the reaper's reclaim is idempotent) but would queue
            // work behind the fixtures that genuinely leaked, so the
            // take-once `Option` is what keeps the reaper's queue
            // proportional to the actual leak.
            return;
        };
        // A `Pool` clone shares one inner state with the handle this struct
        // still owns, so the reaper closing its clone closes this pool — and
        // cloning is a refcount bump, not an operation that can block or
        // fail. The reclaim runs on the reaper's own runtime, which outlives
        // the per-test runtime this destructor is executing on.
        reaper::HarnessReaper::global()
            .submit(reaper::ReclaimRequest::new(self.pool.clone(), schema));
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
    let IsolatedDb {
        pool,
        schema,
        db_config,
    } = establish_isolated_db().await;

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
        federation: FederationConfig {
            secure_mode: false,
            public_key_cache_ttl: Duration::from_secs(24 * 60 * 60),
            received_activity_retention_days: 14,
        },
        // media-pipeline task 1.2: fixed, non-production values mirroring
        // `load_config_from`'s own defaults (`src/config.rs`) — this test
        // harness constructs `AppConfig` directly rather than through TOML/
        // env parsing, the same way every other startup-config group above
        // is fixed here rather than loaded.
        media: MediaConfig {
            storage_root: unique_media_storage_root(),
            max_upload_size_bytes: 10 * 1024 * 1024,
            thumbnail_target_width: 400,
            thumbnail_target_height: 400,
            supported_formats: vec![
                "image/jpeg".to_string(),
                "image/png".to_string(),
                "image/gif".to_string(),
                "image/webp".to_string(),
            ],
            worker_concurrency: 2,
            max_retry_attempts: 5,
            lease_duration: Duration::from_secs(5 * 60),
        },
        // statuses-core task 7.2: fixed, non-production values mirroring
        // `load_config_from`'s own defaults, the same convention every other
        // startup-config group above already follows in this harness.
        statuses: StatusesConfig {
            max_content_chars: 500,
            poll_max_options: 4,
            poll_min_expiration: Duration::from_secs(5 * 60),
            idempotency_key_retention_days: 7,
        },
    };

    // Runs the module-wiring sequence. Everything this function used to
    // spell out here — the nine
    // module builders, `statuses::register_account_ports`, and the
    // load-bearing ordering between them (`build_statuses_module` ->
    // `register_account_ports` -> `build_social_graph_module`) — now lives
    // exactly once in `crate::bootstrap::wiring`, shared with `bootstrap()`'s
    // production path and the federation-pair harness. This harness no
    // longer "recomposes the same building blocks `bootstrap()` does" stage
    // by stage (see this module's own doc comment): it calls the same
    // sequence.
    //
    // What deliberately stays here is only what is genuinely specific to
    // this startup path: the isolated-schema `pool`, the
    // deterministic `runtime`, the ephemeral listener bound above, the
    // synthesized `config` above, and the never-resolving background-task
    // shutdown signal below.
    let ComposedModules {
        oauth,
        federation,
        media,
        accounts,
        statuses,
        social_graph,
        timelines,
        notifications,
        search,
        federation_background,
        media_background,
    } = compose_modules(ModuleWiringInput {
        pool: pool.clone(),
        runtime: runtime.clone(),
        actor_module: &actor_module,
        config: &config,
        // `ReqwestFederationHttpClient::new()` — the production constructor,
        // same as `bootstrap()`'s own path (only
        // `crate::federation::test_harness` needs `insecure_loopback()`).
        // A single shared instance where this function previously built
        // four independent ones (statuses-core's fetcher, social graph's
        // fetcher, the federation module, and the accounts module); only the
        // underlying connection pool is now shared, the requests emitted are
        // unchanged. See `wiring`'s own "Shared federation HTTP client".
        http_client: Arc::new(ReqwestFederationHttpClient::new()),
        // Much shorter poll intervals than production, so integration tests
        // observing delivery/pruning completion do not need to wait
        // production's several-seconds interval (see
        // `TEST_DELIVERY_POLL_INTERVAL`'s own doc comment).
        federation_cadence: FederationPollCadence {
            delivery_poll_interval: TEST_DELIVERY_POLL_INTERVAL,
            delivery_poll_batch_size: TEST_DELIVERY_POLL_BATCH_SIZE,
            pruning_interval: TEST_PRUNING_INTERVAL,
        },
    })
    .await
    .expect("composing this test instance's module wiring must succeed");

    // `compose_modules` returns both background handles unstarted so each
    // startup path can spawn them against its own shutdown signal. This
    // harness starts them with a signal that never
    // resolves (`std::future::pending`): tests never explicitly stop these
    // background tasks; the per-test tokio runtime simply aborts them when
    // the test function returns (`FederationBackgroundTasks`'s own doc
    // comment says as much). Plumbing this harness's own single-consumer
    // `oneshot`-based HTTP listener shutdown through them instead is not an
    // option — it cannot fan out to several worker tasks.
    federation_background.spawn();
    media_background.spawn(std::future::pending::<()>);

    let state = AppState::new(
        pool.clone(),
        runtime.clone(),
        config,
        actor_module.clone(),
        oauth,
        federation,
        media,
        accounts,
        statuses,
        social_graph,
        timelines,
        notifications,
        search,
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
