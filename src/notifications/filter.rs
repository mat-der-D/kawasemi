//! `NotificationFilter` (design.md "Generation / 生成層" ->
//! `NotificationFilter`, Requirements 7.1, 7.2, 7.3, 7.4; task 2.3,
//! `Boundary: NotificationFilter`).
//!
//! Scope: this module owns exactly [`NotificationFilter::should_suppress`]
//! — a thin decision function that answers "should this notification
//! generation be suppressed?" by consuming social-graph's already-correct,
//! already-expiry-aware [`crate::social_graph::FilterQuery::blocked_set`].
//! It holds no block/mute state of its own, performs no expiry
//! computation of its own, and issues no SQL of its own (Requirement 7.4:
//! "関係状態・期限判定は再実装しない") — every fact this module needs is
//! read straight off the [`RelationshipSets`] `FilterQuery` already
//! returns. No `NotificationGenerator` (task 2.4, the eventual caller that
//! wires this filter into the single generation point), no persistence, no
//! HTTP surface, and no `NotificationModule` composition live here.
//!
//! ## One `FilterQuery::blocked_set` call, from the recipient's viewpoint
//! Requirement 7.1's two block directions ("受信者が通知元アクターにブロ
//! ックされている" — origin has blocked recipient — and its mirror,
//! recipient has blocked origin) and Requirement 7.2's notification-mute
//! condition are all answerable from a *single* `blocked_set(recipient)`
//! call, because [`RelationshipSets`]'s fields are already defined from
//! `viewer`'s (here, `recipient`'s) own perspective (see that struct's own
//! field doc comments in `src/social_graph/providers.rs`):
//! - `origin` appearing in `sets.blocked` means recipient has blocked
//!   origin (the "受信者が...ブロックしている" half of 7.1).
//! - `origin` appearing in `sets.blocked_by` means origin has blocked
//!   recipient ("Accounts that have blocked `viewer`" — `viewer` =
//!   recipient here; the "受信者が...ブロックされている" half of 7.1).
//! - `origin` appearing in `sets.muted_notifications` means recipient has
//!   notification-muted origin (Requirement 7.2, `muting_notifications`).
//!
//! `sets.muted` (plain, non-notification mute) is deliberately never
//! consulted — Requirement 7.2's own wording restricts suppression to the
//! notification-mute flag specifically, not plain mute alone.
//!
//! ## Expiry (Requirement 7.3) is entirely `FilterQuery`'s concern
//! `FilterQuery::blocked_set`'s own doc comment already guarantees
//! `muted`/`muted_notifications` exclude expired mutes before this module
//! ever sees them (driven by its injected `RuntimeContext`/`Clock`). This
//! module performs no `now`/`expires_at` comparison of its own — it only
//! reads whatever `blocked_set` already returned. See this module's own
//! unit tests ("expired mute is not suppressed") for the observable
//! consequence: an expired mute simply never appears in
//! `muted_notifications`, so it never contributes to `should_suppress`'s
//! `true` result.
//!
//! ## `&self` plain `async fn`, not a boxed-future trait method
//! Unlike `src/notifications/ports.rs`'s two delegation traits (which need
//! `Pin<Box<dyn Future<...>>>` because they must be `dyn`-compatible for a
//! runtime-swappable registry slot), [`NotificationFilter`] is a concrete
//! struct, not a trait — object-safety's `async fn`-in-trait restriction
//! never applies here. This matches [`crate::social_graph::FilterQuery`]
//! itself (also a concrete struct) declaring its own
//! `pub async fn blocked_set` with plain `async fn` syntax, no boxing.
//! design.md's Service Interface sketch (`pub async fn should_suppress(&self,
//! recipient: &AccountRef, origin: &AccountRef) -> Result<bool, AppError>;`)
//! is therefore implemented verbatim, with no deviation.

#[cfg(test)]
mod tests;

use crate::domain::AccountRef;
use crate::error::AppError;
use crate::social_graph::FilterQuery;

/// Decides whether a notification generation event should be suppressed
/// because of a block, being-blocked, or notification-mute relationship
/// between `recipient` and `origin` (Requirements 7.1, 7.2, 7.3, 7.4). See
/// this module's own doc comment for the exact suppression conditions and
/// why a single `FilterQuery::blocked_set(recipient)` call answers all of
/// them.
pub struct NotificationFilter {
    filter_query: FilterQuery,
}

impl NotificationFilter {
    /// Builds a `NotificationFilter` bound to `filter_query` — this
    /// module's sole dependency (Requirement 7.4: no relationship state of
    /// its own).
    pub fn new(filter_query: FilterQuery) -> Self {
        Self { filter_query }
    }

    /// `true` iff `origin` is blocked by `recipient`, has blocked
    /// `recipient`, or is currently notification-muted (`muting_notifications`,
    /// already expiry-aware) by `recipient` (Requirements 7.1, 7.2, 7.3).
    /// Every fact behind this decision comes from a single
    /// `FilterQuery::blocked_set(recipient)` call — see this module's own
    /// doc comment ("One `FilterQuery::blocked_set` call, from the
    /// recipient's viewpoint") for why that single call suffices.
    pub async fn should_suppress(
        &self,
        recipient: &AccountRef,
        origin: &AccountRef,
    ) -> Result<bool, AppError> {
        let sets = self.filter_query.blocked_set(recipient).await?;
        let suppress = sets.blocked.contains(origin)
            || sets.blocked_by.contains(origin)
            || sets.muted_notifications.contains(origin);
        Ok(suppress)
    }
}
