//! `IdempotencyStore` (design.md "Data / データ層" -> `PollRepository /
//! IdempotencyStore`; Requirements 5.1, 5.2; task 2.3, `Boundary:
//! PollRepository, IdempotencyStore`): the post-creation `Idempotency-Key`
//! ledger — one-time-use `(actor_id, idempotency_key)` -> `status_id`
//! binding and resend resolution — against `status_idempotency_keys`
//! (`migrations/0007_statuses.sql`, already applied, unmodified by this
//! task).
//!
//! Scope: this module owns exactly [`check_or_reserve`] and [`bind`] —
//! design.md's `IdempotencyStore` half of the shared `PollRepository /
//! IdempotencyStore` Service Interface (design.md lines 447-448). No
//! `StatusRepository`/`InteractionRepository`/`PollRepository`/
//! `TagRepository` functionality, no `StatusService` orchestration (deciding
//! *when* to call these two functions around an actual status-creation flow
//! is that future task's job, not this repository's), and no HTTP surface
//! lives here.
//!
//! ## Two functions, not one atomic "insert-if-absent" (CONCERN — schema-
//! forced, not a stylistic choice)
//! design.md's Service Interface deliberately splits this into
//! [`check_or_reserve`] (called *before* the status is created) and [`bind`]
//! (called *after*), rather than a single atomic "record-if-absent-else-
//! return-existing" call — and the physical schema makes that split
//! mandatory, not optional: `status_idempotency_keys.status_id` is `BIGINT
//! NOT NULL REFERENCES statuses(id) ON DELETE CASCADE`
//! (`migrations/0007_statuses.sql`), so a row here can only ever be inserted
//! *after* the `statuses` row it points at already exists — there is no
//! "reserve a placeholder, backfill `status_id` later" row shape available
//! (no nullable `status_id` column, no separate reservation table). The
//! caller-side sequence this therefore requires (Requirements 5.1, 5.2) is:
//! (1) [`check_or_reserve`] — if [`IdempotencyLookup::Existing`], stop and
//! return that `status_id` (Requirement 5.2, "新規投稿を作成せず...最初に
//! 作成された投稿を...返す"); if [`IdempotencyLookup::Reserved`], the caller
//! is clear to proceed; (2) the caller creates the new `Status` row
//! (`StatusRepository::insert_status`, out of this module's boundary); (3)
//! [`bind`] persists the `(actor_id, key) -> status_id` mapping. "Reserved"
//! is therefore a *permission to proceed*, not a persisted placeholder state
//! — nothing is written to the database by [`check_or_reserve`] itself.
//!
//! ## Race between two concurrent first-uses of the same key
//! design.md's own Consistency note says as much directly: "競合は一意制約
//! で原子化" ("the race is atomized by the unique constraint"). If two
//! concurrent requests both call [`check_or_reserve`] before either has
//! called [`bind`], both see [`IdempotencyLookup::Reserved`] and both may go
//! on to create their own `Status` row — but only the first of their two
//! [`bind`] calls actually persists a `status_idempotency_keys` row; the
//! second hits the `(actor_id, idempotency_key)` primary key and is silently
//! absorbed by this function's own `ON CONFLICT ... DO NOTHING` (mirroring
//! `interaction_repository.rs`'s identical "never a raw `INSERT` that could
//! surface a unique-violation `sqlx::Error`" convention) rather than
//! surfacing as an error. Reconciling the resulting *second* `Status` row
//! that never got bound (e.g. deleting it, or making the loser's response
//! reflect the winner's status instead) is `StatusService`'s job, not this
//! repository's — this module's own contract is only the `(actor_id, key)`
//! uniqueness of the *ledger* itself, which the database's own primary key
//! already guarantees.
//!
//! ## `now: OffsetDateTime` parameter on [`bind`] design.md's sketch omits
//! (CONCERN — same documented judgment call as
//! `interaction_repository.rs`'s identical precedent)
//! design.md's sketch (`pub async fn bind(pool: &PgPool, actor_id: Id, key:
//! &str, status_id: Id) -> Result<(), AppError>;`) gives `bind` no
//! `created_at` parameter, but `status_idempotency_keys.created_at` is `NOT
//! NULL` with no database-side default. This crate's established convention
//! — callers mint timestamps via `RuntimeContext::clock`, repositories never
//! call a SQL-side `NOW()` (`status_repository.rs::apply_edit`'s `now`
//! parameter; `interaction_repository.rs`'s identical addition to
//! `add_favourite`/`add_bookmark`/`set_pin`) — is treated as load-bearing
//! here too, so this module adds `now: OffsetDateTime` to [`bind`].

#[cfg(test)]
mod tests;

use sqlx::postgres::PgPool;
use time::OffsetDateTime;

use crate::api::db::map_server_error;
use crate::domain::Id;
use crate::error::AppError;

/// The result of consulting the ledger for `(actor_id, key)` *before*
/// creating a new post — see this module's doc comment ("Two functions, not
/// one atomic...") for the full caller-side protocol this participates in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdempotencyLookup {
    /// `(actor_id, key)` was already bound to `Id` (the `status_id`) by a
    /// prior request (Requirement 5.2): the caller must not create a new
    /// post, and should instead treat this as the response.
    Existing(Id),
    /// `(actor_id, key)` has no existing binding: the caller is clear to
    /// create a new post and then call [`bind`] to record it. Not a
    /// persisted state — see this module's doc comment for why nothing is
    /// written to the database at this point.
    Reserved,
}

/// Looks up whether `(actor_id, key)` has already been used to create a
/// post (Requirements 5.1's "最初の要求で投稿を作成し...記録する" implies a
/// *first*-request check, 5.2's resend resolution). Called *before* the
/// caller creates a new `Status` row — see this module's doc comment for the
/// full protocol.
pub async fn check_or_reserve(
    pool: &PgPool,
    actor_id: Id,
    key: &str,
) -> Result<IdempotencyLookup, AppError> {
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT status_id FROM status_idempotency_keys WHERE actor_id = $1 AND idempotency_key = $2",
    )
    .bind(actor_id.as_i64())
    .bind(key)
    .fetch_optional(pool)
    .await
    .map_err(map_server_error)?;

    Ok(match row {
        Some((status_id,)) => IdempotencyLookup::Existing(Id::from_i64(status_id)),
        None => IdempotencyLookup::Reserved,
    })
}

/// Records that `(actor_id, key)` now resolves to `status_id` (Requirement
/// 5.1). Called *after* the caller has created the new `Status` row `bind`
/// is about to reference (`status_idempotency_keys.status_id`'s real,
/// mandatory FK to `statuses(id)` requires the referenced row to already
/// exist).
///
/// A losing racer's `bind` call for an already-bound `(actor_id, key)` (see
/// this module's doc comment, "Race between two concurrent first-uses") is a
/// silent no-op, not an error — mirrors `interaction_repository.rs`'s
/// established idempotent-write convention.
pub async fn bind(
    pool: &PgPool,
    actor_id: Id,
    key: &str,
    status_id: Id,
    now: OffsetDateTime,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO status_idempotency_keys (actor_id, idempotency_key, status_id, created_at) \
         VALUES ($1, $2, $3, $4) ON CONFLICT (actor_id, idempotency_key) DO NOTHING",
    )
    .bind(actor_id.as_i64())
    .bind(key)
    .bind(status_id.as_i64())
    .bind(now)
    .execute(pool)
    .await
    .map_err(map_server_error)?;

    Ok(())
}
