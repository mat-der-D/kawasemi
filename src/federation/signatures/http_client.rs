//! `FederationHttpClient` (design.md "PublicKeyResolver / FederationHttpClient
//! （モック可能境界）" -> Service Interface; Requirements 2.7, 1.3, 2.5; task
//! 1.4, `Boundary: FederationHttpClient`): the outbound network boundary —
//! sending signed requests and fetching remote resources (public keys,
//! remote actors) — placed behind a trait so tests can substitute a
//! deterministic mock instead of talking to the network (Requirement 2.7:
//! "署名検証・公開鍵取得・ネットワーク取得を差し替え可能な境界の背後に置き、
//! テストでモック実装へ差し替えられるようにする"; design.md: "`FederationHttpClient`
//! は送信 HTTP（公開鍵/アクター取得・配送送信）を表す port。本番実装と決定的
//! モック実装を差し替え可能にする").
//!
//! Scope: this module owns exactly the [`FederationHttpClient`] trait, the
//! [`OutboundRequest`] / [`HttpResponse`] shapes it speaks, a production
//! implementation backed by `reqwest` ([`ReqwestFederationHttpClient`]), and
//! a deterministic in-memory mock ([`MockFederationHttpClient`]) for tests.
//! It does not decide *when* to call `send`/`fetch`, does not sign
//! anything, and does not resolve public keys — those are `RequestSigner`
//! (task 1.5), `SignatureNegotiator` (task 3.x), `DeliveryWorker` (task
//! 11.x), and `PublicKeyResolver` (task 2.1), all out of this task's
//! boundary. `PublicKeyResolver` in particular is *not* implemented here —
//! only this port is, in a shape a future `PublicKeyResolver`
//! implementation can call `fetch` against.
//!
//! ## `OutboundRequest` / `HttpResponse`: load-bearing shape decisions
//! design.md's Service Interface pins only the trait signature —
//! `async fn send(&self, req: OutboundRequest) -> Result<HttpResponse, AppError>`
//! and `async fn fetch(&self, url: &str, signed_as: Option<&Handle>) ->
//! Result<HttpResponse, AppError>` — and does not define `OutboundRequest`/
//! `HttpResponse` anywhere else in this spec. Later tasks build on these
//! shapes directly (task 1.5 `RequestSigner` mutates an `&mut
//! OutboundRequest` to attach signature headers per design.md's
//! `sign_request(&self, actor: &Handle, format: SignatureFormat, req: &mut
//! OutboundRequest)`; task 2.2/2.3 read an incoming request's headers/body
//! to verify it), so both types are defined here as plain, `pub`-field
//! structs — no builder-only or private-field encapsulation — carrying
//! exactly the fields HTTP semantics require:
//! - [`OutboundRequest`]: `method`, `url`, `headers`, and an optional
//!   `body` (absent for a bodyless `GET`, present for a signed `POST`
//!   delivery). `headers` is mutable in place by design (`&mut
//!   OutboundRequest`) so `RequestSigner` can insert `Signature`/`Digest`/
//!   `Date` headers without rebuilding the request.
//! - [`HttpResponse`]: `status`, `headers`, `body` — everything
//!   `SignatureVerifier`/`PublicKeyResolver` need to inspect a fetched
//!   document or a delivery target's response.
//!
//! `method`/`status`/`headers` reuse `axum::http`'s `Method` / `StatusCode`
//! / `HeaderMap` (the same `http` crate types `reqwest` itself speaks — both
//! resolve to `http` 1.x in `Cargo.lock`, so no conversion glue is needed
//! between [`ReqwestFederationHttpClient`] and these types) rather than
//! introducing parallel HTTP vocabulary types.

#[cfg(test)]
mod tests;

use std::collections::VecDeque;
use std::sync::Mutex;

use axum::http::{HeaderMap, Method, StatusCode};

use crate::actor::Handle;
use crate::error::AppError;

/// The `Accept` header value [`ReqwestFederationHttpClient::fetch`] sends on
/// every request (task 6.4). Mirrors
/// `crate::federation::jsonld`'s own private `ACTIVITY_JSON_MEDIA_TYPE`
/// (not `pub` there, so duplicated verbatim rather than imported) — the
/// primary ActivityPub media type `crate::federation::jsonld::accepts_activitypub`
/// checks for, and the same value this crate's own AP GET content
/// negotiation requires a fetcher to send.
const ACTIVITYPUB_ACCEPT_HEADER_VALUE: &str = "application/activity+json";

/// An outbound HTTP request federation-core needs sent — either a signed
/// delivery (`send`) or an unsigned/actor-signed fetch turned into a request
/// by a caller. See this module's doc comment ("`OutboundRequest` /
/// `HttpResponse`: load-bearing shape decisions") for why these are the
/// fields and why they are `pub`.
#[derive(Debug, Clone)]
pub struct OutboundRequest {
    pub method: Method,
    pub url: String,
    pub headers: HeaderMap,
    /// Absent for a bodyless request (e.g. a `GET` fetch); present for a
    /// request carrying a JSON-LD Activity body (e.g. a signed `POST`
    /// delivery), whose digest `RequestSigner` (task 1.5) computes via
    /// [`super::digest::Digest`] and includes in the signing input
    /// (Requirement 1.3).
    pub body: Option<Vec<u8>>,
}

impl OutboundRequest {
    /// Builds a bodyless `OutboundRequest` with no headers set yet.
    pub fn new(method: Method, url: impl Into<String>) -> Self {
        Self {
            method,
            url: url.into(),
            headers: HeaderMap::new(),
            body: None,
        }
    }

    /// Attaches a body to this request (builder-style), e.g. a serialized
    /// ActivityPub document for a signed delivery.
    pub fn with_body(mut self, body: Vec<u8>) -> Self {
        self.body = Some(body);
        self
    }
}

/// The response to a [`FederationHttpClient::send`] or
/// [`FederationHttpClient::fetch`] call. See this module's doc comment
/// ("`OutboundRequest` / `HttpResponse`: load-bearing shape decisions").
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

/// Send-network port for federation (Requirements 2.7, 1.1, 11.2):
/// abstracts the actual HTTP transport so a production implementation
/// ([`ReqwestFederationHttpClient`]) and a deterministic mock
/// ([`MockFederationHttpClient`]) are interchangeable behind this trait —
/// design.md's exact pinned Service Interface.
///
/// `#[allow(async_fn_in_trait)]`: design.md pins this trait's methods as
/// literal `async fn` (matching `SignatureVerifier::verify_request`'s
/// documented rationale, "`FederationHttpClient` 等、本 spec の他の port と
/// 非同期性を統一"), so this keeps that exact syntax rather than
/// hand-desugaring to `-> impl Future<...> + Send`. The lint exists because
/// `async fn` in a trait does not pin the returned future to `Send`, which
/// matters once a caller needs `Arc<dyn FederationHttpClient>` or
/// `tokio::spawn`s a call (e.g. the future `DeliveryWorker`, task 11.x) —
/// that wiring, and any resulting need to fix the future's `Send`-ness or
/// box it, belongs to whichever task actually introduces it, not this
/// task's `FederationHttpClient, Digest` boundary. Both implementations
/// below ([`ReqwestFederationHttpClient`], [`MockFederationHttpClient`])
/// only ever `.await` other already-`Send` futures internally, so their
/// futures are `Send` in practice today regardless.
#[allow(async_fn_in_trait)]
pub trait FederationHttpClient: Send + Sync {
    /// Sends `req` (typically a signed delivery `POST`) and returns the
    /// response. Used by the delivery worker (task 11.x, out of this task's
    /// boundary) once it exists.
    async fn send(&self, req: OutboundRequest) -> Result<HttpResponse, AppError>;

    /// Fetches `url` (e.g. a remote actor document or a `keyId` URL for
    /// public-key material), optionally signing the request as `signed_as`
    /// for authorized-fetch-capable remotes. `signed_as` is not yet acted
    /// upon by this task's implementations (no `RequestSigner` exists yet,
    /// task 1.5) — the parameter exists so `PublicKeyResolver` (task 2.1)
    /// and the authorized-fetch path (Requirement 6.x) can call `fetch`
    /// with the actor whose signature should accompany the request once
    /// signing exists, without this port's signature changing later.
    async fn fetch(&self, url: &str, signed_as: Option<&Handle>) -> Result<HttpResponse, AppError>;
}

/// Production [`FederationHttpClient`] implementation, backed by a shared
/// `reqwest::Client` (connection pooling, TLS via `rustls`). A thin adapter:
/// its correctness is "compiles, implements the trait faithfully, delegates
/// to `reqwest` correctly" — no signing/retry/negotiation logic belongs
/// here (that is `RequestSigner`/`SignatureNegotiator`/`DeliveryWorker`'s
/// job, all out of this task's boundary).
pub struct ReqwestFederationHttpClient {
    client: reqwest::Client,
    /// When `true`, [`Self::send`]/[`Self::fetch`] rewrite a `https://`
    /// URL's scheme to `http://` immediately before dispatching (see
    /// [`Self::insecure_loopback`]'s doc comment for why this exists and why
    /// it must never be the default).
    downgrade_https_to_http: bool,
}

impl ReqwestFederationHttpClient {
    /// Builds a client with `reqwest`'s default connection settings. Speaks
    /// real TLS for every `https://` URL, unchanged (task 6.4 added
    /// [`Self::insecure_loopback`] alongside this constructor without
    /// altering `new()`'s own behavior or existing callers).
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            downgrade_https_to_http: false,
        }
    }

    /// Builds a client identical to [`Self::new`] except every `https://`
    /// URL passed to [`Self::send`]/[`Self::fetch`] has its scheme rewritten
    /// to `http://` immediately before dispatch (task 6.4, `Boundary:
    /// FederationTestHarness, federation_pair_it`, Requirements 13.1, 13.2,
    /// 13.3, 13.4).
    ///
    /// [`crate::federation::urls::ActorUrls`] hardcodes `https://{domain}/...`
    /// for every URL it builds, and [`crate::federation::test_harness::spawn_federation_pair`]
    /// serves each paired instance over plain HTTP on a real loopback
    /// `127.0.0.1:PORT` listener (mirroring `crate::test_harness::spawn_test_app`'s
    /// own plain-HTTP serving) with `domain` set to that instance's own
    /// bound address. Without this constructor, two such instances'
    /// `https://127.0.0.1:PORT/...` URLs would attempt a real TLS handshake
    /// against a plain-HTTP server and fail before any application-level
    /// federation logic ever ran. This is a narrow, explicit, opt-in escape
    /// hatch for that one test-harness reachability problem — never used by
    /// `crate::bootstrap::bootstrap`'s production wiring, which continues to
    /// build a client via [`Self::new`].
    pub fn insecure_loopback() -> Self {
        Self {
            client: reqwest::Client::new(),
            downgrade_https_to_http: true,
        }
    }

    /// Returns `url` unchanged unless [`Self::insecure_loopback`] built this
    /// client, in which case a `https://` prefix is rewritten to `http://`
    /// (any other scheme, or a URL already `http://`, passes through
    /// unchanged).
    fn effective_url<'a>(&self, url: &'a str) -> std::borrow::Cow<'a, str> {
        if self.downgrade_https_to_http
            && let Some(rest) = url.strip_prefix("https://")
        {
            return std::borrow::Cow::Owned(format!("http://{rest}"));
        }
        std::borrow::Cow::Borrowed(url)
    }
}

impl Default for ReqwestFederationHttpClient {
    fn default() -> Self {
        Self::new()
    }
}

impl FederationHttpClient for ReqwestFederationHttpClient {
    async fn send(&self, req: OutboundRequest) -> Result<HttpResponse, AppError> {
        let url = self.effective_url(&req.url).into_owned();
        let mut builder = self.client.request(req.method, url).headers(req.headers);
        if let Some(body) = req.body {
            builder = builder.body(body);
        }
        let response = builder
            .send()
            .await
            .map_err(|source| AppError::server(StatusCode::BAD_GATEWAY, source))?;
        response_from_reqwest(response).await
    }

    async fn fetch(
        &self,
        url: &str,
        _signed_as: Option<&Handle>,
    ) -> Result<HttpResponse, AppError> {
        // `_signed_as`: see this trait method's doc comment -- not yet acted
        // on, no `RequestSigner` exists in this task's boundary.
        //
        // `Accept: application/activity+json` (task 6.4, discovered via
        // `tests/federation_pair_it.rs`'s own real cross-instance round
        // trip, Requirement 13.3): this crate's own AP GET content
        // negotiation (`crate::federation::endpoints::ap_get`'s own doc
        // comment, "An absent `Accept` header is treated as 'not requesting
        // AP'") treats a request with no `Accept` header the same as one
        // requesting a non-AP representation, returning `406`. Every prior
        // task exercising `fetch` did so either against
        // `MockFederationHttpClient` (headers irrelevant) or against a
        // deliberately-unreachable host (the request never actually
        // completes), so this real, latent gap was never previously
        // exercised. A remote `keyId`/actor document fetch is always an
        // ActivityPub GET, so this is unconditional -- not specific to
        // `insecure_loopback`, and matches how a genuine remote federation
        // fetch already needs to behave against any AP-content-negotiating
        // instance (this one's own `ap_get.rs`, and every other
        // ActivityPub-conformant implementation).
        let response = self
            .client
            .get(self.effective_url(url).into_owned())
            .header(axum::http::header::ACCEPT, ACTIVITYPUB_ACCEPT_HEADER_VALUE)
            .send()
            .await
            .map_err(|source| AppError::server(StatusCode::BAD_GATEWAY, source))?;
        response_from_reqwest(response).await
    }
}

/// Converts a `reqwest::Response` into this module's [`HttpResponse`]
/// shape, consuming the body. Shared by both [`FederationHttpClient`]
/// methods on [`ReqwestFederationHttpClient`].
async fn response_from_reqwest(response: reqwest::Response) -> Result<HttpResponse, AppError> {
    let status = response.status();
    let headers = response.headers().clone();
    let body = response
        .bytes()
        .await
        .map_err(|source| AppError::server(StatusCode::BAD_GATEWAY, source))?
        .to_vec();
    Ok(HttpResponse {
        status,
        headers,
        body,
    })
}

/// A single queued outcome for [`MockFederationHttpClient`]: either a
/// canned response or a canned failure.
enum QueuedOutcome {
    Response(HttpResponse),
    Error(StatusCode, String),
}

/// Deterministic, in-memory [`FederationHttpClient`] implementation for
/// tests (Requirement 2.7's "テストでモック実装へ差し替えられるようにする"):
/// `send`/`fetch` return pre-queued outcomes (FIFO, one per call) instead of
/// making any network call, and every call is recorded for assertions —
/// e.g. asserting the exact `OutboundRequest` a `RequestSigner`-produced
/// call carried, once that task exists.
///
/// "Deterministic" here means: given the same sequence of
/// `queue_send_*`/`queue_fetch_*` calls, `send`/`fetch` always return the
/// same outcomes in the same order, with no dependency on wall-clock time,
/// randomness, or an actual network — mirroring
/// `FixedSigningKeyProvider`/`SeededRng`'s established mocking discipline
/// elsewhere in this crate.
#[derive(Default)]
pub struct MockFederationHttpClient {
    state: Mutex<MockState>,
}

#[derive(Default)]
struct MockState {
    send_outcomes: VecDeque<QueuedOutcome>,
    fetch_outcomes: VecDeque<QueuedOutcome>,
    sent_requests: Vec<OutboundRequest>,
    fetched_urls: Vec<(String, Option<Handle>)>,
}

impl MockFederationHttpClient {
    /// Builds a mock with no queued outcomes and no recorded calls yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues `response` to be returned by the next [`FederationHttpClient::send`]
    /// call (FIFO).
    pub fn queue_send_response(&self, response: HttpResponse) {
        self.state
            .lock()
            .expect("MockFederationHttpClient mutex must not be poisoned")
            .send_outcomes
            .push_back(QueuedOutcome::Response(response));
    }

    /// Queues a failure to be returned by the next [`FederationHttpClient::send`]
    /// call (FIFO).
    pub fn queue_send_error(&self, status: StatusCode, message: impl Into<String>) {
        self.state
            .lock()
            .expect("MockFederationHttpClient mutex must not be poisoned")
            .send_outcomes
            .push_back(QueuedOutcome::Error(status, message.into()));
    }

    /// Queues `response` to be returned by the next [`FederationHttpClient::fetch`]
    /// call (FIFO).
    pub fn queue_fetch_response(&self, response: HttpResponse) {
        self.state
            .lock()
            .expect("MockFederationHttpClient mutex must not be poisoned")
            .fetch_outcomes
            .push_back(QueuedOutcome::Response(response));
    }

    /// Queues a failure to be returned by the next [`FederationHttpClient::fetch`]
    /// call (FIFO).
    pub fn queue_fetch_error(&self, status: StatusCode, message: impl Into<String>) {
        self.state
            .lock()
            .expect("MockFederationHttpClient mutex must not be poisoned")
            .fetch_outcomes
            .push_back(QueuedOutcome::Error(status, message.into()));
    }

    /// Every `OutboundRequest` passed to [`FederationHttpClient::send`] so
    /// far, in call order.
    pub fn sent_requests(&self) -> Vec<OutboundRequest> {
        self.state
            .lock()
            .expect("MockFederationHttpClient mutex must not be poisoned")
            .sent_requests
            .clone()
    }

    /// Every `(url, signed_as)` pair passed to
    /// [`FederationHttpClient::fetch`] so far, in call order.
    pub fn fetched_urls(&self) -> Vec<(String, Option<Handle>)> {
        self.state
            .lock()
            .expect("MockFederationHttpClient mutex must not be poisoned")
            .fetched_urls
            .clone()
    }
}

impl FederationHttpClient for MockFederationHttpClient {
    async fn send(&self, req: OutboundRequest) -> Result<HttpResponse, AppError> {
        let mut state = self
            .state
            .lock()
            .expect("MockFederationHttpClient mutex must not be poisoned");
        state.sent_requests.push(req);
        match state.send_outcomes.pop_front() {
            Some(QueuedOutcome::Response(response)) => Ok(response),
            Some(QueuedOutcome::Error(status, message)) => Err(AppError::server(status, message)),
            None => Err(AppError::server(
                StatusCode::INTERNAL_SERVER_ERROR,
                "MockFederationHttpClient: no queued send() outcome",
            )),
        }
    }

    async fn fetch(&self, url: &str, signed_as: Option<&Handle>) -> Result<HttpResponse, AppError> {
        let mut state = self
            .state
            .lock()
            .expect("MockFederationHttpClient mutex must not be poisoned");
        state
            .fetched_urls
            .push((url.to_string(), signed_as.cloned()));
        match state.fetch_outcomes.pop_front() {
            Some(QueuedOutcome::Response(response)) => Ok(response),
            Some(QueuedOutcome::Error(status, message)) => Err(AppError::server(status, message)),
            None => Err(AppError::server(
                StatusCode::INTERNAL_SERVER_ERROR,
                "MockFederationHttpClient: no queued fetch() outcome",
            )),
        }
    }
}
