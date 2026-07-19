//! Composition Root for the application startup sequence (Bootstrap
//! boundary, Requirements 1.1, 1.2, design.md's "Bootstrap" component).
//!
//! Sequences dependencies in exactly the order design.md's Bootstrap
//! component and "起動シーケンスと安全停止" flowchart specify: config ->
//! telemetry -> db pool -> migrate -> runtime context -> `AppState` -> serve.
//! Any failure at any of these pre-serve stages aborts before the HTTP
//! listener ever starts (Requirement 1.2) and is aggregated into a single
//! [`BootstrapError`] that retains the real underlying `*Error` (never
//! stringly-typed), per design.md's Error Strategy: "起動時エラー
//! （Config/Db/Migrate/Telemetry/Bootstrap）は `*Error` 型で原因を保持し、
//! `BootstrapError` に集約して `main` が診断出力 + 非ゼロ終了に変換する".
//!
//! As of actor-model's task 6.1, the "runtime context" stage itself expands
//! into: load every active signing key from the database, open each one's
//! sealed private key via a KEK-bound `ChaCha20Poly1305KeyCipher`, warm a
//! `KeyCache`, build the real `DbSigningKeyProvider` around it, and *then*
//! construct `RuntimeContext` around that provider (replacing
//! [`crate::runtime::RuntimeContext::production`]'s
//! `FixedSigningKeyProvider` placeholder) — see [`build_actor_wiring`]. A
//! failure at this stage surfaces as [`BootstrapError::KeySupply`], still
//! strictly before the HTTP listener is touched.
//!
//! ## Diagnostic-output placement
//! `main.rs` already turns every `Err(BootstrapError)` it receives into an
//! `eprintln!` (using `BootstrapError`'s `Display`, which forwards to each
//! wrapped stage error's own already-descriptive `Display`) plus a non-zero
//! exit, uniformly regardless of which stage failed — including the
//! earliest stages (config, telemetry), for which no `tracing` subscriber
//! may exist yet to log through. This module therefore does not duplicate
//! that print for config/telemetry failures. For the later stages (db pool,
//! migrate, and the HTTP listener bind inside `serve`), telemetry has
//! already been initialized by the time they run, so this module also emits
//! a structured `tracing::error!` before propagating — additional
//! diagnostic detail a log-aggregation setup can capture even if the
//! process is torn down before `main`'s `eprintln!` is otherwise visible.
//!
//! ## `pub mod bootstrap` on the lib target, and the `tests/*.rs` split
//! `bootstrap` is exposed as `pub mod bootstrap` from `src/lib.rs` (rather
//! than staying a `main.rs`-local module as it was through task 7.3) so this
//! task's own integration tests can reach it. Those tests live under
//! `tests/bootstrap_lifecycle_it.rs` and `tests/bootstrap_fail_fast_it.rs`
//! — separate compiled test binaries/processes — rather than as a
//! `#[cfg(test)] mod tests` inside this file (this codebase's usual
//! convention for integration-style tests, see `src/db/tests.rs`,
//! `src/migrate/tests.rs`, `src/server/tests.rs`). That deviation is
//! necessary, not stylistic: `bootstrap()` calls
//! `crate::telemetry::init_telemetry`, which installs a *global*,
//! install-once-per-process `tracing` subscriber (see `src/telemetry.rs`).
//! `src/telemetry/tests.rs` already has a unit test that deliberately calls
//! `init_telemetry` twice in the same process to prove the second call fails
//! with `TelemetryError::AlreadyInitialized`. If a `bootstrap()`-driving test
//! lived in the same `--lib` unit-test binary/process as that test, the two
//! would race for the one-time global install regardless of which file
//! declared the test, corrupting whichever one lost the race — not a flaw
//! in either test, an unavoidable consequence of a process-global resource.
//! Compiling this task's tests as separate `tests/*.rs` binaries (each an
//! independent process, Cargo's normal behavior for that directory) removes
//! the shared process entirely. The same reasoning caps each of those two
//! files at exactly one test that drives `bootstrap()`/
//! `bootstrap_with_shutdown_signal` far enough to reach `init_telemetry`
//! successfully — see their own module doc comments.
//!
//! ## Reusability for task 8.1 (TestHarness, not implemented here)
//! Every step `bootstrap()` composes — [`crate::config::load_config`],
//! [`crate::telemetry::init_telemetry`], [`crate::db::establish_pool`],
//! [`crate::migrate::apply_migrations`],
//! [`crate::runtime::RuntimeContext::production`] /
//! [`crate::runtime::RuntimeContext::deterministic`], and
//! [`crate::state::AppState::new`] — is already an independently `pub`
//! function/constructor on its own owning module, not something buried
//! unreachably inside this file. A future test harness (task 8.1) that needs
//! an isolated database and a deterministic `RuntimeContext` (neither of
//! which this module's own production-oriented `bootstrap()` /
//! [`bootstrap_with_shutdown_signal`] parameterizes) can therefore recompose
//! those same steps directly against its own `DatabaseConfig`/seed, without
//! needing this file's internal (private) [`build_state`] to become
//! reusable as one atomic unit. This file's own public surface stays limited
//! to [`bootstrap`] (design.md's exact documented interface) plus
//! [`bootstrap_with_shutdown_signal`] (this task's own test seam, see
//! above) — nothing here is a dead end for 8.1's purposes.

use std::fmt;
use std::future::Future;
use std::sync::Arc;

use sqlx::PgPool;

use crate::actor;
use crate::actor::ActorModule;
use crate::actor::keys::cache::KeyCache;
use crate::actor::keys::cipher::{ChaCha20Poly1305KeyCipher, KeyCipher};
use crate::actor::keys::provider::DbSigningKeyProvider;
use crate::config::{self, AppConfig, ConfigError};
use crate::db::{self, DbError};
use crate::error::AppError;
use crate::federation::signatures::ReqwestFederationHttpClient;
use crate::federation::{self, FederationWiringConfig};
use crate::media;
use crate::migrate::{self, MigrateError};
use crate::oauth::OauthModule;
use crate::runtime::{RuntimeContext, SnowflakeIdGenerator, SystemClock, SystemRng};
use crate::server::{self, ServeError};
use crate::state::AppState;
use crate::telemetry::{self, TelemetryError};

#[cfg(test)]
mod tests;

/// Aggregated startup failure (design.md's Bootstrap Service Interface,
/// Requirement 1.2). Each variant retains the real, original error from the
/// stage that failed — never a stringly-typed summary — so a caller (here,
/// `main.rs`) can render full diagnostic detail (via `Display`, which
/// forwards to the wrapped error) and/or inspect the cause programmatically
/// (via `Error::source`).
#[derive(Debug)]
pub enum BootstrapError {
    /// Startup configuration failed to load or validate (Requirements 2.3,
    /// 2.4), before telemetry, the db pool, or migrations were ever touched.
    Config(ConfigError),
    /// The structured-logging subscriber failed to initialize.
    Telemetry(TelemetryError),
    /// The database connection pool could not be established (Requirement
    /// 3.2), before migrations or the HTTP listener were ever touched.
    Db(DbError),
    /// The embedded migration set failed to apply, or a checksum
    /// inconsistency was detected (Requirements 4.5, 4.6), before the HTTP
    /// listener was ever touched.
    Migrate(MigrateError),
    /// Loading actor-model's persisted signing keys (to warm the startup
    /// `KeyCache` that backs the real `DbSigningKeyProvider`, task 6.1,
    /// Requirement 6.1) failed — either the database read itself failed, or
    /// a persisted key's sealed private key could not be opened (e.g. the
    /// configured `actor.kek` does not match the KEK the key was originally
    /// sealed under). Surfaces before the HTTP listener is ever touched,
    /// same as every other pre-serve stage.
    KeySupply(AppError),
    /// The HTTP listener failed to bind.
    Serve(ServeError),
}

impl fmt::Display for BootstrapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BootstrapError::Config(e) => {
                write!(f, "startup aborted while loading configuration: {e}")
            }
            BootstrapError::Telemetry(e) => {
                write!(f, "startup aborted while initializing telemetry: {e}")
            }
            BootstrapError::Db(e) => {
                write!(
                    f,
                    "startup aborted while establishing the database pool: {e}"
                )
            }
            BootstrapError::Migrate(e) => {
                write!(f, "startup aborted while applying embedded migrations: {e}")
            }
            BootstrapError::KeySupply(e) => {
                // `AppError` implements neither `Display` nor
                // `std::error::Error` (see `src/error.rs`; a core-runtime
                // type out of this task's boundary to change), so `Debug`
                // is the best available rendering here.
                write!(f, "startup aborted while loading actor signing keys: {e:?}")
            }
            BootstrapError::Serve(e) => {
                write!(f, "startup aborted while starting the HTTP listener: {e}")
            }
        }
    }
}

impl std::error::Error for BootstrapError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            BootstrapError::Config(e) => Some(e),
            BootstrapError::Telemetry(e) => Some(e),
            BootstrapError::Db(e) => Some(e),
            BootstrapError::Migrate(e) => Some(e),
            // `AppError` does not implement `std::error::Error`, so there is
            // no `&dyn Error` to hand back here — see this variant's
            // `Display` arm above for why. Diagnostic detail is still
            // available via `Display`/`Debug` and the `tracing::error!`
            // this module emits before wrapping it (see
            // `load_key_cache_with_diagnostics`).
            BootstrapError::KeySupply(_) => None,
            BootstrapError::Serve(e) => Some(e),
        }
    }
}

impl From<ConfigError> for BootstrapError {
    fn from(e: ConfigError) -> Self {
        BootstrapError::Config(e)
    }
}

impl From<TelemetryError> for BootstrapError {
    fn from(e: TelemetryError) -> Self {
        BootstrapError::Telemetry(e)
    }
}

impl From<DbError> for BootstrapError {
    fn from(e: DbError) -> Self {
        BootstrapError::Db(e)
    }
}

impl From<MigrateError> for BootstrapError {
    fn from(e: MigrateError) -> Self {
        BootstrapError::Migrate(e)
    }
}

impl From<AppError> for BootstrapError {
    fn from(e: AppError) -> Self {
        BootstrapError::KeySupply(e)
    }
}

impl From<ServeError> for BootstrapError {
    fn from(e: ServeError) -> Self {
        BootstrapError::Serve(e)
    }
}

/// Establishes the database pool for `cfg`, logging a structured diagnostic
/// (telemetry is already initialized by the time this stage runs — see this
/// module's doc comment) before converting a failure into a
/// [`BootstrapError::Db`] (Requirement 3.2).
async fn establish_pool_with_diagnostics(cfg: &AppConfig) -> Result<PgPool, BootstrapError> {
    db::establish_pool(&cfg.database).await.map_err(|err| {
        tracing::error!(
            error = %err,
            "startup aborted while establishing the database pool"
        );
        BootstrapError::Db(err)
    })
}

/// Applies embedded migrations to `pool`, logging a structured diagnostic
/// before converting a failure into a [`BootstrapError::Migrate`]
/// (Requirements 4.5, 4.6).
async fn apply_migrations_with_diagnostics(pool: &PgPool) -> Result<(), BootstrapError> {
    migrate::apply_migrations(pool).await.map_err(|err| {
        tracing::error!(
            error = %err,
            "startup aborted while applying embedded migrations"
        );
        BootstrapError::Migrate(err)
    })
}

/// Loads and opens every currently active signing key (via
/// [`crate::actor::load_key_cache`]), logging a structured diagnostic before
/// converting a failure into a [`BootstrapError::KeySupply`] (Requirement
/// 6.1). Mirrors [`establish_pool_with_diagnostics`]/
/// [`apply_migrations_with_diagnostics`]'s own `*_with_diagnostics`
/// convention: telemetry is already initialized by the time this stage
/// runs, so a structured `tracing::error!` is emitted here in addition to
/// the aggregated `BootstrapError` this function returns.
///
/// `error = ?err` (not `%err`, this module's usual convention) because
/// [`AppError`] implements `Debug` but not `Display` — see this module's
/// `BootstrapError::KeySupply` `Display` arm for the same reason.
async fn load_key_cache_with_diagnostics(
    pool: &PgPool,
    cipher: &dyn KeyCipher,
) -> Result<KeyCache, BootstrapError> {
    actor::load_key_cache(pool, cipher).await.map_err(|err| {
        tracing::error!(error = ?err, "startup aborted while loading actor signing keys");
        BootstrapError::KeySupply(err)
    })
}

/// Assembles the actor-model wiring (design.md's "ActorModule(bootstrap
/// wiring)" component, Requirements 6.1, 6.4): builds a KEK-bound
/// `ChaCha20Poly1305KeyCipher` from `cfg.actor.kek`, warms a `KeyCache` from
/// every currently active signing key in `pool`
/// ([`load_key_cache_with_diagnostics`]), builds the real
/// `DbSigningKeyProvider` around that cache, and constructs a full
/// production `RuntimeContext` around *that* provider instead of
/// [`RuntimeContext::production`]'s `FixedSigningKeyProvider` placeholder —
/// this task's whole point (see `RuntimeContext::production`'s own doc
/// comment). Finally builds the three actor-model services
/// (`SigningKeyService`, `ActorService`, `ActorDirectory`) via
/// [`crate::actor::build_actor_module`], bundled as an [`ActorModule`].
///
/// Returns the freshly built `RuntimeContext` alongside the `ActorModule`
/// because both are needed by [`build_state`]'s final `AppState::new` call,
/// and this is the one place a real (non-placeholder) `RuntimeContext` is
/// constructed for the production bootstrap path.
async fn build_actor_wiring(
    pool: &PgPool,
    cfg: &AppConfig,
) -> Result<(RuntimeContext, ActorModule), BootstrapError> {
    let cipher: Arc<dyn KeyCipher> =
        Arc::new(ChaCha20Poly1305KeyCipher::new(cfg.actor.kek.clone()));

    let cache = load_key_cache_with_diagnostics(pool, cipher.as_ref()).await?;
    let provider = DbSigningKeyProvider::new(cache.clone());

    let runtime = RuntimeContext {
        clock: Arc::new(SystemClock::new()),
        ids: Arc::new(SnowflakeIdGenerator::new()),
        rng: Arc::new(SystemRng::new()),
        keys: Arc::new(provider),
    };

    let actor_module = actor::build_actor_module(pool.clone(), runtime.clone(), cipher, cache);

    Ok((runtime, actor_module))
}

/// Runs the composition sequence up to (but not including) `serve`: config
/// -> telemetry -> db pool -> migrate -> actor-model key supply -> runtime
/// context -> `AppState` (design.md's Bootstrap component, Requirement 1.1;
/// the actor-model key-supply stage is task 6.1's own addition, Requirement
/// 6.1). Any failure aborts before the next stage runs and before the HTTP
/// listener is ever touched (Requirement 1.2).
///
/// Shared by both [`bootstrap`] and [`bootstrap_with_shutdown_signal`] so
/// the two differ only in which `serve_with_shutdown*` variant they call
/// afterward.
async fn build_state() -> Result<AppState, BootstrapError> {
    // Config failures surface here with no tracing subscriber installed yet;
    // `main.rs`'s own `eprintln!` on the returned `Err` is this stage's
    // diagnostic output (see this module's doc comment).
    let cfg = config::load_config()?;

    // A telemetry failure means telemetry itself is what's broken, so there
    // is still no subscriber to log through here either.
    telemetry::init_telemetry(&cfg.log)?;

    let pool = establish_pool_with_diagnostics(&cfg).await?;
    apply_migrations_with_diagnostics(&pool).await?;

    // Replaces `RuntimeContext::production()`'s `FixedSigningKeyProvider`
    // placeholder with the real, DB-backed `DbSigningKeyProvider` (task
    // 6.1, Requirement 6.1) and assembles the actor-model service bundle
    // `AppState` now carries.
    let (runtime, actor_module) = build_actor_wiring(&pool, &cfg).await?;

    // Assembles the OAuth service bundle (task 7.1, api-foundation's
    // Requirements 1.1, 2.1, 3.1, 5.1): builds the one shared `OauthService`
    // from the same `pool`/`runtime` every other composition-root component
    // shares, plus the startup-configured `oauth.token_hash_key`/
    // `owner.password` secrets (task 1.2) `OauthModule` needs.
    //
    // CONCERN (documented judgment call): `cookie_secure` is hardcoded to
    // `false` here. design.md's Security Considerations call for the
    // owner-session cookie's `Secure` attribute "TLS 配信時" (when served
    // over TLS), but `AppConfig`/`ServerConfig` (`src/config.rs`) has no
    // TLS-termination setting at all — this crate's own HTTP listener
    // (`src/server.rs`) never terminates TLS itself, and Modified Files for
    // this task does not list `src/config.rs`, so adding one is out of this
    // task's boundary. `false` is the conservative choice: a single-owner
    // instance commonly sits behind a reverse proxy terminating TLS in
    // front of a plain-HTTP origin, and a `Secure` cookie would silently
    // break owner login entirely on such a deployment (browsers refuse to
    // send `Secure` cookies over a plain-HTTP connection), whereas omitting
    // `Secure` only weakens (not breaks) a deployment that *is* served
    // directly over TLS. A future task adding TLS-awareness to
    // `ServerConfig` should flip this to that setting instead of a fixed
    // constant.
    let oauth_module = OauthModule::new(
        pool.clone(),
        runtime.clone(),
        cfg.oauth.token_hash_key.clone(),
        cfg.owner.clone(),
        false,
    );

    // Assembles the federation-core port bundle (task 5.4, Requirements 7.3,
    // 10.1, 11.1, 11.2): every federation-core port constructed with one
    // concrete production type (`crate::federation::build_federation_module`),
    // using the same `pool`/`runtime` every other composition-root component
    // shares and `actor_module`'s own `ActorDirectory` handle (federation-core
    // needs handle resolution across several ports; never reconstructs its
    // own directory independently). `FederationWiringConfig::production`
    // converts `cfg.federation`'s `std::time::Duration` fields into the
    // `time::Duration` federation-core's own components take throughout, and
    // supplies this module's own choice of background-task poll/prune
    // cadence (design.md names no numeric value for either — see
    // `FederationWiringConfig::production`'s own doc comment).
    let (federation_module, federation_background) = federation::build_federation_module(
        pool.clone(),
        runtime.clone(),
        Arc::clone(actor_module.directory()),
        FederationWiringConfig::production(
            cfg.server.domain.clone(),
            cfg.federation.secure_mode,
            time::Duration::seconds(cfg.federation.public_key_cache_ttl.as_secs() as i64),
            time::Duration::days(cfg.federation.received_activity_retention_days as i64),
        ),
        Arc::new(ReqwestFederationHttpClient::new()),
    );
    // Starts the delivery-worker poll loop and received-Activity pruning
    // loop as detached background tasks (never awaited here) — this call
    // returns immediately, so it does not delay `bootstrap()`'s own listener
    // bind below (Requirement: background tasks must not block startup).
    federation_background.spawn();

    // Assembles the media-pipeline module bundle (task 5.2, Requirements
    // 1.1, 4.1, 9.5): builds the repository/queue/store/processor/service
    // wiring (`crate::media::build_media_module`) from this same
    // `pool`/`runtime`/`cfg.media` every other composition-root component
    // shares, then starts its resident `ProcessingWorker` pool
    // (`MediaConfig::worker_concurrency` workers) as detached background
    // tasks — mirroring `federation_background.spawn()`'s own "never blocks
    // this function's own startup" contract immediately above. Each
    // worker's own shutdown signal is `server::os_shutdown_signal` itself
    // (now `pub(crate)` for exactly this reuse) passed directly as the
    // signal factory — see `MediaBackgroundWorkers::spawn`'s own doc comment
    // for why calling it once per worker (plus once more, independently,
    // inside `serve_with_shutdown`'s own call below) observes the same real
    // OS shutdown event without needing a broadcast/watch channel to fan one
    // signal out to several tasks.
    let (media_module, media_background) =
        media::build_media_module(pool.clone(), runtime.clone(), cfg.media.clone());
    media_background.spawn(server::os_shutdown_signal);

    Ok(AppState::new(
        pool,
        runtime,
        cfg,
        actor_module,
        oauth_module,
        federation_module,
        media_module,
    ))
}

/// Assembles all application dependencies and runs the server
/// (design.md's Bootstrap Service Interface). Sequences config -> telemetry
/// -> db pool -> migrate -> runtime context -> `AppState` -> serve
/// (Requirement 1.1); any failure at any stage is converted into a
/// [`BootstrapError`] before the HTTP listener ever starts (Requirement
/// 1.2). Waits for a real OS shutdown signal (SIGINT/SIGTERM) before
/// draining and releasing resources (Requirements 1.3-1.5), via
/// [`crate::server::serve_with_shutdown`].
pub async fn bootstrap() -> Result<(), BootstrapError> {
    let state = build_state().await?;
    let server_cfg = state.config().server.clone();
    server::serve_with_shutdown(state, &server_cfg)
        .await
        .map_err(|err| {
            tracing::error!(error = %err, "startup aborted while starting the HTTP listener");
            BootstrapError::Serve(err)
        })
}

/// Test/composition-root seam: identical to [`bootstrap`] except it serves
/// via [`crate::server::serve_with_shutdown_and_signal`] with an
/// caller-supplied `signal` instead of always waiting for a real OS signal.
///
/// This exists so this task's own integration tests
/// (`tests/bootstrap_lifecycle_it.rs`) can prove Requirement 1.1's "正常時は
/// 待ち受け可能になる" (normal startup becomes listen-ready) end-to-end
/// through the *real* composition sequence — including a real
/// `init_telemetry` call — and then stop it cleanly (e.g. via a `oneshot`
/// channel) without sending a real OS signal to the whole test process. It
/// is `pub` (not test-only/`cfg(test)`-gated) purely because it must be
/// reachable from a separate `tests/*.rs` crate; see this module's doc
/// comment for why that separation is required. [`bootstrap`] remains the
/// one production entrypoint design.md documents and the one `main.rs`
/// calls.
pub async fn bootstrap_with_shutdown_signal(
    signal: impl Future<Output = ()> + Send + 'static,
) -> Result<(), BootstrapError> {
    let state = build_state().await?;
    let server_cfg = state.config().server.clone();
    server::serve_with_shutdown_and_signal(state, &server_cfg, signal)
        .await
        .map_err(|err| {
            tracing::error!(error = %err, "startup aborted while starting the HTTP listener");
            BootstrapError::Serve(err)
        })
}
