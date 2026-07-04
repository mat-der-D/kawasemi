//! Integration tests for Bootstrap's "initialization failure aborts before
//! HTTP starts, with a non-zero-equivalent (`Err`) result" contract
//! (Requirement 1.2, task 7.4's second acceptance bullet).
//!
//! Runs in its own `tests/*.rs` binary/process, separate from
//! `tests/bootstrap_lifecycle_it.rs`, for the same reason explained in that
//! file's module doc comment: `bootstrap()` calls
//! `kawasemi::telemetry::init_telemetry`, which installs a global,
//! install-once-per-process `tracing` subscriber. Only the second scenario
//! below gets far enough to call `init_telemetry` for real; the first fails
//! at the config stage, strictly before telemetry is ever touched.
//!
//! Both scenarios are driven from a single `#[tokio::test]` function, in a
//! fixed sequential order, rather than as two separate `#[tokio::test]`
//! functions: both scenarios mutate real process environment variables that
//! `bootstrap()`'s `config::load_config()` reads, and `cargo test` runs
//! `#[tokio::test]` functions within one binary concurrently by default —
//! two separate env-mutating tests would race on that shared process state.
//! A single function sequences them deterministically instead, without
//! needing to hold a lock across `.await` points.
//!
//! Per this task's test-strategy guidance, this file does not re-prove each
//! stage's own error type in isolation (already covered by tasks 2.1's
//! `src/config/tests.rs`, 4.1's `src/db/tests.rs`, 4.2's
//! `src/migrate/tests.rs`); it focuses on the ordering + fail-fast +
//! non-zero-equivalent guarantee `bootstrap()` itself is responsible for.

use std::time::Duration;

use kawasemi::bootstrap::BootstrapError;
use kawasemi::bootstrap::bootstrap;

const CONFIG_FAILURE_BIND_ADDR: &str = "127.0.0.1:58831";
const DB_FAILURE_BIND_ADDR: &str = "127.0.0.1:58832";

async fn bind_addr_is_connectable(addr: &str) -> bool {
    tokio::net::TcpStream::connect(addr).await.is_ok()
}

/// Requirement 1.2 / 2.3: when required startup configuration (here, both
/// the server domain and the database URL) is missing, `bootstrap()` must
/// fail at the config stage — strictly before telemetry, the db pool,
/// migrations, or the HTTP listener are ever touched — and return
/// `Err(BootstrapError::Config(_))`, never `Ok(())`.
async fn assert_bootstrap_fails_fast_on_invalid_startup_configuration() {
    // SAFETY: this whole file drives exactly one `#[tokio::test]` function
    // (see module doc comment), so these mutations are never observed
    // concurrently by another test in this process.
    unsafe {
        std::env::remove_var("KAWASEMI_SERVER_DOMAIN");
        std::env::remove_var("KAWASEMI_DATABASE_URL");
        std::env::set_var(
            "KAWASEMI_CONFIG_PATH",
            "/nonexistent-kawasemi-config-for-bootstrap-fail-fast-it.toml",
        );
        std::env::set_var("KAWASEMI_SERVER_BIND_ADDR", CONFIG_FAILURE_BIND_ADDR);
    }

    assert!(
        !bind_addr_is_connectable(CONFIG_FAILURE_BIND_ADDR).await,
        "precondition: nothing should be listening on {CONFIG_FAILURE_BIND_ADDR}"
    );

    let result = tokio::time::timeout(Duration::from_secs(10), bootstrap())
        .await
        .expect("bootstrap() must fail fast on invalid config, not hang");

    assert!(
        matches!(result, Err(BootstrapError::Config(_))),
        "missing required config fields must surface as BootstrapError::Config, got: {result:?}"
    );

    assert!(
        !bind_addr_is_connectable(CONFIG_FAILURE_BIND_ADDR).await,
        "the HTTP listener must never have started when config loading fails"
    );
}

/// Requirement 1.2 / 3.2: with valid configuration but an unreachable
/// database, `bootstrap()` must get past config and telemetry, fail while
/// establishing the pool — strictly before migrations or the HTTP listener
/// are touched — and return `Err(BootstrapError::Db(_))`, never `Ok(())`.
///
/// Points at `127.0.0.1:1` — a port nothing listens on — mirroring
/// `src/db/tests.rs`'s own convention, so the OS refuses the connection
/// immediately without waiting out an acquire-timeout deadline.
async fn assert_bootstrap_fails_fast_when_the_database_is_unreachable() {
    // SAFETY: see the previous function's SAFETY comment.
    unsafe {
        std::env::set_var(
            "KAWASEMI_CONFIG_PATH",
            "/nonexistent-kawasemi-config-for-bootstrap-fail-fast-it.toml",
        );
        std::env::set_var(
            "KAWASEMI_SERVER_DOMAIN",
            "bootstrap-fail-fast-it.example.test",
        );
        std::env::set_var(
            "KAWASEMI_DATABASE_URL",
            "postgres://baduser:definitely-not-a-real-secret@127.0.0.1:1/kawasemi_nonexistent",
        );
        std::env::set_var("KAWASEMI_DATABASE_ACQUIRE_TIMEOUT_SECS", "2");
        std::env::set_var("KAWASEMI_SERVER_BIND_ADDR", DB_FAILURE_BIND_ADDR);
        std::env::set_var("KAWASEMI_LOG_LEVEL", "error");
    }

    assert!(
        !bind_addr_is_connectable(DB_FAILURE_BIND_ADDR).await,
        "precondition: nothing should be listening on {DB_FAILURE_BIND_ADDR}"
    );

    let result = tokio::time::timeout(Duration::from_secs(10), bootstrap())
        .await
        .expect("bootstrap() must fail fast on an unreachable database, not hang");

    assert!(
        matches!(result, Err(BootstrapError::Db(_))),
        "an unreachable database must surface as BootstrapError::Db, past config and telemetry \
         but before migrate/serve, got: {result:?}"
    );

    assert!(
        !bind_addr_is_connectable(DB_FAILURE_BIND_ADDR).await,
        "the HTTP listener must never have started when the database is unreachable"
    );
}

#[tokio::test]
async fn bootstrap_fails_fast_and_never_starts_http_on_any_initialization_failure() {
    assert_bootstrap_fails_fast_on_invalid_startup_configuration().await;
    assert_bootstrap_fails_fast_when_the_database_is_unreachable().await;
}
