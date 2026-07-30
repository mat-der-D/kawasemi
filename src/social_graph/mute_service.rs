//! `MuteService` (design.md "Service / サービス層" -> "#### FollowService /
//! FollowRequestService / MuteService / BlockService"; Requirements 4.1,
//! 4.2, 4.3, 4.4, 4.5; task 3.3, `Boundary: MuteService`): aggregates the
//! mute/unmute business operations as a **DB-state-only** update — no
//! federation Activity, no `DeliveryService` call, no `FollowApprovalPolicy`
//! involvement (Requirement 4.5: "ミュートを連合 Activity として外部へ配送
//! しないローカル限定の関係として扱う"). This is the simplest of the four
//! task-3.x relationship services precisely because it skips the entire
//! federation/approval-policy machinery `FollowService`/`FollowRequestService`
//! (tasks 3.1/3.2) and `BlockService` (task 3.4) all need.
//!
//! ## Scope
//! This module owns exactly [`MuteService`] and its two public methods,
//! [`MuteService::mute`]/[`MuteService::unmute`] (design.md's File Structure
//! Plan: `mute_service.rs` — "MuteService（mute/unmute 業務集約。連合な
//! し）"). No `ActivityBuilder`/`DeliveryService`/`Transitions` call lives
//! here: design.md's own component note for `MuteService` names only
//! `RelationshipRepository, RelationshipMapper (P0)` as Key Dependencies —
//! unlike every sibling service's row, no `ActivityBuilder`/`Delivery`
//! appears at all. No HTTP surface (scope/auth/404 status-code discipline,
//! Requirement 4.6) lives here either — that is task 5.1's boundary
//! (`SocialGraphEndpoints`), not named in task 3.3's own Requirements list.
//!
//! ## Deliberate deviation from design.md's literal Service Interface
//! design.md's sketch (lines ~511-512) writes:
//! ```text
//! pub async fn mute(&self, viewer: &RequestActorContext, target: &str, opts: MuteOptions) -> Result<serde_json::Value, AppError>;
//! pub async fn unmute(&self, viewer: &RequestActorContext, target: &str) -> Result<serde_json::Value, AppError>;
//! ```
//! This module's actual signatures take `viewer_id: Id` instead of
//! `viewer: &RequestActorContext`, for the exact same reason (and citing the
//! exact same precedent, `interaction_service.rs::reblog`/`unreblog`)
//! `follow_service.rs`'s own doc comment already documents for
//! `FollowService::follow`/`unfollow` — `RequestActorContext -> Id`
//! extraction is task 5.1's (endpoints) responsibility, not this service's.
//! `target: &str` and `opts: MuteOptions` are kept exactly as design.md
//! specifies.
//!
//! ## `target: &str` resolution
//! Mirrors `follow_service.rs::FollowService::resolve_target`'s local-first
//! then remote-cache discipline (see that module's own doc comment,
//! "`target: &str` resolution", for the full rationale this module does not
//! repeat) — parsed as an already-known internal numeric account id, local
//! actor existence checked first via the injected [`LocalActorLookup`], a
//! known `remote_accounts` cache row checked second, a 404-shaped
//! [`AppError`] if neither resolves. Unlike `FollowService::resolve_target`,
//! this module's resolution needs neither the target's lock state (no
//! approval judgment here) nor a ready-to-deliver `Recipient` (no delivery
//! here) — it only needs the resolved [`AccountRef`] itself, so it returns
//! that directly rather than a richer `ResolvedTarget` bundle.
//!
//! ## No self-mute rejection
//! Unlike `FollowService::follow`'s self-follow rejection (Requirement 1.7),
//! Requirement 4's Acceptance Criteria name no analogous self-mute
//! restriction, so none is added here — muting oneself is permitted and
//! behaves identically to muting any other resolved target.
//!
//! ## `expires_at` computation: resolved once, at mute time, via the
//! injected `Clock`
//! Requirement 4.3's "期限指定" is satisfied by resolving
//! [`MuteOptions::duration`] (an optional *relative* seconds count) into
//! [`Mute::expires_at`] (an *absolute* timestamp) exactly once, at mute
//! time, as `runtime.clock.now() + Duration::seconds(duration)` — never a
//! direct wall-clock read (steering's "注入可能な非決定性境界" determinism
//! rule, the same `RuntimeContext`/`Clock` injection `follow_service.rs`
//! already uses for its own `now`). The *enforcement* of that expiry (a
//! muted account no longer reading as muted once `expires_at` has passed)
//! is entirely [`crate::social_graph::relationship_mapper::RelationshipMapper`]/
//! [`crate::social_graph::repository::load_states`]'s job (task 2.4,
//! already implemented) at *read* time — this service only ever writes a
//! correctly-resolved absolute timestamp; it performs no expiry check of
//! its own.
//!
//! ## Idempotent upsert
//! [`crate::social_graph::repository::upsert_mute`]'s `ON CONFLICT DO
//! UPDATE` already makes a repeat `mute` call for the same `(muter, muted)`
//! pair idempotent at the storage layer (refreshing `notifications`/
//! `expires_at` rather than erroring or duplicating) — this service adds no
//! additional idempotency logic of its own, it simply always calls
//! `upsert_mute`.
//!
//! ## No `ActivityBuilder`/`DeliveryService`/`Transitions` generic parameters
//! Unlike `FollowService<AL, AR, D, LS, HS>`/`FollowRequestService<AL, AR, D,
//! LS, HS>`, [`MuteService`] is generic over exactly one type parameter, `AL:
//! LocalActorLookup` (local-actor-existence resolution only) — there is no
//! `RemoteActorLookup`/`DeliveryLocalActorLookup`/`DeliverySink` parameter
//! because this service never builds or delivers an Activity (Requirement
//! 4.5). Remote-target existence is checked directly via
//! `crate::accounts::remote_repository::find_remote_by_id` (a free function,
//! not a port), mirroring `FollowService::resolve_target`'s own identical
//! direct use of that same free function for its own remote-cache check.

#[cfg(test)]
mod tests;

use axum::http::StatusCode;
use sqlx::PgPool;
use time::Duration;

use crate::accounts::relationship_serializer::RelationshipSerializer;
use crate::accounts::remote_repository;
use crate::domain::{AccountRef, Id};
use crate::error::{AppError, ErrorKind};
use crate::runtime::RuntimeContext;
use crate::social_graph::activity_builder::LocalActorLookup;
use crate::social_graph::model::{Mute, MuteOptions};
use crate::social_graph::relationship_mapper::RelationshipMapper;
use crate::social_graph::repository::{self, RelationshipState};

fn account_not_found(target: &str) -> AppError {
    AppError::client(
        StatusCode::NOT_FOUND,
        format!("account '{target}' was not found"),
    )
}

/// The mute/unmute business-service layer (design.md's exact `MuteService`,
/// Requirements 4.1-4.5). See this module's doc comment for the full
/// scope/deviation rationale.
pub struct MuteService<AL>
where
    AL: LocalActorLookup,
{
    pool: PgPool,
    runtime: RuntimeContext,
    local: AL,
}

impl<AL> MuteService<AL>
where
    AL: LocalActorLookup,
{
    /// Builds a `MuteService` bound to `pool` (target resolution:
    /// `remote_repository`, plus every `RelationshipRepository` call this
    /// service makes), `runtime` (`Clock` injection for `expires_at`
    /// computation and `load_states`'s `now`, `IdGenerator` injection for
    /// minting a fresh `mutes.id` — never a direct wall-clock read or raw
    /// counter, steering's determinism rule), and `local` (this service's
    /// own `AccountRef::Local` existence check).
    pub fn new(pool: PgPool, runtime: RuntimeContext, local: AL) -> Self {
        Self {
            pool,
            runtime,
            local,
        }
    }

    /// Parses `target` as an already-known internal numeric account id and
    /// resolves it to its [`AccountRef`], local-first then remote-cache
    /// (see this module's doc comment, "`target: &str` resolution") — a
    /// 404-shaped [`AppError`] if `target` does not parse as a numeric id or
    /// resolves to neither a local actor nor a known remote account.
    async fn resolve_target(&self, target: &str) -> Result<AccountRef, AppError> {
        let target_id = target
            .parse::<i64>()
            .map(Id::from_i64)
            .map_err(|_| account_not_found(target))?;

        match self.local.resolve_handle(target_id).await {
            Ok(_) => return Ok(AccountRef::Local(target_id)),
            Err(err) if err.kind == ErrorKind::Client => {
                // Not a local actor -- fall through to the remote-cache
                // check below. A genuine server-side failure (the `Err(err)`
                // arm below) is never swallowed this way.
            }
            Err(err) => return Err(err),
        }

        if remote_repository::find_remote_by_id(&self.pool, target_id)
            .await?
            .is_some()
        {
            return Ok(AccountRef::Remote(target_id));
        }

        Err(account_not_found(target))
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

    /// Mutes `target` on `viewer_id`'s behalf (Requirements 4.1-4.3): resolves
    /// `target` (404 if neither a local actor nor a known remote account),
    /// resolves `opts.duration` (if any) into an absolute `expires_at` via
    /// the injected `Clock` (see this module's doc comment, "`expires_at`
    /// computation"), and idempotently upserts the mute row — no Activity is
    /// built, no delivery is attempted (Requirement 4.5) — before returning
    /// the updated relationship (`muting` true, `muting_notifications`
    /// reflecting `opts.notifications`).
    pub async fn mute(
        &self,
        viewer_id: Id,
        target: &str,
        opts: MuteOptions,
    ) -> Result<serde_json::Value, AppError> {
        let target_account = self.resolve_target(target).await?;
        let viewer_ref = AccountRef::Local(viewer_id);
        let now = self.runtime.clock.now();

        let expires_at = opts
            .duration
            .map(|seconds| now + Duration::seconds(seconds));

        let mute = Mute {
            muter: viewer_ref,
            muted: target_account,
            notifications: opts.notifications,
            expires_at,
            created_at: now,
        };

        let id = self.runtime.ids.next_id();
        repository::upsert_mute(&self.pool, id, &mute).await?;

        let state = self.load_state(&viewer_ref, &target_account, now).await?;
        Ok(self.build_relationship(&state))
    }

    /// Unmutes `target` on `viewer_id`'s behalf (Requirement 4.4): resolves
    /// `target` (404 if neither a local actor nor a known remote account),
    /// deletes the mute row if any (idempotent no-op success if none
    /// exists — no error either way, mirroring `FollowService::unfollow`'s
    /// identical "no relationship to remove" idempotency for a plain state
    /// removal, and Requirement 4.5's "連合 Activity を伴わない" as no
    /// Undo-shaped delivery would even make sense for a relationship that
    /// was never delivered in the first place) — before returning the
    /// updated relationship.
    pub async fn unmute(&self, viewer_id: Id, target: &str) -> Result<serde_json::Value, AppError> {
        let target_account = self.resolve_target(target).await?;
        let viewer_ref = AccountRef::Local(viewer_id);

        repository::delete_mute(&self.pool, &viewer_ref, &target_account).await?;

        let now = self.runtime.clock.now();
        let state = self.load_state(&viewer_ref, &target_account, now).await?;
        Ok(self.build_relationship(&state))
    }
}
