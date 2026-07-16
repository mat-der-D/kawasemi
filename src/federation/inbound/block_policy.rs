//! `BlockPolicy` (design.md `#### BlockPolicy（委譲境界、destination-
//! aware）` -> Service Interface; Requirements 12.1, 12.2, 12.3; task 3.2,
//! `Boundary: BlockPolicy`): the delegation boundary for "is this signer
//! blocked" judgments, kept destination-aware because kawasemi is a
//! single-owner, multi-local-actor server where blocking is scoped per
//! destination local actor rather than global (product.md).
//!
//! ## Why destination-aware
//! [`BlockPolicy::is_blocked`] takes a [`LocalRecipientContext`] alongside
//! the signer's actor URI:
//! - [`LocalRecipientContext::Actor`] — a per-actor inbox delivery, where
//!   the destination local actor's URI is already known from the URL, so an
//!   implementation can judge "is this signer blocked from this actor's
//!   perspective" directly.
//! - [`LocalRecipientContext::SharedInbox`] — a shared-inbox delivery, which
//!   may fan out to several local actors (e.g. followers) at once; at this
//!   point in the pipeline no single destination local actor is yet
//!   resolved.
//!
//! ## This spec's own default never blocks — even for `SharedInbox`
//! This spec owns no block-list storage at all (Requirement 12.3): it only
//! defines the [`BlockPolicy`] trait and ships [`NoopBlockPolicy`], a default
//! that always answers `false` for both variants. This is a deliberate
//! contract, not a stand-in to fill in later: querying with
//! `LocalRecipientContext::SharedInbox` must never be used to bulk-reject an
//! entire shared-inbox delivery at the HTTP layer, because a single shared
//! -inbox Activity can be addressed to several local actors and only some of
//! them may have blocked the signer — bulk-rejecting at this point would
//! also drop the Activity for local actors who never blocked the signer.
//! The real per-actor decision is instead made downstream, once a downstream
//! `InboundActivityHandler` implementation (e.g. social-graph's) has
//! resolved the actual destination local actor(s) and can re-query with
//! `LocalRecipientContext::Actor` for each one individually. A real
//! block-graph-backed `BlockPolicy` is out of this spec's scope entirely —
//! social-graph (a later spec) supplies it.

#[cfg(test)]
mod tests;

use crate::error::AppError;

/// The destination context a block judgment is made against (design.md's
/// exact `LocalRecipientContext` interface). See this module's doc comment
/// ("Why destination-aware") for what each variant means and why both
/// exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalRecipientContext {
    /// Per-actor inbox delivery: the destination local actor's URI is known.
    Actor { actor_uri: String },
    /// Shared-inbox delivery: the destination local actor(s) cannot yet be
    /// uniquely resolved (pre-fan-out).
    SharedInbox,
}

/// The block-judgment delegation boundary (design.md's exact `BlockPolicy`
/// Service Interface; Requirement 12.1: 署名者がブロック対象かをこの境界へ
/// 問い合わせる). This spec owns no block-list storage; see
/// [`NoopBlockPolicy`] for this spec's own default answer.
///
/// `#[allow(async_fn_in_trait)]`: mirrors this crate's established rationale
/// for other design.md-pinned literal-`async fn` traits (e.g.
/// `ReceivedActivityStore`, `PublicKeyResolver`) — boxing/`Send`-pinning
/// concerns belong to whichever later task needs `Arc<dyn BlockPolicy>`
/// across a `tokio::spawn` boundary (e.g. task 4.1's `InboxService`), not
/// this task's boundary.
#[allow(async_fn_in_trait)]
pub trait BlockPolicy: Send + Sync {
    /// Judges whether `actor_uri` (the verified signer) is blocked from
    /// `local_recipient`'s perspective (Requirement 12.1, 12.2). For
    /// [`LocalRecipientContext::SharedInbox`] this is a contractually always-
    /// `false` query for any conforming implementation — see this module's
    /// doc comment ("This spec's own default never blocks") for why bulk
    /// rejection must never happen at this point in the pipeline.
    async fn is_blocked(
        &self,
        actor_uri: &str,
        local_recipient: LocalRecipientContext,
    ) -> Result<bool, AppError>;
}

/// This spec's own default [`BlockPolicy`] (Requirement 12.3: "既定実装は
/// 常に「ブロックなし」"). Always answers `Ok(false)` regardless of signer or
/// destination context — social-graph (a later spec, out of this spec's
/// scope) supplies the real block-graph-backed implementation.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopBlockPolicy;

impl BlockPolicy for NoopBlockPolicy {
    async fn is_blocked(
        &self,
        _actor_uri: &str,
        _local_recipient: LocalRecipientContext,
    ) -> Result<bool, AppError> {
        Ok(false)
    }
}
