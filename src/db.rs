//! Database connection pool establishment (Db boundary).
//!
//! Scope: this module owns establishing the shared PostgreSQL connection
//! pool (`PgPool`) from validated startup configuration (Requirement 3.1),
//! applying the configured pool size and connection-acquisition timeout
//! (Requirement 3.4), and reporting a connectivity failure as a [`DbError`]
//! that retains the underlying cause so a caller can log diagnostics and
//! abort startup before the HTTP listener starts (Requirement 3.2).
//!
//! Publishing the established pool for shared reuse by later components
//! (Requirement 3.3, via `AppState`) and wiring this into the Bootstrap
//! composition root are out of scope here — this module only knows how to
//! build one pool from one [`DatabaseConfig`]; see design.md's "Db"
//! component and tasks 7.1/7.4.
//!
//! This module never applies embedded migrations (that is task 4.2's
//! `Migrate` boundary): it hands back a bare, connected pool.

#[cfg(test)]
mod tests;

use std::fmt;

use sqlx::postgres::{PgPool, PgPoolOptions};

use crate::config::DatabaseConfig;

/// Failure establishing the shared database connection pool.
///
/// Retains the underlying [`sqlx::Error`] (Requirement 3.2's "原因を保持した
/// `DbError`") both as a public field, for callers that want to inspect it
/// directly, and via [`std::error::Error::source`], so a future Bootstrap
/// caller (task 7.4) can log full diagnostic detail and abort startup with a
/// non-zero exit code without losing the original cause.
///
/// Deliberately holds only the `sqlx::Error` — never the connection URL —
/// so that formatting a `DbError` (`Display`/`Debug`) cannot echo the
/// `Secret`-wrapped `DatabaseConfig::url` in plaintext (Requirement 2.5).
#[derive(Debug)]
pub struct DbError {
    /// The underlying error sqlx returned while attempting to establish the
    /// pool's first connection (e.g. connection refused, authentication
    /// failure, invalid connection-string syntax).
    pub source: sqlx::Error,
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "failed to establish database connection pool: {}",
            self.source
        )
    }
}

impl std::error::Error for DbError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Establishes the shared PostgreSQL connection pool described by `cfg`.
///
/// Applies `cfg.max_connections` and `cfg.acquire_timeout` to the pool
/// (Requirement 3.4), and connects using `cfg.url` (unwrapped via
/// [`Secret::expose_secret`](crate::config::Secret::expose_secret) — the
/// only point in this module that touches the plaintext connection string).
///
/// `sqlx`'s `PgPoolOptions::connect` always eagerly opens and validates at
/// least one connection before returning (independent of `min_connections`,
/// which this module leaves at its default of 0), which is what gives the
/// design.md postcondition — "returned `PgPool` has at least 1 successful
/// connection" — its teeth: a database that is unreachable, or an invalid
/// connection string, surfaces here as an `Err` rather than as a pool that
/// only fails lazily on first use (Requirement 3.2).
pub async fn establish_pool(cfg: &DatabaseConfig) -> Result<PgPool, DbError> {
    PgPoolOptions::new()
        .max_connections(cfg.max_connections)
        .acquire_timeout(cfg.acquire_timeout)
        .connect(cfg.url.expose_secret())
        .await
        .map_err(|source| DbError { source })
}
