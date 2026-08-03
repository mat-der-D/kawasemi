//! `ap_get` handlers (design.md File Structure Plan `ap_get.rs`
//! "アクター/オブジェクト/コレクション activity+json GET・authorized
//! fetch・content negotiation（オブジェクト/コレクションは
//! `ObjectDocumentProvider` レジストリへ委譲）"; design.md's `#### Endpoints`
//! Responsibilities: "AP GET: content negotiation（9.4, 6.3）、セキュアモード
//! 時 authorized fetch 検証（6.4）、未検出（6.6）。アクター URL 以外（オブジ
//! ェクト/コレクション）は `ObjectDocumentProvider` レジストリへ委譲し、
//! `None` は未検出（6.6）として応答する。"; Requirements 6.1, 6.2, 6.3, 6.4,
//! 6.6, 9.4; task 5.2, `Boundary: ap_get`): the `application/activity+json`
//! GET path for local actors ([`actor_get`]) and every other local
//! object/collection URL ([`object_get`]).
//!
//! ## Not wired into a router (task 5.4's job)
//! Mirrors task 5.1's `webfinger.rs`/`nodeinfo.rs` precedent (itself mirroring
//! `src/oauth/apps_endpoint.rs`): both handlers here are plain axum handlers
//! shaped for `.route(...).with_state(...)`, exercised by this module's own
//! unit tests (`src/federation/endpoints/ap_get/tests.rs`) and, for full
//! HTTP-observable behavior, by `tests/ap_get_outbox_it.rs`'s test-local
//! `axum::Router`. Nothing in this crate currently mounts them.
//!
//! ## Two handlers, one file, per Requirement 6's own actor/non-actor split
//! Requirement 6.1's actor representation is built directly via
//! [`ActivityPubDocumentBuilder::build_actor_document`] (already-resolved
//! actor-model data, task 3.6); Requirement 6.2's non-actor object/collection
//! representation is delegated entirely to the [`ObjectDocumentRegistry`]
//! (task 3.5) — this module never builds an object/collection body itself.
//! Task 5.4 (bootstrap wiring, out of this task's boundary) is expected to
//! mount [`actor_get`] at [`crate::federation::urls::ActorUrls::actor_url`]'s
//! shape (`/users/{handle}`) and [`object_get`] as a catch-all beneath it for
//! every other local URL this instance serves under its own domain.
//!
//! ## Ordering: Accept check, then authorized fetch, then existence lookup
//! design.md pins each of these three checks to a requirement (9.4/6.3,
//! 6.4, 6.6) but not their relative order. This module checks content
//! negotiation first (a request that does not even ask for an ActivityPub
//! representation is rejected before anything else runs), then — only in
//! secure mode — authorized fetch, and only *after* both of those succeed
//! does it look the resource up at all. Authorized fetch is deliberately
//! checked *before* the existence lookup (rather than after, returning 404
//! for an absent resource before ever checking the signature): checking
//! authorization first means an unsigned/unverifiable request in secure mode
//! always gets the same 401 regardless of whether the requested actor/object
//! actually exists, so this endpoint cannot be used as an oracle to enumerate
//! valid local handles/object ids by comparing 401-vs-404 responses from
//! unauthenticated requests. To keep this property exact, both handlers
//! build the URL they authorize-fetch-check against from the *raw incoming
//! request path* (`https://{domain}{uri.path()}`), not from a canonical URL
//! only reachable after a successful `Handle`-parse/DB lookup — so even a
//! syntactically invalid handle segment gets the same 401-before-404
//! treatment as a syntactically valid but unknown one.
//!
//! ## Non-AP `Accept`: mapped to `406`, not silently falling through
//! Requirement 6.3 says a non-AP-representation `Accept` should not receive
//! the AP representation and should be "delegated to a non-AP-representation
//! extension point" — but this spec owns no such extension point (Non-Goals:
//! "具体 Activity 種別の業務処理" and everything content-related beyond
//! ActivityPub JSON is out of scope). design.md's own API Contract table
//! (`## Endpoints` -> API Contract) resolves this ambiguity concretely: both
//! the actor-URL and object/collection-URL rows list `406` as a documented
//! error response alongside `401(secure)`/`404`. This module returns that
//! literal `406 Not Acceptable` rather than inventing a fallback response
//! body this spec has no content for.
//!
//! ## An absent `Accept` header is treated as "not requesting AP"
//! Requirement 9.4 names exactly two media types as ActivityPub-representation
//! requests; it does not address a wholly absent `Accept` header (which,
//! per plain HTTP semantics, technically means "anything is acceptable").
//! This spec's own callers are federation implementations, not browsers —
//! sending no `Accept` header at all to an actor/object URL is not the
//! ActivityPub-typical way to ask for the JSON representation. This module
//! resolves the ambiguity conservatively: a missing `Accept` header is
//! treated the same as a non-AP one (`406`), never silently assumed to want
//! the AP representation.
//!
//! ## Authorized fetch reuses `SignatureVerifier` directly, not
//! `InboxService::process_inbound`
//! design.md's `InboxService` Responsibilities section says "authorized
//! fetch（6.4）の署名検証もこのサービスの検証経路を共用" (reuse *this
//! service's* verification path) — but per `inbound/service.rs`'s own
//! Implementation Notes (task 4.1), `InboxService::process_inbound` runs the
//! *full* receive pipeline (signature verification -> required-property
//! validation -> block judgment -> dedup -> dispatch) against a request body
//! it expects to be an Activity. An authorized-fetch GET carries no Activity
//! body at all, must not be deduplicated (a GET is not an idempotency-keyed
//! delivery), must not be dispatched to any `InboundActivityHandler`, and has
//! no `LocalRecipientContext` to hand `BlockPolicy` (this spec draws no
//! connection between "who is blocked" and "who may authorized-fetch" —
//! Requirement 6.4 only ever says "署名検証に失敗した要求には...返さない",
//! never mentioning block policy at all). Running the full pipeline for a
//! GET would therefore be actively wrong, not merely mismatched in shape.
//! The literally reusable "verification path" both entry points of
//! `InboxService` themselves converge on *before* dedup/dispatch is
//! `SignatureVerifier::verify_request` (which `InboxService` itself holds
//! and calls first) — this module depends on that same trait directly,
//! exactly the judgment call this task's own brief anticipates ("smallest
//! reasonable deviation, documented precisely", mirroring task 4.1's own
//! `destination`-parameter precedent for a similar design.md-vs-reality
//! gap).
//!
//! ## `ApGetState<V>`: generic over the verifier type, not `Arc<dyn
//! SignatureVerifier>` / a boxed-future adapter
//! An earlier draft of this module tried a local dyn-safe adapter trait
//! (mirroring `document.rs`'s own boxed-future technique for
//! `ObjectDocumentProvider`/`OutboxSource`) so this state could hold `Arc<dyn
//! _>` and the handler functions could stay non-generic. That does not
//! compile: [`SignatureVerifier::verify_request`] is a literal `async fn`
//! with no `+ Send` bound on its returned future in the trait signature (see
//! `verifier.rs`'s own doc comment on why — mirrors
//! `FederationHttpClient`/`PublicKeyResolver`'s identical
//! `#[allow(async_fn_in_trait)]` choice), so *generic* code cannot assume
//! the future any arbitrary `V: SignatureVerifier` produces is `Send` --
//! only a concrete, monomorphized `V` lets the compiler actually prove it,
//! which a `dyn Future + Send` erasure defeats. `ObjectDocumentProvider`
//! could use the boxed-future trick because its own trait definition (task
//! 3.5, this crate's own code) was written with the desugared `Pin<Box<dyn
//! Future<...> + Send + '_>>` signature from the start; `verifier.rs` is a
//! read-only dependency this task may not edit to add that. [`ApGetState`]
//! is therefore generic over `V: SignatureVerifier`, held as `Arc<V>` — the
//! exact same shape `InboxService<V, B, D>` (task 4.1) already established
//! for this crate's identical constraint — and [`actor_get`]/[`object_get`]
//! are themselves generic functions, monomorphized once per concrete `V` a
//! caller mounts them with (e.g. `get(actor_get::<HttpSignatureVerifier<R>>)`).
//! `Clone` is implemented by hand rather than `#[derive(Clone)]` so this
//! state stays `Clone` (required by `axum::extract::State`) without forcing
//! an unnecessary `V: Clone` bound `derive` would otherwise add — every
//! field is `Arc`-wrapped or already cheap to clone.

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri, header};
use axum::response::Response;
use serde_json::Value;

use crate::actor::{ActorDirectory, Handle};
use crate::error::AppError;
use crate::federation::endpoints::document::{ActivityPubDocumentBuilder, ObjectDocumentRegistry};
use crate::federation::jsonld::accepts_activitypub;
use crate::federation::signatures::{IncomingRequest, SignatureVerifier, VerifiedSigner};

/// ActivityPub's primary media type, this endpoint's response
/// `Content-Type` on every success path (Requirement 6.1, 6.2). Mirrors
/// `webfinger.rs`'s own private literal for the same string (not exported
/// from `jsonld.rs`).
const ACTIVITY_JSON_MEDIA_TYPE: &str = "application/activity+json";

/// Everything [`actor_get`]/[`object_get`] need, bundled behind one
/// `axum::extract::State`-compatible handle (mirrors `WebfingerState`'s
/// established shape for a not-yet-wired endpoint). See this module's doc
/// comment ("`ApGetState<V>`") for why this is generic over the verifier
/// type rather than `Arc<dyn SignatureVerifier>`.
pub struct ApGetState<V: SignatureVerifier> {
    /// Resolves a local actor by handle, owner-non-exposing — used only by
    /// [`actor_get`] (Requirement 6.1). Duplicates the
    /// [`ActivityPubDocumentBuilder`]'s own private `actor_directory` field
    /// (no accessor exists there; adding one was judged out of this task's
    /// boundary, the same "narrow duplicate rather than widen an
    /// already-reviewed component's public surface" call `WebfingerState`
    /// documents for its own `domain` field).
    pub directory: Arc<ActorDirectory>,
    /// Builds actor documents (task 3.6). Shared, `Arc`-wrapped so this
    /// state stays cheaply `Clone` despite `ActivityPubDocumentBuilder`
    /// itself not being `Clone`.
    pub document_builder: Arc<ActivityPubDocumentBuilder>,
    /// The downstream-supply delegation registry for every local
    /// object/collection URL that is not an actor URL (task 3.5,
    /// Requirement 6.2). `Ok(None)`/no registered provider both mean
    /// not-found (Requirement 6.6).
    pub object_documents: Arc<ObjectDocumentRegistry>,
    /// This instance's own configured domain (`ServerConfig::domain`), used
    /// by both handlers to reconstruct the canonical `https://{domain}{path}`
    /// URL of the incoming request — the same shape
    /// [`crate::federation::urls::ActorUrls::object_url`]/
    /// [`crate::federation::urls::ActorUrls::actor_url`] build, and what
    /// [`ObjectDocumentRegistry`]'s own registered providers (task 3.5)
    /// expect to see. See this module's doc comment ("Ordering") for why
    /// this is the *raw request path*, not a canonical URL only reachable
    /// after a successful lookup.
    pub domain: String,
    /// Whether secure mode (authorized fetch, Requirement 6.4) is enabled.
    /// An explicit constructor-supplied value here, not read from live
    /// config — task 5.4 (not yet run) is what wires a real secure-mode
    /// config flag into the live app; this task's handlers accept it as
    /// state, the same way task 5.1's handlers took `domain` as explicit
    /// state rather than reading live config.
    pub secure_mode: bool,
    /// The signature-verification boundary authorized fetch calls into
    /// (Requirement 6.4). See this module's doc comment ("Authorized fetch
    /// reuses `SignatureVerifier` directly") for why this is
    /// `SignatureVerifier` itself, not `InboxService`.
    pub verifier: Arc<V>,
}

impl<V: SignatureVerifier> Clone for ApGetState<V> {
    fn clone(&self) -> Self {
        Self {
            directory: Arc::clone(&self.directory),
            document_builder: Arc::clone(&self.document_builder),
            object_documents: Arc::clone(&self.object_documents),
            domain: self.domain.clone(),
            secure_mode: self.secure_mode,
            verifier: Arc::clone(&self.verifier),
        }
    }
}

/// Requirement 6.6's "未検出" (not found) response: a plain `404`, no
/// distinction in the public message between "actor unknown", "object
/// unknown", or "handle syntax invalid" (mirrors `webfinger.rs`'s identical
/// choice for the same reason — no oracle for probing which case applies).
fn not_found() -> AppError {
    AppError::client(StatusCode::NOT_FOUND, "ActivityPub resource not found")
}

/// Requirement 6.3/9.4's content-negotiation gate: `406` unless the
/// request's `Accept` header names an ActivityPub representation media
/// type. See this module's doc comment ("Non-AP `Accept`: mapped to `406`"
/// and "An absent `Accept` header...") for why this is `406` rather than a
/// silent fallback, and why a missing header is treated the same as a
/// non-AP one.
fn require_ap_accept(headers: &HeaderMap) -> Result<(), AppError> {
    let accept = headers
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if accepts_activitypub(accept) {
        Ok(())
    } else {
        Err(AppError::client(
            StatusCode::NOT_ACCEPTABLE,
            "Accept header must request an ActivityPub representation \
             (application/activity+json or application/ld+json)",
        ))
    }
}

/// Requirement 6.4's authorized-fetch gate: builds an [`IncomingRequest`]
/// from `method`/`url`/`headers` (no body — an authorized-fetch GET carries
/// no Activity to verify a body digest against) and verifies it via
/// `state.verifier`. Only called when `state.secure_mode` is set; the
/// caller is responsible for that check. Propagates
/// [`SignatureVerifier::verify_request`]'s own uniform 401 [`AppError`]
/// unchanged on failure.
async fn authorize_fetch<V: SignatureVerifier>(
    state: &ApGetState<V>,
    method: Method,
    url: &str,
    headers: &HeaderMap,
) -> Result<VerifiedSigner, AppError> {
    let incoming = IncomingRequest {
        method,
        url: url.to_string(),
        headers: headers.clone(),
        body: None,
    };
    state.verifier.verify_request(&incoming).await
}

/// Renders `document` as an `application/activity+json` response
/// (Requirements 6.1, 6.2).
fn activity_json_response(document: Value) -> Response {
    let body = serde_json::to_vec(&document).expect("an AP document always serializes to JSON");
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, ACTIVITY_JSON_MEDIA_TYPE)
        .body(Body::from(body))
        .expect("a fixed status/header/JSON-body response is always well-formed")
}

/// `GET {actor_url}` (Requirements 6.1, 6.4, 6.5, 6.6, 9.4): the local
/// actor's ActivityPub actor document, content-negotiated,
/// authorized-fetch-gated in secure mode, owner-free (already enforced by
/// [`ActivityPubDocumentBuilder::build_actor_document`], task 3.6). See this
/// module's doc comment ("Ordering") for the exact check sequence.
pub async fn actor_get<V: SignatureVerifier>(
    State(state): State<ApGetState<V>>,
    Path(handle_str): Path<String>,
    uri: Uri,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    require_ap_accept(&headers)?;

    if state.secure_mode {
        // Built from the raw incoming request path, not a canonical URL
        // only reachable after a successful `Handle`-parse -- see this
        // module's doc comment ("Ordering") for why this must hold even for
        // a syntactically invalid handle segment.
        let requested_url = format!("https://{}{}", state.domain, uri.path());
        authorize_fetch(&state, Method::GET, &requested_url, &headers).await?;
    }

    // A syntactically invalid handle can never name a real local actor
    // (mirrors `webfinger.rs`'s identical "invalid syntax == unknown"
    // treatment).
    let handle = Handle::new(&handle_str).map_err(|_| not_found())?;

    let resolved = state
        .directory
        .resolve_actor_by_handle(&handle)
        .await?
        .ok_or_else(not_found)?;

    let document = state
        .document_builder
        .build_actor_document(&resolved)
        .await?;
    Ok(activity_json_response(document))
}

/// `GET` for every local object/collection URL that is not an actor URL
/// (Requirements 6.2, 6.4, 6.6, 9.4): delegates entirely to
/// [`ObjectDocumentRegistry::resolve`] (task 3.5), with `None` (no
/// registered provider, or a registered provider's own "no such object")
/// mapped to not-found. See this module's doc comment ("Ordering") for the
/// exact check sequence.
pub async fn object_get<V: SignatureVerifier>(
    State(state): State<ApGetState<V>>,
    uri: Uri,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    require_ap_accept(&headers)?;

    let url = format!("https://{}{}", state.domain, uri.path());

    if state.secure_mode {
        authorize_fetch(&state, Method::GET, &url, &headers).await?;
    }

    let document = state
        .object_documents
        .resolve(&url)
        .await?
        .ok_or_else(not_found)?;

    Ok(activity_json_response(document))
}
