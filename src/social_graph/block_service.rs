//! `BlockService` (design.md "Service / サービス層" -> "#### FollowService /
//! FollowRequestService / MuteService / BlockService"; Requirements 5.1, 5.2,
//! 5.3, 5.4, 5.5; task 3.4, `Boundary: BlockService`): aggregates the
//! block/unblock business operation — the relationship-clearing state
//! transition (delegated to the already-implemented
//! [`crate::social_graph::transitions::Transitions::apply_block`]/
//! [`crate::social_graph::transitions::Transitions::clear_block`]), the
//! common Block/Undo(Block) delivery request, and the Relationship response
//! (via the already-implemented
//! [`crate::social_graph::relationship_mapper::RelationshipMapper`] +
//! accounts-and-instance's `RelationshipSerializer`). Structurally this is
//! `FollowService` minus the approval-policy branch: blocking is
//! unconditional, there is no "pending" outcome, and `Block`'s own state
//! transition already performs the full relationship-clearing side effect
//! (design.md's "`apply_block` は単一トランザクションで双方向フォロー・両
//! 方向の保留中フォローリクエストを解消してから block 行を確定") that this
//! service does not need to (and must not) reimplement.
//!
//! ## Scope
//! This module owns exactly [`BlockService`] and its two public methods,
//! [`BlockService::block`]/[`BlockService::unblock`] (design.md's File
//! Structure Plan: `block_service.rs` — "BlockService（block/unblock 業務集
//! 約。関係解消・配送依頼）"). No HTTP surface (scope/auth/404 status-code
//! discipline, Requirement 5.6) lives here — that is task 5.1's boundary
//! (`SocialGraphEndpoints`), not named in task 3.4's own Requirements list.
//! No `BlockPolicyImpl`/federation-core signature-rejection wiring lives here
//! either (task 4.2, out of scope) — this module only handles the
//! API-driven block/unblock + relationship state + Activity delivery, not
//! the inbound signature-rejection *consumption* of the block state this
//! module writes.
//!
//! ## Deliberate deviation from design.md's literal Service Interface
//! design.md's sketch (lines ~513-514) writes:
//! ```text
//! pub async fn block(&self, viewer: &RequestActorContext, target: &str) -> Result<serde_json::Value, AppError>;
//! pub async fn unblock(&self, viewer: &RequestActorContext, target: &str) -> Result<serde_json::Value, AppError>;
//! ```
//! This module's actual signatures take `viewer_id: Id` instead of
//! `viewer: &RequestActorContext`, for the exact same reason (and citing the
//! exact same precedent, `interaction_service.rs::reblog`/`unreblog`)
//! `follow_service.rs`'s own doc comment already documents for
//! `FollowService::follow`/`unfollow` and `mute_service.rs`'s own doc comment
//! documents for `MuteService::mute`/`unmute` — `RequestActorContext -> Id`
//! extraction is task 5.1's (endpoints) responsibility, not this service's.
//! `target: &str` is kept exactly as design.md specifies.
//!
//! ## `target: &str` resolution
//! Mirrors `follow_service.rs::FollowService::resolve_target`'s local-first
//! then remote-cache discipline exactly (see that module's own doc comment,
//! "`target: &str` resolution", for the full rationale this module does not
//! repeat) — parsed as an already-known internal numeric account id, local
//! actor existence checked first via the injected [`LocalActorLookup`], a
//! known `remote_accounts` cache row checked second, a 404-shaped
//! [`AppError`] if neither resolves, the same interim `{actor_uri}/inbox`
//! remote-inbox convention for a resolved remote target (see
//! `follow_service.rs`'s own doc comment, "Remote delivery's `inbox`", for
//! the full rationale — the same documented interim gap applies here
//! unchanged and is called out again in this task's own CONCERNS). Unlike
//! `FollowService::resolve_target`, this module's resolution needs no lock
//! state (there is no approval judgment for a block) — it returns a smaller
//! [`ResolvedTarget`] bundle (`account` + `recipient` only).
//!
//! ## Self-block: rejected (conservative choice, Requirement 5 does not name
//! an explicit rule either way)
//! Unlike `FollowService::follow`'s self-follow rejection (Requirement 1.7,
//! an explicit acceptance criterion), Requirement 5's Acceptance Criteria
//! name no analogous self-block restriction — the same kind of silence
//! `mute_service.rs`'s own doc comment documents for self-mute, where the
//! resolved choice was to permit it since nothing about muting yourself is
//! harmful. Blocking yourself is different: `Transitions::apply_block`
//! would record a `blocks` row with `blocker == blocked == viewer`, making
//! this instance's own local actor a `blocked`/`blocked_by` target of
//! itself — a state `BlockPolicyImpl` (task 4.2, out of this task's
//! boundary but downstream of the state this service writes) would then
//! have to reason about when deciding whether to reject a signed request
//! *from this same actor*, for no legitimate operational benefit (a self-
//! block's declared purpose, Requirement 5's Objective, is severing a
//! relationship with *another* account and rejecting *their* future signed
//! requests — self-signed requests are never a threat this feature is meant
//! to guard against). Since permitting it could only ever produce a
//! meaningless-at-best, actively-confusing-at-worst state with no
//! compensating benefit, this module rejects a self-block with the same
//! `422`-shaped [`AppError`] shape `FollowService::follow` already
//! established for its own self-follow rejection (Requirement 1.7) —
//! deliberately the more conservative of the two available readings, per
//! this task's own explicit instruction to prefer that when the
//! requirements are genuinely silent.
//!
//! ## Idempotency: `block` does not re-block or re-deliver; `unblock` relies
//! on `Transitions::clear_block`'s own `Option` signal (Requirement 5 has no
//! explicit acceptance criterion for this, the same silence
//! `follow_service.rs`'s own "Requirement 1.6" precedent fills in for
//! `follow`/`unfollow` — applied here by analogy since delivering a
//! duplicate Block/Undo(Block) Activity on every repeated call would be
//! poor behavior even though no Requirement explicitly forbids it)
//! [`BlockService::block`] first loads the viewer's current
//! [`crate::social_graph::repository::RelationshipState`] against `target`;
//! if `state.blocking` is already `true`, it returns that state's
//! Relationship view directly, without calling `apply_block` again or
//! building/delivering a second Block Activity — mirroring
//! `FollowService::follow`'s identical "already following or requested ->
//! idempotent, no duplicate Activity" shape for the analogous situation.
//! (Calling `apply_block` again would itself be harmless per
//! `transitions.rs`'s own documented idempotency — but re-*delivering* the
//! same logical Block Activity to a peer on every repeated `block` call is
//! not something this service should do just because the underlying
//! transition happens to tolerate it.) [`BlockService::unblock`] does not
//! need a separate pre-check: `Transitions::clear_block` (widened by this
//! task, see `transitions.rs`'s own doc comment, "Task 3.4 addition") already
//! returns `Ok(None)` when no block existed to remove, which this method
//! uses directly to skip building/delivering an `Undo(Block)` — the same
//! "no relationship existed, no Undo delivered" idempotency
//! `FollowService::unfollow` already established for the analogous
//! not-currently-following case.
//!
//! ## Local/remote delivery symmetry (Requirements 5.3, 5.5)
//! Exactly like `FollowService::follow`/`unfollow`, both
//! [`BlockService::block`]/[`BlockService::unblock`] build the identical
//! logical Block/Undo(Block) Activity regardless of `target`'s locality and
//! hand it to the one common `DeliveryService::deliver` call with a
//! `target`-derived [`Recipient`] (`Local`/`Remote`) — delivery mechanism is
//! the only thing that ever branches on locality (Requirement 10.3's
//! symmetry principle, restated here for `Block`/`Undo(Block)` by
//! Requirement 5.5).
//!
//! ## Generic shape mirrors `FollowService`
//! `BlockService<AL, AR, D, LS, HS>` mirrors `FollowService<AL, AR, D, LS,
//! HS>`'s established shape exactly (see that module's own doc comment,
//! "Generic shape mirrors `InteractionService`", for the full rationale this
//! module does not repeat): `AL`/`AR`
//! ([`crate::social_graph::activity_builder::LocalActorLookup`]/
//! [`RemoteActorLookup`]) back both the embedded `ActivityBuilder<AL, AR>`
//! *and* this service's own separate `local: AL` copy, used to resolve the
//! delivering (`viewer_id`) actor's `Handle` for `DeliveryRequest::sender`
//! and (for a [`AccountRef::Local`] target) the target's own `Handle` for
//! `Recipient::Local`. `D`/`LS`/`HS` back the embedded
//! `Arc<DeliveryService<D, LS, HS>>` exactly as `FollowService`'s does.

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::http::StatusCode;
use sqlx::PgPool;

use crate::accounts::relationship_serializer::RelationshipSerializer;
use crate::accounts::remote_repository;
use crate::domain::{AccountRef, Id};
use crate::error::{AppError, ErrorKind};
use crate::federation::LocalActorLookup as DeliveryLocalActorLookup;
use crate::federation::{DeliveryRequest, DeliveryService, DeliverySink, Recipient};
use crate::runtime::RuntimeContext;
use crate::social_graph::activity_builder::{ActivityBuilder, LocalActorLookup, RemoteActorLookup};
use crate::social_graph::relationship_mapper::RelationshipMapper;
use crate::social_graph::repository::{self, RelationshipState};
use crate::social_graph::transitions::Transitions;

fn account_not_found(target: &str) -> AppError {
    AppError::client(
        StatusCode::NOT_FOUND,
        format!("account '{target}' was not found"),
    )
}

fn cannot_block_self() -> AppError {
    AppError::client(StatusCode::UNPROCESSABLE_ENTITY, "cannot block yourself")
}

/// `target`'s resolved identity and ready-to-deliver [`Recipient`] — this
/// service's own private output of [`BlockService::resolve_target`],
/// mirroring `follow_service.rs::ResolvedTarget` minus the `locked` field (a
/// block has no approval judgment to make).
struct ResolvedTarget {
    account: AccountRef,
    recipient: Recipient,
}

/// The block/unblock business-service layer (design.md's exact
/// `BlockService`, Requirements 5.1-5.5). See this module's doc comment for
/// the full scope/deviation rationale.
pub struct BlockService<AL, AR, D, LS, HS>
where
    AL: LocalActorLookup,
    AR: RemoteActorLookup,
    D: DeliveryLocalActorLookup,
    LS: DeliverySink,
    HS: DeliverySink,
{
    pool: PgPool,
    runtime: RuntimeContext,
    local: AL,
    activity_builder: ActivityBuilder<AL, AR>,
    transitions: Transitions,
    delivery: Arc<DeliveryService<D, LS, HS>>,
}

impl<AL, AR, D, LS, HS> BlockService<AL, AR, D, LS, HS>
where
    AL: LocalActorLookup,
    AR: RemoteActorLookup,
    D: DeliveryLocalActorLookup,
    LS: DeliverySink,
    HS: DeliverySink,
{
    /// Builds a `BlockService` bound to `pool` (target resolution:
    /// `remote_repository`, plus every `RelationshipRepository` call this
    /// service and its collaborators make), `runtime` (`Clock` injection for
    /// `load_states`'s `now` — never a direct wall-clock read, steering's
    /// determinism rule), `local` (this service's own `AccountRef::Local ->
    /// Handle` resolution, independent of `activity_builder`'s embedded
    /// copy — mirrors `FollowService`'s identical precedent), `activity_builder`
    /// (Block/Undo(Block) Activity generation, task 2.3), `transitions` (the
    /// common `apply_block`/`clear_block` state-transition functions, task
    /// 2.2), and `delivery` (the common `DeliveryService` path,
    /// federation-core).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pool: PgPool,
        runtime: RuntimeContext,
        local: AL,
        activity_builder: ActivityBuilder<AL, AR>,
        transitions: Transitions,
        delivery: Arc<DeliveryService<D, LS, HS>>,
    ) -> Self {
        Self {
            pool,
            runtime,
            local,
            activity_builder,
            transitions,
            delivery,
        }
    }

    /// Resolves `target` (an already-parsed internal numeric account id) to
    /// its [`AccountRef`]/[`Recipient`], local-first then remote-cache — see
    /// this module's doc comment ("`target: &str` resolution") for the full
    /// local/remote/404 discipline.
    async fn resolve_target(&self, target_id: Id) -> Result<ResolvedTarget, AppError> {
        match self.local.resolve_handle(target_id).await {
            Ok(handle) => {
                return Ok(ResolvedTarget {
                    account: AccountRef::Local(target_id),
                    recipient: Recipient::Local(handle),
                });
            }
            Err(err) if err.kind == ErrorKind::Client => {
                // Not a local actor -- fall through to the remote-cache
                // check below. A genuine server-side failure (the `Err(err)`
                // arm below) is never swallowed this way.
            }
            Err(err) => return Err(err),
        }

        if let Some(remote) = remote_repository::find_remote_by_id(&self.pool, target_id).await? {
            // See `follow_service.rs`'s own doc comment ("Remote delivery's
            // `inbox`") for why this interim convention, rather than a
            // persisted inbox field, is used here.
            let inbox = format!("{}/inbox", remote.actor_uri);
            return Ok(ResolvedTarget {
                account: AccountRef::Remote(target_id),
                recipient: Recipient::Remote {
                    inbox,
                    shared_inbox: None,
                },
            });
        }

        Err(account_not_found(&target_id.as_i64().to_string()))
    }

    /// Loads `viewer`'s single relationship state to `target` (a thin
    /// wrapper around [`repository::load_states`]'s batched interface for
    /// this service's always-one-target case; mirrors
    /// `follow_service.rs::FollowService::load_state` exactly).
    async fn load_state(
        &self,
        viewer: &AccountRef,
        target: &AccountRef,
        now: time::OffsetDateTime,
    ) -> Result<RelationshipState, AppError> {
        let mut states =
            repository::load_states(&self.pool, viewer, std::slice::from_ref(target), now).await?;
        Ok(states
            .pop()
            .expect("load_states returns exactly one state per requested target"))
    }

    /// Maps `state` to its Relationship JSON response via the already-
    /// implemented [`RelationshipMapper`] + accounts-and-instance's
    /// `RelationshipSerializer` (Requirement 8.3: consumed, never
    /// redefined).
    fn build_relationship(&self, state: &RelationshipState) -> serde_json::Value {
        let view = RelationshipMapper.to_view(state);
        RelationshipSerializer::new().build_relationship(&view)
    }

    /// Blocks `target` on `viewer_id`'s behalf (Requirements 5.1-5.3, 5.5):
    /// rejects a self-block (see this module's doc comment, "Self-block"),
    /// returns the current relationship idempotently if `viewer_id` already
    /// blocks `target` (no duplicate Activity, see this module's doc
    /// comment, "Idempotency"), otherwise executes
    /// [`Transitions::apply_block`] (clears both-direction follows/pending
    /// requests and persists the `Block` row, single transaction, already
    /// implemented) with a fresh Block Activity id, then builds and delivers
    /// the Block Activity via the common `DeliveryService` path regardless
    /// of `target`'s locality (Requirements 5.3, 5.5) before returning the
    /// updated relationship (`blocking=true`, Requirement 5.1).
    pub async fn block(&self, viewer_id: Id, target: &str) -> Result<serde_json::Value, AppError> {
        let target_id = target
            .parse::<i64>()
            .map(Id::from_i64)
            .map_err(|_| account_not_found(target))?;

        if target_id == viewer_id {
            return Err(cannot_block_self());
        }

        let resolved = self.resolve_target(target_id).await?;
        let viewer_ref = AccountRef::Local(viewer_id);
        let now = self.runtime.clock.now();

        let state = self.load_state(&viewer_ref, &resolved.account, now).await?;
        if state.blocking {
            // Already blocking -- idempotent, no duplicate Block Activity
            // (this module's doc comment, "Idempotency").
            return Ok(self.build_relationship(&state));
        }

        let (activity_id, activity_json) = self
            .activity_builder
            .build_block(&viewer_ref, &resolved.account)
            .await?;

        self.transitions
            .apply_block(&viewer_ref, &resolved.account, &activity_id)
            .await?;

        let sender = self.local.resolve_handle(viewer_id).await?;
        self.delivery
            .deliver(DeliveryRequest {
                activity: activity_json,
                sender,
                recipients: vec![resolved.recipient],
            })
            .await?;

        let updated = self.load_state(&viewer_ref, &resolved.account, now).await?;
        Ok(self.build_relationship(&updated))
    }

    /// Unblocks `target` on `viewer_id`'s behalf (Requirement 5.4): removes
    /// an existing block via [`Transitions::clear_block`] (idempotent no-op,
    /// no Undo delivered, if no block exists — see this module's doc
    /// comment, "Idempotency"), then builds and delivers an Undo(Block)
    /// referencing the removed block's own Activity id via the common
    /// `DeliveryService` path, before returning the updated relationship.
    pub async fn unblock(
        &self,
        viewer_id: Id,
        target: &str,
    ) -> Result<serde_json::Value, AppError> {
        let target_id = target
            .parse::<i64>()
            .map(Id::from_i64)
            .map_err(|_| account_not_found(target))?;

        let resolved = self.resolve_target(target_id).await?;
        let viewer_ref = AccountRef::Local(viewer_id);
        let now = self.runtime.clock.now();

        let removed_activity_id = self
            .transitions
            .clear_block(&viewer_ref, &resolved.account)
            .await?;

        if let Some(wrapped_activity_id) = removed_activity_id {
            let (_, wrapped) = self
                .activity_builder
                .build_block(&viewer_ref, &resolved.account)
                .await?;
            let undo = self
                .activity_builder
                .build_undo(&viewer_ref, &wrapped_activity_id, wrapped)
                .await?;

            let sender = self.local.resolve_handle(viewer_id).await?;
            self.delivery
                .deliver(DeliveryRequest {
                    activity: undo,
                    sender,
                    recipients: vec![resolved.recipient],
                })
                .await?;
        }

        let updated = self.load_state(&viewer_ref, &resolved.account, now).await?;
        Ok(self.build_relationship(&updated))
    }
}
