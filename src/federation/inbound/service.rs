//! `InboxService` (design.md `#### InboxService` -> Service Interface;
//! Requirements 6.4, 7.1, 7.2, 7.3, 7.4, 9.3, 12.1, 12.2; task 4.1,
//! `Boundary: InboxService`): the receive-pipeline orchestrator that
//! composes [`super::block_policy::BlockPolicy`], [`super::dedup::ReceivedActivityStore`],
//! [`super::dispatcher::InboundActivityDispatcher`], [`crate::federation::jsonld::parse_activity`]
//! (`JsonLdCodec`), and [`crate::federation::signatures::SignatureVerifier`]
//! into the full receive pipeline: signature verification -> required-
//! property validation -> block judgment -> deduplication -> dispatch. Each
//! rejection happens strictly before the next stage runs (Requirements 7.2,
//! 12.2, 9.3's "業務処理へ受け渡さない"), and no stage after a rejection ever
//! observes the rejected Activity.
//!
//! ## Two public entry points, one private pipeline (the core testable claim)
//! [`InboxService::process_inbound`] (remote HTTP receipt, signature
//! verification included) and [`InboxService::process_local`] (in-process
//! local delivery, signature verification already implied by the fact that
//! the Activity originated from this very instance) both terminate in the
//! exact same private [`InboxService::process_verified`] method — not two
//! independently written implementations that merely happen to behave
//! alike. This is the structural guarantee Requirement 10.3/10.5 demands
//! ("ローカル配送経路と HTTP 連合配送経路が同一の Activity に対して同一の
//! 業務処理結果を生むことを検証可能にする"): the block-check -> dedup ->
//! dispatch sequence is written exactly once, in `process_verified`, and
//! both entry points can only reach it after doing their own entry-specific
//! preamble (signature verification + JSON-LD parsing for `process_inbound`;
//! nothing but building the destination context for `process_local`, since
//! the caller already supplies a parsed `ParsedActivity` and a
//! `VerifiedSigner`). Any future change to the block/dedup/dispatch sequence
//! can only be made in one place, and both callers see it identically —
//! this is enforced by the compiler's call graph, not by convention.
//!
//! ## `InboxOutcome` vs. `AppError`: rejections use this crate's established
//! status-carrying error idiom, not a bespoke rejection enum
//! Every other rejecting component in this spec so far —
//! [`crate::federation::signatures::SignatureVerifier::verify_request`]
//! (401), [`crate::federation::jsonld::parse_activity`] (400/422),
//! [`super::block_policy::BlockPolicy::is_blocked`] (a plain `bool`, no
//! status of its own) — reports failure as `Result<_, AppError>`, where
//! [`AppError`] already carries the exact HTTP status the caller should
//! respond with (`AppError.status`), per `error.rs`'s own documented
//! design: "`AppError`... every downstream handler uses to report
//! failures". This module follows that same idiom rather than inventing a
//! second, parallel way to carry a status code:
//! - Signature-verification failure surfaces [`InboxService::process_inbound`]'s
//!   own `?`-propagated `Err` from `verify_request` untouched — already a
//!   `401 Unauthorized` [`AppError`] (Requirement 7.2).
//! - A missing request body, or a body [`crate::federation::jsonld::parse_activity`]
//!   rejects for a missing/malformed required property, likewise surfaces as
//!   `Err` with a `422 Unprocessable Entity` [`AppError`] (Requirement 9.3;
//!   a missing body is bucketed the same way — there is no Activity to
//!   validate required properties on at all).
//! - A blocked signer is rejected by this module itself (`BlockPolicy` only
//!   returns a `bool`, not a status) by constructing an `Err` with a
//!   `403 Forbidden` [`AppError`] (Requirement 12.2).
//!
//! [`InboxOutcome`] therefore only needs to distinguish the pipeline's two
//! *non-rejecting* terminal states — [`InboxOutcome::Duplicate`] (Requirement
//! 7.4: recorded before, not re-dispatched) and [`InboxOutcome::Accepted`]
//! (newly seen, dispatched) — both of which an HTTP inbox endpoint (task
//! 5.3) maps to the same `202 Accepted` response, but which remain
//! observably distinct here so a test (or future observability hook) can
//! assert whether a given call actually re-ran dispatch or not, without
//! inspecting the dispatcher's own side effects.
//!
//! ## `process_inbound`'s signature: `destination` is an explicit parameter,
//! not derived from `req.url` internally
//! design.md's literal `InboxService` Service Interface prints
//! `process_inbound(&self, req: IncomingRequest) -> Result<InboxOutcome, AppError>`
//! (single parameter), with prose in the same section ("リモート受信は URL
//! （アクター個別 inbox / shared inbox）から宛先コンテキストを組み立てて") that
//! reads as if `InboxService` itself parses `req.url` to decide between
//! [`super::block_policy::LocalRecipientContext::Actor`] and `::SharedInbox`.
//! Doing that literally inside this module would require it to depend on
//! [`crate::federation::urls::ActorUrls`]'s URL *shape* convention (matching
//! `{actor_url}/inbox` against `req.url`, extracting the handle segment,
//! and distinguishing it from the single domain-wide `shared_inbox_url()`)
//! purely by string inspection — but:
//! - design.md's own Components table lists `InboxService`'s dependencies as
//!   exactly "Verifier, BlockPolicy, Dedup, Dispatcher, JsonLdCodec (P0)" —
//!   `ActorUrls` is conspicuously absent, unlike `ActivityPubDocumentBuilder`
//!   (task 3.6) or `RequestSigner` (task 2.2), which both list it.
//! - design.md's own Endpoints-handler section (`#### Endpoints`) assigns
//!   this exact judgment to the HTTP handler, not to `InboxService`: "アクタ
//!   ー個別 inbox は URL 上の宛先アクターを `LocalRecipientContext::Actor` と
//!   して渡し、shared inbox は `LocalRecipientContext::SharedInbox` を渡す". A
//!   handler that dispatches via axum's own per-route registration
//!   (`/users/{handle}/inbox` vs. the single `/inbox` shared route) already
//!   knows unambiguously which case applies *before* calling this service —
//!   it needs no string parsing of its own request's URL at all, whereas
//!   `InboxService` re-deriving the same fact from `req.url` would require
//!   duplicating `ActorUrls`'s URL-shape convention in a second place
//!   (`urls.rs` already owns "アクター URL の構築・公開を本 spec が所有").
//! - task 5.3's own task text (this spec's `tasks.md`) makes the same
//!   assignment explicit: "アクター個別 inbox は URL 上の宛先アクターをブロ
//!   ック判定の宛先コンテキストとして渡し、shared inbox は宛先未確定の宛先
//!   コンテキストを渡す" is task 5.3's (`_Boundary: inbox_`) own observable
//!   completion condition, not task 4.1's.
//!
//! Given task 5.3 (the inbox/shared-inbox endpoint handlers) does not exist
//! yet, and given the conflict above between `InboxService`'s own prose and
//! its Components table / the Endpoints section / task 5.3's own text, this
//! module resolves it by taking `destination: LocalRecipientContext` as an
//! explicit second parameter on `process_inbound`, to be supplied by
//! whichever endpoint handler task 5.3 builds (which will construct it from
//! its own already-matched route, exactly as design.md's Endpoints section
//! describes) — deviating from design.md's literal one-parameter signature.
//! This is flagged to the parent controller as an Implementation Notes
//! candidate (see this task's status report) rather than silently guessed
//! past. `process_inbound` still forwards whatever `destination` it is
//! given to `BlockPolicy` unchanged — it never itself chooses `Actor` over
//! `SharedInbox` or vice versa.
//!
//! ## `process_local` always uses `LocalRecipientContext::Actor`
//! Per design.md's own `InboxService` Invariants ("両者ともブロック判定は
//! `LocalRecipientContext::Actor` で行う（in-process 配送は常に宛先ローカル
//! アクターが確定済みのため `SharedInbox` は用いない）"), [`InboxService::process_local`]
//! always builds [`super::block_policy::LocalRecipientContext::Actor`] from
//! its `recipient: Handle` parameter — in-process delivery (design.md's
//! `DeliveryTarget::Local { handle }`) always has a single, already-resolved
//! destination local actor by construction, so `SharedInbox`'s "destination
//! not yet resolved" case can never apply here. The `actor_uri` half of that
//! context is built via [`crate::federation::urls::ActorUrls::actor_url`] —
//! the same canonical actor-URL construction task 5.3's endpoint handler is
//! expected to use when building the per-actor-inbox `Actor` context for
//! `process_inbound` (deriving it from the matched route's handle segment),
//! so a signer blocked from one path is blocked identically from the other
//! (this is why `InboxService` *does* depend on `ActorUrls` after all,
//! despite the Components table omission noted above — needed here, not for
//! `process_inbound`'s destination derivation).

#[cfg(test)]
mod tests;

use axum::http::StatusCode;

use super::block_policy::{BlockPolicy, LocalRecipientContext};
use super::dedup::ReceivedActivityStore;
use super::dispatcher::{InboundActivityDispatcher, InboundContext};
use crate::actor::Handle;
use crate::error::AppError;
use crate::federation::jsonld::{ParsedActivity, parse_activity};
use crate::federation::signatures::{IncomingRequest, SignatureVerifier, VerifiedSigner};
use crate::federation::urls::ActorUrls;

/// The receive pipeline's non-rejecting terminal outcomes (design.md
/// references this type as `InboxService`'s return value but does not
/// define it — see this module's doc comment, "`InboxOutcome` vs.
/// `AppError`", for why only these two variants exist and why every
/// rejection instead surfaces as `Err(AppError)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboxOutcome {
    /// A newly-seen Activity id: recorded by [`ReceivedActivityStore`] for
    /// the first time and dispatched to [`InboundActivityDispatcher`]
    /// (Requirement 7.3).
    Accepted,
    /// An already-seen Activity id: [`ReceivedActivityStore::record_if_new`]
    /// reported it as known, so dispatch was **not** re-run (Requirement
    /// 7.4). Still a successful receipt from the caller's perspective (an
    /// HTTP endpoint acks this the same as `Accepted`).
    Duplicate,
}

/// The receive-pipeline orchestrator (design.md's exact `InboxService`
/// component; Requirements 6.4, 7.1, 7.2, 7.3, 7.4, 9.3, 12.1, 12.2). See
/// this module's doc comment for the full pipeline, the `process_inbound`/
/// `process_local` convergence guarantee, and the `destination` parameter
/// deviation from design.md's literal signature.
///
/// Generic over `V: SignatureVerifier`, `B: BlockPolicy`, `D:
/// ReceivedActivityStore` (all three are `#[allow(async_fn_in_trait)]`
/// literal-`async fn` traits in this crate, per their own modules'
/// documented rationale — `verifier.rs`, `block_policy.rs`, `dedup.rs` —
/// none of which is `dyn`-compatible; `Arc<dyn SignatureVerifier>` etc.
/// would fail to compile with E0038 exactly as `Arc<dyn FederationHttpClient>`
/// did for task 2.1's `key_resolver.rs`). `InboundActivityDispatcher` is not
/// a type parameter: it is this crate's one concrete, already-`dyn`-safe-
/// internally registry struct (its own `Arc<dyn InboundActivityHandler>`
/// entries are boxed-future-based specifically so the registry itself can
/// stay a plain, non-generic struct — see `dispatcher.rs`'s own doc
/// comment), so it is held directly, not through a further type parameter.
pub struct InboxService<V, B, D>
where
    V: SignatureVerifier,
    B: BlockPolicy,
    D: ReceivedActivityStore,
{
    verifier: V,
    block_policy: B,
    dedup: D,
    dispatcher: InboundActivityDispatcher,
    actor_urls: ActorUrls,
}

impl<V, B, D> InboxService<V, B, D>
where
    V: SignatureVerifier,
    B: BlockPolicy,
    D: ReceivedActivityStore,
{
    /// Builds an `InboxService` composing `verifier` (signature
    /// verification, Requirement 7.1), `block_policy` (block-judgment
    /// delegation, Requirements 12.1/12.2), `dedup` (idempotency ledger,
    /// Requirement 7.4), `dispatcher` (business-processing hand-off,
    /// Requirement 7.3), and `actor_urls` (canonical local-actor URL
    /// construction, used only by [`Self::process_local`] — see this
    /// module's doc comment, "`process_local` always uses
    /// `LocalRecipientContext::Actor`").
    pub fn new(
        verifier: V,
        block_policy: B,
        dedup: D,
        dispatcher: InboundActivityDispatcher,
        actor_urls: ActorUrls,
    ) -> Self {
        Self {
            verifier,
            block_policy,
            dedup,
            dispatcher,
            actor_urls,
        }
    }

    /// Remote receipt: the full pipeline including signature verification
    /// (Requirement 7.1). Verifies `req`'s HTTP Signature, parses and
    /// validates `req.body` as a JSON-LD Activity (Requirement 9.3), then
    /// converges on [`Self::process_verified`] with the caller-supplied
    /// `destination` (see this module's doc comment, "`process_inbound`'s
    /// signature", for why `destination` is an explicit parameter here
    /// rather than derived from `req.url` internally).
    ///
    /// Each stage's rejection happens strictly before the next stage runs
    /// (Requirements 7.2, 9.3): an unverifiable signature never reaches
    /// JSON-LD parsing, and a malformed/incomplete body never reaches block
    /// judgment, deduplication, or dispatch.
    pub async fn process_inbound(
        &self,
        req: IncomingRequest,
        destination: LocalRecipientContext,
    ) -> Result<InboxOutcome, AppError> {
        // 1. Signature verification (Requirement 7.1). Any failure —
        // missing/invalid/expired signature, unresolvable public key —
        // surfaces `verify_request`'s own 401 `AppError` untouched
        // (Requirement 7.2) and this pipeline goes no further.
        let signer = self.verifier.verify_request(&req).await?;

        // 2. Required-property validation (Requirement 9.3). A bodyless
        // inbox POST cannot carry an Activity to validate at all, so it is
        // rejected in the same "malformed" bucket `parse_activity` itself
        // uses (422) rather than treated as a separate case.
        let body = req.body.as_deref().ok_or_else(|| {
            AppError::client(
                StatusCode::UNPROCESSABLE_ENTITY,
                "inbox POST must carry a JSON-LD Activity body",
            )
        })?;
        let activity = parse_activity(body)?;

        // 3-5. Block judgment -> dedup -> dispatch, identical to
        // `process_local`'s own tail. See `process_verified`.
        self.process_verified(activity, signer, destination).await
    }

    /// Local in-process delivery: the same semantic path as
    /// `process_inbound` *minus* signature verification (design.md:
    /// "署名検証を除く同一意味論経路"), because `activity` and `signer` are
    /// already known-good by construction — `activity` was already parsed
    /// and validated by this same instance's own outbound pipeline
    /// (`DeliveryService`, task 4.2), and `signer` is the sending local
    /// actor's own identity, not a remote claim requiring cryptographic
    /// verification.
    ///
    /// `recipient` is the already-resolved destination local actor (design.md:
    /// "recipient は `DeliveryTarget::Local` から確定済みで渡される") — always
    /// converted to [`LocalRecipientContext::Actor`], never `SharedInbox` (see
    /// this module's doc comment for why in-process delivery can never hit
    /// the "destination not yet resolved" case).
    pub async fn process_local(
        &self,
        activity: ParsedActivity,
        signer: VerifiedSigner,
        recipient: Handle,
    ) -> Result<InboxOutcome, AppError> {
        let destination = LocalRecipientContext::Actor {
            actor_uri: self.actor_urls.actor_url(&recipient),
        };

        self.process_verified(activity, signer, destination).await
    }

    /// The single shared block-judgment -> dedup -> dispatch tail both
    /// [`Self::process_inbound`] (post signature-verification) and
    /// [`Self::process_local`] converge on (this module's doc comment, "Two
    /// public entry points, one private pipeline" — the structural
    /// enforcement of Requirements 10.3/10.5's semantic-symmetry
    /// invariant).
    ///
    /// Order matters and each rejection stops the pipeline immediately
    /// (Requirements 7.2, 12.2, 9.3's "業務処理へ受け渡さない"):
    /// 1. Block judgment (Requirement 12.1): `destination` is passed through
    ///    unchanged to [`BlockPolicy::is_blocked`] exactly as this method
    ///    received it — a blocked signer is rejected (403, Requirement 12.2)
    ///    before deduplication or dispatch ever observes the Activity.
    /// 2. Deduplication (Requirement 7.4): an already-known `activity.id` is
    ///    reported [`InboxOutcome::Duplicate`] *without* recording again and
    ///    *without* invoking the dispatcher a second time.
    /// 3. Dispatch (Requirement 7.3): only a newly-recorded Activity reaches
    ///    [`InboundActivityDispatcher::dispatch`].
    async fn process_verified(
        &self,
        activity: ParsedActivity,
        signer: VerifiedSigner,
        destination: LocalRecipientContext,
    ) -> Result<InboxOutcome, AppError> {
        let blocked = self
            .block_policy
            .is_blocked(&signer.actor_uri, destination)
            .await?;
        if blocked {
            return Err(AppError::client(
                StatusCode::FORBIDDEN,
                "signer is blocked by this recipient",
            ));
        }

        let is_new = self.dedup.record_if_new(&activity.id).await?;
        if !is_new {
            return Ok(InboxOutcome::Duplicate);
        }

        let ctx = InboundContext { signer };
        self.dispatcher.dispatch(&activity, &ctx).await?;

        Ok(InboxOutcome::Accepted)
    }
}
