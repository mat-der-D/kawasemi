//! Minimal integration-style tests for the `TestHarness` boundary itself
//! (task 8.1, Requirements 8.1-8.5): proving `spawn_test_app()` boots a real,
//! connectable instance with an applied-migrations, isolated database and a
//! deterministic `RuntimeContext`, and that `cleanup()` actually releases the
//! resources it acquired.
//!
//! Mirrors `src/db/tests.rs`'/`src/migrate/tests.rs`'s convention: a cheap
//! raw-TCP preflight probe against the default local test database gates the
//! real assertions, so this suite skips (with a diagnostic) in an environment
//! with no local PostgreSQL rather than failing outright. The target
//! database/role (`kawasemi_test`) is the same fixed local-only,
//! non-production credential those modules already use, overridable via
//! `KAWASEMI_TEST_DATABASE_URL`.

use std::net::TcpStream;
use std::time::Duration;

use sqlx::Row;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

const TEST_DB_HOST: &str = "127.0.0.1";
const TEST_DB_PORT: u16 = 5432;
const TEST_DB_URL_ENV: &str = "KAWASEMI_TEST_DATABASE_URL";

/// Best-effort raw-TCP reachability probe, independent of sqlx/the harness
/// itself. Used only to decide whether to skip these tests in an environment
/// with no local PostgreSQL at all; never used to swallow a real regression.
fn default_test_db_reachable() -> bool {
    TcpStream::connect_timeout(
        &format!("{TEST_DB_HOST}:{TEST_DB_PORT}")
            .parse()
            .expect("hardcoded host:port is valid"),
        Duration::from_millis(500),
    )
    .is_ok()
}

/// Returns `true` if the caller should proceed, `false` if it should skip
/// (having already printed a diagnostic).
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

async fn raw_http_get(addr: std::net::SocketAddr, path: &str) -> String {
    let mut stream = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::net::TcpStream::connect(addr),
    )
    .await
    .expect("connecting to spawn_test_app's address must not time out")
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

/// Requirements 8.1, 8.2, 8.3: `spawn_test_app` starts a real, connectable
/// instance whose database already has the embedded migrations applied and
/// whose `RuntimeContext` is the deterministic implementation (not
/// production), all reusing the same Bootstrap building blocks (`db`,
/// `migrate`, `runtime`, `server::build_router`) as the real composition
/// root.
#[tokio::test]
async fn spawn_test_app_boots_with_applied_migrations_and_deterministic_runtime() {
    if !should_run_against_real_database(
        "spawn_test_app_boots_with_applied_migrations_and_deterministic_runtime",
    ) {
        return;
    }

    let app = spawn_test_app().await;

    // Requirement 8.1: a real, connectable instance — proven by actually
    // serving the foundation router's `/health` route over the returned
    // address, not merely that some socket accepted a TCP connection.
    let health_response = raw_http_get(app.address, "/health").await;
    assert!(
        health_response.starts_with("HTTP/1.1 200"),
        "spawn_test_app's address must serve the real foundation router: {health_response}"
    );

    // Requirement 8.2: embedded migrations are already applied against the
    // isolated database this instance was given.
    let migration_count: i64 = sqlx::query("SELECT count(*) AS c FROM _sqlx_migrations")
        .fetch_one(&app.pool)
        .await
        .expect("_sqlx_migrations must exist and be queryable: migrations must be pre-applied")
        .get("c");
    assert!(
        migration_count > 0,
        "spawn_test_app must provide a database with the embedded migrations already applied"
    );

    // Requirement 8.3: the runtime context is the deterministic
    // implementation, not `RuntimeContext::production()` — proven
    // behaviorally by reproducing the same clock/id/rng/key sequence as an
    // independently-constructed deterministic context built from the same
    // fixed seed spawn_test_app uses.
    let reference = crate::runtime::RuntimeContext::deterministic(default_test_seed());
    assert_eq!(app.runtime.clock.now(), reference.clock.now());
    let mut app_buf = [0u8; 16];
    let mut ref_buf = [0u8; 16];
    app.runtime.rng.fill_bytes(&mut app_buf);
    reference.rng.fill_bytes(&mut ref_buf);
    assert_eq!(
        app_buf, ref_buf,
        "spawn_test_app's RuntimeContext must be the deterministic implementation"
    );

    app.cleanup().await;
}

/// Requirement 8.4: two concurrently-running test instances never share
/// persisted database state — each gets its own isolated schema, so a row
/// visible to one is invisible to the other even though both point at the
/// same underlying PostgreSQL server/database.
#[tokio::test]
async fn spawn_test_app_isolates_database_state_between_instances() {
    if !should_run_against_real_database("spawn_test_app_isolates_database_state_between_instances")
    {
        return;
    }

    let app_a = spawn_test_app().await;
    let app_b = spawn_test_app().await;

    let schema_a: String = sqlx::query("SELECT current_schema() AS s")
        .fetch_one(&app_a.pool)
        .await
        .expect("app_a must be able to query its own current_schema")
        .get("s");
    let schema_b: String = sqlx::query("SELECT current_schema() AS s")
        .fetch_one(&app_b.pool)
        .await
        .expect("app_b must be able to query its own current_schema")
        .get("s");

    assert_ne!(
        schema_a, schema_b,
        "two concurrently spawned test instances must not resolve to the same schema"
    );

    // A table created in app_a's isolated schema must not be visible from
    // app_b's connection (different search_path / isolated schema).
    sqlx::query("CREATE TABLE isolation_probe (id integer)")
        .execute(&app_a.pool)
        .await
        .expect("creating a table in app_a's isolated schema must succeed");

    let visible_in_b = sqlx::query(
        "SELECT to_regclass('isolation_probe') IS NOT NULL AS visible",
    )
    .fetch_one(&app_b.pool)
    .await
    .expect("app_b must be able to run the visibility probe query")
    .get::<bool, _>("visible");

    assert!(
        !visible_in_b,
        "a table created in app_a's isolated schema must not be visible from app_b"
    );

    app_a.cleanup().await;
    app_b.cleanup().await;
}

/// Requirement 8.5: after `cleanup()` is called, the resources
/// `spawn_test_app` acquired are actually released — the connection pool no
/// longer accepts new work, the listener no longer accepts connections, and
/// the isolated schema no longer exists.
#[tokio::test]
async fn cleanup_releases_pool_listener_and_isolated_schema() {
    if !should_run_against_real_database("cleanup_releases_pool_listener_and_isolated_schema") {
        return;
    }

    let app = spawn_test_app().await;
    let address = app.address;
    let schema = app
        .schema
        .clone()
        .expect("schema must still be present before cleanup() is called");
    let pool = app.pool.clone();

    // Precondition: the listener is actually up before cleanup.
    assert!(
        tokio::net::TcpStream::connect(address).await.is_ok(),
        "precondition: spawn_test_app's address must be connectable before cleanup"
    );

    app.cleanup().await;

    // Give the (now-signaled) listener task a brief moment to actually stop
    // accepting connections; cleanup() itself already awaits it, but leave a
    // small safety margin for the OS to release the socket.
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert!(
        tokio::net::TcpStream::connect(address).await.is_err(),
        "the listener must no longer accept connections after cleanup() completes"
    );
    assert!(
        pool.is_closed(),
        "the connection pool must be closed after cleanup() completes"
    );

    // The isolated schema must be gone: verify via a fresh admin connection
    // (the just-closed pool can no longer be used).
    let admin_cfg = crate::config::DatabaseConfig {
        url: crate::config::Secret::new(base_test_db_url()),
        max_connections: 1,
        acquire_timeout: Duration::from_secs(5),
    };
    let admin_pool = crate::db::establish_pool(&admin_cfg)
        .await
        .expect("a fresh admin connection to the shared test database must succeed");
    let schema_still_exists: bool = sqlx::query(
        "SELECT EXISTS (SELECT 1 FROM information_schema.schemata WHERE schema_name = $1) AS e",
    )
    .bind(&schema)
    .fetch_one(&admin_pool)
    .await
    .expect("querying information_schema.schemata must succeed")
    .get("e");
    admin_pool.close().await;

    assert!(
        !schema_still_exists,
        "the isolated schema must be dropped after cleanup() completes"
    );
}
