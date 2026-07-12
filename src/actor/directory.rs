//! `ActorDirectory` (design.md "Service / サービス層" -> "ActorDirectory";
//! Requirements 2.5, 3.1, 3.2, 3.3, 8.1, 8.2, 8.3, 8.4; task 5.2): downstream-
//! facing actor reference operations. Splits, at the type level, a
//! management-layer-only operation (`list_actors_for_owner`, the only method
//! in this component allowed to take an `owner_id` parameter at all —
//! Requirement 3.3) from the protocol-layer/public reference operations
//! (`resolve_actor_by_handle`, `actor_public_key`), neither of which may
//! return owner information (Requirements 3.1, 3.2, 8.4).
//!
//! Scope: this module owns the three operations design.md's Service
//! Interface originally specified for this component (task 5.2), each a
//! thin read-only projection over an already-implemented repository call,
//! plus one later narrow addition (`sole_owner`, task 4.1 — see this
//! module's own "`sole_owner`" doc-comment section below):
//! - [`ActorDirectory::list_actors_for_owner`] delegates to
//!   `repository::list_by_owner` (task 2.2, already owner-scoped and tested)
//!   and projects each returned [`LocalActor`] into an owner-free
//!   [`ActorSummary`] (Requirement 8.1; the owner-scoping itself is already
//!   `list_by_owner`'s job — this method's own job is the type-level
//!   projection that structurally drops `owner_id`, which is what makes this
//!   the one explicit management-layer-named operation Requirement 3.3 asks
//!   for).
//! - [`ActorDirectory::resolve_actor_by_handle`] delegates to
//!   `repository::find_by_handle` (task 2.2) and projects the returned
//!   `Option<LocalActor>` into an owner-free `Option<ResolvedActor>`
//!   (Requirements 3.1, 3.2, 8.2). Returns `Ok(None)` (not an error) when no
//!   actor matches `handle` — mirrors `find_by_handle`'s own "no error for
//!   absence" contract (Requirement 8.2's "存在しなければ未検出を示す").
//! - [`ActorDirectory::actor_public_key`] delegates directly to
//!   `keys::repository::find_active_public_key` (task 2.3), which already
//!   returns the owner-free [`ActorPublicKey`] type itself (Requirements 3.1,
//!   8.3) — this method is a near-pass-through, but see this module's doc
//!   comment ("Why `actor_public_key` still exists as its own method") for
//!   why that is not the same as being redundant.
//!
//! `ActorRepository` (task 2.2) and `ActorSigningKeyRepository` (task 2.3) —
//! this component's two dependencies per design.md's Architecture diagram
//! (`Directory --> ActorRepo`, `Directory --> KeyRepo`) — are unmodified,
//! already-implemented, already-tested boundaries this task builds on top
//! of. `ActorService` (task 5.1, a sibling boundary, not a dependency of this
//! task) and bootstrap/`AppState` wiring (task 6.1, out of this task's
//! boundary — this component has no callers yet) are both out of scope here.
//!
//! ## `sole_owner` (api-foundation task 4.1's narrow upstream addition)
//! [`ActorDirectory::sole_owner`] was added later, by api-foundation's task
//! 4.1 (`OwnerGate`), not this task (5.2). design.md's OwnerGate
//! Responsibilities section names it explicitly as an upstream dependency
//! this already-implemented/reviewed component must gain: "actor-model の
//! 単一オーナー取得アクセサ（`ActorDirectory::sole_owner()` 相当）...一人鯖
//! 前提でインスタンスに厳密に1件のみ存在する `owners` 行を返す", and
//! design.md's Data Contracts & Integration section frames it the same way
//! ("actor-model 側に本アクセサは未提供のため...上流依存として扱う"). Unlike
//! this component's other three methods, `sole_owner` does not delegate to a
//! `repository::` function in a sibling module: no existing repository
//! exposes an "every owner" query (`owner.rs`'s `create_owner`/`find_owner`
//! are both keyed by a single already-known `id`, task 2.1's boundary, out
//! of task 4.1's scope to extend), so it queries `owners` directly,
//! mirroring `owner.rs`'s own query style and `AppError::server`-on-DB-
//! failure convention rather than reusing that module's code. See its own
//! doc comment for why zero or more than one row is treated as a `Server`
//! (5xx) error, never `Ok(None)`/a silent `.first()` pick.
//!
//! ## Why `actor_public_key` still exists as its own method
//! `keys::repository::find_active_public_key` already returns the owner-free
//! `ActorPublicKey` type with no further projection needed. This component
//! still defines its own `actor_public_key` method (rather than downstream
//! callers reaching into `keys::repository` directly) because design.md's
//! Service Interface names `ActorDirectory` as *the* single downstream-facing
//! reference boundary (Requirements 8.1-8.4 are all traced to this one
//! component in design.md's Requirements Traceability table) — downstream
//! specs (api-foundation, federation-core) are meant to depend on this one
//! narrow surface, not reach past it into individual repository modules.
//! `actor_public_key` is real, if thin, indirection work: it is what lets a
//! future change to how the active key is looked up (e.g. adding a cache
//! layer in front of the repository call) stay entirely behind this
//! component's contract instead of becoming a breaking change for every
//! downstream caller.
//!
//! ## `owner_id`-dropping is a real per-call projection, not merely inherited
//! [`local_actor_to_summary`]/[`local_actor_to_resolved`] destructure the
//! `LocalActor` argument field-by-field (no `..`), naming `owner_id` as an
//! explicitly discarded (`owner_id: _`) binding rather than silently omitting
//! it. This is a compile-time guarantee, not merely a convention: if a future
//! change ever added a new field to `LocalActor`, this exhaustive pattern
//! would fail to compile until it were updated, forcing whoever makes that
//! change to consciously decide whether the new field belongs on the
//! protocol-facing types too (revisiting design.md's Revalidation Triggers)
//! rather than it silently flowing through by accident.
//!
//! ## Constructor takes only a `PgPool` (Requirement: no unnecessary
//! ## non-determinism dependency)
//! Every operation this component performs is a read against an already
//! fully-formed row (no id minting, no clock reads, no rng draws) — unlike
//! `ActorService`/`SigningKeyService`, which need `RuntimeContext` for id/
//! time generation, this component needs nothing beyond a `PgPool`. Matches
//! design.md's Architecture diagram, which draws `Directory --> ActorRepo`
//! and `Directory --> KeyRepo` only, with no edge to `RuntimeCtx`.
//!
//! ## Feature Flag Protocol: not applicable
//! Like `ActorService` (task 5.1, see its own doc comment), this is a
//! brand-new internal component with no existing callers or previously
//! observable behavior to gate — bootstrap/`AppState` wiring (task 6.1) is
//! what will eventually invoke it, out of this task's boundary. A standard
//! RED -> GREEN -> REFACTOR cycle against a real Postgres instance (via
//! `spawn_test_app`) is this crate's established verification method for
//! this kind of module, matching every sibling service/repository module.

#[cfg(test)]
mod tests;

use std::fmt;

use axum::http::StatusCode;
use sqlx::postgres::PgPool;
use time::OffsetDateTime;

use super::keys::repository::find_active_public_key;
use super::model::{ActorPublicKey, ActorSummary, Handle, LocalActor, Owner, ResolvedActor};
use super::repository::{find_by_handle, list_by_owner};
use crate::domain::Id;
use crate::error::AppError;

/// Signals that the `owners` table did not contain exactly one row when
/// [`ActorDirectory::sole_owner`] queried it — a violation of this
/// single-owner-per-instance server's foundational invariant ("一人鯖前提で
/// インスタンスに厳密に1件のみ存在する", design.md's OwnerGate
/// Responsibilities). `count` is embedded so the logged `source` of the
/// resulting 5xx [`AppError`] carries enough detail to distinguish an
/// un-bootstrapped instance (`count == 0`) from a corrupted/multiply-
/// provisioned one (`count > 1`), even though neither distinction is ever
/// surfaced to the caller (see [`ActorDirectory::sole_owner`]'s own doc
/// comment for why both collapse to the same response).
#[derive(Debug)]
struct OwnerCountInvariantViolation {
    count: usize,
}

impl fmt::Display for OwnerCountInvariantViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "expected exactly one row in owners (single-owner-per-instance invariant), found {}",
            self.count
        )
    }
}

impl std::error::Error for OwnerCountInvariantViolation {}

/// Projects a management-layer [`LocalActor`] into the owner-free
/// [`ActorSummary`] downstream (management-layer-only) callers see
/// (Requirement 8.1). See this module's doc comment
/// ("`owner_id`-dropping is a real per-call projection") for why `owner_id`
/// is named-and-discarded rather than silently dropped via `..`.
fn local_actor_to_summary(actor: LocalActor) -> ActorSummary {
    let LocalActor {
        id,
        owner_id: _,
        handle,
        actor_type,
        display_name,
        summary: _,
        state,
        created_at: _,
        updated_at: _,
    } = actor;
    ActorSummary {
        id,
        handle,
        actor_type,
        display_name,
        state,
    }
}

/// Projects a management-layer [`LocalActor`] into the owner-free
/// [`ResolvedActor`] protocol-layer callers see (Requirements 3.1, 3.2, 8.2).
/// See [`local_actor_to_summary`]'s doc comment for the same
/// named-and-discarded `owner_id` discipline.
fn local_actor_to_resolved(actor: LocalActor) -> ResolvedActor {
    let LocalActor {
        id,
        owner_id: _,
        handle,
        actor_type,
        display_name,
        summary,
        state,
        created_at: _,
        updated_at: _,
    } = actor;
    ResolvedActor {
        id,
        handle,
        actor_type,
        display_name,
        summary,
        state,
    }
}

/// Downstream-facing actor reference operations (design.md's exact Service
/// Interface): [`list_actors_for_owner`](Self::list_actors_for_owner)
/// (management layer only), and
/// [`resolve_actor_by_handle`](Self::resolve_actor_by_handle) /
/// [`actor_public_key`](Self::actor_public_key) (protocol layer, never
/// exposing owner information).
pub struct ActorDirectory {
    pool: PgPool,
}

impl ActorDirectory {
    /// Builds a directory bound to `pool`. No `RuntimeContext` dependency —
    /// see this module's doc comment ("Constructor takes only a `PgPool`").
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Management-layer-only operation: returns every actor owned by
    /// `owner_id`, as owner-free [`ActorSummary`] values (Requirement 8.1).
    /// This is the *only* method on this component that takes an `owner_id`
    /// parameter at all (Requirement 3.3).
    ///
    /// The owner-scoping itself is `repository::list_by_owner`'s contract
    /// (already implemented and tested, task 2.2); an `owner_id` that owns no
    /// actors (or does not exist) yields an empty `Vec`, not an error,
    /// mirroring that repository call's own "no error for absence"
    /// convention.
    pub async fn list_actors_for_owner(&self, owner_id: Id) -> Result<Vec<ActorSummary>, AppError> {
        let actors = list_by_owner(&self.pool, owner_id).await?;
        Ok(actors.into_iter().map(local_actor_to_summary).collect())
    }

    /// Protocol-layer operation: resolves `handle` to an owner-free
    /// [`ResolvedActor`] (Requirements 3.1, 3.2), returning `Ok(None)` (not
    /// an error) when no actor is registered under `handle` (Requirement
    /// 8.2's "存在しなければ未検出を示す").
    pub async fn resolve_actor_by_handle(
        &self,
        handle: &Handle,
    ) -> Result<Option<ResolvedActor>, AppError> {
        let actor = find_by_handle(&self.pool, handle).await?;
        Ok(actor.map(local_actor_to_resolved))
    }

    /// Protocol-layer operation: returns `actor_id`'s current active signing
    /// key's public material as an owner-free [`ActorPublicKey`] (Requirement
    /// 3.1, 8.3), or `Ok(None)` when `actor_id` has no active key.
    ///
    /// Delegates directly to `keys::repository::find_active_public_key`,
    /// which already returns the owner-free `ActorPublicKey` type with
    /// nothing left to project away — see this module's doc comment ("Why
    /// `actor_public_key` still exists as its own method") for why this is
    /// still real indirection, not redundant pass-through.
    pub async fn actor_public_key(&self, actor_id: Id) -> Result<Option<ActorPublicKey>, AppError> {
        find_active_public_key(&self.pool, actor_id).await
    }

    /// Resolves this instance's single [`Owner`] row (design.md's OwnerGate
    /// Responsibilities: "actor-model の単一オーナー取得アクセサ
    /// （`ActorDirectory::sole_owner()` 相当）"), for a later `OwnerGate`
    /// (api-foundation task 4.1) to resolve `OwnerSession.owner_id` after
    /// credential verification. See this module's doc comment (`sole_owner`
    /// section) for why this method queries `owners` directly instead of
    /// delegating to a sibling `repository::` function.
    ///
    /// Zero or more than one row is a violation of this single-owner-per-
    /// instance server's foundational invariant, not a caller-input problem
    /// — an un-bootstrapped instance (no owner ever created) or data
    /// corruption (more than one), neither of which the calling request can
    /// fix by retrying with different input. Both surface as a `Server`
    /// (5xx) [`AppError`] with the row count confined to the logged
    /// `source`, matching this crate's "5xx never carries internal detail in
    /// the response body" discipline (`src/error.rs`) — deliberately never a
    /// more specific status, and never a silent `.first()`/`.last()` pick
    /// that would mask a real invariant violation as if it were the normal
    /// case.
    pub async fn sole_owner(&self) -> Result<Owner, AppError> {
        let rows: Vec<(i64, OffsetDateTime)> = sqlx::query_as("SELECT id, created_at FROM owners")
            .fetch_all(&self.pool)
            .await
            .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;

        if rows.len() != 1 {
            return Err(AppError::server(
                StatusCode::INTERNAL_SERVER_ERROR,
                OwnerCountInvariantViolation { count: rows.len() },
            ));
        }
        let (id, created_at) = rows.into_iter().next().expect("checked len == 1 above");
        Ok(Owner {
            id: Id::from_i64(id),
            created_at,
        })
    }
}
