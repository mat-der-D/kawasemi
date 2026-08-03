//! Integration tests for task 6.1's own observable completion condition
//! (`.kiro/specs/social-graph/tasks.md`, "6.1 (P) 関係操作の統合テスト") —
//! this file covers exactly the "同一サーバースキップ" bullet of design.md's
//! own "Integration Tests（`spawn_test_app` 上）" Testing Strategy section:
//! "同一サーバースキップ: ローカル間フォローはロック済みでも即時確立、片側
//! リモートは保留（3.1, 3.4）", driven through the real, mounted `POST
//! /api/v1/accounts/:id/follow` handler (`src/social_graph/endpoints.rs`,
//! wired by `src/server.rs`'s `social_graph_router`).
//!
//! `src/social_graph/approval_policy.rs`'s own unit tests already prove
//! `FollowApprovalPolicy::requires_approval`'s judgment in isolation
//! (design.md's Testing Strategy "Unit Tests" bullet) — this file instead
//! proves that judgment is genuinely wired end to end behind the real
//! `follow` HTTP endpoint and its already-reviewed `FollowService::follow`
//! caller (task 3.1), not merely correct as a standalone function.
//!
//! ## Scope: only the `follow`-endpoint side of Requirement 3.4's remote
//! condition
//! Requirement 3.4 states the same-server privilege is withheld "フォロー
//! の送信元または宛先のいずれかがリモートアクターである間" (source *or*
//! target remote). This file exercises the "target remote" half of that
//! disjunction through the real endpoint (a local viewer can genuinely
//! drive that case via `POST .../follow`); the "source remote" half (a
//! remote actor's Follow Activity arriving at a *local* target) requires a
//! genuinely signed inbound Activity POST, which is `InboundHandler`'s own
//! boundary and design.md's own, separate "受信 Activity" Integration Tests
//! bullet — `_Boundary: InboundHandler, ...` names task 6.2, not this
//! task's own `_Boundary: SocialGraphEndpoints, FollowService,
//! FollowRequestService, BlockService, MuteService_`.
//!
//! ## No HTTP client dependency: raw sockets (this crate's established
//! `tests/*_it.rs` convention; duplicated per file since each integration
//! test is its own compiled crate).

use std::net::SocketAddr;
use std::time::Duration;

use serde_json::{Value, json};
use time::OffsetDateTime;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use kawasemi::accounts::model::{ProfileField, ProfilePatch, RemoteAccount};
use kawasemi::accounts::profile_repository::upsert_profile;
use kawasemi::accounts::remote_repository::upsert_remote;
use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor};
use kawasemi::domain::Id;
use kawasemi::oauth::app_repository::{self, NewApp};
use kawasemi::oauth::model::ScopeSet as PlaceholderScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::test_harness::{TestApp, spawn_test_app};

// ---- raw HTTP plumbing -----------------------------------------------------

#[derive(Debug)]
struct RawResponse {
    status: u16,
    body: String,
}

async fn raw_request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> RawResponse {
    let mut stream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr))
        .await
        .expect("connecting to the test listener must not time out")
        .expect("connect");

    let mut request =
        format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    let mut request_bytes = request.into_bytes();
    request_bytes.extend_from_slice(body);

    stream
        .write_all(&request_bytes)
        .await
        .expect("write request");

    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut buf))
        .await
        .expect("read must not time out")
        .expect("read response");

    parse_response(&String::from_utf8_lossy(&buf))
}

async fn raw_post_json(addr: SocketAddr, path: &str, token: &str, body: &Value) -> RawResponse {
    let body_bytes = serde_json::to_vec(body).expect("serializing test request body");
    raw_request(
        addr,
        "POST",
        path,
        &[
            ("Authorization", &bearer_header(token)),
            ("Content-Type", "application/json"),
        ],
        &body_bytes,
    )
    .await
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

fn body_json(response: &RawResponse) -> Value {
    serde_json::from_str(&response.body)
        .unwrap_or_else(|e| panic!("response body must be valid JSON: {e}; body: {response:?}"))
}

fn bearer_header(token: &str) -> String {
    format!("Bearer {token}")
}

// ---- fixtures ---------------------------------------------------------------

async fn register_test_app(app: &TestApp) -> Id {
    let now = app.runtime.clock.now();
    let registered = app_repository::register_app(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        app.state.oauth().token_hash_key(),
        now,
        NewApp {
            name: "same_server_skip_it Client".to_string(),
            redirect_uris: vec!["https://client.example/callback".to_string()],
            scopes: PlaceholderScopeSet::new(["read", "write", "follow"]),
        },
    )
    .await
    .expect("register_app must succeed");
    registered.id
}

async fn create_test_actor(app: &TestApp, handle: &str) -> Id {
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
    upsert_profile(
        &app.pool,
        actor_id,
        ProfilePatch {
            locked: Some(true),
            ..Default::default()
        },
        app.runtime.clock.now(),
    )
    .await
    .expect("locking the test actor's profile must succeed");
}

fn sample_remote_account(
    id: Id,
    actor_uri: &str,
    fetched_at: OffsetDateTime,
    locked: bool,
) -> RemoteAccount {
    RemoteAccount {
        id,
        actor_uri: actor_uri.to_string(),
        username: "remoteactor".to_string(),
        domain: "remote.example".to_string(),
        display_name: "Remote Actor".to_string(),
        note: String::new(),
        url: actor_uri.to_string(),
        avatar_url: None,
        header_url: None,
        fields: Vec::<ProfileField>::new(),
        bot: false,
        locked,
        fetched_at,
    }
}

async fn create_test_remote(app: &TestApp, actor_uri: &str, locked: bool) -> Id {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    upsert_remote(
        &app.pool,
        &sample_remote_account(id, actor_uri, now, locked),
    )
    .await
    .expect("upsert_remote must succeed");
    id
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

async fn follows_row_count(app: &TestApp, follower: (&str, i64), followee: (&str, i64)) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM follows \
         WHERE follower_kind = $1 AND follower_id = $2 \
           AND followee_kind = $3 AND followee_id = $4",
    )
    .bind(follower.0)
    .bind(follower.1)
    .bind(followee.0)
    .bind(followee.1)
    .fetch_one(&app.pool)
    .await
    .expect("counting follows rows must succeed")
}

async fn follow_requests_row_count(
    app: &TestApp,
    requester: (&str, i64),
    target: (&str, i64),
) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM follow_requests \
         WHERE requester_kind = $1 AND requester_id = $2 \
           AND target_kind = $3 AND target_id = $4",
    )
    .bind(requester.0)
    .bind(requester.1)
    .bind(target.0)
    .bind(target.1)
    .fetch_one(&app.pool)
    .await
    .expect("counting follow_requests rows must succeed")
}

// ---- (1) both local, target locked: same-server privilege establishes
// immediately, no pending request (Requirement 3.1, 3.2) ----------------

#[tokio::test]
async fn local_to_local_follow_of_a_locked_target_establishes_immediately() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let follower_id = create_test_actor(&app, "sss_local_follower").await;
    let target_id = create_test_actor(&app, "sss_local_locked_target").await;
    lock_actor(&app, target_id).await;

    let token = issue_token(&app, oauth_app_id, follower_id, &["follow"]).await;
    let response = raw_post_json(
        app.address,
        &format!("/api/v1/accounts/{}/follow", target_id.as_i64()),
        &token,
        &json!({}),
    )
    .await;

    assert_eq!(response.status, 200, "got: {response:?}");
    let body = body_json(&response);
    assert_eq!(
        body["following"].as_bool(),
        Some(true),
        "a locked local target must still be established immediately when both sides are local, got: {body}"
    );
    assert_eq!(
        body["requested"].as_bool(),
        Some(false),
        "the same-server privilege must skip pending entirely, got: {body}"
    );

    assert_eq!(
        follows_row_count(
            &app,
            ("local", follower_id.as_i64()),
            ("local", target_id.as_i64())
        )
        .await,
        1
    );
    assert_eq!(
        follow_requests_row_count(
            &app,
            ("local", follower_id.as_i64()),
            ("local", target_id.as_i64())
        )
        .await,
        0,
        "no pending follow_requests row may be created for a same-server follow"
    );

    app.cleanup().await;
}

// ---- (2) target remote and locked: privilege withheld, request stays
// pending (Requirement 3.4) ------------------------------------------------

#[tokio::test]
async fn local_to_remote_follow_of_a_locked_target_stays_pending() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let follower_id = create_test_actor(&app, "sss_remote_follower").await;
    let actor_uri = "https://remote.example/users/sss_remote_locked_target";
    let target_id = create_test_remote(&app, actor_uri, true).await;

    let token = issue_token(&app, oauth_app_id, follower_id, &["follow"]).await;
    let response = raw_post_json(
        app.address,
        &format!("/api/v1/accounts/{}/follow", target_id.as_i64()),
        &token,
        &json!({}),
    )
    .await;

    assert_eq!(response.status, 200, "got: {response:?}");
    let body = body_json(&response);
    assert_eq!(
        body["following"].as_bool(),
        Some(false),
        "a remote-side target must never be established immediately, got: {body}"
    );
    assert_eq!(
        body["requested"].as_bool(),
        Some(true),
        "the normal approval flow (locked -> pending) must apply once either side is remote, got: {body}"
    );

    assert_eq!(
        follows_row_count(
            &app,
            ("local", follower_id.as_i64()),
            ("remote", target_id.as_i64())
        )
        .await,
        0,
        "no follows row may exist while approval is pending"
    );
    assert_eq!(
        follow_requests_row_count(
            &app,
            ("local", follower_id.as_i64()),
            ("remote", target_id.as_i64())
        )
        .await,
        1,
        "exactly one pending outbound follow_requests row must exist"
    );

    app.cleanup().await;
}

// ---- (3) contrast within a single test: an *unlocked* local target
// establishes immediately regardless of privilege, precisely isolating the
// lock-state variable the same-server privilege is meant to override ----

#[tokio::test]
async fn same_server_privilege_specifically_overrides_lock_not_mere_localness() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let follower_id = create_test_actor(&app, "sss_contrast_follower").await;
    let unlocked_target_id = create_test_actor(&app, "sss_contrast_unlocked").await;
    let locked_target_id = create_test_actor(&app, "sss_contrast_locked").await;
    lock_actor(&app, locked_target_id).await;

    let token = issue_token(&app, oauth_app_id, follower_id, &["follow"]).await;

    let unlocked_response = raw_post_json(
        app.address,
        &format!("/api/v1/accounts/{}/follow", unlocked_target_id.as_i64()),
        &token,
        &json!({}),
    )
    .await;
    assert_eq!(unlocked_response.status, 200, "got: {unlocked_response:?}");
    assert_eq!(
        body_json(&unlocked_response)["following"].as_bool(),
        Some(true)
    );

    let locked_response = raw_post_json(
        app.address,
        &format!("/api/v1/accounts/{}/follow", locked_target_id.as_i64()),
        &token,
        &json!({}),
    )
    .await;
    assert_eq!(locked_response.status, 200, "got: {locked_response:?}");
    assert_eq!(
        body_json(&locked_response)["following"].as_bool(),
        Some(true),
        "the locked local target must establish just as immediately as the unlocked one \
         (same-server privilege applies specifically because both sides are local, not \
         because the target happens to be unlocked)"
    );

    app.cleanup().await;
}
