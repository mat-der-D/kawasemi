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
//!
//! ## [`BlockPolicyRegistry`] (task 5.2, `_Boundary: SocialGraphModule_`,
//! social-graph's own bootstrap-wiring task)
//! [`BlockPolicy::is_blocked`] is declared `-> impl Future<..> + Send`
//! (widened from a literal `async fn` this same task) rather than a plain
//! `async fn`, so [`BlockPolicyRegistry`] (this module's own runtime-
//! replaceable registry, mirroring
//! `crate::statuses::notification_sink::NotificationSinkRegistry`'s/
//! `crate::accounts::ports::AccountPortsRegistry`'s identical "one
//! replaceable slot, boxed future" idiom) can hold `Arc<dyn DynBlockPolicy>`
//! internally. This mirrors `crate::social_graph::activity_builder::
//! LocalActorLookup`/`RemoteActorLookup`'s own identical widening (task
//! 4.1's Implementation Note) for the identical `async fn`-in-trait
//! auto-trait-leakage reason — every existing/future `async fn`-bodied
//! `impl BlockPolicy` (this module's own [`NoopBlockPolicy`],
//! `crate::social_graph::providers::BlockPolicyImpl`) remains
//! source-compatible unchanged: an `async fn` impl body satisfies a trait
//! method declared as `-> impl Future<..> + Send` without modification.
//! `src/federation/module.rs`'s `build_federation_module` mounts
//! `InboxService`'s `B` parameter with [`BlockPolicyRegistry`] (in place of
//! this spec's own hardcoded [`NoopBlockPolicy`]) precisely so a downstream
//! spec's real implementation can be registered *after* `FederationModule`
//! is already constructed — see that module's own doc comment, "Downstream
//! registration surface", and [`BlockPolicyRegistry::set_policy`]'s own doc
//! comment.

#[cfg(test)]
mod tests;

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

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
/// Declared as `-> impl Future<..> + Send` (task 5.2's widening — see this
/// module's doc comment, "`BlockPolicyRegistry`") rather than a plain
/// `async fn`: every existing `async fn`-bodied implementation remains
/// source-compatible unchanged, but a caller generic over `T: BlockPolicy`
/// (here, [`BlockPolicyRegistry`]'s own blanket `DynBlockPolicy` shim) can
/// now box the returned future as `Send` without assuming more than this
/// trait's own declaration guarantees (Rust's `async fn`-in-trait auto-trait
/// leakage rule).
pub trait BlockPolicy: Send + Sync {
    /// Judges whether `actor_uri` (the verified signer) is blocked from
    /// `local_recipient`'s perspective (Requirement 12.1, 12.2). For
    /// [`LocalRecipientContext::SharedInbox`] this is a contractually always-
    /// `false` query for any conforming implementation — see this module's
    /// doc comment ("This spec's own default never blocks") for why bulk
    /// rejection must never happen at this point in the pipeline.
    fn is_blocked(
        &self,
        actor_uri: &str,
        local_recipient: LocalRecipientContext,
    ) -> impl Future<Output = Result<bool, AppError>> + Send;
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

/// Dyn-safe shim used only by [`BlockPolicyRegistry`] — boxes
/// [`BlockPolicy::is_blocked`]'s future so a runtime-replaceable registry
/// slot can hold `Arc<dyn DynBlockPolicy>` (see this module's doc comment,
/// "`BlockPolicyRegistry`"). Blanket-implemented for every [`BlockPolicy`]
/// implementation — no real implementation (here or in a downstream spec)
/// needs to know this shim exists.
trait DynBlockPolicy: Send + Sync {
    fn is_blocked_boxed<'a>(
        &'a self,
        actor_uri: &'a str,
        local_recipient: LocalRecipientContext,
    ) -> Pin<Box<dyn Future<Output = Result<bool, AppError>> + Send + 'a>>;
}

impl<T> DynBlockPolicy for T
where
    T: BlockPolicy + Send + Sync,
{
    fn is_blocked_boxed<'a>(
        &'a self,
        actor_uri: &'a str,
        local_recipient: LocalRecipientContext,
    ) -> Pin<Box<dyn Future<Output = Result<bool, AppError>> + Send + 'a>> {
        Box::pin(self.is_blocked(actor_uri, local_recipient))
    }
}

/// The runtime-replaceable [`BlockPolicy`] slot `src/federation/module.rs`'s
/// `build_federation_module` mounts `InboxService`'s `B` parameter with, in
/// place of a single hardcoded concrete type (see this module's doc
/// comment, "`BlockPolicyRegistry`"). Defaults every fresh instance to
/// [`NoopBlockPolicy`] (this spec's own contractual default, Requirement
/// 12.3) until a downstream spec calls [`Self::set_policy`] — mirrors
/// `crate::statuses::notification_sink::NotificationSinkRegistry`'s/
/// `crate::accounts::ports::AccountPortsRegistry`'s identical "one
/// replaceable slot, `&self`-callable `set_*`, cheap `Clone`" registry
/// idiom, applied here to federation-core's own `BlockPolicy` delegation
/// boundary.
#[derive(Clone)]
pub struct BlockPolicyRegistry {
    policy: Arc<RwLock<Arc<dyn DynBlockPolicy>>>,
}

impl Default for BlockPolicyRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockPolicyRegistry {
    /// Builds a registry defaulting to [`NoopBlockPolicy`] — no downstream
    /// implementation is reachable from a freshly built registry until
    /// [`Self::set_policy`] is called.
    pub fn new() -> Self {
        Self {
            policy: Arc::new(RwLock::new(
                Arc::new(NoopBlockPolicy) as Arc<dyn DynBlockPolicy>
            )),
        }
    }

    /// Replaces the registered [`BlockPolicy`] implementation (a downstream
    /// spec's own registration entry point — social-graph's task 5.2).
    /// `&self`, not `&mut self`: `AppState`/`FederationModule` are
    /// immutable-after-construction, yet a downstream spec's registration
    /// must be able to happen after this registry is already live inside a
    /// constructed `FederationModule` (mirrors
    /// `AccountPortsRegistry::set_relationship_provider`'s identical
    /// rationale).
    pub fn set_policy<P>(&self, policy: P)
    where
        P: BlockPolicy + Send + Sync + 'static,
    {
        *self
            .policy
            .write()
            .expect("BlockPolicyRegistry lock must not be poisoned") = Arc::new(policy);
    }
}

impl BlockPolicy for BlockPolicyRegistry {
    /// Delegates to the currently registered implementation (the built-in
    /// [`NoopBlockPolicy`] until a downstream spec replaces it via
    /// [`Self::set_policy`]) — issues a fresh delegated call on every
    /// invocation, never a cached verdict (mirrors
    /// `crate::social_graph::providers::BlockPolicyImpl`'s own "no caching"
    /// contract, Requirement 6.4).
    async fn is_blocked(
        &self,
        actor_uri: &str,
        local_recipient: LocalRecipientContext,
    ) -> Result<bool, AppError> {
        let policy = self
            .policy
            .read()
            .expect("BlockPolicyRegistry lock must not be poisoned")
            .clone();
        policy.is_blocked_boxed(actor_uri, local_recipient).await
    }
}
