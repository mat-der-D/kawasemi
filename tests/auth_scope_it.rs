//! Bearer authentication + scope integration tests (task 9.2, `_Depends: 7.1_`,
//! design.md's File Structure Plan: `tests/auth_scope_it.rs`).
//!
//! Requirements exercised: 4.2, 4.3, 4.4, 4.5, 5.1, 5.2, 5.3, 5.4, 5.5.
//!
//! `src/oauth/scope.rs`'s own unit tests already prove
//! `ScopeSet::is_satisfied_by`'s inclusion judgment as a pure function, and
//! `src/oauth/middleware/tests.rs` (task 6.4) already proves
//! `BearerAuthMiddleware`'s extractors against a minimal, middleware-only
//! `AuthState` router dispatched via `tower::ServiceExt::oneshot`, not the
//! real production router. `tests/api_foundation_wiring_it.rs` (task 7.1)
//! additionally proves the middleware is reachable at all once wired onto
//! `Router<AppState>`, but only as a byproduct of proving the *wiring* itself
//! (one happy-path token, one 401, one 403). This file is the dedicated
//! integration layer task 9.2 asks for: every one of Requirement 5's
//! auth-resolution branches and Requirement 4's scope-inclusion branches,
//! driven together through real HTTP against a `spawn_test_app`-booted
//! instance's real, mounted router — proving the already-implemented,
//! already-reviewed `BearerAuthMiddleware`/`Scope` components respond with
//! the expected status end to end, not re-deriving their logic.
//!
//! ## No HTTP client dependency: raw sockets, mirroring established precedent
//! This crate has no HTTP client dependency (`Cargo.toml`). This file
//! duplicates `tests/api_foundation_wiring_it.rs`'s/`tests/oauth_flow_it.rs`'s
//! own `RawResponse`/`raw_request`/`parse_response`/`url_encode`/`form_body`
//! helpers verbatim (each `tests/*.rs` file is a separate compiled binary
//! with no shared module, and those helpers are private to their own file)
//! rather than introducing a new dependency or a `tests/common/mod.rs` this
//! task's boundary does not call for.
//!
//! ## Test-only protected routes, mirroring `tests/api_foundation_wiring_it.rs`
//! Nothing in this crate exposes a production endpoint behind
//! `BearerAuthMiddleware` yet (that is a downstream feature spec's job); this
//! file mounts a tiny extra router directly on the real, already-running
//! instance's `AppState`, exactly like `tests/api_foundation_wiring_it.rs`'s
//! own `protected_test_router`/`whoami`/`requires_write_scope` precedent
//! (itself modeled on `src/server/tests.rs`'s "merge a test-only route onto
//! `router()`, then `.with_state(state)`" technique) — never touching
//! production `src/server.rs`'s own mounted routes.
//!
//! ## Access tokens minted directly through the repository, matching task
//! 7.1's own precedent
//! Every token this file presents is a genuinely persisted, hashed row
//! (`token_repository::issue_token`, task 3.3, already reviewed) — never a
//! hand-constructed `RequestActorContext`. The full interactive
//! owner-login/actor-selection/CSRF/token-exchange dance is
//! `tests/oauth_flow_it.rs`'s job (task 9.1, out of this task's own
//! boundary); this file only needs a real, resolvable token bound to a real
//! actor with a chosen scope set, which is exactly what
//! `tests/api_foundation_wiring_it.rs`'s Bearer-wiring test already
//! establishes as the right precedent for this kind of test.
//!
//! ## Revocation via the real, mounted `POST /oauth/revoke` endpoint
//! Mirrors `tests/api_foundation_wiring_it.rs`'s own choice: revoking through
//! real HTTP (rather than calling `token_repository::revoke_token` directly,
//! as `src/oauth/middleware/tests.rs`'s unit-level tests do) additionally
//! proves the revoke endpoint and the Bearer middleware observe the same
//! underlying row.
//!
//! ## Requirement 5.4 (optional auth): "no token presented" vs. "a presented
//! but invalid token" are NOT the same outcome (CONCERN — verified against
//! `src/oauth/middleware.rs`'s own doc comment before writing these
//! assertions, not assumed)
//! `src/oauth/middleware.rs`'s module doc comment ("Optional vs. mandatory
//! auth") is explicit and already-reviewed: [`authenticate`]'s `Ok(None)`
//! (continue unauthenticated) fires *only* when no bearer token was
//! presented at all (no `Authorization` header, or a header using a
//! different auth scheme such as `Basic` — `bearer_token`'s doc comment). A
//! **presented** `Bearer <token>` value that fails to resolve
//! (missing/invalid/revoked) is always a 401 `AppError`, in every auth mode
//! — Requirement 5.4's Japanese text itself only carves out an exception for
//! "提示されなければ" (if not presented), not for a bad token being
//! presented on an optional-auth route. This file's optional-auth coverage
//! therefore asserts three distinct outcomes rather than collapsing them
//! into one: no header at all -> continues unauthenticated (200); a
//! non-Bearer scheme header (e.g. `Basic ...`) -> also continues
//! unauthenticated (200), since it is likewise "no bearer token presented"
//! per `bearer_token`'s own doc comment; a well-formed but invalid/never-issued
//! `Bearer <garbage>` value -> 401, even on the optional-auth route. Writing
//! this file's optional-auth assertions any other way would contradict
//! `src/oauth/middleware.rs`'s own already-reviewed, explicitly documented
//! behavior — see this crate's TDD protocol on treating such a mismatch as a
//! signal to re-check the test against the implementation, not to patch
//! `src/`.

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
use kawasemi::oauth::middleware::{OptionalActor, RequiredActor};
use kawasemi::oauth::model::ScopeSet as PlaceholderScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::oauth::{ScopeSet, require_scope};
use kawasemi::state::AppState;
use kawasemi::test_harness::{TestApp, spawn_test_app};

const REDIRECT_URI: &str = "https://client.example/callback";

// ---- raw HTTP plumbing (duplicated from tests/api_foundation_wiring_it.rs;
// see this file's module doc comment for why) ----

#[derive(Debug)]
struct RawResponse {
    status: u16,
    body: String,
}

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

fn form_body(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={}", url_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
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
    RawResponse {
        status,
        body: body.to_string(),
    }
}

// ---- test-only protected routes (mirrors
// tests/api_foundation_wiring_it.rs's whoami/requires_write_scope) ----

/// `GET /__auth_scope_whoami__`: mandatory auth, no scope requirement beyond
/// authentication itself (Requirements 5.1, 5.2, 5.3).
async fn whoami(RequiredActor(ctx): RequiredActor) -> Json<Value> {
    Json(json!({ "actor_id": ctx.actor_id.as_i64() }))
}

/// `GET /__auth_scope_requires_write_statuses__`: mandatory auth plus a fixed
/// `write:statuses` scope requirement (Requirements 4.2, 4.3, 4.4, 4.5).
async fn requires_write_statuses(
    RequiredActor(ctx): RequiredActor,
) -> Result<Json<Value>, AppError> {
    let required = ScopeSet::parse("write:statuses").expect("\"write:statuses\" is a valid scope");
    require_scope(&ctx, &required)?;
    Ok(Json(json!({ "actor_id": ctx.actor_id.as_i64() })))
}

/// `GET /__auth_scope_optional__`: optional auth (Requirement 5.4) — never
/// rejects merely because no bearer token was presented.
async fn optional_probe(OptionalActor(ctx): OptionalActor) -> Json<Value> {
    Json(json!({
        "authenticated": ctx.is_some(),
        "actor_id": ctx.map(|c| c.actor_id.as_i64()),
    }))
}

fn protected_test_router(state: AppState) -> Router {
    kawasemi::server::router()
        .route("/__auth_scope_whoami__", get(whoami))
        .route(
            "/__auth_scope_requires_write_statuses__",
            get(requires_write_statuses),
        )
        .route("/__auth_scope_optional__", get(optional_probe))
        .with_state(state)
}

/// Serves [`protected_test_router`] on its own freshly bound ephemeral
/// listener (mirroring `tests/api_foundation_wiring_it.rs`'s identical
/// choice) and returns its address.
async fn spawn_protected_router(app: &TestApp) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    let router = protected_test_router(app.state.clone());
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    addr
}

// ---- fixtures ----

/// Registers a real OAuth app via the real, mounted `POST /api/v1/apps`
/// endpoint, returning `(app_id, client_id, client_secret)`. Mirrors
/// `tests/api_foundation_wiring_it.rs`'s own registration fixture.
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
    // `Id` serializes as a decimal *string* (Mastodon-compatible convention,
    // `src/domain/primitives.rs`'s own `Serialize` impl), not a JSON number.
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
/// `(owner_id, actor_id)`. Mirrors `tests/oauth_flow_it.rs`'s own
/// `create_owner_with_actor`.
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
            display_name: "Auth Scope Test Actor".to_string(),
            summary: "an auth/scope integration test fixture".to_string(),
        })
        .await
        .expect("creating the owner's actor fixture must succeed");

    (owner_id, actor.id)
}

/// Mints a real, persisted access token bound to `actor_id` with `scopes`
/// (Requirement 3.5), returning its plaintext bearer value — never a
/// hand-constructed `RequestActorContext` (see this file's module doc
/// comment).
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

/// Revokes `token` through the real, mounted `POST /oauth/revoke` endpoint
/// (mirrors `tests/api_foundation_wiring_it.rs`'s own choice — see this
/// file's module doc comment).
async fn revoke_token_via_http(app: &TestApp, token: &str, client_id: &str, client_secret: &str) {
    let revoke_body = form_body(&[
        ("token", token),
        ("client_id", client_id),
        ("client_secret", client_secret),
    ]);
    let response = raw_request(
        app.address,
        "POST",
        "/oauth/revoke",
        Some("application/x-www-form-urlencoded"),
        &[],
        &revoke_body,
    )
    .await;
    assert_eq!(
        response.status, 200,
        "revoking through the real endpoint must succeed, got: {response:?}"
    );
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

// ---- (1) valid token, exact-match scope: single-actor context resolves
// (Requirements 4.2, 5.1, 5.3) ----

#[tokio::test]
async fn valid_token_with_exact_match_scope_resolves_the_single_actor_context() {
    let app = spawn_test_app().await;
    let (oauth_app_id, _client_id, _client_secret) =
        register_app_via_http(&app, "Exact Scope Test Client").await;
    let (_owner_id, actor_id) = create_owner_with_actor(&app, "exactscopeowner").await;
    let token = issue_token(&app, oauth_app_id, actor_id, &["write:statuses"]).await;

    let protected_addr = spawn_protected_router(&app).await;
    let auth_header = format!("Bearer {token}");

    // (a) plain authenticated route (Requirements 5.1, 5.3).
    let whoami_response = raw_request(
        protected_addr,
        "GET",
        "/__auth_scope_whoami__",
        None,
        &[("Authorization", &auth_header)],
        "",
    )
    .await;
    assert_eq!(whoami_response.status, 200);
    let whoami_body: Value =
        serde_json::from_str(&whoami_response.body).expect("whoami response must be valid JSON");
    assert_eq!(whoami_body["actor_id"].as_i64(), Some(actor_id.as_i64()));

    // (b) exact-match granular scope requirement (Requirement 4.2).
    let scoped_response = raw_request(
        protected_addr,
        "GET",
        "/__auth_scope_requires_write_statuses__",
        None,
        &[("Authorization", &auth_header)],
        "",
    )
    .await;
    assert_eq!(
        scoped_response.status, 200,
        "an exact-match granted scope must satisfy the identical required scope, got: {scoped_response:?}"
    );
    let scoped_body: Value =
        serde_json::from_str(&scoped_response.body).expect("scoped response must be valid JSON");
    assert_eq!(scoped_body["actor_id"].as_i64(), Some(actor_id.as_i64()));

    app.cleanup().await;
}

// ---- (2) missing Authorization header -> 401 (Requirement 5.2) ----

#[tokio::test]
async fn missing_authorization_header_is_401_with_mastodon_error_shape() {
    let app = spawn_test_app().await;
    let protected_addr = spawn_protected_router(&app).await;

    let response = raw_request(
        protected_addr,
        "GET",
        "/__auth_scope_whoami__",
        None,
        &[],
        "",
    )
    .await;
    assert_eq!(response.status, 401);
    assert_mastodon_error_shape(&response, "The access token is invalid");

    app.cleanup().await;
}

// ---- (3) invalid/malformed bearer token -> 401 (Requirement 5.2) ----

#[tokio::test]
async fn invalid_malformed_bearer_token_is_401_with_mastodon_error_shape() {
    let app = spawn_test_app().await;
    let protected_addr = spawn_protected_router(&app).await;

    let response = raw_request(
        protected_addr,
        "GET",
        "/__auth_scope_whoami__",
        None,
        &[("Authorization", "Bearer this-token-was-never-issued-xyz")],
        "",
    )
    .await;
    assert_eq!(response.status, 401);
    assert_mastodon_error_shape(&response, "The access token is invalid");

    app.cleanup().await;
}

// ---- (4) revoked token -> 401 (Requirements 3.4, 5.2) ----

#[tokio::test]
async fn revoked_token_is_401_after_revocation_through_the_real_endpoint() {
    let app = spawn_test_app().await;
    let (oauth_app_id, client_id, client_secret) =
        register_app_via_http(&app, "Revoked Token Test Client").await;
    let (_owner_id, actor_id) = create_owner_with_actor(&app, "revokedtokenowner").await;
    let token = issue_token(&app, oauth_app_id, actor_id, &["read"]).await;

    let protected_addr = spawn_protected_router(&app).await;
    let auth_header = format!("Bearer {token}");

    // Sanity: the token authenticates before revocation.
    let before = raw_request(
        protected_addr,
        "GET",
        "/__auth_scope_whoami__",
        None,
        &[("Authorization", &auth_header)],
        "",
    )
    .await;
    assert_eq!(before.status, 200, "sanity check failed: {before:?}");

    revoke_token_via_http(&app, &token, &client_id, &client_secret).await;

    let after = raw_request(
        protected_addr,
        "GET",
        "/__auth_scope_whoami__",
        None,
        &[("Authorization", &auth_header)],
        "",
    )
    .await;
    assert_eq!(after.status, 401);
    assert_mastodon_error_shape(&after, "The access token is invalid");

    app.cleanup().await;
}

// ---- (5) valid token, insufficient scope -> 403 (Requirements 4.3, 5.2) ----

#[tokio::test]
async fn valid_token_with_insufficient_scope_is_403_with_mastodon_error_shape() {
    let app = spawn_test_app().await;
    let (oauth_app_id, _client_id, _client_secret) =
        register_app_via_http(&app, "Insufficient Scope Test Client").await;
    let (_owner_id, actor_id) = create_owner_with_actor(&app, "insufficientscopeowner").await;
    // Grants only `read`; the route requires `write:statuses`.
    let token = issue_token(&app, oauth_app_id, actor_id, &["read"]).await;

    let protected_addr = spawn_protected_router(&app).await;
    let auth_header = format!("Bearer {token}");

    let response = raw_request(
        protected_addr,
        "GET",
        "/__auth_scope_requires_write_statuses__",
        None,
        &[("Authorization", &auth_header)],
        "",
    )
    .await;
    assert_eq!(
        response.status, 403,
        "insufficient scope must be 403, distinct from every 401 case, got: {response:?}"
    );
    assert_mastodon_error_shape(&response, "This action is outside the authorized scopes");

    app.cleanup().await;
}

// ---- (6) upper scope satisfies narrower requested scope (Requirements 4.2,
// 4.4, 4.5) ----

#[tokio::test]
async fn upper_scope_satisfies_a_narrower_requested_granular_scope() {
    let app = spawn_test_app().await;
    let (oauth_app_id, _client_id, _client_secret) =
        register_app_via_http(&app, "Upper Scope Test Client").await;
    let (_owner_id, actor_id) = create_owner_with_actor(&app, "upperscopeowner").await;
    // Grants top-level `write`, which must subsume the required
    // `write:statuses` per `scope::ScopeSet::is_satisfied_by`'s established
    // inclusion judgment (Requirement 4.4) — this test would fail if
    // `require_scope` reimplemented inclusion instead of reusing it.
    let token = issue_token(&app, oauth_app_id, actor_id, &["write"]).await;

    let protected_addr = spawn_protected_router(&app).await;
    let auth_header = format!("Bearer {token}");

    let response = raw_request(
        protected_addr,
        "GET",
        "/__auth_scope_requires_write_statuses__",
        None,
        &[("Authorization", &auth_header)],
        "",
    )
    .await;
    assert_eq!(
        response.status, 200,
        "a top-level granted scope must satisfy a narrower required granular scope, got: {response:?}"
    );
    let body: Value = serde_json::from_str(&response.body).expect("response must be valid JSON");
    assert_eq!(body["actor_id"].as_i64(), Some(actor_id.as_i64()));

    app.cleanup().await;
}

// ---- (7) optional-auth mode continuation (Requirement 5.4) ----

/// No bearer token presented at all (no `Authorization` header) -> the
/// optional-auth route continues unauthenticated, not rejected.
#[tokio::test]
async fn optional_auth_route_with_no_authorization_header_continues_unauthenticated() {
    let app = spawn_test_app().await;
    let protected_addr = spawn_protected_router(&app).await;

    let response = raw_request(
        protected_addr,
        "GET",
        "/__auth_scope_optional__",
        None,
        &[],
        "",
    )
    .await;
    assert_eq!(response.status, 200);
    let body: Value = serde_json::from_str(&response.body).expect("response must be valid JSON");
    assert_eq!(body["authenticated"], Value::Bool(false));
    assert_eq!(body["actor_id"], Value::Null);

    app.cleanup().await;
}

/// A non-Bearer auth scheme (e.g. `Basic ...`) is likewise "no bearer token
/// presented" (`src/oauth/middleware.rs::bearer_token`'s own doc comment) ->
/// the optional-auth route still continues unauthenticated.
#[tokio::test]
async fn optional_auth_route_with_a_non_bearer_scheme_header_continues_unauthenticated() {
    let app = spawn_test_app().await;
    let protected_addr = spawn_protected_router(&app).await;

    let response = raw_request(
        protected_addr,
        "GET",
        "/__auth_scope_optional__",
        None,
        &[("Authorization", "Basic dXNlcjpwYXNz")],
        "",
    )
    .await;
    assert_eq!(response.status, 200);
    let body: Value = serde_json::from_str(&response.body).expect("response must be valid JSON");
    assert_eq!(body["authenticated"], Value::Bool(false));
    assert_eq!(body["actor_id"], Value::Null);

    app.cleanup().await;
}

/// A valid token on the optional-auth route resolves the actor context
/// exactly like a mandatory-auth route would (Requirement 5.4's other half:
/// "if presented, supply the actor context").
#[tokio::test]
async fn optional_auth_route_with_a_valid_token_resolves_the_actor_context() {
    let app = spawn_test_app().await;
    let (oauth_app_id, _client_id, _client_secret) =
        register_app_via_http(&app, "Optional Auth Valid Token Test Client").await;
    let (_owner_id, actor_id) = create_owner_with_actor(&app, "optionalvalidowner").await;
    let token = issue_token(&app, oauth_app_id, actor_id, &["read"]).await;

    let protected_addr = spawn_protected_router(&app).await;
    let auth_header = format!("Bearer {token}");

    let response = raw_request(
        protected_addr,
        "GET",
        "/__auth_scope_optional__",
        None,
        &[("Authorization", &auth_header)],
        "",
    )
    .await;
    assert_eq!(response.status, 200);
    let body: Value = serde_json::from_str(&response.body).expect("response must be valid JSON");
    assert_eq!(body["authenticated"], Value::Bool(true));
    assert_eq!(body["actor_id"].as_i64(), Some(actor_id.as_i64()));

    app.cleanup().await;
}

/// A **presented but invalid** bearer token on the optional-auth route is
/// still 401, never a silent unauthenticated continuation — see this file's
/// module doc comment ("Requirement 5.4 ... NOT the same outcome") for why
/// this is the correct, already-reviewed behavior rather than a bug.
#[tokio::test]
async fn optional_auth_route_with_a_presented_invalid_token_is_still_401() {
    let app = spawn_test_app().await;
    let protected_addr = spawn_protected_router(&app).await;

    let response = raw_request(
        protected_addr,
        "GET",
        "/__auth_scope_optional__",
        None,
        &[("Authorization", "Bearer this-token-was-never-issued-xyz")],
        "",
    )
    .await;
    assert_eq!(
        response.status, 401,
        "a presented but invalid token must reject even on an optional-auth route, got: {response:?}"
    );
    assert_mastodon_error_shape(&response, "The access token is invalid");

    app.cleanup().await;
}
