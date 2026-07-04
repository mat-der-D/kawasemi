//! Composition Root for the application startup sequence (Bootstrap
//! boundary, Requirements 1.1, 1.2, design.md's "Bootstrap" component).
//!
//! Sequences dependencies in exactly the order design.md's Bootstrap
//! component and "起動シーケンスと安全停止" flowchart specify: config ->
//! telemetry -> db pool -> migrate -> runtime context -> `AppState` -> serve.
//! Any failure at any of the first four stages aborts before the HTTP
//! listener ever starts (Requirement 1.2) and is aggregated into a single
//! [`BootstrapError`] that retains the real underlying `*Error` (never
//! stringly-typed), per design.md's Error Strategy: "起動時エラー
//! （Config/Db/Migrate/Telemetry/Bootstrap）は `*Error` 型で原因を保持し、
//! `BootstrapError` に集約して `main` が診断出力 + 非ゼロ終了に変換する".
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

use sqlx::PgPool;

use crate::config::{self, AppConfig, ConfigError};
use crate::db::{self, DbError};
use crate::migrate::{self, MigrateError};
use crate::runtime::RuntimeContext;
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
                write!(f, "startup aborted while establishing the database pool: {e}")
            }
            BootstrapError::Migrate(e) => {
                write!(f, "startup aborted while applying embedded migrations: {e}")
            }
            BootstrapError::Serve(e) => {
                write!(f, "startup aborted while starting the HTTP listener: {e}")
            }
        }
    }
}

impl std::error::Error for BootstrapError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(match self {
            BootstrapError::Config(e) => e,
            BootstrapError::Telemetry(e) => e,
            BootstrapError::Db(e) => e,
            BootstrapError::Migrate(e) => e,
            BootstrapError::Serve(e) => e,
        })
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

/// Runs the composition sequence up to (but not including) `serve`: config
/// -> telemetry -> db pool -> migrate -> runtime context -> `AppState`
/// (design.md's Bootstrap component, Requirement 1.1). Any failure aborts
/// before the next stage runs and before the HTTP listener is ever touched
/// (Requirement 1.2).
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

    let runtime = RuntimeContext::production();
    Ok(AppState::new(pool, runtime, cfg))
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
