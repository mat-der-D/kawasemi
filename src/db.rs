//! Database connection pool establishment (Requirement 3).
//!
//! `establish_pool()` builds a `sqlx::PgPool` from [`DatabaseConfig`],
//! applying the configured pool size and connection-acquire timeout
//! (Requirement 3.1, 3.4), and eagerly verifies that at least one
//! connection can actually be opened before returning (Requirement 3.2,
//! 3.3): a `PgPoolOptions::connect` call establishes and tests the first
//! connection synchronously, so a database that is unreachable at startup
//! surfaces as an `Err` here rather than lazily failing on the first query
//! later. Callers (the Bootstrap composition root, task 7.4) are expected
//! to treat any `Err` as fatal and abort startup before the HTTP listener
//! is opened.
//!
//! `establish_pool()` itself is not yet wired into the startup sequence:
//! that composition-root wiring is task 7.4's job. Until then this module
//! is exercised only by its own tests, so it is allowed to be otherwise
//! unused.

#![allow(dead_code)]

use std::fmt;

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

use crate::config::DatabaseConfig;

/// The database's initial connection could not be established.
///
/// Carries the original `sqlx::Error` as the diagnostic cause
/// (Requirement 3.2: "失敗理由を診断情報とともに出力"), so callers can
/// report *why* the connection failed (DNS failure, connection refused,
/// authentication failure, timeout, ...) rather than a bare failure
/// signal.
#[derive(Debug)]
pub struct DbError(sqlx::Error);

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "failed to establish database connection pool: {}", self.0)
    }
}

impl std::error::Error for DbError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

/// Establishes a connection pool for `cfg`, applying the configured pool
/// size and connection-acquire timeout (Requirement 3.1, 3.4), and
/// eagerly verifying that the database is actually reachable by opening
/// (and testing) the first connection before returning (Requirement 3.2,
/// 3.3).
pub async fn establish_pool(cfg: &DatabaseConfig) -> Result<PgPool, DbError> {
    PgPoolOptions::new()
        .max_connections(cfg.max_connections)
        .acquire_timeout(cfg.acquire_timeout)
        .connect(cfg.url.expose())
        .await
        .map_err(DbError)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::config::Secret;

    #[tokio::test]
    async fn unreachable_host_yields_db_error_with_preserved_cause() {
        let cfg = DatabaseConfig {
            url: Secret::new("postgres://postgres@127.0.0.1:5999/kawasemi_test".to_string()),
            max_connections: 5,
            acquire_timeout: Duration::from_millis(300),
        };

        let err = establish_pool(&cfg).await.expect_err("unreachable host must not yield a pool");

        assert!(std::error::Error::source(&err).is_some());
        assert!(!err.to_string().is_empty());
    }
}
