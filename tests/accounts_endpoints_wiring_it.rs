//! Integration test proving task 6's own observable completion condition
//! (`.kiro/specs/accounts-and-instance/tasks.md`, "6. 全エンドポイントを横断
//! レイヤーに乗せて mount する", `_Boundary: AccountsEndpoints,
//! AccountsModule_`): "各エンドポイントが期待スコープを要求し、未認証で公開
//! 応答する箇所が応答し、全エラーが互換 JSON で返る統合テストが green"
//! (Requirements 2.3, 3.4, 5.5, 6.4, 10.1, 10.2, 10.3, 10.4, 10.5).
//!
//! This file is deliberately scoped to the **wiring** condition above, not
//! the fuller behavioral round-trip coverage design.md's Testing Strategy
//! assigns to task group 7 (`account_show_it.rs`/`account_statuses_it.rs`/
//! `relationships_it.rs`/`update_credentials_it.rs`/`instance_v2_it.rs`/
//! `custom_emojis_it.rs`, `_Depends: 6_`, none of which this file creates or
//! preempts) — mirroring `tests/accounts_module_wiring_it.rs`'s own already-
//! established "wiring-level, not full behavioral" precedent for this exact
//! spec. Concretely, this file proves, through the real, `spawn_test_app`-
//! booted application router (`crate::server::build_router`):
//!
//! 1. Each of `verify_credentials`/`relationships`/`update_credentials`
//!    requires its documented scope (`read:accounts`/`read:follows`/
//!    `write:accounts`, Requirement 10.1): 401 unauthenticated, 403 with a
//!    granted-but-insufficient scope, 200 with the correct scope.
//! 2. Each of `accounts/:id`/`accounts/:id/statuses`/`instance`/
//!    `custom_emojis` answers without a token (Requirement 10.2).
//! 3. Every error response (401/403/404/422) renders as the Mastodon-
//!    compatible `{"error": ...}` body (Requirement 10.3).
//! 4. `accounts/:id/statuses` attaches a proxy-respecting `Link` header when
//!    the registered `AccountStatusesProvider` actually has cursors to link
//!    (Requirement 10.4); `relationships` resolves every repeated `id`
//!    query parameter (this crate's own documented judgment call, see
//!    `crate::accounts::endpoints`'s doc comment).
//! 5. `update_credentials`'s partial update is reflected by a subsequent
//!    `verify_credentials` call (Requirement 6.1/6.5, reusing the same
//!    already-reviewed `AccountService` — this file only proves the HTTP
//!    surface actually calls through to it, not `AccountService`'s own
//!    business rules a second time).
//!
//! ## No HTTP client dependency: raw sockets, mirroring established
//! precedent (`tests/auth_scope_it.rs`/`tests/api_foundation_wiring_it.rs`)
//! This crate has no HTTP client dependency (`Cargo.toml`). This file
//! duplicates the `RawResponse`/`raw_request` plumbing (each `tests/*.rs`
//! file is a separate compiled binary with no shared module).
//!
//! ## Fixtures: real owner/actor/app/token rows, matching
//! `tests/auth_scope_it.rs`'s own precedent
//! Every actor is a real `local_actors` row (`actor::actor_service()::create_actor`)
//! and every token is a real, persisted, hashed `oauth_access_tokens` row
//! (`token_repository::issue_token`) — never a hand-constructed
//! `RequestActorContext` — because these handlers are reached through the
//! real, mounted router (`app.address`), whose `AuthState` (via
//! `src/server.rs`'s `impl FromRef<AppState> for AuthState`) resolves a
//! Bearer token against the real `oauth_access_tokens` table and a real id
//! against the real `local_actors` table.

use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use kawasemi::accounts::{AccountStatusesProvider, StatusesQuery};
use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor};
use kawasemi::api::pagination::Page;
use kawasemi::domain::Id;
use kawasemi::error::AppError;
use kawasemi::oauth::app_repository::{self, NewApp};
use kawasemi::oauth::model::ScopeSet as PlaceholderScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::test_harness::{TestApp, spawn_test_app};

// ---- raw HTTP plumbing (duplicated from tests/auth_scope_it.rs; see this
// file's module doc comment for why) ----

#[derive(Debug)]
struct RawResponse {
    status: u16,
    headers: HashMap<String, String>,
    body: String,
}

async fn raw_request(
    addr: SocketAddr,
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

async fn raw_get(addr: SocketAddr, path: &str, extra_headers: &[(&str, &str)]) -> RawResponse {
    raw_request(addr, "GET", path, None, extra_headers, "").await
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

/// Asserts `response`'s body is the Mastodon-compatible `{"error": ...}`
/// shape every `AppError` renders through (Requirement 10.3), the same
/// assertion pattern `tests/auth_scope_it.rs`'s own
/// `assert_mastodon_error_shape` already established for this spec's sibling
/// api-foundation task.
fn assert_error_shape(response: &RawResponse) {
    let body: Value =
        serde_json::from_str(&response.body).expect("error response body must be valid JSON");
    assert!(
        body.get("error").and_then(Value::as_str).is_some(),
        "expected a Mastodon-compatible {{\"error\": ...}} body, got: {body}"
    );
}

// ---- fixtures (mirrors tests/auth_scope_it.rs's own precedent) ----

/// Registers a real OAuth app directly through the repository (not via
/// HTTP — `tests/auth_scope_it.rs`'s own registration round-trip is not this
/// file's concern), using the app's own real token-hashing key so a token
/// issued against it resolves correctly through the real, mounted router's
/// `AuthState` (`src/server.rs`'s `impl FromRef<AppState> for AuthState`).
async fn register_test_app(app: &TestApp) -> Id {
    let now = app.runtime.clock.now();
    let registered = app_repository::register_app(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        app.state.oauth().token_hash_key(),
        now,
        NewApp {
            name: "Accounts Endpoints Test Client".to_string(),
            redirect_uris: vec!["https://client.example/callback".to_string()],
            scopes: PlaceholderScopeSet::new(["read", "write"]),
        },
    )
    .await
    .expect("register_app must succeed");
    registered.id
}

/// Creates the sole owner fixture and one real local actor belonging to it
/// (mirrors `tests/auth_scope_it.rs`'s own `create_owner_with_actor`).
async fn create_owner_with_actor(app: &TestApp, handle: &str) -> Id {
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
            display_name: "Accounts Endpoints Test Actor".to_string(),
            summary: "an accounts-endpoints wiring integration test fixture".to_string(),
        })
        .await
        .expect("creating the owner's actor fixture must succeed");

    actor.id
}

/// Mints a real, persisted access token bound to `actor_id` with `scopes`
/// (mirrors `tests/auth_scope_it.rs`'s own `issue_token`).
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

fn bearer_header(token: &str) -> String {
    format!("Bearer {token}")
}

/// Builds a minimal `multipart/form-data` body carrying only plain text
/// fields — this file's `update_credentials` coverage never needs a file
/// part, so this stays much smaller than
/// `tests/media_endpoints_it.rs`'s own `MultipartBuilder` (which does).
fn text_multipart_body(fields: &[(&str, &str)]) -> (String, String) {
    const BOUNDARY: &str = "kawasemi-accounts-endpoints-it-boundary";
    let mut body = String::new();
    for (name, value) in fields {
        body.push_str(&format!("--{BOUNDARY}\r\n"));
        body.push_str(&format!(
            "Content-Disposition: form-data; name=\"{name}\"\r\n\r\n"
        ));
        body.push_str(value);
        body.push_str("\r\n");
    }
    body.push_str(&format!("--{BOUNDARY}--\r\n"));
    (body, format!("multipart/form-data; boundary={BOUNDARY}"))
}

/// A test-only `AccountStatusesProvider` returning a fixed, non-empty page
/// with both cursors set, so [`list_statuses_is_public_and_attaches_a_link_header_when_data_exists`]
/// can prove `build_link_header`'s wiring rather than the always-empty
/// `EmptyStatusesProvider` default (which never has cursors to link, and so
/// can never exercise Requirement 10.4's `Link`-header wiring end to end).
struct FixedPageProvider;

impl AccountStatusesProvider for FixedPageProvider {
    fn list_statuses<'a>(
        &'a self,
        _query: &'a StatusesQuery,
    ) -> Pin<Box<dyn Future<Output = Result<Page<serde_json::Value>, AppError>> + Send + 'a>> {
        Box::pin(async move {
            Ok(Page {
                items: vec![
                    serde_json::json!({"id": "2"}),
                    serde_json::json!({"id": "1"}),
                ],
                prev_cursor: Some("2".to_string()),
                next_cursor: Some("1".to_string()),
            })
        })
    }
}

// ---- (1) verify_credentials requires read:accounts ----

#[tokio::test]
async fn verify_credentials_requires_read_accounts_scope() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let actor_id = create_owner_with_actor(&app, "verifycredsowner").await;

    let unauthenticated = raw_get(app.address, "/api/v1/accounts/verify_credentials", &[]).await;
    assert_eq!(unauthenticated.status, 401, "got: {unauthenticated:?}");
    assert_error_shape(&unauthenticated);

    let wrong_scope_token = issue_token(&app, oauth_app_id, actor_id, &["read:statuses"]).await;
    let forbidden = raw_get(
        app.address,
        "/api/v1/accounts/verify_credentials",
        &[("Authorization", &bearer_header(&wrong_scope_token))],
    )
    .await;
    assert_eq!(forbidden.status, 403, "got: {forbidden:?}");
    assert_error_shape(&forbidden);

    let token = issue_token(&app, oauth_app_id, actor_id, &["read:accounts"]).await;
    let ok = raw_get(
        app.address,
        "/api/v1/accounts/verify_credentials",
        &[("Authorization", &bearer_header(&token))],
    )
    .await;
    assert_eq!(ok.status, 200, "got: {ok:?}");
    let body: Value = serde_json::from_str(&ok.body).expect("200 response must be valid JSON");
    assert_eq!(
        body["id"].as_str(),
        Some(actor_id.as_i64().to_string()).as_deref()
    );
    assert!(
        body.get("source").is_some(),
        "verify_credentials must return a CredentialAccount (with `source`), got: {body}"
    );

    app.cleanup().await;
}

// ---- (2) relationships requires read:follows and resolves repeated `id`
// query parameters ----

#[tokio::test]
async fn relationships_requires_read_follows_scope_and_resolves_every_id_parameter() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let actor_id = create_owner_with_actor(&app, "relationshipsviewer").await;
    let target_id = create_owner_with_actor(&app, "relationshipstarget").await;

    let unauthenticated = raw_get(app.address, "/api/v1/accounts/relationships", &[]).await;
    assert_eq!(unauthenticated.status, 401, "got: {unauthenticated:?}");
    assert_error_shape(&unauthenticated);

    let wrong_scope_token = issue_token(&app, oauth_app_id, actor_id, &["read:accounts"]).await;
    let forbidden = raw_get(
        app.address,
        "/api/v1/accounts/relationships",
        &[("Authorization", &bearer_header(&wrong_scope_token))],
    )
    .await;
    assert_eq!(forbidden.status, 403, "got: {forbidden:?}");
    assert_error_shape(&forbidden);

    let token = issue_token(&app, oauth_app_id, actor_id, &["read:follows"]).await;
    let path = format!(
        "/api/v1/accounts/relationships?id={}&id={}",
        actor_id.as_i64(),
        target_id.as_i64()
    );
    let ok = raw_get(
        app.address,
        &path,
        &[("Authorization", &bearer_header(&token))],
    )
    .await;
    assert_eq!(ok.status, 200, "got: {ok:?}");
    let body: Value = serde_json::from_str(&ok.body).expect("200 response must be valid JSON");
    let array = body
        .as_array()
        .expect("relationships must return a JSON array");
    assert_eq!(
        array.len(),
        2,
        "both repeated `id` query parameters must resolve to a Relationship entry, got: {body}"
    );

    app.cleanup().await;
}

// ---- (3) update_credentials requires write:accounts and its partial
// update is reflected by a subsequent verify_credentials call ----

#[tokio::test]
async fn update_credentials_requires_write_accounts_scope_and_is_reflected_by_verify_credentials() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let actor_id = create_owner_with_actor(&app, "updatecredsowner").await;
    let (body, content_type) = text_multipart_body(&[("display_name", "New Display Name")]);

    let unauthenticated = raw_request(
        app.address,
        "PATCH",
        "/api/v1/accounts/update_credentials",
        Some(&content_type),
        &[],
        &body,
    )
    .await;
    assert_eq!(unauthenticated.status, 401, "got: {unauthenticated:?}");
    assert_error_shape(&unauthenticated);

    let wrong_scope_token = issue_token(&app, oauth_app_id, actor_id, &["read:accounts"]).await;
    let forbidden = raw_request(
        app.address,
        "PATCH",
        "/api/v1/accounts/update_credentials",
        Some(&content_type),
        &[("Authorization", &bearer_header(&wrong_scope_token))],
        &body,
    )
    .await;
    assert_eq!(forbidden.status, 403, "got: {forbidden:?}");
    assert_error_shape(&forbidden);

    // Grants both `write:accounts` (for the update itself) and
    // `read:accounts` (to re-check via `verify_credentials` below, which
    // requires that separate scope) on the same token — the reflection
    // check is about `AccountService`'s persistence, not about re-proving
    // `require_scope`'s already-covered scope-separation behavior above.
    let token = issue_token(
        &app,
        oauth_app_id,
        actor_id,
        &["write:accounts", "read:accounts"],
    )
    .await;
    let auth_header = bearer_header(&token);
    let ok = raw_request(
        app.address,
        "PATCH",
        "/api/v1/accounts/update_credentials",
        Some(&content_type),
        &[("Authorization", &auth_header)],
        &body,
    )
    .await;
    assert_eq!(ok.status, 200, "got: {ok:?}");
    let updated: Value = serde_json::from_str(&ok.body).expect("200 response must be valid JSON");
    assert_eq!(updated["display_name"].as_str(), Some("New Display Name"));

    let verify = raw_get(
        app.address,
        "/api/v1/accounts/verify_credentials",
        &[("Authorization", &auth_header)],
    )
    .await;
    assert_eq!(verify.status, 200, "got: {verify:?}");
    let verify_body: Value =
        serde_json::from_str(&verify.body).expect("200 response must be valid JSON");
    assert_eq!(
        verify_body["display_name"].as_str(),
        Some("New Display Name"),
        "update_credentials's partial update must be reflected by a subsequent \
         verify_credentials call (Requirement 6.5), got: {verify_body}"
    );

    app.cleanup().await;
}

// ---- (4) accounts/:id is public and 404s for an unresolvable id ----

#[tokio::test]
async fn show_account_is_public_and_404s_for_an_unknown_id() {
    let app = spawn_test_app().await;
    let actor_id = create_owner_with_actor(&app, "showaccountowner").await;

    let ok = raw_get(
        app.address,
        &format!("/api/v1/accounts/{}", actor_id.as_i64()),
        &[],
    )
    .await;
    assert_eq!(ok.status, 200, "got: {ok:?}");
    let body: Value = serde_json::from_str(&ok.body).expect("200 response must be valid JSON");
    assert_eq!(
        body["id"].as_str(),
        Some(actor_id.as_i64().to_string()).as_deref()
    );

    let missing = raw_get(app.address, "/api/v1/accounts/999999999", &[]).await;
    assert_eq!(missing.status, 404, "got: {missing:?}");
    assert_error_shape(&missing);

    app.cleanup().await;
}

// ---- (5) accounts/:id/statuses is public and attaches a proxy-respecting
// Link header once a real provider has cursors to link ----

#[tokio::test]
async fn list_statuses_is_public_and_attaches_a_link_header_when_data_exists() {
    let app = spawn_test_app().await;
    let actor_id = create_owner_with_actor(&app, "statusesowner").await;

    app.state
        .accounts()
        .ports()
        .set_statuses_provider(Arc::new(FixedPageProvider));

    let path = format!("/api/v1/accounts/{}/statuses?limit=5", actor_id.as_i64());
    let response = raw_request(
        app.address,
        "GET",
        &path,
        None,
        &[
            ("X-Forwarded-Proto", "https"),
            ("X-Forwarded-Host", "forwarded.example"),
        ],
        "",
    )
    .await;
    assert_eq!(response.status, 200, "got: {response:?}");
    let body: Value =
        serde_json::from_str(&response.body).expect("200 response must be valid JSON");
    assert_eq!(
        body.as_array().map(Vec::len),
        Some(2),
        "expected the registered provider's two items, got: {body}"
    );

    let link = response
        .headers
        .get("link")
        .expect("a page with both cursors must carry a Link header (Requirement 10.4)");
    assert!(
        link.contains("https://forwarded.example"),
        "Link header must respect X-Forwarded-Proto/X-Forwarded-Host, got: {link}"
    );
    assert!(
        link.contains("limit=5"),
        "Link header must preserve the caller's own limit, got: {link}"
    );

    // Unauthenticated the whole way through (Requirement 3.4, 10.2): no
    // Authorization header was ever sent above.

    app.cleanup().await;
}

// ---- (6) instance v2 / custom_emojis are fully public ----

#[tokio::test]
async fn instance_v2_and_custom_emojis_are_public() {
    let app = spawn_test_app().await;

    let instance = raw_get(app.address, "/api/v2/instance", &[]).await;
    assert_eq!(instance.status, 200, "got: {instance:?}");
    let instance_body: Value =
        serde_json::from_str(&instance.body).expect("200 response must be valid JSON");
    assert!(instance_body.get("domain").is_some());
    assert!(instance_body.get("configuration").is_some());

    let emojis = raw_get(app.address, "/api/v1/custom_emojis", &[]).await;
    assert_eq!(emojis.status, 200, "got: {emojis:?}");
    let emojis_body: Value =
        serde_json::from_str(&emojis.body).expect("200 response must be valid JSON");
    assert!(emojis_body.is_array());

    app.cleanup().await;
}

// ---- (7) every error response renders as Mastodon-compatible JSON,
// across every distinct error path this task's endpoints can produce ----

#[tokio::test]
async fn every_error_response_renders_as_mastodon_compatible_json() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let actor_id = create_owner_with_actor(&app, "errorshapeowner").await;

    // 401: missing Bearer token.
    let unauthenticated = raw_get(app.address, "/api/v1/accounts/verify_credentials", &[]).await;
    assert_eq!(unauthenticated.status, 401);
    assert_error_shape(&unauthenticated);

    // 403: valid token, insufficient scope.
    let wrong_scope_token = issue_token(&app, oauth_app_id, actor_id, &["read:statuses"]).await;
    let forbidden = raw_get(
        app.address,
        "/api/v1/accounts/verify_credentials",
        &[("Authorization", &bearer_header(&wrong_scope_token))],
    )
    .await;
    assert_eq!(forbidden.status, 403);
    assert_error_shape(&forbidden);

    // 404: an id that resolves to no local/known-remote/fetchable account.
    let not_found = raw_get(app.address, "/api/v1/accounts/999999999", &[]).await;
    assert_eq!(not_found.status, 404);
    assert_error_shape(&not_found);

    // 422: a validation violation update_credentials's already-reviewed
    // `AccountService` rejects (an unparseable `locked` boolean, this
    // module's own `parse_loose_bool` judgment call).
    let write_token = issue_token(&app, oauth_app_id, actor_id, &["write:accounts"]).await;
    let (body, content_type) = text_multipart_body(&[("locked", "not-a-boolean")]);
    let unprocessable = raw_request(
        app.address,
        "PATCH",
        "/api/v1/accounts/update_credentials",
        Some(&content_type),
        &[("Authorization", &bearer_header(&write_token))],
        &body,
    )
    .await;
    assert_eq!(unprocessable.status, 422, "got: {unprocessable:?}");
    assert_error_shape(&unprocessable);

    app.cleanup().await;
}
