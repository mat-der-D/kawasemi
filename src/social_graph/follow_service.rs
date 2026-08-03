//! `FollowService` (design.md "Service / サービス層" -> "#### FollowService /
//! FollowRequestService / MuteService / BlockService"; Requirements 1.1,
//! 1.2, 1.3, 1.4, 1.5, 1.6, 1.7; task 3.1, `Boundary: FollowService`):
//! aggregates the follow/unfollow business operation — self-follow
//! rejection, idempotent return of an already-existing relationship,
//! approval-necessity judgment (delegated to the already-implemented
//! [`crate::social_graph::approval_policy::FollowApprovalPolicy`]), the
//! actual state transition (delegated to the already-implemented
//! [`crate::social_graph::transitions::Transitions`]), the common Follow/
//! Undo(Follow) delivery request, and the Relationship response (via the
//! already-implemented
//! [`crate::social_graph::relationship_mapper::RelationshipMapper`] +
//! accounts-and-instance's `RelationshipSerializer`).
//!
//! ## Scope
//! This module owns exactly [`FollowService`] and its two public methods,
//! [`FollowService::follow`]/[`FollowService::unfollow`] (design.md's File
//! Structure Plan: `follow_service.rs` — "FollowService（follow/unfollow 業
//! 務集約...）"). `FollowRequestService`/`MuteService`/`BlockService` are
//! separate files/tasks (3.2/3.3/3.4, `follow_request_service.rs`/
//! `mute_service.rs`/`block_service.rs`) — design.md's Service Interface
//! block documents all four together, but the File Structure Plan and each
//! task's own `_Boundary:_` line split them into one file/component per
//! task; this task implements only the two methods task 3.1's own
//! Requirements list (1.1-1.7) and boundary (`FollowService`) name. No HTTP
//! surface (scope/auth/404 status-code discipline, Requirements 1.8, 10.5)
//! lives here either — that is task 5.1's boundary (`SocialGraphEndpoints`),
//! not named in task 3.1's own Requirements list.
//!
//! ## Deliberate deviations from design.md's literal Service Interface
//! design.md's sketch (lines ~504-514) writes:
//! ```text
//! pub async fn follow(&self, viewer: &RequestActorContext, target: &str, opts: FollowOptions) -> Result<serde_json::Value, AppError>;
//! pub async fn unfollow(&self, viewer: &RequestActorContext, target: &str) -> Result<serde_json::Value, AppError>;
//! ```
//! This module's actual signatures take `viewer_id: Id` instead of
//! `viewer: &RequestActorContext`. `RequestActorContext` (`crate::oauth::
//! model`) is a Bearer-token-authenticated-caller type api-foundation/oauth
//! owns; the two already-implemented services in this crate that call into
//! a business layer shaped like this one —
//! `crate::statuses::interaction_service::InteractionService::reblog`/
//! `unreblog`/`favourite` (this task's own named closest precedent) — take
//! a plain `actor_id: Id` and leave `RequestActorContext -> Id` extraction
//! to the endpoints layer (a later task, out of this service's boundary).
//! `crate::accounts::account_service::AccountService` is the one
//! counter-example that *does* thread `&RequestActorContext` straight
//! through into its own service methods — but every one of its
//! `RequestActorContext`-typed parameters is either unused (`show_account`'s
//! `_viewer`) or is immediately narrowed right back down to a plain
//! `ctx.actor_id: Id` at the first line of use (`relationships`). Since this
//! service's every use of "the caller" is exactly that (a plain local actor
//! `Id` — every self-follow/idempotency/transition/delivery-sender
//! computation below only ever needs `viewer_id`, never a scope set or
//! token metadata), taking `Id` directly is the narrower, equally-precedented
//! choice, and avoids adding an `oauth`-module dependency to `FollowService`
//! purely to immediately discard everything `RequestActorContext` carries
//! beyond one `Id`. `target: &str` is kept exactly as design.md specifies.
//!
//! ## `target: &str` resolution (not delegated to a shared port — none
//! exists)
//! Requirement 10.5 (404 for a nonexistent target) is task 5.1's own
//! Requirements list item, not task 3.1's — but this service still must
//! resolve `target`'s local/remote identity itself (Requirement 3.x's
//! `FollowApprovalPolicy::requires_approval` needs a real `AccountRef` +
//! lock state, and `ActivityBuilder`/`Transitions` need a real `AccountRef`
//! too), so a 404-shaped [`AppError`] naturally falls out of that
//! resolution whenever `target` does not resolve to any known account —
//! mirroring `crate::accounts::account_service::AccountService::
//! resolve_account_ref`'s identical shape (that service's own private
//! per-caller resolution helper; no shared cross-service "AccountResolver"
//! port exists anywhere in this codebase for either service to depend on
//! instead). Unlike `resolve_account_ref`, this service's own resolution
//! does **not** attempt a live network fetch for a non-numeric `target`
//! (`AccountService::show_account`'s remote-`actor_uri`-string fallback,
//! via `RemoteAccountFetcher`): a Mastodon `POST /accounts/:id/follow`
//! `:id` path segment is always the target's already-known internal numeric
//! id (a client only ever reaches a follow button after already having
//! fetched/cached that account, e.g. via search or a timeline), never a raw
//! remote actor URI — so resolution here is local-numeric-id-first, then
//! remote-cache-by-id, with no network-fetch fallback and no
//! `FederationHttpClient`/`H` generic parameter this service would
//! otherwise need to carry for a case that cannot occur through this
//! endpoint shape.
//!
//! ## `target_locked` resolution
//! [`FollowApprovalPolicy::requires_approval`] (task 2.1) needs the target's
//! raw lock state. For a resolved [`AccountRef::Local`] target this is
//! `crate::accounts::profile_repository::find_profile`'s `AccountProfile.
//! locked` (defaulting to `false` when no profile row exists yet — the same
//! "unlocked by default" convention `account_profiles`'s own schema
//! default already encodes). For a resolved [`AccountRef::Remote`] target,
//! the already-fetched [`crate::accounts::model::RemoteAccount`] row's own
//! `locked` field (populated by `RemoteAccountFetcher`'s normalization from
//! the remote actor document's `manuallyApprovesFollowers`, task 4 of
//! accounts-and-instance) is reused directly — no second query.
//!
//! ## Remote delivery's `inbox`: a documented interim gap, not a silent
//! guess
//! Requirement 1.2 requires delivering the generated Follow Activity to a
//! remote target's actual inbox URL via the common `DeliveryService` path.
//! `crate::federation::Recipient::Remote` requires an already-known `inbox`
//! string the caller supplies — but `crate::accounts::model::RemoteAccount`
//! (accounts-and-instance's own remote-account cache, out of this spec's
//! boundary to modify) carries no persisted `inbox`/`shared_inbox` field:
//! `RemoteAccountFetcher::normalize_actor_document`'s own doc comment
//! confirms only `username`/`display_name`/`note`/`url`/`avatar_url`/
//! `header_url`/`fields`/`bot`/`locked` are captured from a fetched actor
//! document, and `federation::signatures::key_resolver.rs`'s own doc
//! comment states inbox retrieval is explicitly out of federation-core's
//! own `PublicKeyResolver` boundary too ("本 spec は署名検証に必要な公開鍵
//! 素材の取得・キャッシュのみ"). No component anywhere in this codebase
//! currently persists a real remote inbox URL.
//! `crate::federation::outbound::target`'s own doc comment names exactly
//! this situation ("A remote recipient's individual inbox... must
//! therefore already be known to whatever caller builds a `Recipient` (a
//! future spec, e.g. statuses-core/social-graph...)"), confirming this
//! service is expected to supply it, without saying from where. Until
//! accounts-and-instance's `RemoteAccount` gains a real persisted inbox
//! field, this service derives an interim `inbox` as
//! `format!("{actor_uri}/inbox")` — the same actor-uri-plus-`/inbox`-suffix
//! convention this crate's own `ActorUrls::inbox_url` already uses for
//! *local* actors, and the real convention most Mastodon-compatible
//! instances (this whole crate's own compatibility target) use for their
//! own actors. `shared_inbox` is left `None` (no shared-inbox data exists
//! either). This is flagged as a CONCERNS item in this task's status report
//! rather than silently shipped as if it were a fully general solution: a
//! remote instance whose inbox does not follow this convention will not
//! actually receive the delivered Follow/Undo(Follow), even though this
//! service's own database-state transition still succeeds correctly.
//!
//! ## Generic shape mirrors `InteractionService`
//! `FollowService<AL, AR, D, LS, HS>` mirrors `InteractionService<A, D, L,
//! H, R>`'s established shape exactly: `AL`/`AR`
//! ([`crate::social_graph::activity_builder::LocalActorLookup`]/
//! [`RemoteActorLookup`]) back both the embedded `ActivityBuilder<AL, AR>`
//! *and* this service's own separate `local: AL` copy (mirrors
//! `InteractionService`'s own separate `actor_lookup: A` field, independent
//! of the one embedded in its own `StatusActivityBuilder`) — used to
//! resolve the delivering (`viewer_id`) actor's `Handle` for
//! `DeliveryRequest::sender`, and (for a [`AccountRef::Local`] target) the
//! target's own `Handle` for `Recipient::Local`. `D`
//! ([`crate::federation::LocalActorLookup`]) and `LS`/`HS`
//! ([`crate::federation::DeliverySink`]) back the embedded `Arc<DeliveryService<D,
//! LS, HS>>` exactly as `InteractionService`'s embedded `StatusActivityBuilder`
//! does.

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::http::StatusCode;
use sqlx::PgPool;

use crate::accounts::profile_repository;
use crate::accounts::relationship_serializer::RelationshipSerializer;
use crate::accounts::remote_repository;
use crate::domain::{AccountRef, Id};
use crate::error::{AppError, ErrorKind};
use crate::federation::LocalActorLookup as DeliveryLocalActorLookup;
use crate::federation::{DeliveryRequest, DeliveryService, DeliverySink, Recipient};
use crate::runtime::RuntimeContext;
use crate::social_graph::activity_builder::{ActivityBuilder, LocalActorLookup, RemoteActorLookup};
use crate::social_graph::approval_policy::{FollowApprovalPolicy, FollowDecision};
use crate::social_graph::model::{FollowOptions, FollowRequest, FollowRequestDirection};
use crate::social_graph::relationship_mapper::RelationshipMapper;
use crate::social_graph::repository::{self, RelationshipState};
use crate::social_graph::transitions::Transitions;

fn account_not_found(target: &str) -> AppError {
    AppError::client(
        StatusCode::NOT_FOUND,
        format!("account '{target}' was not found"),
    )
}

fn cannot_follow_self() -> AppError {
    AppError::client(StatusCode::UNPROCESSABLE_ENTITY, "cannot follow yourself")
}

/// `target`'s resolved identity, lock state, and ready-to-deliver
/// [`Recipient`] — this service's own private output of [`FollowService::
/// resolve_target`], bundling everything the rest of `follow`/`unfollow`
/// need about a resolved target in one step (avoiding a second query to
/// re-derive any of it).
struct ResolvedTarget {
    account: AccountRef,
    locked: bool,
    recipient: Recipient,
}

/// The follow/unfollow business-service layer (design.md's exact
/// `FollowService`, Requirements 1.1-1.7). See this module's doc comment for
/// the full generic-shape/deviation rationale.
pub struct FollowService<AL, AR, D, LS, HS>
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

impl<AL, AR, D, LS, HS> FollowService<AL, AR, D, LS, HS>
where
    AL: LocalActorLookup,
    AR: RemoteActorLookup,
    D: DeliveryLocalActorLookup,
    LS: DeliverySink,
    HS: DeliverySink,
{
    /// Builds a `FollowService` bound to `pool` (target resolution:
    /// `profile_repository`/`remote_repository`, plus every
    /// `RelationshipRepository` call this service and its collaborators
    /// make), `runtime` (`Clock` injection for `load_states`'s `now` and a
    /// freshly-recorded outbound `FollowRequest`'s `created_at` — never a
    /// direct wall-clock read, steering's determinism rule), `local` (this
    /// service's own `AccountRef::Local -> Handle` resolution, independent
    /// of `activity_builder`'s embedded copy — see this module's doc
    /// comment, "Generic shape mirrors `InteractionService`"),
    /// `activity_builder` (Follow/Undo(Follow) Activity generation, task
    /// 2.3), `transitions` (the common `establish_follow`/`record_pending`/
    /// `remove_follow` state-transition functions, task 2.2), and `delivery`
    /// (the common `DeliveryService` path, federation-core).
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
    /// its [`AccountRef`]/lock-state/[`Recipient`], local-first then
    /// remote-cache — see this module's doc comment ("`target: &str`
    /// resolution") for the full local/remote/404 discipline and why no
    /// network fetch is attempted.
    async fn resolve_target(&self, target_id: Id) -> Result<ResolvedTarget, AppError> {
        match self.local.resolve_handle(target_id).await {
            Ok(handle) => {
                let locked = profile_repository::find_profile(&self.pool, target_id)
                    .await?
                    .map(|profile| profile.locked)
                    .unwrap_or(false);
                return Ok(ResolvedTarget {
                    account: AccountRef::Local(target_id),
                    locked,
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
            // See this module's doc comment ("Remote delivery's `inbox`")
            // for why this interim convention, rather than a persisted
            // inbox field, is used here.
            let inbox = format!("{}/inbox", remote.actor_uri);
            return Ok(ResolvedTarget {
                account: AccountRef::Remote(target_id),
                locked: remote.locked,
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
    /// this service's always-one-target case).
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

    /// Follows `target` on `viewer_id`'s behalf (Requirements 1.1-1.3, 1.5-1.7):
    /// rejects a self-follow (1.7), returns the current relationship
    /// idempotently if `viewer_id` already follows or has already requested
    /// `target` (1.6, no duplicate relationship/Activity), otherwise judges
    /// approval necessity via [`FollowApprovalPolicy`] (delegating the
    /// same-server admin privilege entirely to that single judgment point,
    /// Requirement 3.x) and executes the corresponding [`Transitions`]
    /// transition, then builds and delivers a Follow Activity via the
    /// common `DeliveryService` path regardless of `target`'s locality
    /// (Requirements 1.2, 1.3) before returning the updated relationship.
    pub async fn follow(
        &self,
        viewer_id: Id,
        target: &str,
        opts: FollowOptions,
    ) -> Result<serde_json::Value, AppError> {
        let target_id = target
            .parse::<i64>()
            .map(Id::from_i64)
            .map_err(|_| account_not_found(target))?;

        if target_id == viewer_id {
            return Err(cannot_follow_self());
        }

        let resolved = self.resolve_target(target_id).await?;
        let viewer_ref = AccountRef::Local(viewer_id);
        let now = self.runtime.clock.now();

        let state = self.load_state(&viewer_ref, &resolved.account, now).await?;
        if state.follow.is_some() || state.requested {
            // Requirement 1.6: already following or already pending --
            // idempotent, no new relationship/Activity.
            return Ok(self.build_relationship(&state));
        }

        let decision =
            FollowApprovalPolicy.requires_approval(&viewer_ref, &resolved.account, resolved.locked);

        let (activity_id, activity_json) = self
            .activity_builder
            .build_follow(&viewer_ref, &resolved.account)
            .await?;

        match decision {
            FollowDecision::Establish => {
                self.transitions
                    .establish_follow(&viewer_ref, &resolved.account, &opts, &activity_id)
                    .await?;
            }
            FollowDecision::RequireApproval => {
                let req = FollowRequest {
                    requester: viewer_ref,
                    target: resolved.account,
                    direction: FollowRequestDirection::Outbound,
                    activity_id: activity_id.clone(),
                    created_at: now,
                };
                self.transitions.record_pending(&req).await?;
            }
        }

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

    /// Unfollows `target` on `viewer_id`'s behalf (Requirement 1.4): removes
    /// an existing established follow, or an existing outbound pending
    /// follow request, whichever applies (idempotent no-op, no Undo
    /// delivered, if neither exists), then builds and delivers an
    /// Undo(Follow) referencing whichever Activity id the removed
    /// relationship/request carried, via the common `DeliveryService` path,
    /// before returning the updated relationship.
    pub async fn unfollow(
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

        let state = self.load_state(&viewer_ref, &resolved.account, now).await?;

        let wrapped_activity_id = if let Some(follow) = &state.follow {
            let activity_id = follow.activity_id.clone();
            self.transitions
                .remove_follow(&viewer_ref, &resolved.account)
                .await?;
            Some(activity_id)
        } else if state.requested {
            let taken = repository::take_request(
                &self.pool,
                &viewer_ref,
                &resolved.account,
                FollowRequestDirection::Outbound,
            )
            .await?;
            taken.map(|req| req.activity_id)
        } else {
            // Requirement 1.6's idempotency principle applied to unfollow:
            // neither an established follow nor a pending request exists --
            // no-op, no Undo delivered.
            None
        };

        if let Some(wrapped_activity_id) = wrapped_activity_id {
            let (_, wrapped) = self
                .activity_builder
                .build_follow(&viewer_ref, &resolved.account)
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
