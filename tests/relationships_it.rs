//! Integration test proving task 7.1's own observable completion condition
//! for `relationships` (`.kiro/specs/accounts-and-instance/tasks.md`,
//! "7.1 アカウント系エンドポイントの統合テストを通す": "401/403...委譲未登
//! 録時の空/既定") against the real, `spawn_test_app`-booted application
//! router (Requirement 5.1, 5.4).
//!
//! design.md's File Structure Plan names this exact filename
//! (`relationships_it.rs`: "relationships（複数 id・provider 未登録時の既
//! 定・スコープ）（統合）") and its own Testing Strategy bullet
//! ("relationships: 複数 id を配列で返す、provider 未登録で全既定、スコープ
//! 不足で 403（5.1, 5.4, 5.5）").
//!
//! `tests/accounts_endpoints_wiring_it.rs` (task 6) already proved the
//! scope-enforcement wiring and a basic two-id array-length smoke case; this
//! file goes further:
//! 1. The unregistered `RelationshipStateProvider` default returns the full
//!    Requirement 5.4 "no relationship" value (every flag `false`, `note`
//!    empty) for every resolved id, not merely a non-empty array.
//! 2. Both accepted repeated-id wire spellings (`id=`/`id[]=`, this
//!    module's own documented judgment call, `src/accounts/endpoints.rs`)
//!    resolve every id, in request order.
//! 3. An id that resolves to no known account is silently omitted from the
//!    response array rather than failing the whole batch
//!    (`AccountService::relationships`'s own documented judgment call).
//! 4. A real, registered `RelationshipStateProvider` is actually consulted
//!    end to end through the live HTTP surface (not just at the `ports.rs`
//!    unit level).
//!
//! ## No HTTP client dependency: raw sockets (see sibling `tests/*.rs` files
//! for this crate's established rationale).

use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use kawasemi::accounts::{RelationshipStateProvider, RelationshipView};
use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor};
use kawasemi::domain::{AccountRef, Id};
use kawasemi::error::AppError;
use kawasemi::oauth::app_repository::{self, NewApp};
use kawasemi::oauth::model::ScopeSet as PlaceholderScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::test_harness::{TestApp, spawn_test_app};

// ---- raw HTTP plumbing ----

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

fn assert_error_shape(response: &RawResponse) {
    let body = body_json(response);
    assert!(
        body.get("error").and_then(Value::as_str).is_some(),
        "expected a Mastodon-compatible {{\"error\": ...}} body, got: {body}"
    );
}

// ---- fixtures ----

async fn register_test_app(app: &TestApp) -> Id {
    let now = app.runtime.clock.now();
    let registered = app_repository::register_app(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        app.state.oauth().token_hash_key(),
        now,
        NewApp {
            name: "Relationships IT Client".to_string(),
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
            display_name: "Relationships IT Actor".to_string(),
            summary: "a relationships_it integration test fixture".to_string(),
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

// ---- (1) default provider: every resolved id gets the full Requirement
// 5.4 "no relationship" value, in request order ----

#[tokio::test]
async fn relationships_returns_the_full_no_relationship_default_for_every_id_in_order() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_owner_with_actor(&app, "relviewerdefault").await;
    let target_a = create_owner_with_actor(&app, "reltargetadefault").await;
    let target_b = create_owner_with_actor(&app, "reltargetbdefault").await;

    let token = issue_token(&app, oauth_app_id, viewer_id, &["read:follows"]).await;
    let path = format!(
        "/api/v1/accounts/relationships?id={}&id={}",
        target_a.as_i64(),
        target_b.as_i64()
    );
    let response = raw_get(
        app.address,
        &path,
        &[("Authorization", &bearer_header(&token))],
    )
    .await;
    assert_eq!(response.status, 200, "got: {response:?}");
    let body = body_json(&response);
    let array = body
        .as_array()
        .expect("relationships must return a JSON array");
    assert_eq!(array.len(), 2, "got: {body}");

    // Request order preserved.
    assert_eq!(
        array[0]["id"].as_str(),
        Some(target_a.as_i64().to_string()).as_deref()
    );
    assert_eq!(
        array[1]["id"].as_str(),
        Some(target_b.as_i64().to_string()).as_deref()
    );

    for entry in array {
        assert_eq!(entry["following"].as_bool(), Some(false));
        assert_eq!(entry["showing_reblogs"].as_bool(), Some(false));
        assert_eq!(entry["notifying"].as_bool(), Some(false));
        assert_eq!(entry["languages"].as_array().map(Vec::len), Some(0));
        assert_eq!(entry["followed_by"].as_bool(), Some(false));
        assert_eq!(entry["blocking"].as_bool(), Some(false));
        assert_eq!(entry["blocked_by"].as_bool(), Some(false));
        assert_eq!(entry["muting"].as_bool(), Some(false));
        assert_eq!(entry["muting_notifications"].as_bool(), Some(false));
        assert_eq!(entry["requested"].as_bool(), Some(false));
        assert_eq!(entry["requested_by"].as_bool(), Some(false));
        assert_eq!(entry["domain_blocking"].as_bool(), Some(false));
        assert_eq!(entry["endorsed"].as_bool(), Some(false));
        assert_eq!(entry["note"].as_str(), Some(""));
    }

    app.cleanup().await;
}

// ---- (2) both repeated-id wire spellings resolve every id ----

#[tokio::test]
async fn relationships_accepts_both_id_and_bracketed_id_repeated_query_spellings() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_owner_with_actor(&app, "relviewerbracket").await;
    let target_a = create_owner_with_actor(&app, "reltargetabracket").await;
    let target_b = create_owner_with_actor(&app, "reltargetbbracket").await;

    let token = issue_token(&app, oauth_app_id, viewer_id, &["read:follows"]).await;

    let plain_path = format!(
        "/api/v1/accounts/relationships?id={}&id={}",
        target_a.as_i64(),
        target_b.as_i64()
    );
    let plain = raw_get(
        app.address,
        &plain_path,
        &[("Authorization", &bearer_header(&token))],
    )
    .await;
    assert_eq!(plain.status, 200, "got: {plain:?}");
    assert_eq!(body_json(&plain).as_array().map(Vec::len), Some(2));

    let bracketed_path = format!(
        "/api/v1/accounts/relationships?id[]={}&id[]={}",
        target_a.as_i64(),
        target_b.as_i64()
    );
    let bracketed = raw_get(
        app.address,
        &bracketed_path,
        &[("Authorization", &bearer_header(&token))],
    )
    .await;
    assert_eq!(bracketed.status, 200, "got: {bracketed:?}");
    let bracketed_body = body_json(&bracketed);
    assert_eq!(
        bracketed_body.as_array().map(Vec::len),
        Some(2),
        "id[]=...&id[]=... must resolve every id just like id=...&id=..., got: {bracketed_body}"
    );

    app.cleanup().await;
}

// ---- (3) an id resolving to no account is silently omitted, not a
// batch-wide failure ----

#[tokio::test]
async fn relationships_silently_omits_an_id_that_resolves_to_no_known_account() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_owner_with_actor(&app, "relviewerunresolvable").await;
    let target = create_owner_with_actor(&app, "reltargetunresolvable").await;

    let token = issue_token(&app, oauth_app_id, viewer_id, &["read:follows"]).await;
    let path = format!(
        "/api/v1/accounts/relationships?id={}&id=999999999",
        target.as_i64()
    );
    let response = raw_get(
        app.address,
        &path,
        &[("Authorization", &bearer_header(&token))],
    )
    .await;
    assert_eq!(response.status, 200, "got: {response:?}");
    let body = body_json(&response);
    let array = body
        .as_array()
        .expect("relationships must return a JSON array");
    assert_eq!(
        array.len(),
        1,
        "the unresolvable id must be silently dropped, not fail the whole batch, got: {body}"
    );
    assert_eq!(
        array[0]["id"].as_str(),
        Some(target.as_i64().to_string()).as_deref()
    );

    app.cleanup().await;
}

// ---- (4) a real, registered RelationshipStateProvider is actually
// consulted end to end ----

struct FixedTrueProvider;

impl RelationshipStateProvider for FixedTrueProvider {
    fn relationships<'a>(
        &'a self,
        _viewer: Id,
        targets: &'a [AccountRef],
    ) -> Pin<Box<dyn Future<Output = Result<Vec<RelationshipView>, AppError>> + Send + 'a>> {
        Box::pin(async move {
            Ok(targets
                .iter()
                .map(|target| {
                    let id = match *target {
                        AccountRef::Local(id) => id,
                        AccountRef::Remote(id) => id,
                    };
                    RelationshipView {
                        id,
                        following: true,
                        showing_reblogs: true,
                        notifying: false,
                        languages: Vec::new(),
                        followed_by: true,
                        blocking: false,
                        blocked_by: false,
                        muting: false,
                        muting_notifications: false,
                        requested: false,
                        requested_by: false,
                        domain_blocking: false,
                        endorsed: false,
                        note: "registered provider".to_string(),
                    }
                })
                .collect())
        })
    }
}

#[tokio::test]
async fn relationships_uses_a_registered_provider_end_to_end_through_the_real_router() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_owner_with_actor(&app, "relviewerregistered").await;
    let target = create_owner_with_actor(&app, "reltargetregistered").await;

    app.state
        .accounts()
        .ports()
        .set_relationship_provider(Arc::new(FixedTrueProvider));

    let token = issue_token(&app, oauth_app_id, viewer_id, &["read:follows"]).await;
    let response = raw_get(
        app.address,
        &format!("/api/v1/accounts/relationships?id={}", target.as_i64()),
        &[("Authorization", &bearer_header(&token))],
    )
    .await;
    assert_eq!(response.status, 200, "got: {response:?}");
    let body = body_json(&response);
    let array = body.as_array().expect("relationships must be an array");
    assert_eq!(array.len(), 1, "got: {body}");
    assert_eq!(array[0]["following"].as_bool(), Some(true));
    assert_eq!(array[0]["followed_by"].as_bool(), Some(true));
    assert_eq!(array[0]["note"].as_str(), Some("registered provider"));

    app.cleanup().await;
}

// ---- (5) scope enforcement: 401 unauthenticated, 403 insufficient scope
// (Requirement 5.5, this file's own self-contained coverage of the
// cross-cutting scope discipline `tests/accounts_endpoints_wiring_it.rs`
// already proves, so this file does not depend on that other file) ----

#[tokio::test]
async fn relationships_requires_read_follows_scope() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_owner_with_actor(&app, "relviewerscope").await;

    let unauthenticated = raw_get(app.address, "/api/v1/accounts/relationships?id=1", &[]).await;
    assert_eq!(unauthenticated.status, 401, "got: {unauthenticated:?}");
    assert_error_shape(&unauthenticated);

    let wrong_scope_token = issue_token(&app, oauth_app_id, viewer_id, &["read:accounts"]).await;
    let forbidden = raw_get(
        app.address,
        "/api/v1/accounts/relationships?id=1",
        &[("Authorization", &bearer_header(&wrong_scope_token))],
    )
    .await;
    assert_eq!(forbidden.status, 403, "got: {forbidden:?}");
    assert_error_shape(&forbidden);

    app.cleanup().await;
}
