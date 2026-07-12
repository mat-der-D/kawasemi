//! `ActorRepository` (design.md "Data / データ層" -> "ActorRepository";
//! Requirements 1.1, 1.2, 1.3, 2.2, 7.3, 8.1, 8.2): the local actor's
//! persistence, state transitions, and lookups.
//!
//! Scope: this module owns exactly the five operations design.md's Service
//! Interface specifies for this component — `insert_actor`, `find_by_handle`,
//! `find_by_id`, `list_by_owner`, `update_state` — against the
//! `local_actors` table (`migrations/0002_actors.sql`, already in place,
//! unmodified by this task). `ActorSigningKeyRepository` (task 2.3),
//! `ActorService` (task 5.1, which will open the transaction `insert_actor`
//! takes and drive it through both actor insertion and signing-key
//! insertion), and `ActorDirectory` (task 5.2) are separate boundaries, out
//! of scope here.
//!
//! ## Why `insert_actor` alone takes a transaction handle
//! design.md's Service Interface gives `insert_actor` a `tx: &mut
//! PgTransaction` parameter, unlike this sibling module's `OwnerRepository`
//! (`&PgPool`) or this module's own `find_by_handle`/`find_by_id`/
//! `list_by_owner`/`update_state` (all `&PgPool`): design.md's "アクター作成
//! （鍵生成連動）" flow inserts the actor row and its signing key in the same
//! transaction so a key-generation failure rolls the actor insertion back
//! too ("アクター挿入と鍵生成は同一トランザクション境界で扱い", design.md
//! System Flows). That transaction is opened and committed by the caller —
//! `ActorService::create_actor` (task 5.1) — not by this function:
//! `insert_actor` only executes its `INSERT` against whatever transaction
//! handle it is handed, and never calls `.commit()`/`.rollback()` itself, so
//! it composes with the signing-key insertion the same caller will also run
//! against the identical transaction. This task's own tests open (and
//! commit) a transaction themselves via `pool.begin()`, since nothing else
//! in the codebase yet does so for them.
//!
//! ## `PgTransaction` type alias
//! No shared transaction type alias exists anywhere else in this crate yet
//! (`OwnerRepository`, the only other repository so far, never needed one).
//! [`PgTransaction`] is defined locally in this file — a plain alias for the
//! concrete `sqlx::Transaction<'_, sqlx::Postgres>` type design.md's
//! `PgTransaction` notation informally refers to — rather than in
//! `src/db.rs` (core-runtime's connection-pool boundary, out of scope for
//! this task's `_Boundary: ActorRepository_`). If a later task needs this
//! alias shared across multiple repository modules, promoting it into
//! `src/db.rs` is a reasonable follow-up, but is a `CONCERNS` note for the
//! reviewer rather than something this task does unilaterally.
//!
//! ## Duplicate-handle mapping (Requirement 1.3)
//! `insert_actor` maps a `local_actors_handle_unique` unique-constraint
//! violation to a caller-facing (`ErrorKind::Client`) [`AppError`]
//! (`409 Conflict`), per design.md's Error Strategy
//! ("利用者起因（4xx 相当）: ハンドル重複（1.3）"). Any other database
//! failure (including a unique violation on a constraint other than the
//! handle — which should not occur given `IdGenerator`'s uniqueness
//! contract, but is not this function's call to treat as a caller error)
//! surfaces as a `Server` (5xx) `AppError`, mirroring `OwnerRepository`'s own
//! convention for unexpected database failures.
//!
//! ## Enum/DB-column mapping kept local to this file
//! `ActorType`/`ActorState` (`src/actor/model.rs`, task 1.2's already-
//! reviewed `model` boundary) need a `TEXT`-column round trip
//! (`'person'|'service'`, `'active'|'deactivated'`, per
//! `migrations/0002_actors.sql`'s column comments). Per this task's
//! boundary note, the mapping is kept as small private free functions in
//! this file rather than added onto the enums in `model.rs`, to keep this
//! task's diff scoped to `ActorRepository` and avoid re-touching an
//! already-reviewed file.

#[cfg(test)]
mod tests;

use axum::http::StatusCode;
use sqlx::postgres::PgPool;
use time::OffsetDateTime;

use crate::actor::model::{ActorState, ActorType, Handle, LocalActor};
use crate::domain::Id;
use crate::error::AppError;

/// Local alias for the concrete sqlx transaction type this component's
/// `insert_actor` accepts — see this module's doc comment ("`PgTransaction`
/// type alias") for why it lives here rather than in a shared core-runtime
/// module.
pub type PgTransaction<'a> = sqlx::Transaction<'a, sqlx::Postgres>;

/// Postgres constraint name enforcing instance-wide handle uniqueness
/// (`migrations/0002_actors.sql`), used to distinguish a handle-uniqueness
/// violation from any other unique-constraint violation.
const HANDLE_UNIQUE_CONSTRAINT: &str = "local_actors_handle_unique";

/// Maps an `ActorType` to its `local_actors.actor_type` `TEXT` column
/// representation (`migrations/0002_actors.sql`'s column comment: `'person'
/// | 'service'`).
fn actor_type_as_str(actor_type: ActorType) -> &'static str {
    match actor_type {
        ActorType::Person => "person",
        ActorType::Service => "service",
    }
}

/// Reconstructs an `ActorType` from an already-persisted
/// `local_actors.actor_type` column value.
///
/// Panics on any value other than `'person'`/`'service'`: such a row could
/// only exist if something wrote outside this repository's own
/// `actor_type_as_str` mapping (e.g. a manual `INSERT`/migration bug), which
/// is a data-corruption invariant violation, not a normal error path this
/// function's `Result<_, AppError>` callers should have to handle.
fn actor_type_from_str(raw: &str) -> ActorType {
    match raw {
        "person" => ActorType::Person,
        "service" => ActorType::Service,
        other => panic!(
            "local_actors.actor_type contained unexpected value {other:?}; expected 'person' or 'service'"
        ),
    }
}

/// Maps an `ActorState` to its `local_actors.state` `TEXT` column
/// representation (`migrations/0002_actors.sql`'s column comment: `'active'
/// | 'deactivated'`).
fn actor_state_as_str(state: ActorState) -> &'static str {
    match state {
        ActorState::Active => "active",
        ActorState::Deactivated => "deactivated",
    }
}

/// Reconstructs an `ActorState` from an already-persisted `local_actors.state`
/// column value. Panics on any other value — see [`actor_type_from_str`]'s
/// doc comment for why that is the right behavior here.
fn actor_state_from_str(raw: &str) -> ActorState {
    match raw {
        "active" => ActorState::Active,
        "deactivated" => ActorState::Deactivated,
        other => panic!(
            "local_actors.state contained unexpected value {other:?}; expected 'active' or 'deactivated'"
        ),
    }
}

/// A `local_actors` row as read directly off the wire, before reconstructing
/// its typed [`LocalActor`] form.
type LocalActorRow = (
    i64,
    i64,
    String,
    String,
    String,
    String,
    String,
    OffsetDateTime,
    OffsetDateTime,
);

/// Reconstructs a [`LocalActor`] from a raw row tuple.
///
/// Uses `Handle::new(...).expect(...)` on the stored handle string: a value
/// already persisted through this repository's own `insert_actor` is by
/// definition already valid (Requirement 1.6 rejects an invalid handle
/// before it ever reaches `insert_actor`), so re-validating it here would
/// only ever fail on the same kind of external data-corruption this file's
/// `actor_type_from_str`/`actor_state_from_str` panic on.
fn row_to_local_actor(row: LocalActorRow) -> LocalActor {
    let (id, owner_id, handle, actor_type, display_name, summary, state, created_at, updated_at) =
        row;
    LocalActor {
        id: Id::from_i64(id),
        owner_id: Id::from_i64(owner_id),
        handle: Handle::new(handle)
            .expect("handle stored in local_actors must already be a validly-formatted handle"),
        actor_type: actor_type_from_str(&actor_type),
        display_name,
        summary,
        state: actor_state_from_str(&state),
        created_at,
        updated_at,
    }
}

/// Maps a failed `INSERT INTO local_actors` to an [`AppError`]: a unique
/// violation on [`HANDLE_UNIQUE_CONSTRAINT`] becomes a caller-facing
/// (`ErrorKind::Client`) `409 Conflict` (Requirement 1.3); anything else
/// (including a unique violation on some other constraint, which should not
/// occur given `IdGenerator`'s uniqueness contract) becomes a `Server` (5xx)
/// `AppError`.
fn map_insert_error(source: sqlx::Error) -> AppError {
    if let Some(db_error) = source.as_database_error()
        && db_error.is_unique_violation()
        && db_error.constraint() == Some(HANDLE_UNIQUE_CONSTRAINT)
    {
        return AppError::client(StatusCode::CONFLICT, "handle is already in use");
    }
    AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source)
}

/// Persists `actor` as a new `local_actors` row, executed against the
/// already-open transaction `tx` (see this module's doc comment for why the
/// caller owns opening/committing it).
///
/// A `local_actors_handle_unique` violation (Requirement 1.3) surfaces as a
/// caller-facing [`AppError`] (`409 Conflict`) rather than a generic 5xx —
/// see [`map_insert_error`].
pub async fn insert_actor(tx: &mut PgTransaction<'_>, actor: &LocalActor) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO local_actors \
            (id, owner_id, handle, actor_type, display_name, summary, state, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(actor.id.as_i64())
    .bind(actor.owner_id.as_i64())
    .bind(actor.handle.as_str())
    .bind(actor_type_as_str(actor.actor_type))
    .bind(&actor.display_name)
    .bind(&actor.summary)
    .bind(actor_state_as_str(actor.state))
    .bind(actor.created_at)
    .bind(actor.updated_at)
    .execute(&mut **tx)
    .await
    .map_err(map_insert_error)?;

    Ok(())
}

/// Looks up the [`LocalActor`] persisted under `handle`, if any (Requirement
/// 8.2's underlying data access; the service-level "ハンドル解決"
/// projection/owner-stripping contract itself belongs to a later task's
/// `ActorDirectory`).
///
/// Returns `Ok(None)` (not an error) when no row matches `handle` — mirrors
/// `OwnerRepository::find_owner`'s "does this exist" contract at this data
/// layer.
pub async fn find_by_handle(
    pool: &PgPool,
    handle: &Handle,
) -> Result<Option<LocalActor>, AppError> {
    let row: Option<LocalActorRow> = sqlx::query_as(
        "SELECT id, owner_id, handle, actor_type, display_name, summary, state, created_at, updated_at \
         FROM local_actors WHERE handle = $1",
    )
    .bind(handle.as_str())
    .fetch_optional(pool)
    .await
    .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;

    Ok(row.map(row_to_local_actor))
}

/// Looks up the [`LocalActor`] persisted under `id`, if any.
///
/// Returns `Ok(None)` (not an error) when no row matches `id` — same
/// "does this exist" contract as [`find_by_handle`].
pub async fn find_by_id(pool: &PgPool, id: Id) -> Result<Option<LocalActor>, AppError> {
    let row: Option<LocalActorRow> = sqlx::query_as(
        "SELECT id, owner_id, handle, actor_type, display_name, summary, state, created_at, updated_at \
         FROM local_actors WHERE id = $1",
    )
    .bind(id.as_i64())
    .fetch_optional(pool)
    .await
    .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;

    Ok(row.map(row_to_local_actor))
}

/// Returns every [`LocalActor`] owned by `owner_id` (Requirement 8.1's
/// underlying data access — this task's own observable completion
/// condition: "オーナー別取得が当該オーナーのアクターのみを返す").
///
/// Returns an empty `Vec` (not an error) when `owner_id` owns no actors, or
/// does not exist at all — this repository layer does not reject an unknown
/// `owner_id` (existence-checking that must reject, e.g. actor creation
/// against a non-existent owner per Requirement 2.3, is `ActorService`'s
/// concern, not this repository's).
pub async fn list_by_owner(pool: &PgPool, owner_id: Id) -> Result<Vec<LocalActor>, AppError> {
    let rows: Vec<LocalActorRow> = sqlx::query_as(
        "SELECT id, owner_id, handle, actor_type, display_name, summary, state, created_at, updated_at \
         FROM local_actors WHERE owner_id = $1 ORDER BY id",
    )
    .bind(owner_id.as_i64())
    .fetch_all(pool)
    .await
    .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;

    Ok(rows.into_iter().map(row_to_local_actor).collect())
}

/// Transitions the actor persisted under `id` to `state`, stamping
/// `updated_at = now` (Requirement 7.3's "無効化での状態遷移" and its
/// symmetric activation case; Requirement 7.5's `Clock`-sourced timestamp).
///
/// Returns `Ok(true)` if a row was actually updated, `Ok(false)` if `id`
/// matched no row — this repository layer does not reject an unknown `id`
/// with an error (existence-checking that must reject is `ActorService`'s
/// concern, mirroring `find_by_id`/`find_by_handle`'s "no error for
/// absence" pattern at this layer).
pub async fn update_state(
    pool: &PgPool,
    id: Id,
    state: ActorState,
    now: OffsetDateTime,
) -> Result<bool, AppError> {
    let result = sqlx::query("UPDATE local_actors SET state = $1, updated_at = $2 WHERE id = $3")
        .bind(actor_state_as_str(state))
        .bind(now)
        .bind(id.as_i64())
        .execute(pool)
        .await
        .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;

    Ok(result.rows_affected() > 0)
}
