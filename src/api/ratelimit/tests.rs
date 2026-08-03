//! Integration-style tests for the `RateLimit` tower layer (task 6.3),
//! driving real requests through a minimal test-only axum router via
//! `tower::ServiceExt::oneshot` (per the task brief: not a
//! `tests/*_it.rs` full-router integration test, since this layer is not
//! wired into the production router yet — that is task 7.1's job).
//!
//! Requirements exercised:
//! - 8.1: every response (normal and over-limit) carries
//!   `X-RateLimit-Limit`/`X-RateLimit-Remaining`/`X-RateLimit-Reset`.
//! - 8.2: the window/reset boundary is computed from an injected `Clock`,
//!   never from wall-clock time — proven with an exact-value assertion
//!   against a `FixedClock`, and again with a custom stepping `Clock` that
//!   proves the window only rolls over when the *injected* clock crosses
//!   the boundary (not merely that some header exists).
//! - 8.3: an over-limit request gets a genuine 429 with a Mastodon-shaped
//!   JSON body (reusing `crate::api::error::mastodon_error_body`), and
//!   never reaches the inner handler.
//! - 8.4: `Remaining` visibly decrements across requests, proving the
//!   header values are live counters, not a static, always-present-but-
//!   meaningless shape.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use serde_json::Value;
use time::macros::datetime;
use tower::ServiceExt;

use super::*;
use crate::runtime::{Clock, FixedClock};

/// Collects a `Response`'s JSON body into a `serde_json::Value`, mirroring
/// `crate::api::error::tests`'s `body_json` helper.
async fn body_json(response: Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("test response body should be readable");
    serde_json::from_slice(&bytes).expect("test response body should be valid JSON")
}

/// Shared state for the test-only handler: counts how many times it was
/// actually invoked, so a test can prove an over-limit request never
/// reached it (Requirement 8.3).
type Calls = Arc<AtomicUsize>;

async fn probe(State(calls): State<Calls>) -> StatusCode {
    calls.fetch_add(1, Ordering::SeqCst);
    StatusCode::OK
}

/// Builds a minimal test-only router: one `GET /probe` route, `layer`
/// attached exactly the way `src/server.rs`'s `TraceLayer` is attached to
/// the production router (`.layer(...)` on an axum `Router`).
fn test_router(layer: RateLimitLayer, calls: Calls) -> Router {
    Router::new()
        .route("/probe", get(probe))
        .layer(layer)
        .with_state(calls)
}

fn probe_request() -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri("/probe")
        .body(Body::empty())
        .expect("valid test request")
}

fn header_str<'a>(response: &'a Response, name: &str) -> &'a str {
    response
        .headers()
        .get(name)
        .unwrap_or_else(|| panic!("expected `{name}` header to be present"))
        .to_str()
        .expect("header value should be ASCII")
}

// ---------------------------------------------------------------------
// 8.1 / 8.2: normal responses carry headers computed from the Clock
// ---------------------------------------------------------------------

#[tokio::test]
async fn normal_response_carries_ratelimit_headers_with_exact_clock_derived_reset() {
    let fixed = datetime!(2026-07-13 12:00:00 UTC);
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(fixed));
    let window = time::Duration::seconds(60);
    let policy = RateLimitPolicy::new(5, window);
    let expected_reset = (fixed + window).unix_timestamp();

    let calls = Arc::new(AtomicUsize::new(0));
    let router = test_router(rate_limit_layer(clock, policy), calls.clone());

    let response = router
        .oneshot(probe_request())
        .await
        .expect("no infallible error");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(header_str(&response, "x-ratelimit-limit"), "5");
    assert_eq!(header_str(&response, "x-ratelimit-remaining"), "4");
    assert_eq!(
        header_str(&response, "x-ratelimit-reset"),
        expected_reset.to_string(),
        "X-RateLimit-Reset must equal FixedClock's now() + window, exactly \
         (Requirement 8.2: computed from the Clock boundary, not wall-clock)"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "inner handler must run for an admitted request"
    );
}

#[tokio::test]
async fn remaining_decrements_across_repeated_requests_in_the_same_window() {
    let fixed = datetime!(2026-01-01 00:00:00 UTC);
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(fixed));
    let policy = RateLimitPolicy::new(5, time::Duration::seconds(3600));

    let calls = Arc::new(AtomicUsize::new(0));
    let layer = rate_limit_layer(clock, policy);
    let router = test_router(layer, calls);

    for expected_remaining in ["4", "3", "2"] {
        let response = router
            .clone()
            .oneshot(probe_request())
            .await
            .expect("no infallible error");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            header_str(&response, "x-ratelimit-remaining"),
            expected_remaining
        );
    }
}

// ---------------------------------------------------------------------
// 8.3: over-limit -> Mastodon-compatible 429, inner handler never reached
// ---------------------------------------------------------------------

#[tokio::test]
async fn over_limit_request_returns_mastodon_compatible_429_and_skips_inner_handler() {
    let fixed = datetime!(2026-01-01 00:00:00 UTC);
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(fixed));
    let policy = RateLimitPolicy::new(1, time::Duration::seconds(3600));

    let calls = Arc::new(AtomicUsize::new(0));
    let router = test_router(rate_limit_layer(clock, policy), calls.clone());

    let first = router
        .clone()
        .oneshot(probe_request())
        .await
        .expect("no infallible error");
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(header_str(&first, "x-ratelimit-remaining"), "0");

    let second = router
        .clone()
        .oneshot(probe_request())
        .await
        .expect("no infallible error");

    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        header_str(&second, "x-ratelimit-limit"),
        "1",
        "Requirement 8.1: the over-limit response is itself a rate-limited \
         response and must carry the same headers"
    );
    assert_eq!(header_str(&second, "x-ratelimit-remaining"), "0");

    let body = body_json(second).await;
    assert_eq!(
        body["error"], "Too many requests",
        "must reuse crate::api::error::mastodon_error_body's canonical \
         429 label, not a hand-rolled body shape"
    );
    assert!(
        body.get("error_description").is_some(),
        "the over-limit AppError's public_message should surface as error_description"
    );

    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "an over-limit request must never reach the inner handler"
    );
}

// ---------------------------------------------------------------------
// 8.2: window rollover is driven strictly by the injected Clock
// ---------------------------------------------------------------------

/// A `Clock` that returns the next value from a fixed, pre-scripted
/// sequence on every call (repeating the last entry once exhausted), so a
/// test can deterministically control what each `clock.now()` call
/// observes without any real time passing. Proves Requirement 8.2's
/// "computed from the Clock boundary" holds across multiple calls, not
/// just once at construction.
struct SteppingClock {
    times: Vec<time::OffsetDateTime>,
    index: std::sync::atomic::AtomicUsize,
}

impl SteppingClock {
    fn new(times: Vec<time::OffsetDateTime>) -> Self {
        assert!(
            !times.is_empty(),
            "SteppingClock needs at least one scripted time"
        );
        SteppingClock {
            times,
            index: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

impl Clock for SteppingClock {
    fn now(&self) -> time::OffsetDateTime {
        let i = self.index.fetch_add(1, Ordering::SeqCst);
        self.times[i.min(self.times.len() - 1)]
    }
}

#[tokio::test]
async fn window_only_rolls_over_once_the_injected_clock_crosses_the_boundary() {
    let t0 = datetime!(2026-01-01 00:00:00 UTC);
    let window = time::Duration::seconds(60);
    let past_boundary = t0 + window + time::Duration::seconds(1);

    // Call sequence: [0] RateLimitLayer::new's initial window start,
    // [1] request 1 (admitted, exhausts limit=1), [2] request 2 (still at
    // t0 -> rejected, window has NOT elapsed), [3] request 3 (at
    // past_boundary -> window rolls over -> admitted again).
    let clock: Arc<dyn Clock> = Arc::new(SteppingClock::new(vec![t0, t0, t0, past_boundary]));
    let policy = RateLimitPolicy::new(1, window);

    let calls = Arc::new(AtomicUsize::new(0));
    let router = test_router(rate_limit_layer(clock, policy), calls.clone());

    let first = router
        .clone()
        .oneshot(probe_request())
        .await
        .expect("no infallible error");
    assert_eq!(first.status(), StatusCode::OK);

    let second = router
        .clone()
        .oneshot(probe_request())
        .await
        .expect("no infallible error");
    assert_eq!(
        second.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "still within the same Clock-observed window, so the limit stays exhausted"
    );

    let third = router
        .clone()
        .oneshot(probe_request())
        .await
        .expect("no infallible error");
    assert_eq!(
        third.status(),
        StatusCode::OK,
        "the Clock reported a time past the window boundary, so the counter \
         must have reset (Requirement 8.2: driven by Clock, not wall-clock, \
         and not by request count alone)"
    );

    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "exactly the two admitted requests (first, third) should reach the inner handler"
    );
}

// ---------------------------------------------------------------------
// 8.4: header shape is maintained even for a deliberately loose/generous
// limit, not just for a tight one that's likely to actually trip
// ---------------------------------------------------------------------

#[tokio::test]
async fn generous_single_owner_style_limit_still_emits_full_header_shape() {
    let fixed = datetime!(2026-01-01 00:00:00 UTC);
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(fixed));
    // A deliberately loose limit, as Requirement 8.4 sanctions for a
    // single-owner deployment -- still must carry the full header shape.
    let policy = RateLimitPolicy::new(10_000, time::Duration::seconds(60));

    let calls = Arc::new(AtomicUsize::new(0));
    let router = test_router(rate_limit_layer(clock, policy), calls);

    let response = router
        .oneshot(probe_request())
        .await
        .expect("no infallible error");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(header_str(&response, "x-ratelimit-limit"), "10000");
    assert_eq!(header_str(&response, "x-ratelimit-remaining"), "9999");
    // Just needs to parse as a sane integer -- exact value already proven
    // by the dedicated exact-reset test above.
    header_str(&response, "x-ratelimit-reset")
        .parse::<i64>()
        .expect("reset header must still be a decimal integer even for a loose policy");
}
