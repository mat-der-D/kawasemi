//! Integration test proving task 7.1's own observable completion condition
//! (`.kiro/specs/accounts-and-instance/tasks.md`, "7.1 アカウント系エンドポイ
//! ントの統合テストを通す") for the `verify_credentials` / `accounts/:id`
//! half of that task's scenario list: "401/403/404... 委譲未登録時の空/既定"
//! against the real, `spawn_test_app`-booted application router
//! (Requirements 2.1, 2.3, 3.1, 3.3, 3.4).
//!
//! design.md's File Structure Plan names this exact filename
//! (`account_show_it.rs`: "verify_credentials / accounts/:id（ローカル/リモ
//! ート/404/任意認証）（統合）") and its own Testing Strategy bullet ("veri
//! fy_credentials: 認証済みで CredentialAccount（source/role 付き）、未認証/
//! スコープ不足で 401/403（2.1, 2.3）。accounts/:id: ローカル/既知リモートを
//! Account で返す、未存在 404、未認証でも公開応答（3.1, 3.2, 3.3, 3.4）。").
//! This file covers the local-account half of that bullet plus 404/optional
//! auth (Requirement 3.2, the known-remote half, is not in task 7.1's own
//! `_Requirements_` list — that is `remote_account_fetch_it.rs`'s concern,
//! task 7.2, `_Depends: 6_`, out of this file's boundary).
//!
//! `tests/accounts_endpoints_wiring_it.rs` (task 6) already proved the
//! scope-enforcement/404 *wiring* itself exists; this file goes further and
//! asserts the full CredentialAccount/Account JSON contract shape produced
//! by the real service layer (`AccountService::verify_credentials`/
//! `show_account`, task 5.1) reachable through that wiring, including the
//! Requirement 1.1/1.5/2.2 field-level assertions (avatar/header never
//! null, `source`/`role` present only on CredentialAccount, zero counts from
//! the unregistered `AccountCountsProvider` default).
//!
//! ## No HTTP client dependency: raw sockets, mirroring established
//! precedent (`tests/auth_scope_it.rs`/`tests/accounts_endpoints_wiring_it.rs`)
//! This crate has no HTTP client dependency (`Cargo.toml`). This file
//! duplicates the `RawResponse`/`raw_request` plumbing (each `tests/*.rs`
//! file is a separate compiled binary with no shared module).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor};
use kawasemi::domain::Id;
use kawasemi::oauth::app_repository::{self, NewApp};
use kawasemi::oauth::model::ScopeSet as PlaceholderScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::test_harness::{TestApp, spawn_test_app};

// ---- raw HTTP plumbing (duplicated across this spec's `tests/*.rs` files
// by established convention; see this file's module doc comment) ----

#[derive(Debug)]
struct RawResponse {
    status: u16,
    #[allow(dead_code)]
    headers: HashMap<String, String>,
    body: String,
}

async fn raw_get(addr: SocketAddr, path: &str, extra_headers: &[(&str, &str)]) -> RawResponse {
    let mut stream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr))
        .await
        .expect("connecting to the test listener must not time out")
        .expect("connect");

    let mut request = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n");
    for (name, value) in extra_headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("Content-Length: 0\r\n\r\n");

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

fn body_json(response: &RawResponse) -> Value {
    serde_json::from_str(&response.body)
        .unwrap_or_else(|e| panic!("response body must be valid JSON: {e}; body: {response:?}"))
}

/// Asserts `response`'s body is the Mastodon-compatible `{"error": ...}`
/// shape every `AppError` renders through (Requirement 10.3).
fn assert_error_shape(response: &RawResponse) {
    let body = body_json(response);
    assert!(
        body.get("error").and_then(Value::as_str).is_some(),
        "expected a Mastodon-compatible {{\"error\": ...}} body, got: {body}"
    );
}

// ---- fixtures (mirrors `tests/accounts_endpoints_wiring_it.rs`'s own
// precedent) ----

async fn register_test_app(app: &TestApp) -> Id {
    let now = app.runtime.clock.now();
    let registered = app_repository::register_app(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        app.state.oauth().token_hash_key(),
        now,
        NewApp {
            name: "Account Show IT Client".to_string(),
            redirect_uris: vec!["https://client.example/callback".to_string()],
            scopes: PlaceholderScopeSet::new(["read", "write"]),
        },
    )
    .await
    .expect("register_app must succeed");
    registered.id
}

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
            display_name: "Account Show IT Actor".to_string(),
            summary: "an account_show_it integration test fixture".to_string(),
        })
        .await
        .expect("creating the owner's actor fixture must succeed");

    actor.id
}

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

// ---- (1) verify_credentials: full CredentialAccount contract + 401/403 ----

#[tokio::test]
async fn verify_credentials_returns_the_full_credential_account_contract_and_enforces_scope() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let actor_id = create_owner_with_actor(&app, "verifyshowowner").await;

    // 401: no Bearer token at all (Requirement 2.3).
    let unauthenticated = raw_get(app.address, "/api/v1/accounts/verify_credentials", &[]).await;
    assert_eq!(unauthenticated.status, 401, "got: {unauthenticated:?}");
    assert_error_shape(&unauthenticated);

    // 403: a valid token lacking `read:accounts` (Requirement 2.3).
    let wrong_scope_token = issue_token(&app, oauth_app_id, actor_id, &["read:statuses"]).await;
    let forbidden = raw_get(
        app.address,
        "/api/v1/accounts/verify_credentials",
        &[("Authorization", &bearer_header(&wrong_scope_token))],
    )
    .await;
    assert_eq!(forbidden.status, 403, "got: {forbidden:?}");
    assert_error_shape(&forbidden);

    // 200: correct scope -> full CredentialAccount contract (Requirement
    // 2.1, 2.2).
    let token = issue_token(&app, oauth_app_id, actor_id, &["read:accounts"]).await;
    let ok = raw_get(
        app.address,
        "/api/v1/accounts/verify_credentials",
        &[("Authorization", &bearer_header(&token))],
    )
    .await;
    assert_eq!(ok.status, 200, "got: {ok:?}");
    let body = body_json(&ok);

    assert_eq!(
        body["id"].as_str(),
        Some(actor_id.as_i64().to_string()).as_deref()
    );
    assert_eq!(body["username"].as_str(), Some("verifyshowowner"));
    assert_eq!(
        body["acct"].as_str(),
        Some("verifyshowowner"),
        "a local actor's acct must carry no domain part, got: {body}"
    );
    assert_eq!(body["locked"].as_bool(), Some(false));
    assert_eq!(body["bot"].as_bool(), Some(false));
    assert_eq!(body["discoverable"].as_bool(), Some(false));
    assert_eq!(body["group"].as_bool(), Some(false));
    assert!(body["created_at"].as_str().is_some());
    assert_eq!(body["note"].as_str(), Some(""));
    assert!(body["url"].as_str().is_some());
    assert!(body["uri"].as_str().is_some());

    // Requirement 1.5: avatar/header are never null, even unset.
    let avatar = body["avatar"]
        .as_str()
        .expect("avatar must never be null (Requirement 1.5)");
    let header = body["header"]
        .as_str()
        .expect("header must never be null (Requirement 1.5)");
    assert!(!avatar.is_empty());
    assert!(!header.is_empty());
    assert_eq!(body["avatar_static"].as_str(), Some(avatar));
    assert_eq!(body["header_static"].as_str(), Some(header));

    // Zero counts: no `AccountCountsProvider` registered on this fresh app.
    assert_eq!(body["followers_count"].as_i64(), Some(0));
    assert_eq!(body["following_count"].as_i64(), Some(0));
    assert_eq!(body["statuses_count"].as_i64(), Some(0));
    assert!(body["last_status_at"].is_null());

    assert_eq!(body["emojis"].as_array().map(Vec::len), Some(0));
    assert_eq!(body["fields"].as_array().map(Vec::len), Some(0));

    // Requirement 2.2: `source`/`role` present only on CredentialAccount.
    let source = body
        .get("source")
        .expect("verify_credentials must return a CredentialAccount (with `source`)");
    assert_eq!(source["privacy"].as_str(), Some("public"));
    assert_eq!(source["sensitive"].as_bool(), Some(false));
    assert!(source["language"].is_null());
    assert_eq!(source["note"].as_str(), Some(""));
    assert_eq!(source["fields"].as_array().map(Vec::len), Some(0));
    assert_eq!(source["follow_requests_count"].as_i64(), Some(0));

    let role = body
        .get("role")
        .expect("CredentialAccount must carry a role");
    assert!(role["id"].as_str().is_some());
    assert!(role["name"].as_str().is_some());

    app.cleanup().await;
}

// ---- (2) accounts/:id: local Account contract, public, 404 for unknown ----

#[tokio::test]
async fn show_account_returns_the_local_account_contract_publicly_and_404s_for_an_unknown_id() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let actor_id = create_owner_with_actor(&app, "targetshowaccount").await;

    // Requirement 3.4: no token at all still gets a full public Account.
    let unauthenticated = raw_get(
        app.address,
        &format!("/api/v1/accounts/{}", actor_id.as_i64()),
        &[],
    )
    .await;
    assert_eq!(unauthenticated.status, 200, "got: {unauthenticated:?}");
    let body = body_json(&unauthenticated);
    assert_eq!(
        body["id"].as_str(),
        Some(actor_id.as_i64().to_string()).as_deref()
    );
    assert_eq!(body["username"].as_str(), Some("targetshowaccount"));
    assert_eq!(body["acct"].as_str(), Some("targetshowaccount"));
    let avatar = body["avatar"]
        .as_str()
        .expect("avatar must never be null (Requirement 1.5)");
    assert!(!avatar.is_empty());
    assert!(
        body.get("source").is_none(),
        "accounts/:id must return a plain Account, not a CredentialAccount, got: {body}"
    );
    assert!(body.get("role").is_none());

    // Requirement 3.4: a present-but-otherwise-unrelated token still gets
    // the same public Account (token presence is optional, not forbidden).
    let some_other_actor = create_owner_with_actor(&app, "viewershowaccount").await;
    let viewer_token = issue_token(&app, oauth_app_id, some_other_actor, &["read:accounts"]).await;
    let with_token = raw_get(
        app.address,
        &format!("/api/v1/accounts/{}", actor_id.as_i64()),
        &[("Authorization", &bearer_header(&viewer_token))],
    )
    .await;
    assert_eq!(with_token.status, 200, "got: {with_token:?}");
    let with_token_body = body_json(&with_token);
    assert_eq!(with_token_body["id"], body["id"]);

    // Requirement 3.3: an id matching neither a local actor nor a known/
    // fetchable remote account is 404, Mastodon-compatible error body.
    let missing = raw_get(app.address, "/api/v1/accounts/999999999", &[]).await;
    assert_eq!(missing.status, 404, "got: {missing:?}");
    assert_error_shape(&missing);

    app.cleanup().await;
}
