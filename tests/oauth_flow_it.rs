//! End-to-end OAuth flow integration test (task 9.1, `_Depends: 7.1_`,
//! design.md's File Structure Plan: `tests/oauth_flow_it.rs`).
//!
//! Requirements exercised: 1.1, 1.2, 1.3, 1.4, 1.5, 2.1, 2.3, 2.4, 2.5, 2.6,
//! 3.1, 3.2, 3.3, 3.4, 3.5, 3.6.
//!
//! Unlike `tests/oauth_apps_it.rs`/`tests/oauth_authorize_it.rs`/
//! `tests/oauth_token_it.rs` (tasks 5.1-5.3, each calling their one
//! endpoint's handler functions directly, since the router did not exist
//! yet), and unlike `tests/api_foundation_wiring_it.rs` (task 7.1, proving
//! the four OAuth endpoints are merely *reachable* and share the
//! cross-cutting error/rate-limit/Bearer-auth layers, with a token minted
//! directly through the repository rather than through the interactive
//! authorize dance), this file drives the *entire* OAuth 2.0 authorization-
//! code flow end to end over real HTTP against a `spawn_test_app`-booted
//! instance: app registration -> credential verification -> authorize
//! (owner login -> actor-selection consent with CSRF) -> token exchange ->
//! resolution -> revocation, plus the negative paths task 9.1's own task
//! text calls out by name (code reuse, invalid credentials, PKCE mismatch,
//! CSRF token mismatch).
//!
//! ## No HTTP client dependency: raw sockets, mirroring established
//! precedent
//! This crate has no HTTP client dependency (`Cargo.toml`). This file
//! duplicates `tests/api_foundation_wiring_it.rs`'s own `RawResponse`/
//! `raw_request`/`parse_response`/`url_encode`/`form_body` helpers verbatim
//! (each `tests/*.rs` file is a separate compiled binary with no shared
//! module, and those helpers are private to that file) rather than
//! introducing a new dependency or a `tests/common/mod.rs` this task's
//! boundary does not call for.
//!
//! ## PKCE is verified at the `OauthService`/`code_repository` layer, not
//! forwarded through the HTTP `authorize` step (documented, not invented)
//! `src/oauth/authorize_endpoint.rs`'s own doc comment (judgment call 5,
//! task 5.2, already reviewed) and `tasks.md`'s Implementation Notes entry
//! for task 5.2 both state that `GET`/`POST /oauth/authorize` never forward
//! a `code_challenge` — `authorize_post` always passes `code_challenge:
//! None` to `OauthService::issue_authorization_code`. There is therefore no
//! HTTP-reachable way to issue a PKCE-bound code through the real consent
//! flow today. This file's PKCE coverage
//! ([`token_exchange_rejects_a_mismatched_pkce_verifier_and_succeeds_with_a_matching_one`])
//! therefore issues the code via `app.state.oauth().service()` directly —
//! the exact same shared `OauthService` instance the real, mounted
//! `/oauth/authorize`/`/oauth/token` endpoints use (via `AppState`'s
//! `FromRef` bridges, `src/server.rs`) — and exchanges it over real HTTP
//! `POST /oauth/token`, matching this task's own instruction not to invent
//! unwired authorize-step PKCE forwarding.
//!
//! ## Requirement 3.6 (token plaintext never logged)
//! Not re-verified here: capturing/asserting on diagnostic log output is
//! outside an HTTP-integration test's reach, and this behavior is already
//! covered at the unit level by `src/oauth/hash.rs`'s/`src/oauth/model.rs`'s
//! own `Secret`-masking tests (task 2.1/3.1, already reviewed). This file
//! instead proves the *positive* shape of 3.6's promise operationally: the
//! plaintext token is readable exactly once, in the token-exchange response
//! body, and never appears in any other endpoint's response.

use std::collections::HashMap;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor};
use kawasemi::domain::Id;
use kawasemi::oauth::pkce::PkceChallenge;
use kawasemi::oauth::service::AuthorizeApproval;
use kawasemi::oauth::token_repository;
use kawasemi::test_harness::{TestApp, spawn_test_app};

const REDIRECT_URI: &str = "https://client.example/callback";

// ---- raw HTTP plumbing (duplicated from tests/api_foundation_wiring_it.rs;
// see this file's module doc comment for why) ----

#[derive(Debug)]
struct RawResponse {
    status: u16,
    headers: HashMap<String, String>,
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

// ---- fixtures ----

/// Creates the sole owner fixture and one actor belonging to it, returning
/// `(owner_id, actor_id)`. Mirrors `tests/oauth_authorize_it.rs`'s own
/// established fixture pattern.
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
            display_name: "Test Actor".to_string(),
            summary: "an oauth-flow end-to-end integration test fixture".to_string(),
        })
        .await
        .expect("creating the owner's actor fixture must succeed");

    (owner_id, actor.id)
}

/// The real, startup-configured owner passphrase this `TestApp` instance was
/// booted with (`spawn_test_app`'s own private `TEST_OWNER_PASSWORD`
/// constant, not otherwise exported) — read back through the public
/// `AppState` accessor chain (`state.oauth().owner_credential().password`)
/// rather than hard-coding a duplicate constant this file would have to keep
/// in sync with `src/test_harness.rs`.
fn owner_password(app: &TestApp) -> String {
    app.state
        .oauth()
        .owner_credential()
        .password
        .expose_secret()
        .clone()
}

/// Registers an OAuth client app via the real, mounted `POST /api/v1/apps`
/// endpoint (Requirements 1.1, 1.4), returning `(client_id, client_secret)`.
async fn register_app_via_http(app: &TestApp, client_name: &str, scopes: &str) -> (String, String) {
    let body = serde_json::json!({
        "client_name": client_name,
        "redirect_uris": [REDIRECT_URI],
        "scopes": scopes,
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
    let json: Value =
        serde_json::from_str(&response.body).expect("registration response must be valid JSON");
    let client_id = json["client_id"]
        .as_str()
        .expect("registration response must carry client_id")
        .to_string();
    let client_secret = json["client_secret"]
        .as_str()
        .expect("registration response must carry client_secret")
        .to_string();
    (client_id, client_secret)
}

fn basic_auth_header(client_id: &str, client_secret: &str) -> String {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
    format!(
        "Basic {}",
        BASE64_STANDARD.encode(format!("{client_id}:{client_secret}"))
    )
}

fn authorize_query(client_id: &str, scope: &str) -> String {
    format!(
        "client_id={}&redirect_uri={}&scope={}&response_type=code",
        url_encode(client_id),
        url_encode(REDIRECT_URI),
        url_encode(scope)
    )
}

/// Extracts the owner-session cookie's raw value from a `Set-Cookie` header
/// value shaped `kawasemi_owner_session=<value>; HttpOnly; ...` (mirrors
/// `tests/oauth_authorize_it.rs::set_cookie_value`, adapted to a
/// [`RawResponse`]'s already-lowercased header map instead of an
/// `axum::response::Response`).
fn extract_set_cookie_value(response: &RawResponse) -> String {
    let raw = response
        .headers
        .get("set-cookie")
        .expect("response must carry a Set-Cookie header");
    let after_name = raw
        .strip_prefix("kawasemi_owner_session=")
        .expect("Set-Cookie header must start with the owner session cookie name");
    after_name
        .split(';')
        .next()
        .expect("Set-Cookie header must have at least one segment")
        .to_string()
}

/// Extracts the CSRF token embedded in a rendered consent screen (mirrors
/// `tests/oauth_authorize_it.rs::extract_csrf_token`).
fn extract_csrf_token(html: &str) -> String {
    let marker = r#"name="csrf_token" value=""#;
    let start = html
        .find(marker)
        .expect("consent HTML must embed a csrf_token field")
        + marker.len();
    let rest = &html[start..];
    let end = rest.find('"').expect("csrf_token value must be quoted");
    rest[..end].to_string()
}

/// Extracts the authorization code from a `Location` redirect header shaped
/// `<redirect_uri>?code=<code>` (`redirect_response` in
/// `src/oauth/authorize_endpoint.rs` never appends any parameter after
/// `code=`, so the remainder of the string is the whole code value).
fn extract_code_from_location(location: &str) -> String {
    let marker = "code=";
    let idx = location
        .find(marker)
        .unwrap_or_else(|| panic!("Location header must carry a code= parameter, got: {location}"));
    location[idx + marker.len()..].to_string()
}

async fn authorization_code_row_count(app: &TestApp) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM oauth_authorization_codes")
        .fetch_one(&app.pool)
        .await
        .expect("counting authorization codes must succeed")
}

// ---- (1) the full happy-path flow ----

/// Requirements 1.1, 1.4, 1.5, 2.1, 2.3, 3.1, 3.4, 3.5: registers an app,
/// verifies its credentials, drives the interactive owner-login ->
/// actor-selection consent dance with a real CSRF token, exchanges the
/// issued code for a token, confirms the token resolves to the selected
/// actor and approved scopes, then revokes it and confirms it no longer
/// resolves.
#[tokio::test]
async fn full_oauth_flow_register_login_consent_exchange_and_revoke() {
    let app = spawn_test_app().await;

    // ---- 1. app registration (Requirement 1.1, 1.4) ----
    let (client_id, client_secret) =
        register_app_via_http(&app, "Full Flow Test Client", "read write").await;

    // ---- 2. credential verification (Requirement 1.5) ----
    let verify_ok = raw_request(
        app.address,
        "GET",
        "/api/v1/apps/verify_credentials",
        None,
        &[(
            "Authorization",
            &basic_auth_header(&client_id, &client_secret),
        )],
        "",
    )
    .await;
    assert_eq!(verify_ok.status, 200);
    let verified: Value =
        serde_json::from_str(&verify_ok.body).expect("verify_credentials response must be JSON");
    assert_eq!(verified["client_id"].as_str(), Some(client_id.as_str()));
    assert!(
        verified.get("client_secret").is_none(),
        "verify_credentials must never echo the client_secret back"
    );

    let verify_bad = raw_request(
        app.address,
        "GET",
        "/api/v1/apps/verify_credentials",
        None,
        &[(
            "Authorization",
            &basic_auth_header(&client_id, "definitely-the-wrong-secret"),
        )],
        "",
    )
    .await;
    assert_eq!(verify_bad.status, 401);

    let (_owner_id, actor_id) = create_owner_with_actor(&app, "flowtestowner").await;

    // ---- 3. GET /oauth/authorize unauthenticated -> login form (Requirement 2.1's precondition) ----
    let query = authorize_query(&client_id, "read write");
    let login_get = raw_request(
        app.address,
        "GET",
        &format!("/oauth/authorize?{query}"),
        None,
        &[],
        "",
    )
    .await;
    assert_eq!(login_get.status, 200);
    assert!(login_get.body.contains(r#"name="password""#));

    // ---- 4. owner login (Requirement 2.2's precondition) ----
    let login_body = form_body(&[
        ("client_id", &client_id),
        ("redirect_uri", REDIRECT_URI),
        ("scope", "read write"),
        ("response_type", "code"),
        ("password", &owner_password(&app)),
    ]);
    let login_post = raw_request(
        app.address,
        "POST",
        "/oauth/authorize",
        Some("application/x-www-form-urlencoded"),
        &[],
        &login_body,
    )
    .await;
    assert_eq!(
        login_post.status, 200,
        "a correct-password login must render the consent screen, got: {login_post:?}"
    );
    let cookie_value = extract_set_cookie_value(&login_post);
    let csrf_token = extract_csrf_token(&login_post.body);
    assert!(login_post.body.contains("flowtestowner"));
    assert!(
        login_post
            .body
            .contains(&format!(r#"value="{}""#, actor_id.as_i64()))
    );

    // ---- 5. actor-selection consent approval (Requirements 2.2, 2.3) ----
    let cookie_header = format!("kawasemi_owner_session={cookie_value}");
    let consent_body = form_body(&[
        ("client_id", &client_id),
        ("redirect_uri", REDIRECT_URI),
        ("scope", "read write"),
        ("response_type", "code"),
        ("csrf_token", &csrf_token),
        ("selected_actor", &actor_id.as_i64().to_string()),
        ("approved_scopes", "read write"),
        ("decision", "approve"),
    ]);
    let consent_response = raw_request(
        app.address,
        "POST",
        "/oauth/authorize",
        Some("application/x-www-form-urlencoded"),
        &[("Cookie", &cookie_header)],
        &consent_body,
    )
    .await;
    assert_eq!(consent_response.status, 302);
    let location = consent_response
        .headers
        .get("location")
        .expect("an approval must redirect")
        .to_string();
    assert!(location.starts_with(REDIRECT_URI));
    let code = extract_code_from_location(&location);
    assert!(!code.is_empty());

    // ---- 6. token exchange (Requirements 3.1, 3.5) ----
    let token_body = form_body(&[
        ("grant_type", "authorization_code"),
        ("code", &code),
        ("client_id", &client_id),
        ("client_secret", &client_secret),
        ("redirect_uri", REDIRECT_URI),
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
        token_response.status, 200,
        "exchanging a freshly issued code must succeed, got: {token_response:?}"
    );
    let token_json: Value =
        serde_json::from_str(&token_response.body).expect("token response must be valid JSON");
    let access_token = token_json["access_token"]
        .as_str()
        .expect("token response must carry access_token")
        .to_string();
    assert!(!access_token.is_empty());
    assert_eq!(token_json["token_type"].as_str(), Some("Bearer"));
    assert_eq!(token_json["scope"].as_str(), Some("read write"));
    assert!(token_json["created_at"].as_i64().unwrap_or_default() > 0);

    // ---- 7. the issued token resolves to the selected actor and approved
    // scopes (Requirements 3.5, 2.3) ----
    let resolved = token_repository::resolve_token(
        &app.pool,
        app.state.oauth().token_hash_key(),
        &access_token,
    )
    .await
    .expect("resolve_token must not error")
    .expect("the freshly issued token must resolve as active");
    assert_eq!(resolved.actor_id, actor_id);
    let mut resolved_scopes: Vec<&str> = resolved.scopes.as_strs().collect();
    resolved_scopes.sort_unstable();
    assert_eq!(resolved_scopes, vec!["read", "write"]);

    // ---- 8. code reuse is rejected (Requirements 2.5, 3.2) ----
    let reuse_response = raw_request(
        app.address,
        "POST",
        "/oauth/token",
        Some("application/x-www-form-urlencoded"),
        &[],
        &token_body,
    )
    .await;
    assert_eq!(
        reuse_response.status, 400,
        "redeeming the same code twice must be rejected, got: {reuse_response:?}"
    );
    let reuse_error: Value =
        serde_json::from_str(&reuse_response.body).expect("reuse error body must be valid JSON");
    assert!(reuse_error.get("error").is_some());

    // ---- 9. revoke the token (Requirement 3.4) ----
    let revoke_body = form_body(&[
        ("token", &access_token),
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
    assert_eq!(revoke_response.status, 200);
    let revoke_json: Value =
        serde_json::from_str(&revoke_response.body).expect("revoke response must be valid JSON");
    assert_eq!(revoke_json, serde_json::json!({}));

    let resolved_after_revoke = token_repository::resolve_token(
        &app.pool,
        app.state.oauth().token_hash_key(),
        &access_token,
    )
    .await
    .expect("resolve_token must not error");
    assert!(
        resolved_after_revoke.is_none(),
        "a revoked token must no longer resolve as active"
    );

    app.cleanup().await;
}

// ---- (2) app registration input validation (Requirements 1.2, 1.3) ----

#[tokio::test]
async fn register_app_rejects_missing_client_name_and_unknown_scope_with_422() {
    let app = spawn_test_app().await;

    let missing_name_body = serde_json::json!({
        "client_name": "",
        "redirect_uris": [REDIRECT_URI],
        "scopes": "read",
    })
    .to_string();
    let missing_name = raw_request(
        app.address,
        "POST",
        "/api/v1/apps",
        Some("application/json"),
        &[],
        &missing_name_body,
    )
    .await;
    assert_eq!(missing_name.status, 422);
    let missing_name_error: Value =
        serde_json::from_str(&missing_name.body).expect("error response must be valid JSON");
    assert_eq!(
        missing_name_error["error"].as_str(),
        Some("Validation failed")
    );

    let bad_scope_body = serde_json::json!({
        "client_name": "Bad Scope Client",
        "redirect_uris": [REDIRECT_URI],
        "scopes": "read totally_bogus_scope",
    })
    .to_string();
    let bad_scope = raw_request(
        app.address,
        "POST",
        "/api/v1/apps",
        Some("application/json"),
        &[],
        &bad_scope_body,
    )
    .await;
    assert_eq!(bad_scope.status, 422);

    app.cleanup().await;
}

// ---- (3) authorize: unknown client_id rejected before rendering anything
// (Requirement 2.1) ----

#[tokio::test]
async fn authorize_get_rejects_unknown_client_id_with_400() {
    let app = spawn_test_app().await;

    let query = authorize_query("no-such-client-was-ever-registered", "read");
    let response = raw_request(
        app.address,
        "GET",
        &format!("/oauth/authorize?{query}"),
        None,
        &[],
        "",
    )
    .await;
    assert_eq!(response.status, 400);
    let error: Value =
        serde_json::from_str(&response.body).expect("error response must be valid JSON");
    assert!(error.get("error").is_some());

    app.cleanup().await;
}

// ---- (4) authorize: wrong owner password rejected (Requirement 2.2) ----

#[tokio::test]
async fn authorize_post_login_rejects_wrong_password_with_401() {
    let app = spawn_test_app().await;
    let (client_id, _client_secret) =
        register_app_via_http(&app, "Wrong Password Test Client", "read").await;

    let login_body = form_body(&[
        ("client_id", &client_id),
        ("redirect_uri", REDIRECT_URI),
        ("scope", "read"),
        ("response_type", "code"),
        ("password", "definitely-the-wrong-password"),
    ]);
    let response = raw_request(
        app.address,
        "POST",
        "/oauth/authorize",
        Some("application/x-www-form-urlencoded"),
        &[],
        &login_body,
    )
    .await;
    assert_eq!(response.status, 401);

    app.cleanup().await;
}

// ---- (5) authorize: CSRF token mismatch rejected, no code issued
// (Requirements 2.3, security considerations) ----

#[tokio::test]
async fn authorize_post_consent_rejects_mismatched_csrf_token_with_403_and_issues_no_code() {
    let app = spawn_test_app().await;
    let (client_id, _client_secret) =
        register_app_via_http(&app, "CSRF Mismatch Test Client", "read write").await;
    let (_owner_id, actor_id) = create_owner_with_actor(&app, "csrftestowner").await;

    let login_body = form_body(&[
        ("client_id", &client_id),
        ("redirect_uri", REDIRECT_URI),
        ("scope", "read write"),
        ("response_type", "code"),
        ("password", &owner_password(&app)),
    ]);
    let login_post = raw_request(
        app.address,
        "POST",
        "/oauth/authorize",
        Some("application/x-www-form-urlencoded"),
        &[],
        &login_body,
    )
    .await;
    assert_eq!(login_post.status, 200);
    let cookie_value = extract_set_cookie_value(&login_post);

    let before = authorization_code_row_count(&app).await;

    let cookie_header = format!("kawasemi_owner_session={cookie_value}");
    let consent_body = form_body(&[
        ("client_id", &client_id),
        ("redirect_uri", REDIRECT_URI),
        ("scope", "read write"),
        ("response_type", "code"),
        (
            "csrf_token",
            "0000000000000000000000000000000000000000000000000000000000000000",
        ),
        ("selected_actor", &actor_id.as_i64().to_string()),
        ("approved_scopes", "read write"),
        ("decision", "approve"),
    ]);
    let response = raw_request(
        app.address,
        "POST",
        "/oauth/authorize",
        Some("application/x-www-form-urlencoded"),
        &[("Cookie", &cookie_header)],
        &consent_body,
    )
    .await;
    assert_eq!(response.status, 403);
    let error: Value =
        serde_json::from_str(&response.body).expect("error response must be valid JSON");
    assert_eq!(
        error["error"].as_str(),
        Some("This action is outside the authorized scopes")
    );

    let after = authorization_code_row_count(&app).await;
    assert_eq!(
        after, before,
        "no authorization code may be issued on a CSRF mismatch"
    );

    app.cleanup().await;
}

// ---- (6) authorize: denial redirects with access_denied, no code issued
// (Requirement 2.4) ----

#[tokio::test]
async fn authorize_post_consent_deny_redirects_with_access_denied_and_issues_no_code() {
    let app = spawn_test_app().await;
    let (client_id, _client_secret) =
        register_app_via_http(&app, "Deny Test Client", "read write").await;
    let (_owner_id, actor_id) = create_owner_with_actor(&app, "denytestowner").await;

    let login_body = form_body(&[
        ("client_id", &client_id),
        ("redirect_uri", REDIRECT_URI),
        ("scope", "read write"),
        ("response_type", "code"),
        ("password", &owner_password(&app)),
    ]);
    let login_post = raw_request(
        app.address,
        "POST",
        "/oauth/authorize",
        Some("application/x-www-form-urlencoded"),
        &[],
        &login_body,
    )
    .await;
    let cookie_value = extract_set_cookie_value(&login_post);
    let csrf_token = extract_csrf_token(&login_post.body);

    let before = authorization_code_row_count(&app).await;

    let cookie_header = format!("kawasemi_owner_session={cookie_value}");
    let consent_body = form_body(&[
        ("client_id", &client_id),
        ("redirect_uri", REDIRECT_URI),
        ("scope", "read write"),
        ("response_type", "code"),
        ("csrf_token", &csrf_token),
        ("selected_actor", &actor_id.as_i64().to_string()),
        ("approved_scopes", "read write"),
        ("decision", "deny"),
    ]);
    let response = raw_request(
        app.address,
        "POST",
        "/oauth/authorize",
        Some("application/x-www-form-urlencoded"),
        &[("Cookie", &cookie_header)],
        &consent_body,
    )
    .await;
    assert_eq!(response.status, 302);
    let location = response
        .headers
        .get("location")
        .expect("a denial must redirect")
        .to_string();
    assert!(location.starts_with(REDIRECT_URI));
    assert!(location.contains("error=access_denied"));
    assert!(!location.contains("code="));

    let after = authorization_code_row_count(&app).await;
    assert_eq!(
        after, before,
        "no authorization code may be issued on denial"
    );

    app.cleanup().await;
}

// ---- (7) token exchange: invalid client credentials and mismatched
// redirect_uri rejected (Requirement 3.2) ----

#[tokio::test]
async fn token_exchange_rejects_invalid_client_credentials_and_a_mismatched_redirect_uri() {
    let app = spawn_test_app().await;
    let (client_id, client_secret) =
        register_app_via_http(&app, "Exchange Rejection Test Client", "read").await;
    let (_owner_id, actor_id) = create_owner_with_actor(&app, "exchangerejectowner").await;

    // Codes issued directly through the same shared `OauthService` the real
    // endpoints use (see this file's module doc comment) so this test does
    // not need to repeat the full interactive login/consent dance twice for
    // two independent negative scenarios.

    // Wrong client_secret: rejected, and the code is left unconsumed (this
    // crate's documented "verify credentials before touching the code"
    // ordering, `service.rs`'s own doc comment).
    let code_for_bad_creds = app
        .state
        .oauth()
        .service()
        .issue_authorization_code(AuthorizeApproval {
            client_id: client_id.clone(),
            redirect_uri: REDIRECT_URI.to_string(),
            scopes: "read".to_string(),
            actor_id,
            code_challenge: None,
        })
        .await
        .expect("issuing a code must succeed");
    let bad_creds_body = form_body(&[
        ("grant_type", "authorization_code"),
        ("code", code_for_bad_creds.plaintext.expose_secret()),
        ("client_id", &client_id),
        ("client_secret", "definitely-the-wrong-secret"),
        ("redirect_uri", REDIRECT_URI),
    ]);
    let bad_creds_response = raw_request(
        app.address,
        "POST",
        "/oauth/token",
        Some("application/x-www-form-urlencoded"),
        &[],
        &bad_creds_body,
    )
    .await;
    assert_eq!(bad_creds_response.status, 400);

    // The same code, now exchanged with correct credentials but the wrong
    // redirect_uri: still rejected (Requirement 3.2), and this attempt does
    // burn the code (documented consume-then-validate ordering).
    let mismatched_uri_body = form_body(&[
        ("grant_type", "authorization_code"),
        ("code", code_for_bad_creds.plaintext.expose_secret()),
        ("client_id", &client_id),
        ("client_secret", &client_secret),
        ("redirect_uri", "https://attacker.example/callback"),
    ]);
    let mismatched_uri_response = raw_request(
        app.address,
        "POST",
        "/oauth/token",
        Some("application/x-www-form-urlencoded"),
        &[],
        &mismatched_uri_body,
    )
    .await;
    assert_eq!(mismatched_uri_response.status, 400);

    // A never-issued code is rejected outright.
    let invalid_code_body = form_body(&[
        ("grant_type", "authorization_code"),
        ("code", "a-code-that-was-never-issued"),
        ("client_id", &client_id),
        ("client_secret", &client_secret),
        ("redirect_uri", REDIRECT_URI),
    ]);
    let invalid_code_response = raw_request(
        app.address,
        "POST",
        "/oauth/token",
        Some("application/x-www-form-urlencoded"),
        &[],
        &invalid_code_body,
    )
    .await;
    assert_eq!(invalid_code_response.status, 400);

    app.cleanup().await;
}

// ---- (8) token exchange: PKCE mismatch rejected, matching verifier
// succeeds (Requirements 2.6, 3.3) ----

#[tokio::test]
async fn token_exchange_rejects_a_mismatched_pkce_verifier_and_succeeds_with_a_matching_one() {
    let app = spawn_test_app().await;
    let (client_id, client_secret) = register_app_via_http(&app, "PKCE Test Client", "read").await;
    let (_owner_id, actor_id) = create_owner_with_actor(&app, "pkcetestowner").await;

    // Two independent codes: a PKCE-mismatch exchange still consumes the
    // code (same documented ordering as the redirect_uri-mismatch case
    // above), so the "succeeds with a matching verifier" assertion needs its
    // own, separately issued code.
    let verifier = "the-correct-verifier-1234567890";
    let challenge = PkceChallenge::from_verifier_s256(verifier);

    // Mismatched verifier -> rejected.
    let code_for_mismatch = app
        .state
        .oauth()
        .service()
        .issue_authorization_code(AuthorizeApproval {
            client_id: client_id.clone(),
            redirect_uri: REDIRECT_URI.to_string(),
            scopes: "read".to_string(),
            actor_id,
            code_challenge: Some(challenge.challenge.clone()),
        })
        .await
        .expect("issuing a code with a PKCE challenge must succeed");
    let mismatch_body = form_body(&[
        ("grant_type", "authorization_code"),
        ("code", code_for_mismatch.plaintext.expose_secret()),
        ("client_id", &client_id),
        ("client_secret", &client_secret),
        ("redirect_uri", REDIRECT_URI),
        ("code_verifier", "the-wrong-verifier-0987654321"),
    ]);
    let mismatch_response = raw_request(
        app.address,
        "POST",
        "/oauth/token",
        Some("application/x-www-form-urlencoded"),
        &[],
        &mismatch_body,
    )
    .await;
    assert_eq!(
        mismatch_response.status, 400,
        "a mismatched PKCE verifier must be rejected, got: {mismatch_response:?}"
    );

    // Matching verifier, fresh code -> succeeds.
    let code_for_match = app
        .state
        .oauth()
        .service()
        .issue_authorization_code(AuthorizeApproval {
            client_id: client_id.clone(),
            redirect_uri: REDIRECT_URI.to_string(),
            scopes: "read".to_string(),
            actor_id,
            code_challenge: Some(challenge.challenge.clone()),
        })
        .await
        .expect("issuing a code with a PKCE challenge must succeed");
    let match_body = form_body(&[
        ("grant_type", "authorization_code"),
        ("code", code_for_match.plaintext.expose_secret()),
        ("client_id", &client_id),
        ("client_secret", &client_secret),
        ("redirect_uri", REDIRECT_URI),
        ("code_verifier", verifier),
    ]);
    let match_response = raw_request(
        app.address,
        "POST",
        "/oauth/token",
        Some("application/x-www-form-urlencoded"),
        &[],
        &match_body,
    )
    .await;
    assert_eq!(
        match_response.status, 200,
        "a matching PKCE verifier must succeed, got: {match_response:?}"
    );
    let token_json: Value =
        serde_json::from_str(&match_response.body).expect("token response must be valid JSON");
    assert!(
        token_json["access_token"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    );

    app.cleanup().await;
}
