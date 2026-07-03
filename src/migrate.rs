//! Embedded migration application at startup (Migrate boundary).
//!
//! Scope: this module owns embedding `migrations/` into the compiled binary
//! (Requirement 4.1) and applying, in order, whichever of those migrations
//! are not yet recorded in the target database's `_sqlx_migrations` history
//! against an already-established [`PgPool`] (Requirement 4.2). When there
//! is nothing unapplied it is a no-op that leaves existing data and history
//! untouched (Requirement 4.3, 4.4). A failure to apply a migration, or a
//! detected checksum mismatch between an already-recorded migration and the
//! embedded definition, is reported as a [`MigrateError`] that retains
//! enough detail to identify the offending migration (Requirement 4.5,
//! 4.6) rather than panicking or silently continuing.
//!
//! This module deliberately does not: establish the connection pool itself
//! (that is task 4.1's `Db` boundary, see [`crate::db::establish_pool`]),
//! decide what to do with a `MigrateError` at process level (aborting
//! startup with a non-zero exit code before the HTTP listener starts is
//! task 7.4's `Bootstrap` composition root), or touch `AppState`. It only
//! knows how to apply one embedded migration set to one already-connected
//! [`PgPool`].
//!
//! All checksum verification and apply-order/idempotency bookkeeping is
//! sqlx's own: `sqlx::migrate!("./migrations")` embeds the migration files
//! at compile time into a `sqlx::migrate::Migrator`, and
//! `Migrator::run` drives sqlx's built-in `_sqlx_migrations` history
//! comparison and checksum check. This module reimplements none of that
//! logic; it only wraps the `Result` sqlx already produces.

#[cfg(test)]
mod tests;

use std::fmt;

use sqlx::migrate::{MigrateError as SqlxMigrateError, Migrator};
use sqlx::postgres::PgPool;

/// The embedded migration set, compiled from `migrations/` (Requirement
/// 4.1). Constructing this at compile time means a corrupt or missing
/// migration file is a build-time error, not a runtime one.
static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// Failure applying the embedded migration set to the database.
///
/// Retains the underlying [`sqlx::migrate::MigrateError`] both as a public
/// field, for callers that want to inspect it directly (its `Display`
/// already names the offending migration version for variants such as
/// `ExecuteMigration` and `VersionMismatch` — see sqlx's
/// `sqlx::migrate::MigrateError`), and via [`std::error::Error::source`],
/// so a future Bootstrap caller (task 7.4) can log full diagnostic detail
/// and abort startup with a non-zero exit code before the HTTP listener
/// starts (Requirement 4.5), including when the failure is a detected
/// checksum inconsistency between an already-applied migration's recorded
/// checksum and the embedded definition (Requirement 4.6,
/// `MigrateError::VersionMismatch`).
#[derive(Debug)]
pub struct MigrateError {
    /// The underlying error sqlx returned while resolving or applying
    /// migrations against `_sqlx_migrations` history.
    pub source: SqlxMigrateError,
}

impl fmt::Display for MigrateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "failed to apply embedded migrations: {}", self.source)
    }
}

impl std::error::Error for MigrateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Applies the embedded migration set to `pool`, in order, skipping
/// whichever migrations are already recorded as applied.
///
/// Delegates entirely to sqlx's `Migrator::run`, which:
/// - Applies only unapplied migrations, in ascending version order
///   (Requirement 4.2), and is a no-op when none are pending
///   (Requirement 4.3).
/// - Never re-applies or alters an already-applied migration, preserving
///   existing data (Requirement 4.4).
/// - Verifies each already-applied migration's recorded checksum in
///   `_sqlx_migrations` against the embedded definition, surfacing a
///   mismatch as `sqlx::migrate::MigrateError::VersionMismatch`
///   (Requirement 4.6).
/// - Surfaces an execution failure as
///   `sqlx::migrate::MigrateError::ExecuteMigration`, which identifies the
///   failed migration's version (Requirement 4.5).
///
/// `pool` must already be an established, live connection pool (task 4.1's
/// `establish_pool`); this function does not create or configure a pool.
pub async fn apply_migrations(pool: &PgPool) -> Result<(), MigrateError> {
    MIGRATOR
        .run(pool)
        .await
        .map_err(|source| MigrateError { source })
}
