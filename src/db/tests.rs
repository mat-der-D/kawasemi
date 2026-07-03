//! Integration-style tests for the Db boundary (Requirements 3.1, 3.2, 3.3,
//! 3.4), per task 4.1's acceptance bullets: "connection succeeds and yields
//! a pool" and "connection is impossible and startup aborts".
//!
//! `spawn_test_app`'s DB-integration test harness (task 8.1) does not exist
//! yet at this point in the plan, so the "succeeds" case here connects
//! directly to a real local PostgreSQL server rather than through a shared
//! harness. To keep this test runnable in environments that do not happen to
//! have that server available (e.g. a future CI image), a cheap raw-TCP
//! preflight check gates the real assertions: if the default test database
//! is not reachable at all, the test prints a diagnostic and skips instead
//! of failing. The preflight is intentionally independent of
//! `establish_pool` itself (a plain `TcpStream::connect_timeout`), so a real
//! regression in `establish_pool` still fails the test rather than being
//! silently swallowed as "environment unavailable".
//!
//! The target database/role used here (`kawasemi_test` / a fixed local-only
//! password) exists only on `127.0.0.1` in the sandbox this task was
//! implemented in; it is not a production credential.

use std::net::TcpStream;
use std::time::Duration;

use super::*;
use crate::config::Secret;

const TEST_DB_HOST: &str = "127.0.0.1";
const TEST_DB_PORT: u16 = 5432;
const TEST_DB_URL_ENV: &str = "KAWASEMI_TEST_DATABASE_URL";
const DEFAULT_TEST_DB_URL: &str =
    "postgres://kawasemi_test:kawasemi_test_pw@127.0.0.1:5432/kawasemi_test";

fn test_db_config(url: &str, max_connections: u32, acquire_timeout: Duration) -> DatabaseConfig {
    DatabaseConfig {
        url: Secret::new(url.to_string()),
        max_connections,
        acquire_timeout,
    }
}

/// Resolves the URL to use for the "reachable database" test: an explicit
/// override env var if set (the caller opted in, so no preflight skip
/// applies), otherwise the fixed local default this task's implementer
/// provisioned in the sandbox.
fn test_db_url() -> String {
    std::env::var(TEST_DB_URL_ENV).unwrap_or_else(|_| DEFAULT_TEST_DB_URL.to_string())
}

/// Best-effort raw-TCP reachability probe for the *default* test database
/// address, independent of sqlx/`establish_pool`. Used only to decide
/// whether to skip the "successful connection" test in environments with no
/// local PostgreSQL at all; never used to swallow a real `establish_pool`
/// failure.
fn default_test_db_reachable() -> bool {
    TcpStream::connect_timeout(
        &format!("{TEST_DB_HOST}:{TEST_DB_PORT}")
            .parse()
            .expect("hardcoded host:port is valid"),
        Duration::from_millis(500),
    )
    .is_ok()
}

/// Requirements 3.1, 3.3, 3.4: given valid config pointing at a reachable
/// database, `establish_pool` returns a `PgPool` whose applied pool size and
/// acquire timeout match the configured values, and which has at least one
/// live connection (design.md postcondition), proven here by round-tripping
/// a trivial query through it.
#[tokio::test]
async fn establish_pool_succeeds_and_applies_configured_pool_parameters() {
    let overridden = std::env::var(TEST_DB_URL_ENV).is_ok();
    if !overridden && !default_test_db_reachable() {
        eprintln!(
            "skipping establish_pool_succeeds_and_applies_configured_pool_parameters: \
             no PostgreSQL reachable at {TEST_DB_HOST}:{TEST_DB_PORT} and {TEST_DB_URL_ENV} \
             was not set"
        );
        return;
    }

    let cfg = test_db_config(&test_db_url(), 3, Duration::from_secs(5));

    let pool = establish_pool(&cfg)
        .await
        .expect("establish_pool should succeed against a reachable database");

    assert_eq!(
        pool.options().get_max_connections(),
        cfg.max_connections,
        "pool size must be applied from DatabaseConfig::max_connections"
    );
    assert_eq!(
        pool.options().get_acquire_timeout(),
        cfg.acquire_timeout,
        "acquire timeout must be applied from DatabaseConfig::acquire_timeout"
    );

    // Design.md postcondition: the returned pool has at least 1 successful
    // connection. Round-tripping a trivial query proves this behaviorally,
    // not just that `connect` returned `Ok`.
    let row: (i32,) = sqlx::query_as("SELECT 1")
        .fetch_one(&pool)
        .await
        .expect("a pool with a live connection must be able to run a trivial query");
    assert_eq!(row.0, 1);

    pool.close().await;
}

/// Requirement 3.2: when the database is unreachable, `establish_pool`
/// returns a `DbError` (rather than a lazily-broken pool) that retains the
/// underlying cause, so a caller can log diagnostics and abort startup
/// before ever starting the HTTP listener.
///
/// Points at `127.0.0.1:1` — a port nothing listens on — so the OS refuses
/// the connection immediately, without needing to wait out an
/// `acquire_timeout` deadline for a determinstic, fast test.
#[tokio::test]
async fn establish_pool_returns_db_error_when_database_is_unreachable() {
    let bogus_password = "definitely-not-a-real-secret-9f3c";
    let cfg = test_db_config(
        &format!("postgres://baduser:{bogus_password}@127.0.0.1:1/kawasemi_nonexistent"),
        3,
        Duration::from_secs(2),
    );

    let err = establish_pool(&cfg)
        .await
        .expect_err("establish_pool must fail when the database is unreachable");

    // The cause must be retained (Requirement 3.2's "原因を保持した
    // DbError"), reachable both directly and through the standard
    // `Error::source` chain.
    use std::error::Error as _;
    assert!(
        err.source().is_some(),
        "DbError must expose the underlying sqlx::Error via Error::source"
    );

    let rendered = format!("{err}");
    assert!(
        !rendered.contains(bogus_password),
        "DbError's Display must never leak connection-string credentials: {rendered}"
    );
}
