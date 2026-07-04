//! `ActorSigningKeyRepository` (design.md "Data / データ層" ->
//! "ActorSigningKeyRepository"; Requirements 4.1, 4.5, 5.2, 5.3, 5.4, 6.2):
//! the per-actor signing key's persistence — active-key insertion,
//! retirement, active-public-key lookup, and the startup bulk load of every
//! active key.
//!
//! Scope: this module owns exactly the four operations design.md's Service
//! Interface specifies for this component — `insert_active_key`,
//! `retire_active_key`, `find_active_public_key`, `load_all_active` —
//! against the `actor_signing_keys` table (`migrations/0002_actors.sql`,
//! already in place, unmodified by this task). `KeyMaterial` (task 3.1),
//! `KeyCipher` (task 3.2), `SigningKeyService` (task 4.1), `KeyCache`/
//! `DbSigningKeyProvider` (task 4.1/4.2) are separate, later boundaries —
//! this repository never generates, seals, or opens key material; it only
//! stores and retrieves whatever already-opaque bytes/PEM strings it is
//! handed.
//!
//! ## `PgTransaction` reuse (not a fresh alias)
//! `insert_active_key`/`retire_active_key` need the same `tx: &mut
//! PgTransaction` shape task 2.2's `ActorRepository::insert_actor` already
//! established. Rather than redefining an identical local alias here, this
//! module imports and reuses [`crate::actor::repository::PgTransaction`]
//! directly. This is a deliberate choice between two defensible options
//! (see task brief): (a) reuse the sibling module's alias — chosen here —
//! or (b) duplicate an identical one-line alias locally to keep the two
//! repository modules structurally independent. Reuse was chosen because
//! task 5.1's future `ActorService::create_actor` will open exactly one
//! transaction and pass the *same* `&mut PgTransaction` value through both
//! `ActorRepository::insert_actor` and this module's `insert_active_key` in
//! one call (design.md's "アクター作成（鍵生成連動）" flow) — since a type
//! alias is fully transparent to the compiler either way (both names would
//! resolve to the exact same concrete `sqlx::Transaction<'_, sqlx::Postgres>`
//! type regardless), the only real difference is which name future readers
//! see, and a single named alias is a clearer single source of truth than
//! two coincidentally-identical ones. This does cross this task's
//! `_Boundary: ActorSigningKeyRepository_` in the sense that it imports from
//! `ActorRepository`'s module rather than staying fully self-contained;
//! `src/actor/repository.rs` itself is not modified by this task. See
//! `CONCERNS` in this task's status report for the reviewer to scrutinize
//! this call.
//!
//! ## Duplicate-active-key handling (partial unique index)
//! `actor_signing_keys_active_unique` (`migrations/0002_actors.sql`)
//! rejects a second `status = 'active'` row for the same `actor_id` at the
//! database layer. Unlike task 2.2's handle-uniqueness violation
//! (Requirement 1.3 explicitly calls out a caller-facing duplicate-handle
//! error), no requirement in this task's scope (4.1, 4.5, 5.2, 5.3, 5.4,
//! 6.2) calls out a caller-facing "duplicate active key" error — and
//! design.md's rotation flow ("旧鍵失効＋新鍵有効化を原子的に行う") always
//! retires the current active key before inserting a new one within the
//! same transaction, so a second concurrent `insert_active_key` racing the
//! same actor is the only way this constraint could ever fire in the
//! intended call pattern (a caller-programming-error/race case, not a
//! normal user-triggerable 4xx condition). `insert_active_key` therefore
//! maps *any* database failure — including this constraint violation — to a
//! generic `Server` (5xx) [`AppError`], mirroring `OwnerRepository`'s
//! convention for unexpected database failures, rather than inventing a
//! caller-facing mapping no requirement asks for.
//!
//! ## `retire_active_key`'s `now` parameter is currently unused for storage
//! design.md's Service Interface gives `retire_active_key` a `now:
//! OffsetDateTime` parameter (matching the `Clock`-injection convention used
//! throughout this spec), but `migrations/0002_actors.sql`'s
//! `actor_signing_keys` table has no `retired_at`/`updated_at` column to
//! store it into — only `created_at`. This task's boundary explicitly does
//! not modify that migration. The parameter is accepted (as `_now`, to avoid
//! an unused-parameter lint) to match design.md's exact signature so this
//! function's shape does not have to change again if a future migration adds
//! a `retired_at` column; it is not currently persisted anywhere. Flagged
//! prominently in `CONCERNS` for the reviewer.
//!
//! ## `retire_active_key` on an actor with no active key is a no-op success
//! The partial unique index already guarantees at most one `active` row per
//! actor, so this operation affects at most one row. When zero rows match
//! (the actor has no currently-active key — e.g. it was never given one, or
//! it was already retired), design.md's literal contract (`Result<(),
//! AppError>`, no `bool`/count) is honored by treating this as a success
//! with no error, mirroring tasks 2.1/2.2's "no error for absence"
//! repository-layer convention (`find_owner`/`find_by_id`/`find_by_handle`
//! all return `Ok(None)` rather than erroring on absence).
//!
//! ## `algorithm` and `status` representations
//! [`StoredSigningKey::algorithm`] is a plain `String` (not a `KeyAlgorithm`
//! enum): design.md's `KeyMaterial` component (task 3.1, out of scope here)
//! is what will eventually own a typed `KeyAlgorithm`, and this task must
//! not invent or pre-empt that type. `status` is modeled as
//! [`SigningKeyStatus`], a small local enum (`Active` | `Retired`) mirroring
//! the `actor_type_from_str`/`actor_state_from_str` pattern
//! `src/actor/repository.rs` already established for `TEXT`-column round
//! trips — defined in this file (not `src/actor/model.rs`, which this task
//! does not touch) since no shared home for it exists yet.

#[cfg(test)]
mod tests;

use axum::http::StatusCode;
use sqlx::postgres::PgPool;
use time::OffsetDateTime;

use crate::actor::model::ActorPublicKey;
use crate::actor::repository::PgTransaction;
use crate::domain::Id;
use crate::error::AppError;

/// A signing key's lifecycle state (`actor_signing_keys.status`
/// `TEXT` column: `'active'` | `'retired'`; Requirement 5.4's
/// "失効鍵を有効鍵と区別して識別できる状態で保持する").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SigningKeyStatus {
    /// Currently the actor's single supply-eligible key (Requirement 6.2).
    Active,
    /// No longer supply-eligible, but retained for history (Requirement
    /// 5.4).
    Retired,
}

/// Maps a [`SigningKeyStatus`] to its `actor_signing_keys.status` `TEXT`
/// column representation.
fn signing_key_status_as_str(status: SigningKeyStatus) -> &'static str {
    match status {
        SigningKeyStatus::Active => "active",
        SigningKeyStatus::Retired => "retired",
    }
}

/// Reconstructs a [`SigningKeyStatus`] from an already-persisted
/// `actor_signing_keys.status` column value.
///
/// Panics on any other value — mirrors
/// `src/actor/repository.rs`'s `actor_type_from_str`/`actor_state_from_str`:
/// such a row could only exist via data corruption outside this
/// repository's own mapping.
fn signing_key_status_from_str(raw: &str) -> SigningKeyStatus {
    match raw {
        "active" => SigningKeyStatus::Active,
        "retired" => SigningKeyStatus::Retired,
        other => panic!(
            "actor_signing_keys.status contained unexpected value {other:?}; expected 'active' or 'retired'"
        ),
    }
}

/// A full `actor_signing_keys` row (design.md: `StoredSigningKey`), used for
/// insertion and for [`load_all_active`]'s startup cache-warming bulk load.
///
/// `sealed_private_key` is already-opaque, already-sealed ciphertext bytes
/// (`KeyCipher`'s output, task 3.2, out of scope here) — this repository
/// never attempts to open/decrypt it, and this type is never handed to a
/// protocol-facing reference path (unlike [`ActorPublicKey`], which
/// deliberately excludes it entirely).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSigningKey {
    pub id: Id,
    pub actor_id: Id,
    pub algorithm: String,
    pub public_key_pem: String,
    pub sealed_private_key: Vec<u8>,
    pub status: SigningKeyStatus,
    pub created_at: OffsetDateTime,
}

/// A raw `actor_signing_keys` row as read directly off the wire, before
/// reconstructing its typed [`StoredSigningKey`] form.
type StoredSigningKeyRow = (i64, i64, String, String, Vec<u8>, String, OffsetDateTime);

/// Reconstructs a [`StoredSigningKey`] from a raw row tuple.
fn row_to_stored_signing_key(row: StoredSigningKeyRow) -> StoredSigningKey {
    let (id, actor_id, algorithm, public_key_pem, sealed_private_key, status, created_at) = row;
    StoredSigningKey {
        id: Id::from_i64(id),
        actor_id: Id::from_i64(actor_id),
        algorithm,
        public_key_pem,
        sealed_private_key,
        status: signing_key_status_from_str(&status),
        created_at,
    }
}

/// Persists `key` as a new `actor_signing_keys` row, executed against the
/// already-open transaction `tx` (Requirement 4.1, 4.5).
///
/// A unique-constraint violation on `actor_signing_keys_active_unique` (a
/// second concurrent active key for the same actor) surfaces as a generic
/// `Server` (5xx) [`AppError`], not a caller-facing duplicate error — see
/// this module's doc comment ("Duplicate-active-key handling") for why.
pub async fn insert_active_key(
    tx: &mut PgTransaction<'_>,
    key: &StoredSigningKey,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO actor_signing_keys \
            (id, actor_id, algorithm, public_key_pem, sealed_private_key, status, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(key.id.as_i64())
    .bind(key.actor_id.as_i64())
    .bind(&key.algorithm)
    .bind(&key.public_key_pem)
    .bind(&key.sealed_private_key)
    .bind(signing_key_status_as_str(key.status))
    .bind(key.created_at)
    .execute(&mut **tx)
    .await
    .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;

    Ok(())
}

/// Transitions the current active `actor_signing_keys` row for `actor_id`
/// (if any) from `status = 'active'` to `status = 'retired'`, executed
/// against the already-open transaction `tx` (Requirement 5.2, 5.3).
///
/// A no-op success (not an error) when `actor_id` currently has no active
/// key — see this module's doc comment ("no active key is a no-op success").
///
/// `_now` is currently unused for storage — see this module's doc comment
/// ("`retire_active_key`'s `now` parameter is currently unused for
/// storage").
pub async fn retire_active_key(
    tx: &mut PgTransaction<'_>,
    actor_id: Id,
    _now: OffsetDateTime,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE actor_signing_keys SET status = 'retired' \
         WHERE actor_id = $1 AND status = 'active'",
    )
    .bind(actor_id.as_i64())
    .execute(&mut **tx)
    .await
    .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;

    Ok(())
}

/// Looks up `actor_id`'s current active signing key's public material, if
/// any (Requirement 6.2, 8.3's underlying data access).
///
/// Returns `Ok(None)` (not an error) when `actor_id` has no active key —
/// mirrors `OwnerRepository::find_owner`/`ActorRepository::find_by_id`'s
/// "does this exist" contract at this data layer. Never includes
/// `sealed_private_key`: [`ActorPublicKey`] structurally has no such field
/// (Requirement 3.1/4.4's private-key-never-in-protocol-paths principle).
pub async fn find_active_public_key(
    pool: &PgPool,
    actor_id: Id,
) -> Result<Option<ActorPublicKey>, AppError> {
    let row: Option<(i64, i64, String)> = sqlx::query_as(
        "SELECT actor_id, id, public_key_pem FROM actor_signing_keys \
         WHERE actor_id = $1 AND status = 'active'",
    )
    .bind(actor_id.as_i64())
    .fetch_optional(pool)
    .await
    .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;

    Ok(row.map(|(actor_id, key_id, public_key_pem)| ActorPublicKey {
        actor_id: Id::from_i64(actor_id),
        key_id: Id::from_i64(key_id),
        public_key_pem,
    }))
}

/// Returns every currently-active signing key across all actors, for
/// startup cache warming (design.md: "起動時キャッシュ温め", a later
/// bootstrap task, 6.1).
///
/// Includes `sealed_private_key` (unlike [`find_active_public_key`]): the
/// future `KeyCache` needs the full sealed key material to eventually be
/// opened at signing time (`KeyCipher::open`, task 3.2) — this repository
/// itself never opens/decrypts it.
pub async fn load_all_active(pool: &PgPool) -> Result<Vec<StoredSigningKey>, AppError> {
    let rows: Vec<StoredSigningKeyRow> = sqlx::query_as(
        "SELECT id, actor_id, algorithm, public_key_pem, sealed_private_key, status, created_at \
         FROM actor_signing_keys WHERE status = 'active' ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;

    Ok(rows.into_iter().map(row_to_stored_signing_key).collect())
}
