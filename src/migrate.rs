//! Embedded migration auto-apply on startup (Requirement 4: 埋め込み
//! マイグレーションと起動時自動実行).
//!
//! `run_migrations()` embeds the `migrations/` directory into the binary
//! at compile time via [`sqlx::migrate!`] (Requirement 4.1) and applies
//! every unapplied migration, in order, against an already-established
//! [`PgPool`] (Requirement 4.2). If nothing is unapplied it is a no-op and
//! startup continues normally (Requirement 4.3). sqlx tracks applied
//! migrations in the `_sqlx_migrations` history table, so already-applied
//! migrations are never re-executed and existing data is preserved
//! (Requirement 4.4).
//!
//! Any failure -- a migration whose SQL errors while executing, or a
//! checksum mismatch between the embedded migration source and the
//! `_sqlx_migrations` history -- is surfaced as a [`MigrateError`] that
//! identifies the offending migration (Requirement 4.5, 4.6). Callers
//! (the Bootstrap composition root, task 7.4) are expected to treat any
//! `Err` as fatal and abort startup before the HTTP listener is opened.
//!
//! `run_migrations()` itself is not yet wired into the startup sequence:
//! that composition-root wiring is task 7.4's job. Until then this module
//! is exercised only by its own tests, so it is allowed to be otherwise
//! unused.

#![allow(dead_code)]

use std::fmt;

use sqlx::PgPool;
use sqlx::migrate::MigrateError;

/// Applying the embedded migrations against `pool` failed.
///
/// Carries the original [`sqlx::migrate::MigrateError`] as the
/// diagnostic cause. That type's `Display` implementation already names
/// the specific migration version involved for both failure modes this
/// module cares about:
///  - `ExecuteMigration(_, version)` when a migration's SQL fails to
///    execute (Requirement 4.5), and
///  - `VersionMismatch(version)` when a previously-applied migration's
///    checksum no longer matches the embedded definition (Requirement
///    4.6).
///
/// Wrapping (rather than re-exporting the bare sqlx error) keeps this
/// module's public error surface independent of the `sqlx::migrate`
/// module path, mirroring `DbError` in `src/db.rs`.
#[derive(Debug)]
pub struct MigrationApplyError(MigrateError);

impl fmt::Display for MigrationApplyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "failed to apply embedded migrations: {}", self.0)
    }
}

impl std::error::Error for MigrationApplyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

/// Applies every unapplied embedded migration to `pool`, in order
/// (Requirement 4.2). If none are unapplied this is a no-op and `pool`'s
/// schema is left untouched (Requirement 4.3). Already-applied
/// migrations are never re-executed and their data is preserved
/// (Requirement 4.4); sqlx enforces this via the `_sqlx_migrations`
/// history table it maintains on `pool`.
///
/// The migration set embedded here is fixed at compile time from
/// `./migrations` (Requirement 4.1): no external migration files or
/// tools are consulted at runtime.
///
/// # Errors
/// Returns [`MigrationApplyError`] if a migration's SQL fails to execute,
/// or if a previously-applied migration's checksum no longer matches the
/// embedded definition (Requirement 4.5, 4.6). Callers must treat any
/// `Err` as fatal and abort startup before opening the HTTP listener.
pub async fn run_migrations(pool: &PgPool) -> Result<(), MigrationApplyError> {
    sqlx::migrate!("./migrations").run(pool).await.map_err(MigrationApplyError)
}
