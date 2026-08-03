//! `SocialGraphInboundHandler` (design.md "Federation / 連合層" ->
//! `#### InboundHandler`, design.md lines ~520-542; Requirements 7.1, 7.2,
//! 7.3, 7.4, 7.5, 7.6, 7.7, 2.5, 2.6, 3.2; task 4.1, `Boundary: InboundHandler,
//! Transitions, ActivityBuilder`): implements federation-core's
//! [`InboundActivityHandler`] for the received Follow / Accept / Reject /
//! Block / Undo(Follow|Block) Activities, converging them onto the exact same
//! [`FollowApprovalPolicy`] (task 2.1) and [`Transitions`] (task 2.2) state
//! transitions the API path (`FollowService`/`FollowRequestService`/
//! `BlockService`, task 3.x) already calls — design.md's "意味論対称"
//! requirement applied to the receiving side.
//!
//! ## Scope
//! Owns exactly [`SocialGraphInboundHandler`] and the narrow
//! [`ActorUriResolver`] delegation port (plus its production implementation,
//! [`ProdActorUriResolver`]) it depends on to turn a wire-level actor URI
//! (`ctx.signer.actor_uri`, or an Activity's own `actor`/`object` reference)
//! into this spec's own [`AccountRef`]. Does not touch federation-core's
//! `InboundActivityDispatcher`/`InboundActivityHandler` themselves (already
//! implemented, `src/federation/inbound/dispatcher.rs`), does not register
//! this handler against a live dispatcher or `AppState`/bootstrap (task 5.2's
//! `SocialGraphModule` wiring boundary, not this task's), and does not touch
//! `BlockPolicyImpl`/`RelProviderImpl`/`AccountCountsProviderImpl` (tasks
//! 4.2-4.4). Mirrors `crate::statuses::inbound_handlers`'s own "standalone,
//! independently unit-testable module with no live caller yet" precedent
//! (that module's own doc comment, "Scope").
//!
//! ## One handler for all five outer types (matches design.md's literal
//! sketch, unlike `statuses::inbound_handlers`'s one-struct-per-type shape)
//! design.md's own `InboundHandler` Service Interface sketch (lines ~536-542)
//! writes a *single* `impl InboundActivityHandler for SocialGraphInbound`
//! whose `activity_types()` returns all five outer types at once —
//! `["Follow","Accept","Reject","Block","Undo"]` — unlike
//! `statuses::inbound_handlers`'s six separate one-type-per-struct handlers.
//! This module follows design.md's own literal shape here rather than the
//! statuses precedent: [`SocialGraphInboundHandler::handle`] dispatches
//! internally on `activity.activity_type` to one of five private methods.
//! `Accept`/`Reject`/`Block`/`Follow` are outer types this spec owns
//! exclusively (no other spec in this crate registers a handler for them —
//! `grep`-verified against `statuses::inbound_handlers`'s own six
//! registrations), so a malformed payload for any of those four is reported
//! as a genuine `Err` (422-shaped, mirrors `ingest_note_object`'s/
//! `UpdateHandler`'s own "unambiguously-owned type, malformed body ->
//! `Err`, not silent `Ignored`" convention), never silently swallowed.
//! `Undo` is the one outer type genuinely shared with `statuses::
//! inbound_handlers::UndoHandler` (that module's own doc comment names this
//! exact multimap situation) — this handler inspects the wrapped inner
//! object's own `type` and returns [`HandleOutcome::Ignored`] for anything
//! other than `Follow`/`Block`, so `UndoHandler`'s registration for
//! `Undo(Announce|Like)` is unaffected (`dispatcher.rs`'s own doc comment,
//! "Multimap, not one-handler-per-type").
//!
//! ## `ActorUriResolver`: a narrow `actor_uri -> AccountRef` port, not a
//! dependency on `crate::statuses`'s own `ProdRemoteActorResolver`
//! Every handler here needs to turn a bare wire-level actor URI string into
//! this spec's own [`AccountRef`] — for `ctx.signer.actor_uri` (the acting
//! party, security-critical: see "Resolving the acting party" below), and
//! for an Activity's own `actor`/`object` URI references (the resource being
//! addressed, not security-critical the same way — see the same section).
//! `crate::statuses::inbound_handlers`'s own `RemoteActorResolver` port and
//! its production `crate::statuses::ProdRemoteActorResolver` implementation
//! solve an adjacent problem (`actor_uri -> Id`, collapsing local/remote into
//! one opaque id) but do not distinguish [`AccountRef::Local`] from
//! [`AccountRef::Remote`] — a distinction [`FollowApprovalPolicy::
//! requires_approval`] (Requirement 3.2's same-server admin privilege) and
//! every [`Transitions`] call this module makes structurally require. Rather
//! than depending on a sibling spec's own module for a type-shape it does not
//! provide (and widening that module's own already-reviewed boundary to
//! serve this one), this module defines its own narrow port,
//! [`ActorUriResolver`], and its own production implementation,
//! [`ProdActorUriResolver`] — deliberately mirroring
//! `crate::statuses::ProdRemoteActorResolver`'s already-reviewed
//! local-shape-shortcut/genuinely-remote-fallback strategy and its
//! `tokio::spawn`-wrapped fetch (that type's own doc comment explains why the
//! spawn is needed even for a fully concrete `RemoteAccountFetcher<
//! ReqwestFederationHttpClient>` instantiation), adapted to return
//! [`AccountRef`] instead of a bare [`Id`].
//!
//! ## Resolving the acting party: `ctx.signer.actor_uri`, never an
//! Activity's own `actor` property (security decision, mirrors
//! `statuses::inbound_handlers`'s identical precedent)
//! Every handler below resolves *who is acting* (the Follow's follower, the
//! Accept/Reject/Undo's sender, the Block's blocker) exclusively from
//! [`InboundContext::signer`] — federation-core's already-HTTP-Signature-
//! verified identity — never from the wire JSON's own `actor` property (which
//! an attacker could set to any value without invalidating the signature,
//! since a signature covers headers/digest, not a deep inspection of every
//! embedded property). This mirrors `statuses::inbound_handlers`'s own doc
//! comment, "Resolving the acting remote actor", applied here to Follow/
//! Accept/Reject/Block/Undo instead of Create/Announce/Like/Delete/Update.
//!
//! `ctx.signer.actor_uri` is not always genuinely remote: design.md's own
//! "ローカル/リモート対称" requirement (1.3, 10.3) means a **local**-to-local
//! Follow (the same-server admin privilege, Requirement 3.1/3.2) is delivered
//! via `InboxService::process_local`'s in-process path with `signer` set to
//! the *local* follower's own identity (`InboxService::process_local`'s own
//! doc comment: "`signer` は送信するローカルアクター自身の識別子...署名検証
//! を除く同一意味論経路"), not a remote one. [`ActorUriResolver`]'s local-
//! shape-shortcut therefore correctly resolves such a `signer` to
//! [`AccountRef::Local`], which is exactly what
//! [`FollowApprovalPolicy::requires_approval`] needs to see to grant the
//! same-server privilege on the *receiving* side symmetrically with the
//! *sending* side's own `FollowService::follow` (task 3.1).
//!
//! ## Resolving the addressed resource: an Activity's own `actor`/`object`
//! properties, read directly (not a security decision — see above)
//! By contrast, the *target* of a Follow/Block (`object`, always this
//! instance's own local recipient) and the *original requester* embedded in
//! an Accept/Reject's inner `Follow` object (`actor`) are read directly off
//! the wire JSON, mirroring `statuses::inbound_handlers::object_reference_uri`'s
//! own "reading a target reference off the body is fine, only the acting
//! identity needs cryptographic verification" precedent. This is safe by
//! construction for the Accept/Reject case specifically because
//! [`Transitions::promote_pending`]/[`Transitions::drop_pending`] can only
//! ever *consume an already-existing* pending row (via
//! [`crate::social_graph::repository::take_request`]) — they can never
//! fabricate one — so a forged `actor` value naming an unrelated account
//! can, at worst, select a pending row that does not exist (an idempotent
//! no-op, `Ok(None)`), never grant a follow/promotion that was not already
//! legitimately pending.
//!
//! ## `establish_follow`'s `activity_id` and `record_pending`'s
//! `FollowRequest.activity_id`: the *received* Follow's own `activity.id`
//! [`ParsedActivity::id`] (the received Follow's own wire `id`, already
//! extracted by `federation::jsonld::parse_activity`) is what this module
//! passes as both `Transitions::establish_follow`'s `activity_id` parameter
//! and the inbound `FollowRequest.activity_id` field — mirroring `model.rs`'s
//! own documented convention that this field is "the Follow Activity id the
//! eventual Accept/Reject Activity references" regardless of which side
//! originally sent it. This is exactly the value
//! [`crate::social_graph::follow_request_service::FollowRequestService::
//! authorize_request`]/`reject_request` (task 3.2) later reads back out via
//! `Transitions::promote_pending`/`drop_pending`'s own widened `Option<
//! String>` return, to build `Accept`/`Reject`'s embedded reference.
//!
//! ## Idempotency (Requirement 7.7): inherited from `transitions.rs`, plus
//! one pre-check this module adds for the inbound-Follow "received twice"
//! scenario
//! Every [`Transitions`] call this module makes (`establish_follow`,
//! `record_pending`, `promote_pending`, `drop_pending`, `mark_blocked_by`,
//! `remove_follow`, `clear_blocked_by`) is already idempotent at the DB layer
//! (`transitions.rs`'s own doc comment, "Idempotency... is inherited
//! structurally, not re-implemented") — a second call with the same logical
//! (source, target) pair never duplicates a row or re-emits a notification.
//! [`Self::handle_follow`] additionally pre-checks whether the source already
//! follows (or already has a pending inbound request toward) the target
//! *before* deciding Establish-vs-RequireApproval and *before* building/
//! delivering a fresh `Accept(Follow)` — the one inbound-handler-level
//! scenario this task's own brief names explicitly ("receiving a Follow
//! twice"): without this pre-check, a second, distinct `Follow` Activity id
//! for an already-established pair would still leave `follows`/
//! `follow_requests` correctly idempotent (no duplicate row), but would
//! needlessly rebuild and redeliver a second `Accept(Follow)` to the sender.
//! No other handler here needs an analogous pre-check: `Accept`/`Reject`
//! /`Undo` only ever *consume or remove* a row (never build a corresponding
//! reply Activity), so `transitions.rs`'s own `Option`-returning /
//! delete-is-idempotent semantics are already sufficient on their own.
//!
//! ## `deliver: BoxedDeliver`, not `Arc<DeliveryService<D, LS, HS>>` (the
//! `Send`-boxed-trait-object architecture this task's own boundary runs
//! into, and why it cannot be solved the way `LocalActorLookup`/
//! `RemoteActorLookup` were, above)
//! `FollowService`/`BlockService`/`FollowRequestService` (task 3.x) all hold
//! `Arc<DeliveryService<D, LS, HS>>` as a plain generic field, because none
//! of *their* own methods are ever required to return a `Send`-boxed trait
//! object — they are plain generic `async fn`s, awaited directly by whatever
//! calls them. [`SocialGraphInboundHandler::handle`] is different: it
//! implements [`InboundActivityHandler::handle`], whose signature
//! federation-core already fixed as `Pin<Box<dyn Future<..> + Send + 'a>>`
//! (that trait's own doc comment: needed because
//! `InboundActivityDispatcher` holds a heterogeneous `Vec<Arc<dyn
//! InboundActivityHandler>>`, requiring real dynamic dispatch). For a
//! *generic* `impl<AL, AR, AU> InboundActivityHandler for
//! SocialGraphInboundHandler<..>` block to satisfy that `+ Send` bound, every
//! trait method it transitively calls from inside the box must *itself*
//! guarantee a `Send` future in its own signature (Rust's `async fn`-in-trait
//! auto-trait leakage rule: a caller generic over `T: Trait` may only assume
//! what `Trait`'s own declaration promises, never what a specific impl's body
//! happens to satisfy) — this is exactly why [`LocalActorLookup::
//! resolve_handle`]/[`RemoteActorLookup::resolve_actor_uri`] (task 2.3,
//! `activity_builder.rs`, within *this* task's own `Boundary:
//! ActivityBuilder`) were widened to `-> impl Future<..> + Send` by this same
//! task (see those two methods' own doc comments) — a minimal, in-boundary,
//! additive signature widening, the same category of change tasks.md's own
//! Implementation Notes already document repeatedly for this spec (e.g. task
//! 3.2's `Transitions::promote_pending`/`drop_pending` widening). Since that
//! widening covers `AL`/`AR`, `SocialGraphInboundHandler` stays generic over
//! them (same ergonomics as `FollowService`/`BlockService`), unlike the
//! `deliver` seam below.
//!
//! `crate::federation::LocalActorLookup` (`target.rs`, `DeliveryService`'s own
//! `D` type parameter) and `crate::federation::DeliverySink` (`sink.rs`, `L`/
//! `H`) are the *same* class of `#[allow(async_fn_in_trait)]`,
//! not-Send-declared trait — `DeliveryService<D, L, H>::deliver`'s own body
//! (federation-core, `src/federation/outbound/delivery.rs`) transitively
//! calls both — but federation-core is *not* this task's boundary (`_Boundary:
//! InboundHandler, Transitions, ActivityBuilder_`; task 5.2 owns
//! `SocialGraphModule` wiring, and no task in this spec owns federation-core
//! itself), so widening those two traits the same way is not available here.
//! Making `SocialGraphInboundHandler` generic over `D`/`L`/`H` the same way
//! `FollowService` is would therefore make the `impl InboundActivityHandler`
//! block itself fail to compile (empirically confirmed while implementing
//! this task).
//!
//! [`SocialGraphInboundHandler`] therefore depends on [`BoxedDeliver`] — a
//! type-erased `Arc<dyn Fn(DeliveryRequest) -> Pin<Box<dyn Future<..> + Send>>
//! + Send + Sync>` — instead of a generic `Arc<DeliveryService<D, LS, HS>>`
//! field. This is not a workaround that silently drops the requirement: it
//! delivers the exact same `Accept(Follow)` via the exact same
//! `DeliveryService::deliver` call (Requirement 7.2) — only the *type* the
//! caller supplies changes. The erasure closure itself must be built at a
//! call site where `D`/`LS`/`HS` are already fully concrete (never inside a
//! function generic over them — the same leakage rule applies to *building*
//! the closure as to boxing `handle`'s own future) — i.e. by whichever
//! non-generic constructor already knows its own concrete
//! `DeliveryService<ConcreteD, ConcreteLS, ConcreteHS>` instantiation: this
//! task's own `inbound/tests.rs::build_handler` (concrete `ActorDirectory`/
//! `Arc<RecordingSink>`) does exactly this, and task 5.2's eventual bootstrap
//! wiring (concrete `crate::federation::module::ConcreteDeliveryService`)
//!   would do the same at its own call site.

#[cfg(test)]
mod tests;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::Value;
use sqlx::PgPool;

use crate::accounts::profile_repository;
use crate::accounts::remote_repository;
use crate::actor::{ActorDirectory, Handle};
use crate::domain::AccountRef;
use crate::error::AppError;
use crate::federation::inbound::dispatcher::{
    HandleOutcome, InboundActivityHandler, InboundContext,
};
use crate::federation::jsonld::ParsedActivity;
use crate::federation::signatures::ReqwestFederationHttpClient;
use crate::federation::{DeliveryRequest, Recipient};
use crate::runtime::RuntimeContext;
use crate::social_graph::activity_builder::{ActivityBuilder, LocalActorLookup, RemoteActorLookup};
use crate::social_graph::approval_policy::{FollowApprovalPolicy, FollowDecision};
use crate::social_graph::model::{FollowOptions, FollowRequest, FollowRequestDirection};
use crate::social_graph::repository;
use crate::social_graph::transitions::Transitions;

/// A type-erased "deliver this Activity" callback — see this module's doc
/// comment ("`deliver: BoxedDeliver`") for why [`SocialGraphInboundHandler`]
/// depends on this instead of a generic `Arc<DeliveryService<D, LS, HS>>`
/// field directly. A caller builds one from a concrete
/// `Arc<crate::federation::DeliveryService<D, LS, HS>>` at its own
/// non-generic call site, e.g.:
/// ```text
/// let svc: Arc<DeliveryService<ConcreteD, ConcreteLS, ConcreteHS>> = /* .. */;
/// let deliver: BoxedDeliver = Arc::new(move |req| {
///     let svc = Arc::clone(&svc);
///     Box::pin(async move { svc.deliver(req).await })
/// });
/// ```
pub type BoxedDeliver = Arc<
    dyn Fn(DeliveryRequest) -> Pin<Box<dyn Future<Output = Result<(), AppError>> + Send>>
        + Send
        + Sync,
>;

fn malformed(message: impl Into<String>) -> AppError {
    AppError::client(StatusCode::UNPROCESSABLE_ENTITY, message.into())
}

/// Reads `activity.raw`'s top-level object map, if `activity.raw` is a JSON
/// object at all (it always is — [`crate::federation::jsonld::parse_activity`]
/// already guarantees this at the [`ParsedActivity`] construction boundary;
/// mirrors `statuses::inbound_handlers::activity_map`'s identical defensive
/// helper).
fn activity_map(activity: &ParsedActivity) -> Option<&serde_json::Map<String, Value>> {
    activity.raw.as_object()
}

/// The narrow `actor_uri -> AccountRef` port this handler depends on. See
/// this module's doc comment ("`ActorUriResolver`") for why this exists
/// rather than reusing `crate::statuses`'s own adjacent-but-different-shaped
/// `RemoteActorResolver` port.
pub trait ActorUriResolver: Send + Sync {
    /// Resolves `actor_uri` to this spec's own [`AccountRef`] —
    /// [`AccountRef::Local`] when `actor_uri` names one of this instance's
    /// own actors, [`AccountRef::Remote`] otherwise (fetching and caching the
    /// remote actor document on a cache miss/stale entry as needed, mirroring
    /// [`crate::accounts::RemoteAccountFetcher::fetch_and_normalize`]'s own
    /// contract).
    ///
    /// Written as `impl Future<Output = ..> + Send` rather than a plain
    /// `async fn` (unlike this crate's other `#[allow(async_fn_in_trait)]`
    /// delegation-port traits) for the exact reason
    /// `statuses::inbound_handlers::RemoteActorResolver`'s own doc comment
    /// gives for its identical choice: every call site here awaits this port
    /// from *inside* the `Pin<Box<dyn Future<..> + Send + 'a>>`
    /// [`InboundActivityHandler::handle`] requires, so this port's own future
    /// must carry an explicit `Send` bound for that outer box to type-check.
    fn resolve_account_ref(
        &self,
        actor_uri: &str,
    ) -> impl Future<Output = Result<AccountRef, AppError>> + Send;
}

/// The real, DB/network-backed [`ActorUriResolver`] — deliberately mirrors
/// `crate::statuses::ProdRemoteActorResolver`'s already-reviewed
/// local-shape-shortcut / genuinely-remote-fallback strategy (see that
/// type's own doc comment for the full rationale this module does not
/// repeat), adapted to return [`AccountRef`] instead of a bare [`Id`](crate::domain::Id).
#[derive(Clone)]
pub struct ProdActorUriResolver {
    domain: String,
    directory: Arc<ActorDirectory>,
    fetcher: Arc<crate::accounts::RemoteAccountFetcher<ReqwestFederationHttpClient>>,
}

impl ProdActorUriResolver {
    /// Builds a resolver bound to `domain` (this instance's own configured
    /// server domain, for the local-actor shortcut — must match
    /// `ActorUrls`'s own domain exactly, mirrors `ProdRemoteActorResolver::new`'s
    /// identical requirement), `directory` (the local-actor lookup the
    /// shortcut resolves through), and `fetcher` (the genuinely-remote
    /// fallback).
    pub fn new(
        domain: impl Into<String>,
        directory: Arc<ActorDirectory>,
        fetcher: Arc<crate::accounts::RemoteAccountFetcher<ReqwestFederationHttpClient>>,
    ) -> Self {
        Self {
            domain: domain.into(),
            directory,
            fetcher,
        }
    }

    /// Extracts `{handle}` from `actor_uri` when it matches this instance's
    /// own `https://{domain}/users/{handle}` shape
    /// ([`crate::federation::urls::ActorUrls::actor_url`]'s exact
    /// construction) — verbatim copy of `ProdRemoteActorResolver::
    /// local_handle`'s own logic (that type's own doc comment).
    fn local_handle(&self, actor_uri: &str) -> Option<Handle> {
        let prefix = format!("https://{}/users/", self.domain);
        actor_uri
            .strip_prefix(prefix.as_str())
            .filter(|rest| !rest.is_empty() && !rest.contains('/'))
            .and_then(|rest| Handle::new(rest).ok())
    }
}

impl ActorUriResolver for ProdActorUriResolver {
    fn resolve_account_ref(
        &self,
        actor_uri: &str,
    ) -> impl Future<Output = Result<AccountRef, AppError>> + Send {
        let local_handle = self.local_handle(actor_uri);
        let directory = Arc::clone(&self.directory);
        let fetcher = Arc::clone(&self.fetcher);
        let actor_uri = actor_uri.to_string();
        async move {
            if let Some(handle) = local_handle
                && let Some(resolved) = directory.resolve_actor_by_handle(&handle).await?
            {
                return Ok(AccountRef::Local(resolved.id));
                // Otherwise: shaped like one of our own actor URLs, but no
                // currently registered local actor matches -- fall through to
                // the genuinely-remote path below, mirroring
                // `ProdRemoteActorResolver`'s identical fallback.
            }

            let joined = tokio::spawn(async move { fetcher.fetch_and_normalize(&actor_uri).await });
            match joined.await {
                Ok(result) => result.map(|account| AccountRef::Remote(account.id)),
                Err(join_err) => Err(AppError::server(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    join_err,
                )),
            }
        }
    }
}

/// Implements federation-core's [`InboundActivityHandler`] for Follow /
/// Accept / Reject / Block / Undo(Follow|Block) (design.md's exact
/// `InboundHandler`, Requirements 7.1-7.7, 2.5, 2.6, 3.2). See this module's
/// doc comment for the full per-Activity-type contract.
pub struct SocialGraphInboundHandler<AL, AR, AU>
where
    AL: LocalActorLookup,
    AR: RemoteActorLookup,
    AU: ActorUriResolver,
{
    pool: PgPool,
    runtime: RuntimeContext,
    local: AL,
    activity_builder: ActivityBuilder<AL, AR>,
    transitions: Transitions,
    deliver: BoxedDeliver,
    actor_uris: AU,
}

impl<AL, AR, AU> SocialGraphInboundHandler<AL, AR, AU>
where
    AL: LocalActorLookup,
    AR: RemoteActorLookup,
    AU: ActorUriResolver,
{
    /// Builds a `SocialGraphInboundHandler` bound to `pool`/`runtime`
    /// (mirrors `FollowService::new`'s identical rationale), `local`/
    /// `activity_builder`/`transitions` (the same collaborators, same roles,
    /// as `FollowService`/`BlockService` — see those modules' own doc
    /// comments, "Generic shape mirrors `InteractionService`"), `deliver`
    /// (the type-erased delivery callback — see this module's doc comment,
    /// "`deliver: BoxedDeliver`", for why this is not a generic
    /// `Arc<DeliveryService<D, LS, HS>>` field), and `actor_uris` (this
    /// module's own [`ActorUriResolver`] port, resolving `ctx.signer.actor_uri`
    /// and an Activity's own `actor`/`object` references to [`AccountRef`] —
    /// see this module's doc comment).
    pub fn new(
        pool: PgPool,
        runtime: RuntimeContext,
        local: AL,
        activity_builder: ActivityBuilder<AL, AR>,
        transitions: Transitions,
        deliver: BoxedDeliver,
        actor_uris: AU,
    ) -> Self {
        Self {
            pool,
            runtime,
            local,
            activity_builder,
            transitions,
            deliver,
            actor_uris,
        }
    }

    /// Resolves `account` to a ready-to-deliver [`Recipient`] — mirrors
    /// `follow_service.rs::FollowService::resolve_target`'s identical
    /// local/remote [`Recipient`] construction (including the same
    /// documented interim `{actor_uri}/inbox` remote-inbox convention, see
    /// that module's own doc comment, "Remote delivery's `inbox`"), applied
    /// here to an already-resolved [`AccountRef`] rather than a freshly
    /// parsed numeric target id.
    async fn resolve_recipient(&self, account: &AccountRef) -> Result<Recipient, AppError> {
        match account {
            AccountRef::Local(id) => {
                let handle = self.local.resolve_handle(*id).await?;
                Ok(Recipient::Local(handle))
            }
            AccountRef::Remote(id) => {
                let remote = remote_repository::find_remote_by_id(&self.pool, *id)
                    .await?
                    .ok_or_else(|| {
                        AppError::client(
                            StatusCode::NOT_FOUND,
                            format!("remote account {id:?} does not resolve to a known account"),
                        )
                    })?;
                let inbox = format!("{}/inbox", remote.actor_uri);
                Ok(Recipient::Remote {
                    inbox,
                    shared_inbox: None,
                })
            }
        }
    }

    /// Handles a received `Follow` (Requirements 7.2, 7.3, 3.2): resolves
    /// the addressed local target from the wire `object` property and the
    /// acting source from `ctx.signer` (this module's doc comment,
    /// "Resolving the acting party"), skips re-processing when the pair is
    /// already following/pending (this module's doc comment, "Idempotency"),
    /// then judges approval necessity via the same [`FollowApprovalPolicy`]
    /// the API path uses and executes the matching [`Transitions`]
    /// transition — establishing the follow and delivering `Accept(Follow)`
    /// back to the source, or recording an inbound pending request.
    /// [`HandleOutcome::Ignored`] when `object` does not resolve to one of
    /// this instance's own local actors (not addressed to us).
    async fn handle_follow(
        &self,
        activity: &ParsedActivity,
        ctx: &InboundContext,
    ) -> Result<HandleOutcome, AppError> {
        let Some(top) = activity_map(activity) else {
            return Ok(HandleOutcome::Ignored);
        };
        let Some(object_uri) = top.get("object").and_then(Value::as_str) else {
            return Err(malformed(
                "Follow is missing a required string 'object' (the followee's actor URI)",
            ));
        };

        let target = self.actor_uris.resolve_account_ref(object_uri).await?;
        let AccountRef::Local(target_id) = target else {
            // Not addressed to one of our own local actors -- not ours to
            // own (mirrors `statuses::inbound_handlers`'s "target not local
            // -> Ignored" convention).
            return Ok(HandleOutcome::Ignored);
        };

        let source = self
            .actor_uris
            .resolve_account_ref(&ctx.signer.actor_uri)
            .await?;

        // Idempotency pre-check (this module's doc comment, "Idempotency"):
        // `target` (us, local) is the viewer here so `followed_by`/
        // `requested_by` correctly reflect "has `source` already
        // followed/requested us", regardless of `source`'s own locality.
        let now = self.runtime.clock.now();
        let mut states =
            repository::load_states(&self.pool, &target, std::slice::from_ref(&source), now)
                .await?;
        let state = states
            .pop()
            .expect("load_states returns exactly one state per requested target");
        if state.followed_by || state.requested_by {
            return Ok(HandleOutcome::Handled);
        }

        let locked = profile_repository::find_profile(&self.pool, target_id)
            .await?
            .map(|profile| profile.locked)
            .unwrap_or(false);

        let decision = FollowApprovalPolicy.requires_approval(&source, &target, locked);

        match decision {
            FollowDecision::Establish => {
                let opts = FollowOptions {
                    reblogs: true,
                    notify: false,
                    languages: Vec::new(),
                };
                self.transitions
                    .establish_follow(&source, &target, &opts, &activity.id)
                    .await?;

                let accept = self
                    .activity_builder
                    .build_accept(&target, &activity.id, &source)
                    .await?;
                let sender = self.local.resolve_handle(target_id).await?;
                let recipient = self.resolve_recipient(&source).await?;
                (self.deliver)(DeliveryRequest {
                    activity: accept,
                    sender,
                    recipients: vec![recipient],
                })
                .await?;
            }
            FollowDecision::RequireApproval => {
                let req = FollowRequest {
                    requester: source,
                    target,
                    direction: FollowRequestDirection::Inbound,
                    activity_id: activity.id.clone(),
                    created_at: now,
                };
                self.transitions.record_pending(&req).await?;
            }
        }

        Ok(HandleOutcome::Handled)
    }

    /// Handles a received `Accept`/`Reject` (Requirements 2.5, 2.6): reads
    /// the original requester off the embedded inner `Follow` object's own
    /// `actor` (safe by construction — see this module's doc comment,
    /// "Resolving the addressed resource"), resolves the approving/declining
    /// party from `ctx.signer`, then promotes or drops the matching pending
    /// request via [`Transitions`]. Idempotent no-op (still `Handled`,
    /// `Transitions`'s own `Ok(None)` path) when no matching pending request
    /// exists.
    async fn handle_accept_or_reject(
        &self,
        activity: &ParsedActivity,
        ctx: &InboundContext,
        promote: bool,
    ) -> Result<HandleOutcome, AppError> {
        let outer_type = if promote { "Accept" } else { "Reject" };
        let Some(top) = activity_map(activity) else {
            return Ok(HandleOutcome::Ignored);
        };
        let Some(Value::Object(inner)) = top.get("object") else {
            return Err(malformed(format!(
                "{outer_type} is missing a required object 'object' (the referenced Follow)"
            )));
        };
        if inner.get("type").and_then(Value::as_str) != Some("Follow") {
            return Err(malformed(format!(
                "{outer_type}'s inner object must be of type 'Follow'"
            )));
        }
        let Some(requester_uri) = inner.get("actor").and_then(Value::as_str) else {
            return Err(malformed(format!(
                "{outer_type}'s inner Follow object is missing a required 'actor'"
            )));
        };

        let requester = self.actor_uris.resolve_account_ref(requester_uri).await?;
        let target = self
            .actor_uris
            .resolve_account_ref(&ctx.signer.actor_uri)
            .await?;

        if promote {
            self.transitions
                .promote_pending(&requester, &target)
                .await?;
        } else {
            self.transitions.drop_pending(&requester, &target).await?;
        }

        Ok(HandleOutcome::Handled)
    }

    /// Handles a received `Block` (Requirement 7.4): resolves the addressed
    /// local target from `object` and the acting blocker from `ctx.signer`,
    /// then records the `blocked_by` state (and clears both-direction
    /// follows/pending requests, `Transitions::mark_blocked_by`'s own
    /// single-transaction contract). [`HandleOutcome::Ignored`] when
    /// `object` does not resolve to one of this instance's own local actors.
    async fn handle_block(
        &self,
        activity: &ParsedActivity,
        ctx: &InboundContext,
    ) -> Result<HandleOutcome, AppError> {
        let Some(top) = activity_map(activity) else {
            return Ok(HandleOutcome::Ignored);
        };
        let Some(object_uri) = top.get("object").and_then(Value::as_str) else {
            return Err(malformed(
                "Block is missing a required string 'object' (the blocked actor URI)",
            ));
        };

        let target = self.actor_uris.resolve_account_ref(object_uri).await?;
        if !matches!(target, AccountRef::Local(_)) {
            return Ok(HandleOutcome::Ignored);
        }

        let source = self
            .actor_uris
            .resolve_account_ref(&ctx.signer.actor_uri)
            .await?;

        self.transitions.mark_blocked_by(&source, &target).await?;

        Ok(HandleOutcome::Handled)
    }

    /// Handles a received `Undo` wrapping `Follow` (Requirement 7.5) or
    /// `Block` (Requirement 7.6). [`HandleOutcome::Ignored`] for any other
    /// inner object type — see this module's doc comment ("One handler for
    /// all five outer types") for why `Undo` alone must stay permissive this
    /// way (shared outer type with `statuses::inbound_handlers::UndoHandler`).
    /// Once the inner type is recognized as `Follow`/`Block`, an unresolvable
    /// (non-local) `object` is a safe no-op `Handled` (nothing local to
    /// undo) rather than `Ignored` — this handler *does* own
    /// `Undo(Follow|Block)` semantics unconditionally past that point,
    /// mirroring `statuses::inbound_handlers::UndoHandler`'s own identical
    /// "unknown target -> Handled, not Ignored" convention.
    async fn handle_undo(
        &self,
        activity: &ParsedActivity,
        ctx: &InboundContext,
    ) -> Result<HandleOutcome, AppError> {
        let Some(top) = activity_map(activity) else {
            return Ok(HandleOutcome::Ignored);
        };
        let Some(Value::Object(inner)) = top.get("object") else {
            return Ok(HandleOutcome::Ignored);
        };

        let is_follow = match inner.get("type").and_then(Value::as_str) {
            Some("Follow") => true,
            Some("Block") => false,
            _ => return Ok(HandleOutcome::Ignored),
        };

        let Some(object_uri) = inner.get("object").and_then(Value::as_str) else {
            return Err(malformed(
                "Undo's inner Activity is missing a required string 'object' reference",
            ));
        };

        let target = self.actor_uris.resolve_account_ref(object_uri).await?;
        if !matches!(target, AccountRef::Local(_)) {
            // Owned type (Follow/Block), but nothing local to undo -- a safe
            // no-op success (mirrors `UndoHandler`'s identical convention).
            return Ok(HandleOutcome::Handled);
        }

        let source = self
            .actor_uris
            .resolve_account_ref(&ctx.signer.actor_uri)
            .await?;

        if is_follow {
            self.transitions.remove_follow(&source, &target).await?;
        } else {
            self.transitions.clear_blocked_by(&source, &target).await?;
        }

        Ok(HandleOutcome::Handled)
    }
}

impl<AL, AR, AU> InboundActivityHandler for SocialGraphInboundHandler<AL, AR, AU>
where
    AL: LocalActorLookup + Send + Sync,
    AR: RemoteActorLookup + Send + Sync,
    AU: ActorUriResolver,
{
    fn activity_types(&self) -> &[&str] {
        &["Follow", "Accept", "Reject", "Block", "Undo"]
    }

    fn handle<'a>(
        &'a self,
        activity: &'a ParsedActivity,
        ctx: &'a InboundContext,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<HandleOutcome, AppError>> + Send + 'a>,
    > {
        Box::pin(async move {
            match activity.activity_type.as_str() {
                "Follow" => self.handle_follow(activity, ctx).await,
                "Accept" => self.handle_accept_or_reject(activity, ctx, true).await,
                "Reject" => self.handle_accept_or_reject(activity, ctx, false).await,
                "Block" => self.handle_block(activity, ctx).await,
                "Undo" => self.handle_undo(activity, ctx).await,
                _ => Ok(HandleOutcome::Ignored),
            }
        })
    }
}
