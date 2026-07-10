//! `ActorDirectory` (design.md "Service / サービス層" -> "ActorDirectory";
//! Requirements 2.5, 3.1, 3.2, 3.3, 8.1, 8.2, 8.3, 8.4; task 5.2): downstream-
//! facing actor reference operations. Splits, at the type level, a
//! management-layer-only operation (`list_actors_for_owner`, the only method
//! in this component allowed to take an `owner_id` parameter at all —
//! Requirement 3.3) from the protocol-layer/public reference operations
//! (`resolve_actor_by_handle`, `actor_public_key`), neither of which may
//! return owner information (Requirements 3.1, 3.2, 8.4).
//!
//! Scope: this module owns exactly the three operations design.md's Service
//! Interface specifies for this component, each a thin read-only projection
//! over an already-implemented repository call:
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

use sqlx::postgres::PgPool;

use super::keys::repository::find_active_public_key;
use super::model::{ActorPublicKey, ActorSummary, Handle, LocalActor, ResolvedActor};
use super::repository::{find_by_handle, list_by_owner};
use crate::domain::Id;
use crate::error::AppError;

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
}
