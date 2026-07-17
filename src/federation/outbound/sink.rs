//! `DeliverySink` (design.md `#### DeliverySink` -> Service Interface;
//! Requirements 10.3, 10.4, 11.1; task 4.2, `Boundary: DeliverySink`): the
//! single trait `DeliveryService` (`delivery.rs`) branches to once it has
//! already produced one canonical, validated Activity and resolved
//! recipients to physical [`super::target::DeliveryTarget`]s — never before
//! that point. This module owns exactly the trait and its two concrete
//! implementations:
//! - [`LocalDeliverySink`] -> [`InboxService::process_local`] (in-process,
//!   no queue — Requirement 10.3).
//! - [`HttpDeliverySink`] -> [`DeliveryQueue::enqueue`] (Requirement 10.4,
//!   11.1).
//!
//! ## `CanonicalActivity`: the structural proof of Requirement 10.5
//! design.md's `DeliverySink` Service Interface names a `CanonicalActivity`
//! parameter type but never defines it (the same situation task 3.4's
//! `Recipient`/task 3.5's `PageCursor` were in — this module is the natural
//! owner, being the sole consumer of the type across both of its
//! implementations). [`CanonicalActivity`] wraps the
//! [`crate::federation::jsonld::ParsedActivity`] that `DeliveryService::deliver`
//! (`delivery.rs`) produces exactly once per call, before the per-target
//! branch loop — see that module's doc comment for the "one canonical
//! Activity" construction. Wrapping (rather than passing a bare
//! `ParsedActivity`) documents, at the type level, that this specific value
//! is the *already-generated-and-validated* common-part output the
//! `DeliverySink` contract requires, not just any parsed Activity a caller
//! might construct ad hoc.
//!
//! Because `DeliveryService::deliver` builds exactly one `CanonicalActivity`
//! and passes `&canonical` by shared reference to every per-target
//! `dispatch` call in its loop (never re-serializing/re-parsing per target,
//! never cloning the value into a second, independently-constructed copy),
//! whichever mix of local and remote targets a single `deliver()` call
//! resolves to are structurally guaranteed to observe the exact same value
//! — this is the literal mechanism enforcing Requirement 10.5 ("同一の正規
//! Activity を扱う"), not merely a coincidental equality two independent
//! code paths happen to produce.
//!
//! `CanonicalActivity` exposes both representations each sink needs without
//! either one re-deriving/re-validating it: [`CanonicalActivity::parsed`]
//! (an owned-on-clone [`ParsedActivity`] for [`InboxService::process_local`],
//! which takes it by value) and [`CanonicalActivity::as_value`] (a
//! `&serde_json::Value` for [`super::queue::NewDeliveryJob::activity`]).
//!
//! ## `dispatch`'s `target: DeliveryTarget` parameter: the full enum, not a
//! narrower per-variant type
//! design.md's literal `DeliverySink::dispatch` signature takes the whole
//! [`super::target::DeliveryTarget`] enum, not e.g. a bare `Handle` for
//! [`LocalDeliverySink`] or a bare inbox `String` for [`HttpDeliverySink`].
//! `DeliveryService`'s per-target loop already knows which sink to call
//! before calling it (it matches on the target's variant to choose between
//! `local_sink`/`http_sink` — see `delivery.rs`), so in correct operation
//! each sink only ever receives the variant it was built for. Both
//! implementations below still defensively check the variant they receive
//! and return a `Server` (5xx) [`AppError`] on a mismatch, rather than
//! panicking or silently misinterpreting the other variant's data (e.g.
//! `LocalDeliverySink` reading a `Remote { inbox }` as if it were a stale
//! `Local { handle }`) — this can only happen from a bug in `DeliveryService`'s
//! own branch, never from caller input, hence `Server`/5xx rather than
//! `Client`/4xx.
//!
//! ## `sender: &Handle`: constructing identity, not looking anything up in
//! `DeliveryService` itself
//! Neither sink receives a pre-resolved sender identity — each derives what
//! it needs from `sender: &Handle` on every call:
//! - [`LocalDeliverySink`] builds a synthetic [`VerifiedSigner`] via
//!   [`ActorUrls::actor_url`]/[`ActorUrls::key_id`] — this is the *sending*
//!   local actor's own identity, not a remote cryptographic claim, since an
//!   in-process hand-off to [`InboxService::process_local`] never goes
//!   through HTTP or signature verification at all (mirrors
//!   `InboxService::process_local`'s own doc comment on how its
//!   `LocalRecipientContext::Actor` is built the same way for the
//!   *recipient* side).
//! - [`HttpDeliverySink`] resolves `sender` to its local-actor [`Id`] via
//!   [`LocalActorLookup`] (the same narrow port task 3.4's
//!   `RecipientTargetResolver` depends on) to populate
//!   [`NewDeliveryJob::sender_actor_id`]. This is a second, independent use
//!   of `LocalActorLookup` from `RecipientTargetResolver`'s own (which
//!   resolves *recipients*, not the *sender*) — `DeliveryService` does not
//!   couple the two together, so a caller is free to wire the same
//!   `ActorDirectory` value into both in production without either
//!   component depending on the other's generic parameter.

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::http::StatusCode;

use super::queue::{DeliveryQueue, NewDeliveryJob};
use super::target::{DeliveryTarget, LocalActorLookup};
use crate::actor::Handle;
use crate::error::AppError;
use crate::federation::inbound::{BlockPolicy, InboxService, ReceivedActivityStore};
use crate::federation::jsonld::ParsedActivity;
use crate::federation::signatures::{SignatureVerifier, VerifiedSigner};
use crate::federation::urls::ActorUrls;
use crate::runtime::{Clock, IdGenerator};

/// The canonical, validated Activity `DeliveryService::deliver` produces
/// exactly once per call, before branching to any sink (design.md's exact
/// `DeliverySink` interface type; Requirements 10.1, 10.2, 10.5). See this
/// module's doc comment ("`CanonicalActivity`: the structural proof of
/// Requirement 10.5") for why a newtype rather than a bare
/// [`ParsedActivity`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalActivity(ParsedActivity);

impl CanonicalActivity {
    /// Wraps an already-generated-and-validated [`ParsedActivity`]
    /// (`delivery.rs`'s own one-time serialize+validate step) as the shared
    /// value every per-target `dispatch` call in a single `deliver()` call
    /// observes.
    pub fn from_parsed(parsed: ParsedActivity) -> Self {
        Self(parsed)
    }

    /// The parsed representation [`LocalDeliverySink`] hands to
    /// [`InboxService::process_local`] (which takes a [`ParsedActivity`] by
    /// value, hence [`Clone`] at the call site rather than here).
    pub fn parsed(&self) -> &ParsedActivity {
        &self.0
    }

    /// The raw JSON representation (already carrying the stamped
    /// `@context`) [`HttpDeliverySink`] persists onto
    /// [`NewDeliveryJob::activity`].
    pub fn as_value(&self) -> &serde_json::Value {
        &self.0.raw
    }
}

/// The physical-delivery branch point (design.md's exact `DeliverySink`
/// Service Interface; Requirements 10.3, 10.4, 11.1). See this module's doc
/// comment for the full contract, including why `target` carries the whole
/// [`DeliveryTarget`] enum rather than a narrower per-implementation type.
///
/// `#[allow(async_fn_in_trait)]`: mirrors this crate's established rationale
/// for other design.md-pinned literal-`async fn` traits in this same spec
/// (e.g. `queue.rs`'s `DeliveryQueue`, `inbound/dedup.rs`'s
/// `ReceivedActivityStore`) — `DeliveryService` (`delivery.rs`) holds its two
/// sinks as concrete generic type parameters, never `Arc<dyn DeliverySink>`,
/// so no `dyn`-compatibility concern arises within this task's own boundary.
#[allow(async_fn_in_trait)]
pub trait DeliverySink: Send + Sync {
    /// Physically delivers `activity` to `target`, as the local actor
    /// `sender`. Returns `Ok(())` once the physical delivery mechanism
    /// (in-process hand-off, or queue insertion) has succeeded — not once
    /// the remote recipient has actually received anything, for
    /// [`HttpDeliverySink`] (Requirement 11.1: enqueuing must not block the
    /// caller on completion of the eventual HTTP send).
    async fn dispatch(
        &self,
        target: DeliveryTarget,
        activity: &CanonicalActivity,
        sender: &Handle,
    ) -> Result<(), AppError>;
}

/// [`DeliverySink`] implementation that hands a canonical Activity to
/// [`InboxService::process_local`] in-process — no queue, no HTTP
/// (Requirement 10.3). Holds an `Arc<InboxService<..>>` (rather than an
/// owned value) because production wiring (task 5.4) shares the exact same
/// `InboxService` instance between this sink and the inbox/shared-inbox HTTP
/// endpoint handlers' own `process_inbound` calls (task 5.3) — both must
/// converge on the identical `ReceivedActivityStore`/dispatcher state
/// (Requirement 10.5), which only holds if they share one instance, not two
/// separately-constructed copies.
pub struct LocalDeliverySink<V, B, D>
where
    V: SignatureVerifier,
    B: BlockPolicy,
    D: ReceivedActivityStore,
{
    inbox: Arc<InboxService<V, B, D>>,
    actor_urls: ActorUrls,
}

impl<V, B, D> LocalDeliverySink<V, B, D>
where
    V: SignatureVerifier,
    B: BlockPolicy,
    D: ReceivedActivityStore,
{
    /// Builds a sink delivering into `inbox`, using `actor_urls` to build the
    /// synthetic sender [`VerifiedSigner`] (see this module's doc comment,
    /// "`sender: &Handle`").
    pub fn new(inbox: Arc<InboxService<V, B, D>>, actor_urls: ActorUrls) -> Self {
        Self { inbox, actor_urls }
    }
}

impl<V, B, D> DeliverySink for LocalDeliverySink<V, B, D>
where
    V: SignatureVerifier,
    B: BlockPolicy,
    D: ReceivedActivityStore,
{
    /// Requires `target` to be [`DeliveryTarget::Local`] (see this module's
    /// doc comment on the defensive variant check). Builds a synthetic
    /// [`VerifiedSigner`] for `sender` (this instance's own identity, never
    /// a remote cryptographic claim — `process_local` never verifies a
    /// signature) and hands `activity`'s [`ParsedActivity`] to
    /// [`InboxService::process_local`], discarding the resulting
    /// [`crate::federation::inbound::InboxOutcome`] (`Accepted` vs.
    /// `Duplicate` is an `InboxService`-internal distinction this sink's own
    /// contract does not need to surface — a `DeliverySink::dispatch` only
    /// promises "delivered", the same way [`HttpDeliverySink`] only promises
    /// "enqueued").
    async fn dispatch(
        &self,
        target: DeliveryTarget,
        activity: &CanonicalActivity,
        sender: &Handle,
    ) -> Result<(), AppError> {
        let DeliveryTarget::Local { handle } = target else {
            return Err(AppError::server(
                StatusCode::INTERNAL_SERVER_ERROR,
                "LocalDeliverySink received a non-local delivery target",
            ));
        };

        let signer = VerifiedSigner {
            key_id: self.actor_urls.key_id(sender),
            actor_uri: self.actor_urls.actor_url(sender),
        };

        self.inbox
            .process_local(activity.parsed().clone(), signer, handle)
            .await
            .map(|_outcome| ())
    }
}

/// [`DeliverySink`] implementation that persists a delivery job onto
/// [`DeliveryQueue`] (Requirement 10.4, 11.1) — the caller's `deliver()` call
/// returns as soon as the job is durably queued, never waiting for the
/// eventual signed HTTP send (task 4.3's `DeliveryWorker`, out of this
/// task's boundary).
pub struct HttpDeliverySink<Q, D>
where
    Q: DeliveryQueue,
    D: LocalActorLookup,
{
    queue: Q,
    sender_lookup: D,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
}

impl<Q, D> HttpDeliverySink<Q, D>
where
    Q: DeliveryQueue,
    D: LocalActorLookup,
{
    /// Builds a sink enqueuing onto `queue`, resolving the sender's local
    /// [`crate::domain::Id`] via `sender_lookup` (see this module's doc
    /// comment, "`sender: &Handle`"), and minting each job's `id`/
    /// `next_attempt_at` from `ids`/`clock` — never a raw
    /// wall-clock/random call, per this spec's determinism convention.
    pub fn new(
        queue: Q,
        sender_lookup: D,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
    ) -> Self {
        Self {
            queue,
            sender_lookup,
            clock,
            ids,
        }
    }
}

impl<Q, D> DeliverySink for HttpDeliverySink<Q, D>
where
    Q: DeliveryQueue,
    D: LocalActorLookup,
{
    /// Requires `target` to be [`DeliveryTarget::Remote`] (see this module's
    /// doc comment on the defensive variant check). Resolves `sender` to its
    /// local-actor [`Id`](crate::domain::Id) (failing with a `404`-shaped
    /// [`AppError`] if `sender` no longer resolves to an existing local
    /// actor — mirroring [`super::target::RecipientTargetResolver`]'s own
    /// "fail loudly rather than silently drop" convention for the same
    /// situation), then persists a [`NewDeliveryJob`] built from `activity`'s
    /// already-canonical JSON value (no re-serialization) and this sink's
    /// injected `Clock`/`IdGenerator`.
    async fn dispatch(
        &self,
        target: DeliveryTarget,
        activity: &CanonicalActivity,
        sender: &Handle,
    ) -> Result<(), AppError> {
        let DeliveryTarget::Remote { inbox } = target else {
            return Err(AppError::server(
                StatusCode::INTERNAL_SERVER_ERROR,
                "HttpDeliverySink received a non-remote delivery target",
            ));
        };

        let resolved_sender = self
            .sender_lookup
            .resolve_actor_by_handle(sender)
            .await?
            .ok_or_else(|| {
                AppError::client(
                    StatusCode::NOT_FOUND,
                    format!(
                        "sender handle {:?} does not resolve to an existing local actor",
                        sender.as_str()
                    ),
                )
            })?;

        let job = NewDeliveryJob {
            id: self.ids.next_id(),
            sender_actor_id: resolved_sender.id,
            target_inbox: inbox,
            activity: activity.as_value().clone(),
            next_attempt_at: self.clock.now(),
        };

        self.queue.enqueue(job).await
    }
}
