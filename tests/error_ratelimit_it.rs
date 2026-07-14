//! Error/rate-limit compatibility integration tests (task 9.4, `_Depends:
//! 7.1_`, design.md's File Structure Plan: `tests/error_ratelimit_it.rs`).
//!
//! Requirements exercised: 7.1, 7.2, 7.3, 7.4, 7.5, 8.1, 8.2, 8.3, 8.4.
//!
//! ## What is already proven elsewhere, and what this file adds
//! `src/api/error/tests.rs` (task 6.1) already proves, at the unit level,
//! the full status -> canonical-label table (422/401/403/404/429), the
//! `error`/`error_description` split, and that a `Server` (5xx) error's
//! rendered body never varies with `source` content. `src/api/ratelimit/tests.rs`
//! (task 6.3) already proves, driving a minimal test-only router via
//! `tower::ServiceExt::oneshot`, that headers are Clock-derived, decrement
//! per request, and that an over-limit request gets a genuine 429 and never
//! reaches the inner handler. `tests/api_foundation_wiring_it.rs` (task 7.1)
//! additionally proves — as a byproduct of proving the cross-cutting layers
//! are *wired* onto the real production router at all — one 422 case and
//! that `X-RateLimit-*` headers appear on one ordinary response.
//!
//! None of those prove the full status/body matrix (422/401/403/404/5xx) at
//! the HTTP level through a `spawn_test_app`-booted instance, nor that the
//! real, deployed rate-limit policy (`src/server.rs::build_router`'s fixed
//! window, not a test-only substitute policy) actually trips over HTTP and
//! renders the compatible over-limit response. This file is task 9.4's
//! dedicated integration layer closing that gap, mirroring
//! `tests/auth_scope_it.rs`'s (task 9.2) established precedent of being a
//! focused, exhaustive companion to a broader wiring test.
//!
//! ## Test-only routes for 404/5xx, mirroring established precedent
//! No production endpoint in this spec's boundary naturally produces a
//! domain "not found" (404) or a genuine internal/system-classified (5xx)
//! error yet (every current OAuth endpoint's failure paths are 400/401/403,
//! per `src/oauth/*_endpoint.rs`). Rather than fabricate a contrived trigger
//! for a case that wouldn't occur in the real app, this file mounts two tiny
//! test-only handlers directly returning `AppError::client(NOT_FOUND, ..)`
//! / `AppError::server(INTERNAL_SERVER_ERROR, ..)`, mirroring
//! `src/server/tests.rs`'s own already-reviewed `app_error_handler`/
//! `APP_ERROR_PATH` precedent (task 9.2) for proving the 5xx secrecy
//! contract through a real HTTP round trip — the task brief explicitly
//! sanctions this ("a tiny test-only route/handler that deliberately
//! returns a system-classified AppError to prove the HTTP-level contract").
//! 401/403 reuse `tests/auth_scope_it.rs`'s own established
//! `RequiredActor`/`require_scope`-protected test-route pattern instead of
//! inventing a third way to trigger them. 422 reuses the real, mounted
//! `POST /api/v1/apps` endpoint's existing validation path (same trigger
//! `tests/api_foundation_wiring_it.rs` already uses).
//!
//! ## No HTTP client dependency: raw sockets, mirroring established precedent
//! This crate has no HTTP client dependency (`Cargo.toml`). This file
//! duplicates `tests/api_foundation_wiring_it.rs`'s/`tests/auth_scope_it.rs`'s
//! own `RawResponse`/`raw_request`/`parse_response` helpers verbatim (each
//! `tests/*.rs` file is a separate compiled binary with no shared module).
//!
//! ## Over-limit test drives the real production policy, not a test-only one
//! The over-limit test (`rate_limit_exceeded_...`) issues repeated real HTTP
//! requests against `app.address` — the exact `crate::server::build_router`
//! instance `spawn_test_app` boots, carrying `src/server.rs`'s real
//! `RATE_LIMIT_PER_WINDOW`/`RATE_LIMIT_WINDOW` policy — rather than mounting
//! a second router with a test-only low-limit policy (which would just
//! re-exercise `src/api/ratelimit/tests.rs`'s already-proven oneshot-level
//! mechanism over TCP instead of proving anything about the actual deployed
//! wiring). This is deterministic, not flaky: `spawn_test_app` always builds
//! its `RuntimeContext` via `RuntimeContext::deterministic`, whose `clock`
//! is a `FixedClock` that never advances (`src/runtime.rs`), so the shared
//! fixed window never rolls over mid-test and the counter monotonically
//! exhausts after exactly the policy's `limit` admitted requests.

use std::collections::HashMap;
use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use axum::routing::get;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor};
use kawasemi::domain::Id;
use kawasemi::error::{AppError, GENERIC_SERVER_MESSAGE};
use kawasemi::oauth::middleware::RequiredActor;
use kawasemi::oauth::model::ScopeSet as PlaceholderScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::oauth::{ScopeSet, require_scope};
use kawasemi::state::AppState;
use kawasemi::test_harness::{TestApp, spawn_test_app};

const REDIRECT_URI: &str = "https://client.example/callback";

/// Distinctive marker so if the 5xx handler's internal `source` detail ever
/// leaked into the HTTP response body, this test would unmistakably catch
/// it (mirrors `src/error/tests.rs`'s/`src/server/tests.rs`'s own
/// `server_error_body_never_contains_source_detail`/
/// `APP_ERROR_SOURCE_MARKER` technique).
const SERVER_ERROR_SOURCE_MARKER: &str = "test-9-4-internal-diagnostic-marker-53107";

/// Safety cap on how many `GET /health` requests the over-limit test will
/// send before giving up — well above `src/server.rs`'s real
/// `RATE_LIMIT_PER_WINDOW` (300 at time of writing) so the test still finds
/// the trip point even if that constant changes, without looping forever if
/// the layer were ever (incorrectly) not applied at all.
const OVER_LIMIT_SAFETY_CAP: u32 = 1_000;

// ---- raw HTTP plumbing (duplicated from tests/api_foundation_wiring_it.rs;
// see this file's module doc comment for why) ----

#[derive(Debug)]
struct RawResponse {
    status: u16,
    headers: HashMap<String, String>,
    body: String,
}

async fn raw_request(
    addr: std::net::SocketAddr,
    method: &str,
    path: &str,
    content_type: Option<&str>,
    extra_headers: &[(&str, &str)],
    body: &str,
) -> RawResponse {
    let mut stream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr))
        .await
        .expect("connecting to the test listener must not time out")
        .expect("connect");

    let mut request =
        format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n");
    if let Some(ct) = content_type {
        request.push_str(&format!("Content-Type: {ct}\r\n"));
    }
    for (name, value) in extra_headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    request.push_str(body);

    stream
        .write_all(request.as_bytes())
        .await
        .expect("write request");

    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut buf))
        .await
        .expect("read must not time out")
        .expect("read response");

    parse_response(&String::from_utf8_lossy(&buf))
}

fn parse_response(raw: &str) -> RawResponse {
    let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((raw, ""));
    let mut lines = head.lines();
    let status_line = lines.next().unwrap_or("");
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    let mut headers = HashMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    RawResponse {
        status,
        headers,
        body: body.to_string(),
    }
}

fn assert_mastodon_error_shape(response: &RawResponse, expected_error: &str) {
    let body: Value =
        serde_json::from_str(&response.body).expect("error response must be valid JSON");
    assert_eq!(
        body.get("error").and_then(Value::as_str),
        Some(expected_error),
        "expected the canonical Mastodon-compatible error label, got: {body}"
    );
}

// ---- test-only routes (mirrors tests/auth_scope_it.rs's/
// src/server/tests.rs's own established precedents) ----

/// `GET /__error_rl_whoami__`: mandatory auth, no scope requirement beyond
/// authentication itself (Requirements 7.1, 7.3 — the 401 case).
async fn whoami(RequiredActor(_ctx): RequiredActor) -> StatusCode {
    StatusCode::OK
}

/// `GET /__error_rl_requires_write_statuses__`: mandatory auth plus a fixed
/// `write:statuses` scope requirement (Requirements 7.1, 7.3 — the 403
/// case), mirroring `tests/auth_scope_it.rs`'s identical route.
async fn requires_write_statuses(
    RequiredActor(ctx): RequiredActor,
) -> Result<StatusCode, AppError> {
    let required = ScopeSet::parse("write:statuses").expect("\"write:statuses\" is a valid scope");
    require_scope(&ctx, &required)?;
    Ok(StatusCode::OK)
}

/// `GET /__error_rl_missing__`: deliberately always returns a domain
/// "not found" `AppError` (Requirements 7.1, 7.3 — the 404 case). No
/// authentication required; the case under test is the status/body mapping,
/// not auth.
async fn missing_resource() -> AppError {
    AppError::client(StatusCode::NOT_FOUND, "resource not found")
}

/// `GET /__error_rl_5xx__`: deliberately always returns a genuine
/// `ErrorKind::Server` `AppError` carrying [`SERVER_ERROR_SOURCE_MARKER`] as
/// its `source` (Requirement 7.5 — internal detail must never reach the
/// response body). Mirrors `src/server/tests.rs`'s own already-reviewed
/// `app_error_handler` precedent (task 9.2).
async fn system_error() -> AppError {
    AppError::server(
        StatusCode::INTERNAL_SERVER_ERROR,
        std::io::Error::other(SERVER_ERROR_SOURCE_MARKER),
    )
}

/// Merges this file's four test-only routes onto the real production
/// `kawasemi::server::router()` (never `build_router`, so this file's own
/// listener carries neither the production `TraceLayer` nor rate-limit
/// layer — those cross-cutting concerns are exercised separately against
/// `app.address` itself, see [`rate_limit_normal_response_carries_headers`]/
/// [`rate_limit_exceeded_returns_mastodon_compatible_429_with_headers`]).
fn test_only_router(state: AppState) -> Router {
    kawasemi::server::router()
        .route("/__error_rl_whoami__", get(whoami))
        .route(
            "/__error_rl_requires_write_statuses__",
            get(requires_write_statuses),
        )
        .route("/__error_rl_missing__", get(missing_resource))
        .route("/__error_rl_5xx__", get(system_error))
        .with_state(state)
}

/// Serves [`test_only_router`] on its own freshly bound ephemeral listener
/// (mirroring `tests/auth_scope_it.rs`'s identical `spawn_protected_router`)
/// and returns its address.
async fn spawn_test_only_router(app: &TestApp) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    let router = test_only_router(app.state.clone());
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    addr
}

// ---- fixtures (duplicated from tests/auth_scope_it.rs; see this file's
// module doc comment for why) ----

/// Registers a real OAuth app via the real, mounted `POST /api/v1/apps`
/// endpoint, returning `(app_id, client_id, client_secret)`.
async fn register_app_via_http(app: &TestApp, client_name: &str) -> (Id, String, String) {
    let body = json!({
        "client_name": client_name,
        "redirect_uris": [REDIRECT_URI],
        "scopes": "read write",
    })
    .to_string();
    let response = raw_request(
        app.address,
        "POST",
        "/api/v1/apps",
        Some("application/json"),
        &[],
        &body,
    )
    .await;
    assert_eq!(
        response.status, 200,
        "app registration must succeed, got: {response:?}"
    );
    let registered: Value =
        serde_json::from_str(&response.body).expect("registration response must be valid JSON");
    let app_id = Id::from_i64(
        registered["id"]
            .as_str()
            .and_then(|raw| raw.parse::<i64>().ok())
            .expect("registration response must carry a decimal-string id"),
    );
    let client_id = registered["client_id"]
        .as_str()
        .expect("registration response must carry client_id")
        .to_string();
    let client_secret = registered["client_secret"]
        .as_str()
        .expect("registration response must carry client_secret")
        .to_string();
    (app_id, client_id, client_secret)
}

/// Creates the sole owner fixture and one actor belonging to it, returning
/// `(owner_id, actor_id)`.
async fn create_owner_with_actor(app: &TestApp, handle: &str) -> (Id, Id) {
    let now = app.runtime.clock.now();
    let owner_id = app.runtime.ids.next_id();
    create_owner(&app.pool, owner_id, now)
        .await
        .expect("creating the owner fixture must succeed");

    let actor = app
        .actor
        .actor_service()
        .create_actor(NewActor {
            owner_id,
            handle: Handle::new(handle).expect("valid handle"),
            actor_type: ActorType::Person,
            display_name: "Error/RateLimit Test Actor".to_string(),
            summary: "an error/rate-limit integration test fixture".to_string(),
        })
        .await
        .expect("creating the owner's actor fixture must succeed");

    (owner_id, actor.id)
}

/// Mints a real, persisted access token bound to `actor_id` with `scopes`,
/// returning its plaintext bearer value.
async fn issue_token(app: &TestApp, oauth_app_id: Id, actor_id: Id, scopes: &[&str]) -> String {
    let now = app.runtime.clock.now();
    let issued = token_repository::issue_token(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        app.state.oauth().token_hash_key(),
        now,
        NewAccessToken {
            app_id: oauth_app_id,
            actor_id,
            scopes: PlaceholderScopeSet::new(scopes.iter().copied()),
        },
    )
    .await
    .expect("issuing a real access token must succeed");
    issued.plaintext.expose_secret().clone()
}

// ---- (1) 422 validation error: body shape + status (Requirements 7.1,
// 7.2, 7.3) ----

#[tokio::test]
async fn validation_error_returns_422_with_mastodon_compatible_body() {
    let app = spawn_test_app().await;

    // Blank client_name fails registration's required-field validation
    // (same trigger `tests/api_foundation_wiring_it.rs` already uses).
    let bad_body = json!({
        "client_name": "",
        "redirect_uris": [REDIRECT_URI],
        "scopes": "read"
    })
    .to_string();
    let response = raw_request(
        app.address,
        "POST",
        "/api/v1/apps",
        Some("application/json"),
        &[],
        &bad_body,
    )
    .await;

    assert_eq!(response.status, 422, "got: {response:?}");
    assert_mastodon_error_shape(&response, "Validation failed");
    let body: Value = serde_json::from_str(&response.body).expect("valid JSON");
    assert!(
        body.get("error_description").is_some(),
        "a validation failure should carry the specific caller-authored detail as \
         error_description, got: {body}"
    );

    app.cleanup().await;
}

// ---- (2) 401 auth error: body shape + status (Requirements 7.1, 7.3) ----

#[tokio::test]
async fn missing_bearer_token_returns_401_with_mastodon_compatible_body() {
    let app = spawn_test_app().await;
    let addr = spawn_test_only_router(&app).await;

    let response = raw_request(addr, "GET", "/__error_rl_whoami__", None, &[], "").await;

    assert_eq!(response.status, 401, "got: {response:?}");
    assert_mastodon_error_shape(&response, "The access token is invalid");

    app.cleanup().await;
}

// ---- (3) 403 permission error: body shape + status (Requirements 7.1,
// 7.3) ----

#[tokio::test]
async fn insufficient_scope_returns_403_with_mastodon_compatible_body() {
    let app = spawn_test_app().await;
    let (oauth_app_id, _client_id, _client_secret) =
        register_app_via_http(&app, "Error/RL 403 Test Client").await;
    let (_owner_id, actor_id) = create_owner_with_actor(&app, "errorrl403owner").await;
    // Grants only `read`; the route requires `write:statuses`.
    let token = issue_token(&app, oauth_app_id, actor_id, &["read"]).await;

    let addr = spawn_test_only_router(&app).await;
    let auth_header = format!("Bearer {token}");
    let response = raw_request(
        addr,
        "GET",
        "/__error_rl_requires_write_statuses__",
        None,
        &[("Authorization", &auth_header)],
        "",
    )
    .await;

    assert_eq!(response.status, 403, "got: {response:?}");
    assert_mastodon_error_shape(&response, "This action is outside the authorized scopes");

    app.cleanup().await;
}

// ---- (4) 404 not-found: body shape + status (Requirements 7.1, 7.3) ----

#[tokio::test]
async fn unknown_resource_returns_404_with_mastodon_compatible_body() {
    let app = spawn_test_app().await;
    let addr = spawn_test_only_router(&app).await;

    let response = raw_request(addr, "GET", "/__error_rl_missing__", None, &[], "").await;

    assert_eq!(response.status, 404, "got: {response:?}");
    assert_mastodon_error_shape(&response, "Record not found");
    let body: Value = serde_json::from_str(&response.body).expect("valid JSON");
    assert_eq!(
        body.get("error_description").and_then(Value::as_str),
        Some("resource not found"),
        "got: {body}"
    );

    app.cleanup().await;
}

// ---- (5) 5xx system error: internal detail never leaks (Requirements 7.1,
// 7.5) ----

#[tokio::test]
async fn system_error_hides_internal_detail_and_returns_generic_mastodon_body() {
    let app = spawn_test_app().await;
    let addr = spawn_test_only_router(&app).await;

    let response = raw_request(addr, "GET", "/__error_rl_5xx__", None, &[], "").await;

    assert_eq!(response.status, 500, "got: {response:?}");
    assert!(
        !response.body.contains(SERVER_ERROR_SOURCE_MARKER),
        "5xx response body must never leak internal source detail, got: {response:?}"
    );
    let body: Value = serde_json::from_str(&response.body).expect("valid JSON");
    assert_eq!(
        body.get("error").and_then(Value::as_str),
        Some(GENERIC_SERVER_MESSAGE),
        "a system error must render only the generic compatible message, got: {body}"
    );
    assert!(
        body.get("error_description").is_none(),
        "a system error must never carry error_description, got: {body}"
    );

    app.cleanup().await;
}

// ---- (6) X-RateLimit-* headers on a normal response (Requirements 8.1,
// 8.2, 8.4) ----

#[tokio::test]
async fn rate_limit_normal_response_carries_headers() {
    let app = spawn_test_app().await;

    let response = raw_request(app.address, "GET", "/health", None, &[], "").await;

    assert_eq!(response.status, 200, "got: {response:?}");
    let limit: u32 = response
        .headers
        .get("x-ratelimit-limit")
        .expect("X-RateLimit-Limit must be present on a normal response")
        .parse()
        .expect("X-RateLimit-Limit must be a decimal integer");
    let remaining: u32 = response
        .headers
        .get("x-ratelimit-remaining")
        .expect("X-RateLimit-Remaining must be present on a normal response")
        .parse()
        .expect("X-RateLimit-Remaining must be a decimal integer");
    assert!(
        remaining < limit,
        "one request must have been counted against the window: remaining={remaining}, limit={limit}"
    );
    response
        .headers
        .get("x-ratelimit-reset")
        .expect("X-RateLimit-Reset must be present on a normal response")
        .parse::<i64>()
        .expect("X-RateLimit-Reset must be a decimal Unix-epoch-seconds integer");

    app.cleanup().await;
}

// ---- (7) over-limit: compatible 429 + headers, driven through the real
// production policy (Requirements 8.1, 8.2, 8.3, 8.4) ----

#[tokio::test]
async fn rate_limit_exceeded_returns_mastodon_compatible_429_with_headers() {
    let app = spawn_test_app().await;

    // Drives `app.address`'s real, deployed rate-limit policy
    // (`src/server.rs::build_router`) to exhaustion by repeating a cheap,
    // side-effect-free request. Deterministic (not flaky) because
    // `spawn_test_app`'s `FixedClock` never advances -- see this file's
    // module doc comment ("Over-limit test drives the real production
    // policy").
    let mut tripped: Option<RawResponse> = None;
    for _ in 0..OVER_LIMIT_SAFETY_CAP {
        let response = raw_request(app.address, "GET", "/health", None, &[], "").await;
        if response.status != 200 {
            tripped = Some(response);
            break;
        }
    }

    let tripped = tripped.unwrap_or_else(|| {
        panic!(
            "expected the rate limit to trip within {OVER_LIMIT_SAFETY_CAP} requests, but every \
             response stayed 200 -- is the rate-limit layer still wired into build_router?"
        )
    });

    assert_eq!(tripped.status, 429, "got: {tripped:?}");
    assert_mastodon_error_shape(&tripped, "Too many requests");
    let body: Value = serde_json::from_str(&tripped.body).expect("valid JSON");
    assert!(
        body.get("error_description").is_some(),
        "the over-limit AppError's public_message should surface as error_description, got: {body}"
    );
    assert_eq!(
        tripped
            .headers
            .get("x-ratelimit-remaining")
            .map(String::as_str),
        Some("0"),
        "an over-limit response must still carry the same headers, remaining=0, got: {tripped:?}"
    );
    assert!(
        tripped.headers.contains_key("x-ratelimit-limit"),
        "got: {tripped:?}"
    );
    tripped
        .headers
        .get("x-ratelimit-reset")
        .expect("X-RateLimit-Reset must be present on the over-limit response too")
        .parse::<i64>()
        .expect("X-RateLimit-Reset must be a decimal Unix-epoch-seconds integer");

    app.cleanup().await;
}
