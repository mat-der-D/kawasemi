//! Integration tests for database connection pool establishment
//! (Requirement 3: データベース接続プール).
//!
//! These tests exercise `kawasemi::db::establish_pool` against a real
//! PostgreSQL instance for the success path, and against an unreachable
//! address for the failure path, so both halves of Requirement 3.2 ("初回
//! 接続が確立できない場合は原因を保持した DbError を返す") and 3.1/3.3/3.4
//! are observed end-to-end rather than only unit-tested.

use std::time::Duration;

use kawasemi::config::{DatabaseConfig, Secret};
use kawasemi::db::establish_pool;

fn db_config(url: &str, max_connections: u32, acquire_timeout: Duration) -> DatabaseConfig {
    DatabaseConfig { url: Secret::new(url.to_string()), max_connections, acquire_timeout }
}

#[tokio::test]
async fn successful_connection_yields_a_usable_pool() {
    let cfg = db_config(
        "postgres://postgres@127.0.0.1:5432/kawasemi_test",
        5,
        Duration::from_secs(5),
    );

    let pool = establish_pool(&cfg).await.expect("expected pool establishment to succeed");

    // The pool must be genuinely usable (Requirement 3.3), not merely
    // constructed: run a trivial query through it.
    let row: (i32,) =
        sqlx::query_as("SELECT 1").fetch_one(&pool).await.expect("pool should serve queries");
    assert_eq!(row.0, 1);

    // Requirement 3.4: pool size configured from the supplied settings.
    assert!(pool.size() <= 5);
}

#[tokio::test]
async fn unreachable_database_aborts_with_diagnostic_db_error() {
    // Nothing listens on this port; a short acquire_timeout keeps the test
    // fast instead of hanging for the default connect timeout.
    let cfg = db_config(
        "postgres://postgres@127.0.0.1:5999/kawasemi_test",
        5,
        Duration::from_millis(500),
    );

    let err = establish_pool(&cfg).await.expect_err("expected connection failure to yield DbError");

    // Requirement 3.2: the failure reason must be preserved and
    // observable, not swallowed.
    let message = err.to_string();
    assert!(!message.is_empty(), "DbError should carry a non-empty diagnostic message");
    assert!(
        std::error::Error::source(&err).is_some(),
        "DbError should preserve the underlying sqlx cause for diagnosis"
    );
}
