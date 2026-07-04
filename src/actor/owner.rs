//! `OwnerRepository` (design.md "Data / データ層" -> "OwnerRepository";
//! Requirements 2.1, 2.4): the management-layer-only "owner" concept's
//! creation and lookup.
//!
//! Scope: this module owns exactly the two operations design.md's Service
//! Interface specifies for this component — `create_owner` (identifier
//! minted by the caller via core-runtime's `IdGenerator` boundary, timestamp
//! via the `Clock` boundary; Requirement 2.4) and `find_owner` — against a
//! plain `&PgPool`. No transaction plumbing is introduced here: the design's
//! Service Interface for this component takes `pool: &PgPool`, not
//! `tx: &mut PgTransaction` (that concern belongs to later tasks — 2.2/2.3's
//! `ActorRepository`/`ActorSigningKeyRepository` and 5.1's `ActorService`,
//! which insert an actor and its signing key together in one transaction).
//! `ActorRepository`/`ActorSigningKeyRepository`/`ActorService`/
//! `ActorDirectory` themselves are out of scope for this module (separate
//! boundaries, tasks 2.2/2.3/5.1/5.2).

#[cfg(test)]
mod tests;

use axum::http::StatusCode;
use sqlx::postgres::PgPool;
use time::OffsetDateTime;

use crate::actor::model::Owner;
use crate::domain::Id;
use crate::error::AppError;

/// Persists a new [`Owner`] row keyed by `id` (minted by the caller via
/// core-runtime's `IdGenerator` boundary, Requirement 2.4) with `created_at`
/// set to `now` (supplied by the caller via core-runtime's `Clock`
/// boundary).
///
/// Returns the `Owner` as actually stored (read back from the same `INSERT
/// ... RETURNING` round trip, not merely echoing the `now` argument), so
/// that this return value and a subsequent [`find_owner`] call for the same
/// `id` observe bit-identical `created_at` precision — this is what makes
/// "作成すると...取得で同一オーナーが返る" (this task's observable
/// completion condition) a property actually enforced by this
/// implementation, rather than one that happens to hold only because the
/// caller passed the same `now` value twice.
///
/// A database-layer failure (e.g. a duplicate `id`, which should not occur
/// given `IdGenerator`'s uniqueness contract, or connectivity loss) surfaces
/// as a `Server` (5xx) [`AppError`]; this task's boundary defines no
/// caller-facing (4xx) rejection for owner creation.
pub async fn create_owner(pool: &PgPool, id: Id, now: OffsetDateTime) -> Result<Owner, AppError> {
    let (stored_id, created_at): (i64, OffsetDateTime) = sqlx::query_as(
        "INSERT INTO owners (id, created_at) VALUES ($1, $2) RETURNING id, created_at",
    )
    .bind(id.as_i64())
    .bind(now)
    .fetch_one(pool)
    .await
    .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;

    Ok(Owner {
        id: Id::from_i64(stored_id),
        created_at,
    })
}

/// Looks up the [`Owner`] persisted under `id`, if any.
///
/// Returns `Ok(None)` (not an error) when no row matches `id` — this
/// operation's contract is "does this owner exist", not "this owner must
/// exist" (unlike, e.g., a future operation that must reject actor creation
/// against a non-existent owner, Requirement 2.3, which belongs to
/// `ActorService`, not here).
pub async fn find_owner(pool: &PgPool, id: Id) -> Result<Option<Owner>, AppError> {
    let row: Option<(i64, OffsetDateTime)> =
        sqlx::query_as("SELECT id, created_at FROM owners WHERE id = $1")
            .bind(id.as_i64())
            .fetch_optional(pool)
            .await
            .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;

    Ok(row.map(|(stored_id, created_at)| Owner {
        id: Id::from_i64(stored_id),
        created_at,
    }))
}
