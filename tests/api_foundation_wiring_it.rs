//! Integration tests for api-foundation's Composition Root wiring (task 7.1,
//! `_Boundary: ApiModule wiring_`, Requirements 1.1, 2.1, 3.1, 5.1, 7.1, 8.1).
//!
//! Tasks 5.1-5.3 (`AppsEndpoint`/`AuthorizeEndpoint`/`TokenEndpoint`) and
//! 6.1/6.3/6.4 (`MastodonError`/`RateLimit`/`BearerAuthMiddleware`) were each
//! implemented and reviewed already, but every one of their own doc comments
//! says the same thing: none of them are mounted on any router yet, and
//! their own tests call the handler/extractor functions directly rather than
//! over real HTTP (see e.g. `src/oauth/apps_endpoint.rs`'s "Not wired into a
//! router" section). This task's own job is exactly that mounting — the
//! four OAuth endpoints onto the real production router
//! (`crate::server::router`/`build_router`), plus the error-conversion,
//! rate-limit, and Bearer-auth cross-cutting layers applied across it.
//!
//! This file therefore proves the *wiring* itself, over real HTTP against a
//! `spawn_test_app`-booted instance — not a full OAuth business-logic flow
//! (single-use code consumption, PKCE, the owner-login/consent/CSRF dance):
//! design.md's File Structure Plan reserves the combined end-to-end
//! authorize -> token -> revoke flow test for `tests/oauth_flow_it.rs`
//! (task 9.1, `_Depends: 7.1_`, out of this task's own boundary — see
//! `tests/oauth_token_it.rs`'s own doc comment, which already documents this
//! naming/ownership split). What this file asserts:
//!
//! 1. All four OAuth endpoints (`POST /api/v1/apps`, `GET /oauth/authorize`,
//!    `POST /oauth/token`, `POST /oauth/revoke`) are reachable over real HTTP
//!    through the real, booted router (not called as bare functions).
//! 2. Every error response (success or failure path) comes back in the
//!    Mastodon-compatible `{"error": ..., "error_description": ...}` shape —
//!    proving `crate::error::AppError`'s default `IntoResponse` now renders
//!    through `crate::api::error::mastodon_error_body` for every handler,
//!    cross-cuttingly, without each handler needing to opt in.
//! 3. `X-RateLimit-*` headers are attached to ordinary responses.
//! 4. The Bearer auth middleware is genuinely wired against the real,
//!    booted `AppState` (`AuthState: FromRef<AppState>`, `src/server.rs`):
//!    a real access token (minted through the same repository
//!    `OauthService::exchange_token` itself calls, task 3.3, already
//!    reviewed) authenticates a protected route mounted the same way a
//!    downstream feature spec's own business endpoint would be, an absent
//!    token is rejected with 401, an insufficient scope is rejected with
//!    403, and revoking the token through the real, mounted
//!    `POST /oauth/revoke` endpoint invalidates it for that protected route.

use std::collections::HashMap;
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::routing::get;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor};
use kawasemi::domain::Id;
use kawasemi::error::AppError;
use kawasemi::oauth::middleware::RequiredActor;
use kawasemi::oauth::model::ScopeSet as PlaceholderScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::oauth::{ScopeSet, require_scope};
use kawasemi::state::AppState;
use kawasemi::test_harness::spawn_test_app;

/// A parsed raw HTTP/1.1 response: status code, lowercased header map, and
/// body text. Built by [`parse_response`] from the exact bytes read off the
/// wire — no HTTP client dependency exists in this crate (mirrors
/// `tests/test_harness_lifecycle_it.rs`'s/`src/server/tests.rs`'s own
/// `raw_http_get` precedent, extended here to POST bodies/headers/status
/// parsing since this file needs more than a bare `GET /health` check).
#[derive(Debug)]
struct RawResponse {
    status: u16,
    headers: HashMap<String, String>,
    body: String,
}

/// Percent-encodes `input` for safe inclusion in a query string or an
/// `application/x-www-form-urlencoded` body (this crate has no URL-encoding
/// dependency of its own — mirrors `src/oauth/authorize_endpoint.rs`'s own
/// documented "no URI crate" constraint — so this is a minimal, purpose-built
/// encoder covering exactly the characters this file's fixture values need
/// escaped, principally the space in `"read write"`-shaped scope strings).
fn url_encode(input: &str) -> String {
    let mut out = String::new();
    for b in input.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Builds an `application/x-www-form-urlencoded` body from `pairs`.
fn form_body(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={}", url_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Speaks a minimal raw HTTP/1.1 request over a fresh `TcpStream` and parses
/// the response. `Connection: close` (mirroring this codebase's existing
/// `raw_http_get` helpers) tells the server to close the socket once the
/// response is fully written, so `read_to_end` terminates instead of waiting
/// on a keep-alive connection.
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

// ---- (1) apps endpoint reachable, Mastodon error shape, rate-limit headers ----

#[tokio::test]
async fn apps_endpoint_reachable_with_mastodon_error_shape_and_rate_limit_headers() {
    let app = spawn_test_app().await;

    let register_body = json!({
        "client_name": "Wiring Test Client",
        "redirect_uris": ["https://client.example/callback"],
        "scopes": "read write"
    })
    .to_string();
    let response = raw_request(
        app.address,
        "POST",
        "/api/v1/apps",
        Some("application/json"),
        &[],
        &register_body,
    )
    .await;
    assert_eq!(
        response.status, 200,
        "expected POST /api/v1/apps to be reachable through the real router, got: {response:?}"
    );
    assert!(
        response.headers.contains_key("x-ratelimit-limit"),
        "expected X-RateLimit-Limit to be attached to a normal response, got headers: {:?}",
        response.headers
    );
    assert!(response.headers.contains_key("x-ratelimit-remaining"));
    assert!(response.headers.contains_key("x-ratelimit-reset"));
    let registered: Value =
        serde_json::from_str(&response.body).expect("registration response must be valid JSON");
    assert!(registered.get("client_id").is_some());
    assert!(registered.get("client_secret").is_some());

    // Missing client_name -> Mastodon-compatible 422.
    let bad_body = json!({
        "client_name": "",
        "redirect_uris": ["https://client.example/callback"],
        "scopes": "read"
    })
    .to_string();
    let bad_response = raw_request(
        app.address,
        "POST",
        "/api/v1/apps",
        Some("application/json"),
        &[],
        &bad_body,
    )
    .await;
    assert_eq!(bad_response.status, 422);
    let error_body: Value =
        serde_json::from_str(&bad_response.body).expect("error response must be valid JSON");
    assert_eq!(
        error_body.get("error").and_then(Value::as_str),
        Some("Validation failed"),
        "expected the Mastodon-compatible canonical error label, got: {error_body}"
    );
    assert!(
        error_body.get("error_description").is_some(),
        "expected error_description to carry the call site's specific detail, got: {error_body}"
    );

    app.cleanup().await;
}

// ---- (2) authorize/token/revoke endpoints reachable, Mastodon error shape ----

#[tokio::test]
async fn authorize_token_and_revoke_endpoints_are_reachable_with_mastodon_error_shapes() {
    let app = spawn_test_app().await;

    // GET /oauth/authorize with an unknown client_id -> reachable, rejected
    // before rendering anything (Requirement 2.1), Mastodon-compatible body.
    let query = format!(
        "client_id={}&redirect_uri={}&scope={}&response_type=code",
        url_encode("no-such-client-was-ever-registered"),
        url_encode("https://client.example/callback"),
        url_encode("read")
    );
    let authorize_response = raw_request(
        app.address,
        "GET",
        &format!("/oauth/authorize?{query}"),
        None,
        &[],
        "",
    )
    .await;
    assert_eq!(
        authorize_response.status, 400,
        "expected GET /oauth/authorize to be reachable through the real router, got: {authorize_response:?}"
    );
    let authorize_error: Value = serde_json::from_str(&authorize_response.body)
        .expect("authorize error response must be valid JSON");
    assert!(authorize_error.get("error").is_some());

    // POST /oauth/token with an invalid code/client -> reachable, Mastodon-compatible 400.
    let token_body = form_body(&[
        ("grant_type", "authorization_code"),
        ("code", "not-a-real-code"),
        ("client_id", "no-such-client"),
        ("client_secret", "wrong"),
        ("redirect_uri", "https://client.example/callback"),
    ]);
    let token_response = raw_request(
        app.address,
        "POST",
        "/oauth/token",
        Some("application/x-www-form-urlencoded"),
        &[],
        &token_body,
    )
    .await;
    assert_eq!(
        token_response.status, 400,
        "expected POST /oauth/token to be reachable through the real router, got: {token_response:?}"
    );
    let token_error: Value = serde_json::from_str(&token_response.body)
        .expect("token error response must be valid JSON");
    assert!(token_error.get("error").is_some());

    // POST /oauth/revoke with wrong client credentials -> reachable, Mastodon-compatible 401.
    let revoke_body = form_body(&[
        ("token", "whatever"),
        ("client_id", "no-such-client"),
        ("client_secret", "wrong"),
    ]);
    let revoke_response = raw_request(
        app.address,
        "POST",
        "/oauth/revoke",
        Some("application/x-www-form-urlencoded"),
        &[],
        &revoke_body,
    )
    .await;
    assert_eq!(
        revoke_response.status, 401,
        "expected POST /oauth/revoke to be reachable through the real router, got: {revoke_response:?}"
    );
    let revoke_error: Value = serde_json::from_str(&revoke_response.body)
        .expect("revoke error response must be valid JSON");
    assert_eq!(
        revoke_error.get("error").and_then(Value::as_str),
        Some("The access token is invalid"),
        "expected the canonical 401 Mastodon label, got: {revoke_error}"
    );

    app.cleanup().await;
}

// ---- (3) Bearer auth middleware wired against the real AppState ----

/// Minimal protected handler proving `RequiredActor` resolves through
/// `Router<AppState>` (not only through `oauth::middleware`'s own
/// `AuthState`-only test router) once `AuthState: FromRef<AppState>`
/// (`src/server.rs`) is in place.
async fn whoami(RequiredActor(ctx): RequiredActor) -> Json<Value> {
    Json(json!({ "actor_id": ctx.actor_id.as_i64() }))
}

/// Mirrors [`whoami`] but additionally requires the `write` scope
/// (Requirements 4.2, 4.3), proving `require_scope` is reachable and
/// enforced the same way against a real `AppState`-mounted route.
async fn requires_write_scope(RequiredActor(ctx): RequiredActor) -> Result<Json<Value>, AppError> {
    let required = ScopeSet::parse("write").expect("\"write\" is a valid scope literal");
    require_scope(&ctx, &required)?;
    Ok(Json(json!({ "actor_id": ctx.actor_id.as_i64() })))
}

/// Builds a tiny extra protected router directly against the real,
/// already-running instance's `AppState` — mirroring
/// `src/server/tests.rs`'s own established "merge a test-only route onto
/// `router()`, then `.with_state(state)`" technique
/// (`router_with_slow_route`/`build_router_with_test_error_route`). This
/// never touches production `src/server.rs`'s own mounted routes (no new
/// business endpoint is added there); it only proves the `FromRef`-based
/// wiring this task adds is genuinely usable by *any* handler mounted on
/// `Router<AppState>`, which is exactly what a downstream feature spec's
/// own protected business endpoint will rely on.
fn protected_test_router(state: AppState) -> Router {
    kawasemi::server::router()
        .route("/__wiring_test_whoami__", get(whoami))
        .route("/__wiring_test_requires_write__", get(requires_write_scope))
        .with_state(state)
}

#[tokio::test]
async fn bearer_auth_middleware_is_wired_against_the_real_app_state() {
    let app = spawn_test_app().await;

    // Register a real OAuth app through the real, mounted endpoint.
    let register_body = json!({
        "client_name": "Bearer Wiring Test Client",
        "redirect_uris": ["https://client.example/callback"],
        "scopes": "read write"
    })
    .to_string();
    let register_response = raw_request(
        app.address,
        "POST",
        "/api/v1/apps",
        Some("application/json"),
        &[],
        &register_body,
    )
    .await;
    assert_eq!(register_response.status, 200);
    let registered: Value = serde_json::from_str(&register_response.body)
        .expect("registration response must be valid JSON");
    // `Id` serializes as a decimal *string* (Mastodon-compatible convention;
    // see `src/domain/primitives.rs`'s own `Serialize` impl doc comment),
    // not a JSON number.
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

    // Owner + actor fixture, via the real actor-model wiring `spawn_test_app`
    // already boots (mirrors `tests/oauth_authorize_it.rs`'s own established
    // fixture pattern).
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
            handle: Handle::new("wiring_test_actor").expect("valid handle"),
            actor_type: ActorType::Person,
            display_name: "Wiring Test Actor".to_string(),
            summary: "api-foundation task 7.1 wiring test fixture".to_string(),
        })
        .await
        .expect("creating the actor fixture must succeed");

    // Mints a real access token bound to that actor via the same repository
    // function `OauthService::exchange_token` (task 4.2, already reviewed)
    // calls internally — the full authorize -> consent -> CSRF -> exchange
    // dance is task 9.1's own end-to-end flow test (out of this wiring
    // task's boundary; see this file's module doc comment).
    let read_only_token = token_repository::issue_token(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        app.state.oauth().token_hash_key(),
        now,
        NewAccessToken {
            app_id,
            actor_id: actor.id,
            // `NewAccessToken::scopes` is `model::ScopeSet` (the placeholder
            // type `token_repository.rs`/`AccessToken` actually store), not
            // the real `scope::ScopeSet` `require_scope`/`ScopeSet::parse`
            // operate on below — see `tasks.md`'s Implementation Notes entry
            // for task 4.2 documenting this same placeholder/real split.
            scopes: PlaceholderScopeSet::new(["read"]),
        },
    )
    .await
    .expect("issuing a real access token must succeed");
    let bearer_token = read_only_token.plaintext.expose_secret().clone();

    // Serve the protected test router directly against the real `AppState`
    // (see `protected_test_router`'s doc comment).
    let protected_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let protected_addr = protected_listener
        .local_addr()
        .expect("a just-bound listener must have a local address");
    let protected_router = protected_test_router(app.state.clone());
    tokio::spawn(async move {
        let _ = axum::serve(protected_listener, protected_router).await;
    });

    // No Authorization header -> 401, Mastodon-compatible.
    let unauthenticated =
        raw_request(protected_addr, "GET", "/__wiring_test_whoami__", None, &[], "").await;
    assert_eq!(unauthenticated.status, 401);
    let unauth_body: Value = serde_json::from_str(&unauthenticated.body)
        .expect("unauthenticated error response must be valid JSON");
    assert!(unauth_body.get("error").is_some());

    // A valid bearer token authenticates and resolves to the actor it was
    // minted for.
    let auth_header = format!("Bearer {bearer_token}");
    let authed = raw_request(
        protected_addr,
        "GET",
        "/__wiring_test_whoami__",
        None,
        &[("Authorization", &auth_header)],
        "",
    )
    .await;
    assert_eq!(
        authed.status, 200,
        "expected a valid bearer token to authenticate through the real AppState wiring, got: {authed:?}"
    );
    let authed_body: Value =
        serde_json::from_str(&authed.body).expect("authed response must be valid JSON");
    assert_eq!(authed_body["actor_id"].as_i64(), Some(actor.id.as_i64()));

    // The token was minted with only the `read` scope -> a route requiring
    // `write` must reject with 403.
    let forbidden = raw_request(
        protected_addr,
        "GET",
        "/__wiring_test_requires_write__",
        None,
        &[("Authorization", &auth_header)],
        "",
    )
    .await;
    assert_eq!(forbidden.status, 403);

    // Revoke the token through the real, mounted POST /oauth/revoke endpoint.
    let revoke_body = form_body(&[
        ("token", &bearer_token),
        ("client_id", &client_id),
        ("client_secret", &client_secret),
    ]);
    let revoke_response = raw_request(
        app.address,
        "POST",
        "/oauth/revoke",
        Some("application/x-www-form-urlencoded"),
        &[],
        &revoke_body,
    )
    .await;
    assert_eq!(
        revoke_response.status, 200,
        "expected POST /oauth/revoke to succeed through the real router, got: {revoke_response:?}"
    );

    // The now-revoked token must no longer authenticate.
    let revoked_check = raw_request(
        protected_addr,
        "GET",
        "/__wiring_test_whoami__",
        None,
        &[("Authorization", &auth_header)],
        "",
    )
    .await;
    assert_eq!(
        revoked_check.status, 401,
        "expected a revoked bearer token to be rejected, got: {revoked_check:?}"
    );

    app.cleanup().await;
}
