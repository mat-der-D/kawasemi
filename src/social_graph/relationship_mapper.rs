//! `RelationshipMapper` (design.md "Serialization / 写像層" ->
//! `#### RelationshipMapper`, design.md lines ~466-485; Requirements 8.3,
//! 8.4, 8.5, 4.3; task 2.4, `Boundary: RelationshipMapper`): the single
//! point that maps this spec's own relationship state
//! ([`crate::social_graph::repository::RelationshipState`]) onto
//! accounts-and-instance's `RelationshipView` contract
//! (`crate::accounts::model::RelationshipView`).
//!
//! ## Scope
//! This module owns exactly design.md's `RelationshipMapper` Service
//! Interface: [`RelationshipMapper::to_view`]. It does not load
//! `RelationshipState` itself (that is `RelationshipRepository::load_states`'s
//! job, task 1.3, already complete and out of this task's boundary), does
//! not hand the resulting `RelationshipView` to `RelationshipSerializer`
//! (that is the eventual `FollowService`/`MuteService`/`BlockService`/
//! `RelProviderImpl` callers' job, later tasks), and does not redefine the
//! `RelationshipView` type or any of its fields (Requirement 8.3 — "本 spec
//! は消費し Relationship 契約を再定義しない"). It consumes
//! `crate::accounts::model::RelationshipView` verbatim, unmodified.
//!
//! ## Mapping rules (Requirements 8.3, 8.4, 8.5, 4.3)
//! - `id`: `state.target`'s id, extracted via the same
//!   `AccountRef::Local(id) | AccountRef::Remote(id) => id` match pattern
//!   already established at `crate::social_graph::repository::account_id`
//!   and `FollowApprovalPolicy`'s same-server judgment — no new helper.
//! - `following` = `state.follow.is_some()`; `showing_reblogs` = the
//!   follow's `reblogs` field (`false` if no follow); `notifying` = the
//!   follow's `notify` field (`false` if no follow); `languages` = the
//!   follow's `languages` field, cloned (empty `Vec` if no follow) —
//!   Requirement 1.5's flags, backed by [`crate::social_graph::model::Follow`]'s
//!   identically-named fields.
//! - `followed_by` / `blocking` / `blocked_by` / `requested` /
//!   `requested_by` are direct passthroughs of `state`'s identically-named
//!   fields.
//! - `muting` = `state.mute.is_some()`; `muting_notifications` = the mute's
//!   `notifications` field (`false` if no mute). `state.mute` is *already*
//!   expiry-filtered by `RelationshipRepository::load_states` before this
//!   mapper ever sees it (see `RelationshipState::mute`'s own doc comment,
//!   Requirements 4.3/9.3): an expired mute is represented as `None`, not as
//!   `Some` with a past `expires_at`, so this mapper performs no expiry
//!   check of its own — it only asks "is there a (non-expired) mute at
//!   all?".
//! - `domain_blocking` is always `false` (Requirement 8.5 — domain blocks
//!   are out of this spec's scope).
//! - `endorsed` is always `false`, `note` is always `""` (this spec has no
//!   endorsement/note feature; design.md: "本 spec が機能を持たないため既
//!   定（false / 空）").
//!
//! ## Zero-field unit struct, mirroring `RelationshipSerializer`
//! Like `crate::accounts::relationship_serializer::RelationshipSerializer`,
//! [`RelationshipMapper`] carries no state and performs no I/O — every input
//! it needs is already resolved and passed in via `&RelationshipState`. It
//! exists as a unit struct purely for interface parity with design.md's
//! literal `pub fn to_view(&self, state: &RelationshipState) ->
//! RelationshipView;` method-on-a-mapper shape (and so a future caller can
//! hold it alongside `RelationshipSerializer`), not because it needs `self`
//! for anything.
//!
//! ## Pure, synchronous, infallible — no DB, no async gap
//! Unlike task 2.3's `ActivityBuilder` (which needed an async actor-URI
//! resolution gap design.md's sketch didn't have), this mapper's single
//! input, `RelationshipState`, already carries everything the output needs
//! — the caller (`RelationshipRepository::load_states`, a later service, or
//! this module's own tests) is responsible for resolving that state first.
//! So `to_view` matches design.md's literal signature exactly: synchronous,
//! infallible, no `Result`.
//!
//! No persistence, no HTTP, and no other component's boundary are touched
//! here — this module only constructs `RelationshipView` values from an
//! already-resolved `RelationshipState`.

#[cfg(test)]
mod tests;

use crate::accounts::model::RelationshipView;
use crate::domain::AccountRef;
use crate::social_graph::repository::RelationshipState;

/// The single relationship-state -> `RelationshipView` mapping point
/// (design.md's exact `RelationshipMapper`). See this module's doc comment
/// for the full field-by-field mapping rules.
#[derive(Debug, Clone, Copy, Default)]
pub struct RelationshipMapper;

impl RelationshipMapper {
    /// Maps `state` (this spec's own relationship-state truth) onto
    /// accounts-and-instance's `RelationshipView` contract, deriving every
    /// flag deterministically per this module's doc comment (Requirements
    /// 8.3, 8.4, 8.5, 4.3). Pure, synchronous, infallible — no DB, no async.
    pub fn to_view(&self, state: &RelationshipState) -> RelationshipView {
        let id = match state.target {
            AccountRef::Local(id) | AccountRef::Remote(id) => id,
        };

        let following = state.follow.is_some();
        let showing_reblogs = state.follow.as_ref().map(|f| f.reblogs).unwrap_or(false);
        let notifying = state.follow.as_ref().map(|f| f.notify).unwrap_or(false);
        let languages = state
            .follow
            .as_ref()
            .map(|f| f.languages.clone())
            .unwrap_or_default();

        let muting = state.mute.is_some();
        let muting_notifications = state
            .mute
            .as_ref()
            .map(|m| m.notifications)
            .unwrap_or(false);

        RelationshipView {
            id,
            following,
            showing_reblogs,
            notifying,
            languages,
            followed_by: state.followed_by,
            blocking: state.blocking,
            blocked_by: state.blocked_by,
            muting,
            muting_notifications,
            requested: state.requested,
            requested_by: state.requested_by,
            domain_blocking: false,
            endorsed: false,
            note: String::new(),
        }
    }
}
