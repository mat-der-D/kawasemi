//! End-to-end integration test for Bootstrap's "normal startup becomes
//! listen-ready, and shuts down cleanly" contract (Requirement 1.1, task
//! 7.4's first acceptance bullet).
//!
//! ## Why this lives in its own `tests/*.rs` binary (process), not
//! `src/bootstrap/tests.rs`
//! `bootstrap()`/`bootstrap_with_shutdown_signal()` call
//! `kawasemi::telemetry::init_telemetry`, which installs a *global*,
//! process-wide `tracing` default subscriber that `tracing` itself only
//! allows to be installed once per process (see `src/telemetry.rs`'s module
//! doc comment, and its own
//! `init_telemetry_installs_a_global_subscriber_exactly_once` unit test,
//! which deliberately calls `init_telemetry` twice in the same process to
//! prove the *second* call fails with `TelemetryError::AlreadyInitialized`).
//! If this test ran inside the same test binary as that unit test (i.e. as
//! `#[cfg(test)] mod tests` inside `src/bootstrap.rs`, compiled into the
//! `--lib` unit-test binary), whichever of the two tests' calls into
//! `init_telemetry` executed second under `cargo test`'s default parallel
//! scheduling would get `TelemetryError::AlreadyInitialized` instead of the
//! outcome it expects — a real, unavoidable race given `tracing`'s
//! global-install-once constraint, not a flaw in either test. Rust
//! integration tests under `tests/` each compile to an independent process,
//! which sidesteps this entirely; that isolation is why this test exists
//! here instead of next to `src/bootstrap.rs`.
//!
//! For the same reason, this file contains exactly one test that drives a
//! full `bootstrap`-equivalent sequence past the telemetry stage: a second
//! such call in this same process would also collide on the global
//! subscriber. `tests/bootstrap_fail_fast_it.rs` covers the failure-ordering
//! contract (Requirement 1.2) separately, in its own process, and is
//! likewise limited to a single telemetry-touching call.
//!
//! ## How "listen-ready" is proven without a real OS signal
//! `bootstrap()` itself blocks in `serve_with_shutdown` until a real OS
//! SIGINT/SIGTERM arrives, which would be awkward to drive from within this
//! same test process. Instead this test calls
//! `kawasemi::bootstrap::bootstrap_with_shutdown_signal` — task 7.4's
//! injectable-shutdown seam over the identical composition sequence (see
//! `src/bootstrap.rs`'s and `src/server.rs`'s `serve_with_shutdown_and_signal`
//! doc comments) — spawns it as a background task, polls the configured
//! bind address until it accepts a real TCP connection and `/health`
//! responds (proving Requirement 1.1's "待ち受け可能になった"), then resolves
//! a `oneshot` channel to trigger a clean shutdown and asserts the spawned
//! task resolves to `Ok(())`.

use std::net::TcpStream;
use std::time::Duration;

use kawasemi::bootstrap::bootstrap_with_shutdown_signal;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;

const TEST_DB_HOST: &str = "127.0.0.1";
const TEST_DB_PORT: u16 = 5432;
const TEST_DB_URL_ENV: &str = "KAWASEMI_TEST_DATABASE_URL";
const DEFAULT_TEST_DB_URL: &str =
    "postgres://kawasemi_test:kawasemi_test_pw@127.0.0.1:5432/kawasemi_test";

/// Fixed test bind address, distinct from `tests/bootstrap_fail_fast_it.rs`'s
/// (a separate process, so collision is not actually possible, but distinct
/// ports keep intent obvious).
const TEST_BIND_ADDR: &str = "127.0.0.1:58731";

fn test_db_url() -> String {
    std::env::var(TEST_DB_URL_ENV).unwrap_or_else(|_| DEFAULT_TEST_DB_URL.to_string())
}

/// Mirrors `src/db/tests.rs`'/`src/migrate/tests.rs`'s own cheap raw-TCP
/// preflight probe: skip (with a diagnostic) in an environment with no local
/// PostgreSQL at all, rather than failing.
fn default_test_db_reachable() -> bool {
    TcpStream::connect_timeout(
        &format!("{TEST_DB_HOST}:{TEST_DB_PORT}")
            .parse()
            .expect("hardcoded host:port is valid"),
        Duration::from_millis(500),
    )
    .is_ok()
}

fn should_run_against_real_database(test_name: &str) -> bool {
    let overridden = std::env::var(TEST_DB_URL_ENV).is_ok();
    if !overridden && !default_test_db_reachable() {
        eprintln!(
            "skipping {test_name}: no PostgreSQL reachable at {TEST_DB_HOST}:{TEST_DB_PORT} \
             and {TEST_DB_URL_ENV} was not set"
        );
        return false;
    }
    true
}

/// Sets the process env vars `bootstrap()`'s `config::load_config()` reads.
/// Safe without additional synchronization: this is the only test in this
/// file/process (see module doc comment), so nothing else in this binary
/// reads or mutates these same variables concurrently.
fn set_bootstrap_env() {
    // SAFETY: this is the only test in this binary (a dedicated
    // `tests/*.rs` process, see module doc comment), and it runs to
    // completion before the process exits, so no other test observes a
    // torn/partial view of the environment.
    unsafe {
        std::env::set_var(
            "KAWASEMI_CONFIG_PATH",
            "/nonexistent-kawasemi-config-for-bootstrap-lifecycle-it.toml",
        );
        std::env::set_var("KAWASEMI_SERVER_DOMAIN", "bootstrap-it.example.test");
        std::env::set_var("KAWASEMI_SERVER_BIND_ADDR", TEST_BIND_ADDR);
        std::env::set_var("KAWASEMI_SERVER_SHUTDOWN_GRACE_SECS", "2");
        std::env::set_var("KAWASEMI_DATABASE_URL", test_db_url());
        std::env::set_var("KAWASEMI_LOG_LEVEL", "error");
    }
}

async fn bind_addr_is_connectable() -> bool {
    tokio::net::TcpStream::connect(TEST_BIND_ADDR).await.is_ok()
}

async fn raw_http_get(addr: &str, path: &str) -> String {
    let mut stream = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::net::TcpStream::connect(addr),
    )
    .await
    .expect("connecting to the listen-ready address must not time out")
    .expect("connect");
    let request = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write request");
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut buf))
        .await
        .expect("reading the response must not time out")
        .expect("read response");
    String::from_utf8_lossy(&buf).to_string()
}

/// Requirement 1.1: a normal startup (valid config, reachable database)
/// completes config -> telemetry -> pool -> migrate -> runtime context ->
/// `AppState` -> serve in order and becomes listen-ready — proven
/// behaviorally by connecting to the configured bind address and getting a
/// real `/health` response, not merely by `bootstrap_with_shutdown_signal`
/// returning without error (it wouldn't return at all until shutdown).
/// Additionally proves the clean-shutdown half of the round trip: resolving
/// the injected shutdown signal makes it return `Ok(())`.
#[tokio::test]
async fn bootstrap_succeeds_reaches_listen_ready_and_shuts_down_cleanly() {
    if !should_run_against_real_database(
        "bootstrap_succeeds_reaches_listen_ready_and_shuts_down_cleanly",
    ) {
        return;
    }
    set_bootstrap_env();

    assert!(
        !bind_addr_is_connectable().await,
        "precondition: nothing should be listening on {TEST_BIND_ADDR} before bootstrap runs"
    );

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let signal = async move {
        let _ = shutdown_rx.await;
    };

    let bootstrap_task = tokio::spawn(bootstrap_with_shutdown_signal(signal));

    // Poll until the listener is up, rather than sleeping a fixed duration.
    let became_ready = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if bind_addr_is_connectable().await {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .is_ok();
    assert!(
        became_ready,
        "bootstrap must become listen-ready on {TEST_BIND_ADDR} within 10s"
    );

    // Confirm the listener actually serves the foundation router (not just
    // that *some* socket accepted a TCP connection).
    let health_response = raw_http_get(TEST_BIND_ADDR, "/health").await;
    assert!(
        health_response.starts_with("HTTP/1.1 200"),
        "a listen-ready bootstrap must serve /health with 200: {health_response}"
    );

    // Trigger shutdown and confirm the task completes with Ok(()) — the
    // zero-exit-equivalent contract (Requirements 1.3-1.5) — rather than
    // hanging or erroring.
    let _ = shutdown_tx.send(());
    let result = tokio::time::timeout(Duration::from_secs(10), bootstrap_task)
        .await
        .expect("the bootstrap task must finish within 10s of shutdown being triggered")
        .expect("the bootstrap task must not panic");
    assert!(
        result.is_ok(),
        "bootstrap_with_shutdown_signal must return Ok(()) after a clean graceful shutdown: \
         {result:?}"
    );

    // And the listener must actually be gone afterward.
    assert!(
        !bind_addr_is_connectable().await,
        "the listener must no longer accept connections after shutdown completes"
    );
}
