//! `inbox` handlers (design.md File Structure Plan `inbox.rs` "inbox /
//! shared inbox POST ハンドラ（`LocalRecipientContext::Actor` /
//! `SharedInbox` を `BlockPolicy` へ渡す）"; Requirements 7.1, 7.2; task
//! 5.3, `Boundary: inbox`): the two HTTP entry points that hand a POSTed
//! signed Activity to [`InboxService::process_inbound`] — the per-actor
//! inbox ([`actor_inbox`]) and the domain-wide shared inbox
//! ([`shared_inbox`]) — differing from each other only in which
//! [`LocalRecipientContext`] they construct before calling into the shared
//! service.
//!
//! ## Not wired into a router (task 5.4's job)
//! Mirrors task 5.1/5.2's established precedent (`webfinger.rs`/`nodeinfo.rs`/
//! `ap_get.rs`/`outbox.rs`): both handlers here are plain axum handlers
//! shaped for `.route(...).with_state(...)`, exercised by this module's own
//! unit tests (`src/federation/endpoints/inbox/tests.rs`) and, for full
//! HTTP-observable behavior, by `tests/inbox_it.rs`'s test-local
//! `axum::Router`. Nothing in this crate currently mounts them. Task 5.4 is
//! expected to mount [`actor_inbox`] at
//! [`crate::federation::urls::ActorUrls::inbox_url`]'s shape
//! (`/users/{handle}/inbox`) and [`shared_inbox`] at
//! [`crate::federation::urls::ActorUrls::shared_inbox_url`]'s shape
//! (`/inbox`).
//!
//! ## `InboxState<V, B, D>`: generic over `InboxService`'s own three type
//! parameters, not `Arc<dyn _>`
//! `InboxService<V, B, D>` (task 4.1, `inbound/service.rs`) is itself
//! generic because none of `SignatureVerifier`/`BlockPolicy`/
//! `ReceivedActivityStore` is `dyn`-compatible (each is a literal
//! `#[allow(async_fn_in_trait)]` `async fn` trait with no provable `Send`
//! bound on its returned future through a `dyn` object — see
//! `inbound/service.rs`'s own doc comment, and `ap_get.rs`'s doc comment
//! "`ApGetState<V>`" for the identical, already-documented reasoning for
//! `SignatureVerifier` alone). This module holds an `Arc<InboxService<V, B,
//! D>>` directly and threads the same three type parameters through
//! [`actor_inbox`]/[`shared_inbox`], exactly the shape `ApGetState<V>`
//! already established for one such parameter — task 5.4 mounts each
//! handler once per concrete `(V, B, D)` triple (e.g.
//! `post(actor_inbox::<HttpSignatureVerifier<R>, RealBlockPolicy,
//! DbReceivedActivityStore>)`), monomorphized so the compiler can actually
//! prove the returned futures are `Send`.
//!
//! ## Destination-context construction: this task's whole point
//! Per this task's own text (`tasks.md` 5.3) and `inbound/service.rs`'s own
//! documented deviation (`InboxService::process_inbound` takes
//! `destination: LocalRecipientContext` as an explicit second parameter
//! precisely so the *endpoint*, not the service, makes this judgment from
//! the already-matched route):
//! - [`actor_inbox`] is mounted per local actor, so the destination local
//!   actor is unambiguous from the matched `{handle}` path segment —
//!   [`LocalRecipientContext::Actor { actor_uri }`] is built via
//!   [`crate::federation::urls::ActorUrls::actor_url`], the exact same
//!   construction `InboxService::process_local` uses for its own `Actor`
//!   context (`inbound/service.rs`'s doc comment, "`process_local` always
//!   uses `LocalRecipientContext::Actor`") — so a signer blocked from one
//!   entry point is blocked identically from the other.
//! - [`shared_inbox`] is one domain-wide endpoint with no path segment
//!   naming a destination actor at all, so it always builds
//!   [`LocalRecipientContext::SharedInbox`] (design.md's own contract: this
//!   variant's `BlockPolicy` query is never a bulk-reject point — see
//!   `inbound/block_policy.rs`'s doc comment).
//!
//! The `url` each handler hands to [`crate::federation::signatures::IncomingRequest`]
//! (what the signer's HTTP Signature actually covers) is built the same
//! way: [`crate::federation::urls::ActorUrls::inbox_url`] /
//! [`crate::federation::urls::ActorUrls::shared_inbox_url`] — the same
//! canonical shape a real `RequestSigner`/`SignatureNegotiator` on the
//! sending side signs against — rather than reconstructed from the
//! incoming request's own `Uri` (contrast `ap_get.rs`'s raw-request-path
//! approach, whose entire point is to authorize-fetch-check *before* any
//! lookup that could leak existence information; this module has no such
//! existence lookup at all — see "No actor-existence check" below — so
//! there is nothing for a raw-path reconstruction to protect against here).
//!
//! ## No actor-existence check
//! Mirrors `outbox.rs`'s own precedent and its own documented reasoning:
//! [`InboxService::process_inbound`] never checks whether the destination
//! local actor named by `{handle}` actually exists — `BlockPolicy` judges
//! the *signer's* identity, and dispatch is Activity-type-keyed, not
//! recipient-keyed, so a POST to an unregistered handle's inbox is handled
//! exactly like one to a registered handle whose downstream handlers simply
//! have nothing relevant to do with it. design.md's own API Contract table
//! (`## Endpoints` -> API Contract) lists only `401, 403, 422` as this
//! endpoint's documented error responses — `404` is conspicuously absent,
//! unlike the actor/object GET rows in the same table, which both list it.
//! This module therefore does *not* query `ActorDirectory` at all: a
//! syntactically valid `{handle}` segment is used purely to build the
//! per-actor inbox/actor URLs above, regardless of whether that actor is
//! actually registered.
//!
//! A syntactically **invalid** `{handle}` segment is the one case this
//! module does reject before calling into `InboxService` at all — with
//! `404`, mirroring every other endpoint in this crate's identical
//! "invalid syntax == unknown resource" convention (`webfinger.rs`,
//! `ap_get.rs`, `outbox.rs`) — because [`crate::actor::Handle::new`]'s
//! validation is a prerequisite for building *any* URL via [`ActorUrls`] at
//! all (`actor_url`/`inbox_url` both take `&Handle`, not an arbitrary
//! `&str`), not a business-level existence judgment this task's boundary
//! forbids. This is a narrow, precedented deviation from the literal API
//! Contract table (which does not enumerate `404` for this row) rather than
//! a silent one — flagged here per this task's own documentation
//! obligation.
//!
//! ## Body handling: empty body maps to `None`, not `Some(vec![])`
//! [`InboxService::process_inbound`] distinguishes "no body at all" (`422`,
//! Requirement 9.3's malformed-document bucket) from "a body present but
//! not valid JSON" (`400`, `jsonld::parse_activity`'s own distinct
//! failure). Axum's `Bytes` extractor cannot itself distinguish a request
//! with no body from one with a zero-length body — both yield an empty
//! `Bytes` value — so this module treats an empty extracted body the same
//! as "no body" ([`build_incoming_request`]), keeping a genuinely bodyless
//! inbox POST inside the documented `422` bucket rather than falling
//! through to `parse_activity`'s JSON-syntax `400`.
//!
//! ## Both `InboxOutcome` variants ack identically
//! Per `inbound/service.rs`'s own doc comment ("`InboxOutcome` vs.
//! `AppError`": "both of which an HTTP inbox endpoint (task 5.3) maps to
//! the same `202 Accepted` response") and this task's own text ("受理は
//! 202、重複は再処理なしの受領応答とする" — a duplicate is *still* a
//! successful receipt, just one that was not re-dispatched): both
//! [`InboxOutcome::Accepted`] and [`InboxOutcome::Duplicate`] map to
//! `202 Accepted` here, with no response body distinguishing them (the
//! `Duplicate` distinction exists for tests/observability at the
//! `InboxService` layer, not for this endpoint's own HTTP contract). Every
//! rejection (signature failure, malformed body, blocked signer) instead
//! surfaces as `Err(AppError)`, whose own `IntoResponse` impl
//! (`error.rs`) already carries the correct status (`401`/`422`/`403`
//! respectively) — this module never constructs those responses itself.

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method, StatusCode};

use crate::actor::Handle;
use crate::error::AppError;
use crate::federation::LocalRecipientContext;
use crate::federation::inbound::{BlockPolicy, InboxOutcome, InboxService, ReceivedActivityStore};
use crate::federation::signatures::{IncomingRequest, SignatureVerifier};
use crate::federation::urls::ActorUrls;

/// Everything [`actor_inbox`]/[`shared_inbox`] need, bundled behind one
/// `axum::extract::State`-compatible handle (mirrors `ApGetState<V>`'s
/// established shape for a not-yet-wired, multi-type-parameter endpoint
/// state). See this module's doc comment ("`InboxState<V, B, D>`") for why
/// this is generic over all three of `InboxService`'s type parameters.
pub struct InboxState<V, B, D>
where
    V: SignatureVerifier,
    B: BlockPolicy,
    D: ReceivedActivityStore,
{
    /// The receive-pipeline orchestrator (task 4.1) both handlers in this
    /// module call into. `Arc`-wrapped so this state stays cheaply `Clone`
    /// despite `InboxService` itself not being `Clone`.
    pub inbox: Arc<InboxService<V, B, D>>,
    /// This instance's canonical URL builder (task 1.3), used by both
    /// handlers to build the per-actor inbox/actor URLs and the shared
    /// inbox URL — see this module's doc comment ("Destination-context
    /// construction") for why this is the same construction
    /// `InboxService::process_local` itself uses.
    pub urls: ActorUrls,
}

impl<V, B, D> Clone for InboxState<V, B, D>
where
    V: SignatureVerifier,
    B: BlockPolicy,
    D: ReceivedActivityStore,
{
    fn clone(&self) -> Self {
        Self {
            inbox: Arc::clone(&self.inbox),
            urls: self.urls.clone(),
        }
    }
}

/// This module's one rejection case that never reaches `InboxService` at
/// all: a syntactically invalid `{handle}` path segment on the per-actor
/// inbox route. See this module's doc comment ("No actor-existence check")
/// for why this is `404`, and why it is the only existence-adjacent check
/// this module performs.
fn invalid_handle() -> AppError {
    AppError::client(StatusCode::NOT_FOUND, "inbox not found")
}

/// Builds the [`IncomingRequest`] `InboxService::process_inbound` verifies,
/// from `url` (the canonical inbox/shared-inbox URL a real sender's HTTP
/// Signature covers — see this module's doc comment,
/// "Destination-context construction"), the raw request `method`/`headers`,
/// and the extracted `body`. See this module's doc comment ("Body
/// handling") for why an empty `body` maps to `None`, not `Some(vec![])`.
fn build_incoming_request(
    method: Method,
    url: String,
    headers: HeaderMap,
    body: Bytes,
) -> IncomingRequest {
    IncomingRequest {
        method,
        url,
        headers,
        body: if body.is_empty() {
            None
        } else {
            Some(body.to_vec())
        },
    }
}

/// Maps both of `InboxService`'s non-rejecting outcomes to the same
/// `202 Accepted` (see this module's doc comment, "Both `InboxOutcome`
/// variants ack identically").
fn ack_status(outcome: InboxOutcome) -> StatusCode {
    match outcome {
        InboxOutcome::Accepted | InboxOutcome::Duplicate => StatusCode::ACCEPTED,
    }
}

/// `POST {inbox_url}` (Requirements 7.1, 7.2): a local actor's per-actor
/// inbox. Builds [`LocalRecipientContext::Actor`] from the matched
/// `{handle}` path segment and hands the request to
/// [`InboxService::process_inbound`]. See this module's doc comment for the
/// full contract (destination-context construction, no actor-existence
/// check, body handling, outcome mapping).
pub async fn actor_inbox<V, B, D>(
    State(state): State<InboxState<V, B, D>>,
    Path(handle_str): Path<String>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, AppError>
where
    V: SignatureVerifier,
    B: BlockPolicy,
    D: ReceivedActivityStore,
{
    let handle = Handle::new(&handle_str).map_err(|_| invalid_handle())?;

    let url = state.urls.inbox_url(&handle);
    let destination = LocalRecipientContext::Actor {
        actor_uri: state.urls.actor_url(&handle),
    };

    let req = build_incoming_request(method, url, headers, body);
    let outcome = state.inbox.process_inbound(req, destination).await?;

    Ok(ack_status(outcome))
}

/// `POST {shared_inbox_url}` (Requirements 7.1, 7.2): the domain-wide
/// shared inbox. Always builds [`LocalRecipientContext::SharedInbox`] (no
/// path segment names a destination actor here) and hands the request to
/// [`InboxService::process_inbound`]. See this module's doc comment for the
/// full contract.
pub async fn shared_inbox<V, B, D>(
    State(state): State<InboxState<V, B, D>>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, AppError>
where
    V: SignatureVerifier,
    B: BlockPolicy,
    D: ReceivedActivityStore,
{
    let url = state.urls.shared_inbox_url();

    let req = build_incoming_request(method, url, headers, body);
    let outcome = state
        .inbox
        .process_inbound(req, LocalRecipientContext::SharedInbox)
        .await?;

    Ok(ack_status(outcome))
}
