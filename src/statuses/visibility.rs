//! `VisibilityPolicy` / `RelationshipQuery(port)` (design.md "Visibility /
//! 可視性層" -> `#### VisibilityPolicy / Addressing`, design.md lines
//! ~451-481; Requirements 4.1, 6.1, 6.3, 6.4, 9.5, 12.4; task 3.1,
//! `Boundary: VisibilityPolicy, RelationshipQuery(port)`): the single
//! visibility judgment [`is_visible`] that "取得・context・操作の可視性チェ
//! ックで同一適用" (Requirement 4.1) must all funnel through, plus the
//! `RelationshipQuery` delegation-port contract and its safe-by-default
//! implementation [`NoRelationshipQuery`] that [`is_visible`]'s `private`
//! branch needs a resolved [`ViewerRelation`] from.
//!
//! ## Scope: `is_visible` + the port only — not `Addressing`
//! design.md bundles `VisibilityPolicy` and `Addressing` under one heading,
//! but this task's own instruction and `tasks.md`'s task 3.2 (a separate,
//! `_Depends: 3.1_` task) split them: this module owns exactly
//! [`ViewerRelation`], [`RelationshipQuery`], [`NoRelationshipQuery`], and
//! [`is_visible`]. `derive_addressing`/`derive_recipients`/`Addressing`
//! (design.md's `to`/`cc`/recipient-set derivation) are task 3.2's
//! boundary, not implemented here.
//!
//! ## Replaces `status_repository`'s temporary fail-closed stand-in
//! `src/statuses/status_repository.rs`'s own `is_visible_to` (its module
//! doc comment, "Visible-scope filtering without `VisibilityPolicy`") is an
//! explicitly-documented **temporary** stand-in for *this* function,
//! narrower than the real policy (its `private`/`direct` branches are
//! author-only, admitting no followers at all). [`is_visible`] here is that
//! real policy. Per this task's own boundary instruction, wiring
//! `status_repository`'s queries to actually call [`is_visible`] is
//! explicitly **not** this task's job (a later task's responsibility) — this
//! module only supplies the correct, complete, standalone function.
//!
//! ## `RelationshipQuery`: reuses the existing `Recipient` type
//! design.md's own `followers_of` signature returns `Vec<Recipient>` "配送
//! 先" (delivery-destination) values without spelling out `Recipient`'s
//! fields anywhere. A `Recipient` type already exists in this codebase —
//! [`crate::federation::Recipient`] (`Local(Handle)` / `Remote{inbox,
//! shared_inbox}`, federation-core's own recipient-classification type for
//! `DeliveryService`) — and is exactly the shape design.md's own note
//! implies this port's followers must ultimately feed into ("`private`/
//! `unlisted` の recipient 確定は... followers を実際の受信者集合へ展開"
//! for `StatusActivityBuilder`/`DeliveryService`, design.md's `Addressing`
//! Responsibilities). Reusing it here — rather than defining a second,
//! structurally identical `Recipient` type in this module that a later task
//! would just have to reconcile with the federation-core one — keeps
//! exactly one canonical "who receives this delivery" type in the crate.
//! Depending on `crate::federation` from `crate::statuses` introduces no
//! cycle: `src/federation.rs`'s own module tree never references
//! `crate::statuses`.
//!
//! ## `#[allow(async_fn_in_trait)]`
//! Mirrors this crate's established pattern for other design.md-pinned
//! literal-`async fn` delegation-port traits with a safe default
//! implementation (e.g. `BlockPolicy`/`NoopBlockPolicy`,
//! `LocalActorLookup`) — boxing/`Send`-pinning concerns belong to whichever
//! later task needs `Arc<dyn RelationshipQuery>` across a `tokio::spawn`
//! boundary (e.g. task 5.x's services), not this task's boundary.
//!
//! ## `direct` visibility: author-only (documented judgment call — see
//! CONCERNS in this task's status report)
//! [`is_visible`]'s exact signature is design.md's own literal interface —
//! `fn is_visible(status: &Status, viewer: Option<Id>, rel: &ViewerRelation)
//! -> bool` — and takes no mentions/addressee list at all. [`Status`]
//! itself (`src/statuses/model.rs`) also carries no mentions field (mentions
//! are extracted from `content` and threaded through `Addressing`/
//! `StatusActivityBuilder` as a separate `&[ActorRef]` parameter, per
//! design.md's `derive_addressing`/`derive_recipients` signatures — never
//! stored on `Status` itself, consistent with `Status::
//! status_holds_no_dialect_fields_beyond_the_core_field_set`'s exhaustive
//! field-set proof in `model.rs`). With neither the function's parameters
//! nor its subject type carrying mentee identities, this function has no
//! data by which to recognize "viewer is one of this direct post's mentioned
//! recipients" — so it cannot implement full Mastodon `direct` semantics
//! (visible to author + mentioned recipients) without extending the
//! signature design.md itself pins. This implementation therefore treats
//! `direct` as visible only to the post's own author, the narrowest
//! interpretation consistent with the given signature (never wrongly
//! *admitting* a non-mentioned stranger) and a strict, structural superset
//! of `status_repository::is_visible_to`'s existing stand-in for the same
//! case (so replacing that stand-in with this function, whenever a later
//! task does so, cannot newly leak a `direct` post to anyone it did not
//! already leak to). A caller that additionally needs "is `viewer` among
//! this post's mentioned recipients" (e.g. a future `StatusService::
//! find_by_id`) must resolve that separately and OR it with this function's
//! result — out of this task's boundary to decide how.

#[cfg(test)]
mod tests;

use crate::domain::{Id, Visibility};
use crate::error::AppError;
use crate::federation::Recipient;
use crate::statuses::model::Status;

/// Whether `viewer` follows a post's author, as resolved by
/// [`RelationshipQuery::viewer_relation`] (design.md's exact `ViewerRelation`
/// interface). [`is_visible`]'s `private` branch is the sole consumer within
/// this task's boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ViewerRelation {
    pub is_follower: bool,
}

/// The relationship-seam delegation port (design.md's exact
/// `RelationshipQuery` interface; Requirements 4.1, 6.1) that [`is_visible`]
/// (via a caller-resolved [`ViewerRelation`]) and a later task's
/// `Addressing`/`derive_recipients` (via `followers_of`) depend on to
/// resolve viewer-follows-author state and a post author's followers'
/// delivery recipients without this spec owning any follow-graph storage
/// itself — social-graph (a later, out-of-boundary spec) supplies the real
/// implementation; this spec owns only the contract and the safe default
/// ([`NoRelationshipQuery`]).
#[allow(async_fn_in_trait)]
pub trait RelationshipQuery: Send + Sync {
    /// Resolves whether `viewer` currently follows `author`. `viewer:
    /// None` (unauthenticated) always resolves to "not a follower" —
    /// implementations must not error on an absent viewer.
    async fn viewer_relation(
        &self,
        author: Id,
        viewer: Option<Id>,
    ) -> Result<ViewerRelation, AppError>;

    /// Resolves `author`'s followers as delivery recipients, for a
    /// `private`/`unlisted` post's followers-collection addressing
    /// (design.md's `Addressing` Responsibilities; task 3.2's boundary, not
    /// consumed within this task).
    async fn followers_of(&self, author: Id) -> Result<Vec<Recipient>, AppError>;
}

/// This spec's own default [`RelationshipQuery`] (design.md: "既定実装
/// `NoRelationshipQuery`: `viewer_relation` は常に `ViewerRelation {
/// is_follower: false }`、`followers_of` は常に空 `Vec` を返す"). Lets
/// [`is_visible`] and a later task's `Addressing` operate correctly and
/// safely (non-follower / empty-followers, never leaking a `private` post
/// to a non-author or silently dropping a delivery) even when social-graph
/// is not wired in at all.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoRelationshipQuery;

impl RelationshipQuery for NoRelationshipQuery {
    async fn viewer_relation(
        &self,
        _author: Id,
        _viewer: Option<Id>,
    ) -> Result<ViewerRelation, AppError> {
        Ok(ViewerRelation { is_follower: false })
    }

    async fn followers_of(&self, _author: Id) -> Result<Vec<Recipient>, AppError> {
        Ok(Vec::new())
    }
}

/// The single visibility judgment (design.md's exact `is_visible`
/// interface; Requirements 4.1, 6.1, 6.3, 6.4, 9.5, 12.4) every retrieval
/// (6.1), context traversal (6.3), and interaction-target check (9.5,
/// 12.4's underlying "is the pin/reblog target visible to the actor at
/// all" question) is meant to funnel through, so ローカル宛/リモート宛
/// judgments never diverge (4.1).
///
/// - The post's own author always sees their own post, regardless of
///   `visibility` — otherwise an author could not retrieve/edit/pin their
///   own `private`/`direct` post, which no requirement intends.
/// - `Public`: visible to everyone, authenticated or not.
/// - `Unlisted`: visible to any *authenticated* viewer, but — like
///   `Private`/`Direct` — invisible to an unauthenticated one (Requirement
///   6.4: "未認証は公開のみ可視", i.e. unauthenticated retrieval returns
///   *only* `Public` posts, not `Public` + `Unlisted`).
/// - `Private`: visible to an authenticated viewer only when `rel.
///   is_follower` is `true` (Requirement 4.1's "`private` の可視判定は
///   viewer が投稿者のフォロワーかどうか"). [`NoRelationshipQuery`]'s
///   always-`false` default therefore makes every `Private` post
///   non-author-invisible until a real `RelationshipQuery` is wired in
///   (design.md: "social-graph 未配線でも安全側の結果...で単独動作する").
/// - `Direct`: author-only — see this module's doc comment ("`direct`
///   visibility: author-only") for why, given this exact signature.
pub fn is_visible(status: &Status, viewer: Option<Id>, rel: &ViewerRelation) -> bool {
    if viewer == Some(status.actor_id) {
        return true;
    }
    match status.visibility {
        Visibility::Public => true,
        Visibility::Unlisted => viewer.is_some(),
        Visibility::Private => viewer.is_some() && rel.is_follower,
        Visibility::Direct => false,
    }
}
