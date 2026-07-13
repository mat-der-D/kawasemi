//! `X-RateLimit-*` header attachment + over-limit response tower layer
//! (api-foundation `RateLimit` boundary, task 6.3).
//!
//! Scope: this module owns exactly one thing — a genuine `tower::Layer`/
//! `tower::Service` ([`RateLimitLayer`] / [`RateLimitService`]) that (a)
//! attaches `X-RateLimit-Limit`/`X-RateLimit-Remaining`/`X-RateLimit-Reset`
//! headers to every response it lets through (Requirement 8.1), (b)
//! computes the window/reset time from a `crate::runtime::Clock`, never
//! from wall-clock time read directly (Requirement 8.2), and (c) once the
//! configured window is exhausted, short-circuits to a Mastodon-compatible
//! 429 response instead of calling the inner service (Requirement 8.3),
//! while still emitting the same three headers on that 429 (Requirement
//! 8.1's "レート制限の対象応答" covers the over-limit response too).
//!
//! Wiring this layer into the live production router is task 7.1's job
//! (`_Boundary: ApiModule wiring`); this module deliberately stops at the
//! layer/service pair and does not touch `src/server.rs`,
//! `src/bootstrap.rs`, or `src/state.rs`. `tests.rs` proves the layer's
//! behavior by driving real requests through a minimal, test-only axum
//! router via `tower::ServiceExt::oneshot` (not a `tests/*_it.rs` full
//! production-router integration test, since nothing wires this layer into
//! that router yet).
//!
//! ## Design notes: reconciling design.md's sketch with what this needs
//!
//! design.md's illustrative Service Interface for this component is:
//! ```ignore
//! pub fn rate_limit_layer(clock: Arc<dyn Clock>, policy: RateLimitPolicy) -> RateLimitLayer;
//! ```
//! The function name and signature are kept exactly as sketched
//! ([`rate_limit_layer`]). `RateLimitPolicy`'s fields are not specified by
//! design.md, so this module invents the minimal shape a fixed-window
//! counter needs: `limit: u32` (requests admitted per window) and
//! `window: time::Duration` (window length). Requirement 8.4 explicitly
//! sanctions a loose real algorithm/limit ("一人鯖前提" — a single-owner
//! server is unlikely to ever legitimately hit any reasonable limit), so a
//! single shared fixed-window counter (not per-client/per-token keyed) is
//! sufficient here — there is exactly one caller-class (the server's own
//! owner and whatever standard client they're using) worth rate-limiting
//! the *shape* of, not the *precision* of.
//!
//! ## `X-RateLimit-Reset` value convention
//!
//! Real Mastodon renders `X-RateLimit-Reset` as an ISO 8601 timestamp
//! string. Producing that would need `time`'s `formatting`/`parsing`
//! feature, which is not currently enabled in `Cargo.toml` (only `macros`,
//! dev-only, is). Requirement 8.4 says the header *shape* and *computation
//! convention* — not necessarily the exact Mastodon string encoding — must
//! be consistently maintained even though the real limit values are loose;
//! this module renders `X-RateLimit-Reset` as a decimal Unix-epoch-seconds
//! string instead (via [`time::OffsetDateTime::unix_timestamp`], already
//! available with no new feature/dependency), which is an equally
//! unambiguous, widely-used rate-limit-header convention (e.g. GitHub's
//! API) and is trivially exact-value-testable. `X-RateLimit-Limit` /
//! `X-RateLimit-Remaining` are decimal integers either way, matching real
//! Mastodon already. Header *names* are emitted in the canonical
//! lowercase `http`-crate form (`x-ratelimit-limit`, ...); HTTP header
//! names are case-insensitive on the wire (RFC 9110 §5.1), so this is
//! wire-compatible with clients matching `X-RateLimit-Limit` by any casing.
//!
//! ## Sharing state across `tower::Layer::layer` clones
//!
//! [`RateLimitLayer`] holds its window counter behind `Arc<Mutex<Window>>`.
//! axum clones the layered service per accepted connection/request, so the
//! counter must be shared (not re-initialized per clone) for the limit to
//! mean anything; [`RateLimitLayer::layer`] clones the `Arc`, not the
//! `Mutex`'s contents, so every [`RateLimitService`] produced from one
//! [`RateLimitLayer`] instance observes and mutates the same window.

#[cfg(test)]
mod tests;

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use axum::body::Body;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Request, StatusCode};
use axum::response::Response;
use time::OffsetDateTime;
use tower_layer::Layer;
use tower_service::Service;

use crate::api::error::mastodon_error_body;
use crate::error::AppError;
use crate::runtime::Clock;

/// `X-RateLimit-Limit` header name, lowercase per the `http` crate's
/// canonical representation (see module doc's casing note).
const LIMIT_HEADER: HeaderName = HeaderName::from_static("x-ratelimit-limit");
/// `X-RateLimit-Remaining` header name.
const REMAINING_HEADER: HeaderName = HeaderName::from_static("x-ratelimit-remaining");
/// `X-RateLimit-Reset` header name.
const RESET_HEADER: HeaderName = HeaderName::from_static("x-ratelimit-reset");

/// Caller-facing message for the over-limit `AppError` (Requirement 8.3).
/// Rendered as `error_description` alongside [`mastodon_error_body`]'s
/// canonical `error: "Too many requests"` label for
/// `StatusCode::TOO_MANY_REQUESTS` (`crate::api::error::mastodon_error_label`).
const OVER_LIMIT_MESSAGE: &str = "rate limit exceeded; retry once the window resets";

/// Fixed-window rate-limit policy: `limit` requests are admitted per
/// `window`-length interval (Requirement 8.4's "実値は緩くてよい" —
/// call sites are free to choose a generous `limit`/`window` for a
/// single-owner deployment; only this module's header shape and
/// reset-computation convention are load-bearing).
#[derive(Debug, Clone, Copy)]
pub struct RateLimitPolicy {
    pub limit: u32,
    pub window: time::Duration,
}

impl RateLimitPolicy {
    pub fn new(limit: u32, window: time::Duration) -> Self {
        Self { limit, window }
    }
}

/// The shared, mutable fixed-window counter state (Requirement 8.2:
/// advanced only by comparing `Clock`-sourced instants, never wall-clock
/// time read directly).
#[derive(Debug)]
struct Window {
    start: OffsetDateTime,
    count: u32,
}

/// One request's admission outcome plus the header values every response
/// (admitted or rejected) carries (Requirement 8.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Decision {
    allowed: bool,
    limit: u32,
    remaining: u32,
    reset_unix: i64,
}

/// Advances `window` past its boundary (Clock-derived, Requirement 8.2) if
/// `now` has reached or passed it, then admits or rejects the current
/// request against `policy.limit`.
fn evaluate(window: &Mutex<Window>, now: OffsetDateTime, policy: RateLimitPolicy) -> Decision {
    let mut guard = window.lock().expect("rate limit window mutex poisoned");

    if now >= guard.start + policy.window {
        guard.start = now;
        guard.count = 0;
    }

    let reset_unix = (guard.start + policy.window).unix_timestamp();

    if guard.count >= policy.limit {
        Decision {
            allowed: false,
            limit: policy.limit,
            remaining: 0,
            reset_unix,
        }
    } else {
        guard.count += 1;
        Decision {
            allowed: true,
            limit: policy.limit,
            remaining: policy.limit - guard.count,
            reset_unix,
        }
    }
}

/// A decimal ASCII digit string is always a valid [`HeaderValue`]; this
/// exists purely so [`apply_headers`] doesn't repeat the `expect` message.
fn digits_header_value(value: impl ToString) -> HeaderValue {
    HeaderValue::from_str(&value.to_string())
        .expect("a decimal-digit string is always a valid HeaderValue")
}

/// Attaches [`Decision`]'s three headers (Requirement 8.1) to `headers`,
/// overwriting any pre-existing values of the same name.
fn apply_headers(headers: &mut HeaderMap, decision: Decision) {
    headers.insert(LIMIT_HEADER, digits_header_value(decision.limit));
    headers.insert(REMAINING_HEADER, digits_header_value(decision.remaining));
    headers.insert(RESET_HEADER, digits_header_value(decision.reset_unix));
}

/// Builds the Mastodon-compatible over-limit response (Requirement 8.3):
/// reuses `crate::error::AppError` (no new error type, consistent with
/// task 6.1's discipline) rendered through [`mastodon_error_body`] (task
/// 6.1's renderer, whose status→label table already covers
/// `StatusCode::TOO_MANY_REQUESTS`), with the same rate-limit headers as
/// any other response attached on top.
fn over_limit_response(decision: Decision) -> Response {
    let mut response = AppError::client(StatusCode::TOO_MANY_REQUESTS, OVER_LIMIT_MESSAGE)
        .into_response_with(mastodon_error_body);
    apply_headers(response.headers_mut(), decision);
    response
}

/// `tower::Layer` that produces [`RateLimitService`] (design.md's
/// `RateLimit` component). Construct via [`rate_limit_layer`].
#[derive(Clone)]
pub struct RateLimitLayer {
    clock: Arc<dyn Clock>,
    policy: RateLimitPolicy,
    window: Arc<Mutex<Window>>,
}

impl RateLimitLayer {
    /// Builds a layer whose window starts at `clock.now()` at construction
    /// time (Requirement 8.2: even this initial boundary is Clock-sourced,
    /// not `SystemTime`/`Instant` read directly).
    pub fn new(clock: Arc<dyn Clock>, policy: RateLimitPolicy) -> Self {
        let start = clock.now();
        RateLimitLayer {
            clock,
            policy,
            window: Arc::new(Mutex::new(Window { start, count: 0 })),
        }
    }
}

/// Builds a [`RateLimitLayer`] (design.md's sketched `RateLimit` Service
/// Interface, kept as-named: `rate_limit_layer(clock, policy) -> RateLimitLayer`).
pub fn rate_limit_layer(clock: Arc<dyn Clock>, policy: RateLimitPolicy) -> RateLimitLayer {
    RateLimitLayer::new(clock, policy)
}

impl<S> Layer<S> for RateLimitLayer {
    type Service = RateLimitService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RateLimitService {
            inner,
            clock: Arc::clone(&self.clock),
            policy: self.policy,
            window: Arc::clone(&self.window),
        }
    }
}

/// `tower::Service` wrapping an inner `axum`-compatible service: evaluates
/// the shared [`Window`] against the request's arrival time (via `clock`)
/// before deciding whether to call `inner` at all (Requirement 8.3: an
/// over-limit request never reaches `inner`).
#[derive(Clone)]
pub struct RateLimitService<S> {
    inner: S,
    clock: Arc<dyn Clock>,
    policy: RateLimitPolicy,
    window: Arc<Mutex<Window>>,
}

impl<S, E> Service<Request<Body>> for RateLimitService<S>
where
    S: Service<Request<Body>, Response = Response, Error = E> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = E;
    type Future = Pin<Box<dyn Future<Output = Result<Response, E>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let decision = evaluate(&self.window, self.clock.now(), self.policy);

        if !decision.allowed {
            let response = over_limit_response(decision);
            return Box::pin(async move { Ok(response) });
        }

        // Standard tower middleware technique for a `&mut self`-taking
        // `call`: clone `inner` (cheap — axum handlers/routers are built to
        // be `Clone`) so the boxed future can own it independently of
        // `self`, which the caller may reuse for the next request
        // immediately.
        let mut inner = self.inner.clone();
        let future = inner.call(req);
        Box::pin(async move {
            let mut response = future.await?;
            apply_headers(response.headers_mut(), decision);
            Ok(response)
        })
    }
}
