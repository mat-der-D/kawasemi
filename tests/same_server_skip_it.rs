//! Integration tests for social-graph's same-server approval-skip privilege
//! (`.kiro/specs/social-graph/tasks.md`, task 6.1 "関係操作の統合テスト",
//! `_Boundary: SocialGraphEndpoints, FollowService, FollowRequestService,
//! BlockService, MuteService_`), driven against the real, `spawn_test_app`-
//! booted application router.
//!
//! design.md's File Structure Plan names this exact filename
//! (`same_server_skip_it.rs`: "同一サーバー承認スキップ特権（ローカル間は
//! 即時確立、片側リモートは通常承認）（統合）") and its own Testing
//! Strategy bullet ("同一サーバースキップ: ローカル間フォローはロック済み
//! でも即時確立、片側リモートは保留（3.1, 3.4）").
//!
//! Covers Requirements 3.1, 3.4 (this task's own assigned subset; 3.2/3.3
//! are `FollowApprovalPolicy`'s own unit-level coverage from task 2.1, out
//! of this integration task's boundary).
//!
//! Both scenarios below drive `POST /api/v1/accounts/:id/follow` only (this
//! task's own boundary, `FollowService`) — see `tests/follow_request_it.rs`'s
//! own doc comment for why the complementary "remote source -> local
//! target" direction of Requirement 3.4 is not (and cannot be, from this
//! task's own boundary) exercised here: it requires
//! `SocialGraphInboundHandler` (task 6.2's boundary), and
//! `FollowApprovalPolicy::requires_approval`'s own internal same-server
//! judgment is identical regardless of which caller (`FollowService` here,
//! `SocialGraphInboundHandler` there) invokes it (design.md's own
//! "呼び出し側は同一サーバー判定ロジックを持たない" invariant, already unit-
//! tested at task 2.1) — so this file's local-caller-only coverage of
//! "either side remote -> normal approval flow" is not a narrower claim than
//! Requirement 3.4 itself.
//!
//! ## No HTTP client dependency: raw sockets (this crate's established
//! per-file-duplicated convention).

use std::collections::HashMap;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use kawasemi::accounts::model::{ProfileField, RemoteAccount};
use kawasemi::accounts::profile_repository;
use kawasemi::accounts::remote_repository::upsert_remote;
use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor};
use kawasemi::domain::Id;
use kawasemi::oauth::app_repository::{self, NewApp};
use kawasemi::oauth::model::ScopeSet as PlaceholderScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::test_harness::{TestApp, spawn_test_app};

// ---- raw HTTP plumbing (duplicated per-file; see this module's doc
// comment) ----

#[derive(Debug)]
struct RawResponse {
    status: u16,
    #[allow(dead_code)]
    headers: HashMap<String, String>,
    body: String,
}

async fn raw_request(
    addr: std::net::SocketAddr,
    method: &str,
    path: &str,
    extra_headers: &[(&str, &str)],
) -> RawResponse {
    let mut stream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr))
        .await
        .expect("connecting to the test listener must not time out")
        .expect("connect");

    let mut request =
        format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n");
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

async fn raw_post(
    addr: std::net::SocketAddr,
    path: &str,
    extra_headers: &[(&str, &str)],
) -> RawResponse {
    raw_request(addr, "POST", path, extra_headers).await
}

async fn raw_get(
    addr: std::net::SocketAddr,
    path: &str,
    extra_headers: &[(&str, &str)],
) -> RawResponse {
    raw_request(addr, "GET", path, extra_headers).await
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
            name: "Same Server Skip IT Client".to_string(),
            redirect_uris: vec!["https://client.example/callback".to_string()],
            scopes: PlaceholderScopeSet::new(["read", "write", "follow"]),
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
            display_name: "Same Server Skip IT Actor".to_string(),
            summary: "a same_server_skip_it integration test fixture".to_string(),
        })
        .await
        .expect("creating the owner's actor fixture must succeed");

    actor.id
}

async fn lock_actor(app: &TestApp, actor_id: Id) {
    let now = app.runtime.clock.now();
    profile_repository::upsert_profile(
        &app.pool,
        actor_id,
        kawasemi::accounts::model::ProfilePatch {
            locked: Some(true),
            ..Default::default()
        },
        now,
    )
    .await
    .expect("locking the actor's profile must succeed");
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

async fn create_remote_account_with_id(app: &TestApp, id: Id, actor_uri: &str, locked: bool) {
    upsert_remote(
        &app.pool,
        &RemoteAccount {
            id,
            actor_uri: actor_uri.to_string(),
            username: "same_server_remote".to_string(),
            domain: "remote.example".to_string(),
            display_name: "Same Server Skip Remote".to_string(),
            note: String::new(),
            url: actor_uri.to_string(),
            avatar_url: None,
            header_url: None,
            fields: Vec::<ProfileField>::new(),
            bot: false,
            locked,
            fetched_at: app.runtime.clock.now(),
        },
    )
    .await
    .expect("seeding the cached remote account must succeed");
}

async fn follow(app: &TestApp, token: &str, target_id: Id) -> RawResponse {
    let path = format!("/api/v1/accounts/{}/follow", target_id.as_i64());
    raw_post(
        app.address,
        &path,
        &[("Authorization", &bearer_header(token))],
    )
    .await
}

// ==========================================================================
// (1) Requirement 3.1: local -> local follow bypasses the target's lock,
// establishing immediately (no pending request created)
// ==========================================================================

#[tokio::test]
async fn local_to_local_follow_bypasses_lock_and_establishes_immediately() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_owner_with_actor(&app, "sss_local_viewer").await;
    let target_id = create_owner_with_actor(&app, "sss_local_target").await;
    lock_actor(&app, target_id).await;
    let viewer_token = issue_token(&app, oauth_app_id, viewer_id, &["follow"]).await;

    let response = follow(&app, &viewer_token, target_id).await;
    assert_eq!(response.status, 200, "got: {response:?}");
    let body = body_json(&response);
    assert_eq!(
        body["following"].as_bool(),
        Some(true),
        "same-server follow to a locked target must establish immediately, got: {body}"
    );
    assert_eq!(
        body["requested"].as_bool(),
        Some(false),
        "same-server follow must never leave a pending request behind, got: {body}"
    );

    // The target's own pending-request inbox must be empty -- no approval
    // was ever required, so nothing was ever recorded as pending.
    let target_token = issue_token(&app, oauth_app_id, target_id, &["follow"]).await;
    let list_response = raw_get(
        app.address,
        "/api/v1/follow_requests",
        &[("Authorization", &bearer_header(&target_token))],
    )
    .await;
    assert_eq!(list_response.status, 200, "got: {list_response:?}");
    let list_body = body_json(&list_response);
    assert_eq!(
        list_body.as_array().map(Vec::len),
        Some(0),
        "same-server follow must never create a pending follow request, got: {list_body}"
    );

    app.cleanup().await;
}

// ==========================================================================
// (2) Requirement 3.4: local -> remote locked follow does NOT get the
// same-server privilege -- normal approval flow (pending)
// ==========================================================================

#[tokio::test]
async fn local_to_remote_locked_follow_creates_pending_request_not_established() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_owner_with_actor(&app, "sss_remote_viewer").await;
    let token = issue_token(&app, oauth_app_id, viewer_id, &["follow"]).await;

    let remote_id = app.runtime.ids.next_id();
    let actor_uri = "https://remote.example/users/sss_remote_locked_target";
    create_remote_account_with_id(&app, remote_id, actor_uri, true).await;

    let response = follow(&app, &token, remote_id).await;
    assert_eq!(response.status, 200, "got: {response:?}");
    let body = body_json(&response);
    assert_eq!(
        body["following"].as_bool(),
        Some(false),
        "a remote locked target must not be established immediately, got: {body}"
    );
    assert_eq!(
        body["requested"].as_bool(),
        Some(true),
        "a remote locked target must go through the normal pending-approval flow, got: {body}"
    );

    app.cleanup().await;
}
