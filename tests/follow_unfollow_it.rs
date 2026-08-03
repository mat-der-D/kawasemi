//! Integration tests for task 6.1's own observable completion condition
//! (`.kiro/specs/social-graph/tasks.md`, "6.1 (P) 関係操作の統合テスト":
//! "follow/unfollow（ローカル/リモート・冪等・自己拒否・オプション反映）...
//! が上記シナリオがエンドポイント経由で期待どおりの Relationship 応答と状態
//! 遷移・配送依頼を起こすことをテストで確認できる状態") against the real,
//! `spawn_test_app`-booted application router (Requirements 1.1, 1.2, 1.4,
//! 1.5, 1.6, 1.7, 1.8, 10.2, 10.5).
//!
//! design.md's File Structure Plan splits task 6.1's scenarios into several
//! files; this one covers exactly the `follow`/`unfollow` bullet of design.md's
//! own "Integration Tests（`spawn_test_app` 上）" Testing Strategy section:
//! "follow/unfollow: ローカル/リモートで Relationship 応答、重複フォロー冪
//! 等、自己フォロー拒否、オプション（reblogs/notify/languages）反映（1.1,
//! 1.5, 1.6, 1.7）". `tests/same_server_skip_it.rs` covers Requirement 3.x
//! specifically; `tests/follow_request_it.rs` covers Requirement 2.x
//! (pending/authorize/reject); `tests/mute_block_it.rs` covers Requirements
//! 4.x/5.x.
//!
//! Every scenario below drives the real, mounted `POST
//! /api/v1/accounts/:id/follow`/`.../unfollow` handlers
//! (`src/social_graph/endpoints.rs`, wired by `src/server.rs`'s
//! `social_graph_router`) through the real socket `spawn_test_app` binds,
//! never a service call or a test-local router — this is deliberately an
//! end-to-end proof, not a re-run of `follow_service/tests.rs`'s already-
//! reviewed unit-level coverage. Local delivery is synchronous/in-process
//! (`crate::federation::outbound::sink::LocalDeliverySink::dispatch` awaits
//! `InboxService::process_local` directly, `src/federation/outbound/
//! sink.rs`), so a local-target scenario's follow-on state is already
//! settled by the time the HTTP response returns; remote delivery is
//! queue-backed (`HttpDeliverySink` enqueues a `delivery_jobs` row and
//! returns immediately, `src/federation/outbound/delivery.rs`), so remote
//! scenarios assert against that row directly (mirrors
//! `tests/federation_bootstrap_it.rs::delivery_job_exists`'s established
//! convention) rather than waiting for the delivery worker to actually
//! attempt a network send to an unreachable test host.
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

use kawasemi::accounts::model::{ProfileField, RemoteAccount};
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

async fn raw_post(addr: SocketAddr, path: &str, headers: &[(&str, &str)]) -> RawResponse {
    raw_request(addr, "POST", path, headers, b"").await
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
    // Headers are not needed by this file's own assertions (unlike
    // `tests/follow_request_it.rs`, which reads `Link`) -- only status/body
    // are kept.
    RawResponse {
        status,
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
            name: "follow_unfollow_it Client".to_string(),
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
            display_name: "Follow Unfollow IT Actor".to_string(),
            summary: "a follow_unfollow_it integration test fixture".to_string(),
        })
        .await
        .expect("creating the owner's actor fixture must succeed");

    actor.id
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

// ---- DB assertions (mirrors `tests/federation_bootstrap_it.rs`'s own
// `delivery_job_exists` convention: the *action* goes through the real HTTP
// endpoint; verifying its side effect on a table the endpoint itself does
// not expose is a well-established pattern in this crate's own `tests/`
// directory) ----

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

async fn delivery_job_count_for_type(
    app: &TestApp,
    target_inbox: &str,
    activity_type: &str,
) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM delivery_jobs \
         WHERE target_inbox = $1 AND activity->>'type' = $2",
    )
    .bind(target_inbox)
    .bind(activity_type)
    .fetch_one(&app.pool)
    .await
    .expect("counting delivery_jobs rows must succeed")
}

fn remote_inbox(actor_uri: &str) -> String {
    // Mirrors `follow_service.rs`'s own documented interim convention
    // (`.kiro/specs/social-graph/tasks.md`'s Implementation Notes, task
    // 3.1): `RemoteAccount` has no persisted `inbox_uri` field yet, so
    // `resolve_target` falls back to `"{actor_uri}/inbox"`.
    format!("{actor_uri}/inbox")
}

// ---- (1) local target: establishes and returns Relationship -----------

#[tokio::test]
async fn follow_local_target_establishes_relationship_and_persists_a_follows_row() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_test_actor(&app, "fu_local_viewer").await;
    let target_id = create_test_actor(&app, "fu_local_target").await;

    let token = issue_token(&app, oauth_app_id, viewer_id, &["follow"]).await;
    let response = raw_post_json(
        app.address,
        &format!("/api/v1/accounts/{}/follow", target_id.as_i64()),
        &token,
        &json!({}),
    )
    .await;

    assert_eq!(response.status, 200, "got: {response:?}");
    let body = body_json(&response);
    assert_eq!(body["following"].as_bool(), Some(true), "got: {body}");
    assert_eq!(body["requested"].as_bool(), Some(false), "got: {body}");

    assert_eq!(
        follows_row_count(
            &app,
            ("local", viewer_id.as_i64()),
            ("local", target_id.as_i64())
        )
        .await,
        1,
        "exactly one follows row must be persisted"
    );

    app.cleanup().await;
}

// ---- (2) remote target: same Follow Activity, delivered via the queue --

#[tokio::test]
async fn follow_remote_target_establishes_relationship_and_enqueues_a_follow_delivery_job() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_test_actor(&app, "fu_remote_viewer").await;
    let actor_uri = "https://remote.example/users/fu_remote_target";
    let target_id = create_test_remote(&app, actor_uri, false).await;

    let token = issue_token(&app, oauth_app_id, viewer_id, &["follow"]).await;
    let response = raw_post_json(
        app.address,
        &format!("/api/v1/accounts/{}/follow", target_id.as_i64()),
        &token,
        &json!({}),
    )
    .await;

    assert_eq!(response.status, 200, "got: {response:?}");
    let body = body_json(&response);
    assert_eq!(body["following"].as_bool(), Some(true), "got: {body}");

    assert_eq!(
        follows_row_count(
            &app,
            ("local", viewer_id.as_i64()),
            ("remote", target_id.as_i64())
        )
        .await,
        1
    );
    assert_eq!(
        delivery_job_count_for_type(&app, &remote_inbox(actor_uri), "Follow").await,
        1,
        "a Follow Activity must be enqueued for the remote target's inbox"
    );

    app.cleanup().await;
}

// ---- (3) follow options reflected: reblogs/notify/languages -----------

#[tokio::test]
async fn follow_reflects_reblogs_notify_and_languages_options() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_test_actor(&app, "fu_opts_viewer").await;
    let target_id = create_test_actor(&app, "fu_opts_target").await;

    let token = issue_token(&app, oauth_app_id, viewer_id, &["follow"]).await;
    let response = raw_post_json(
        app.address,
        &format!("/api/v1/accounts/{}/follow", target_id.as_i64()),
        &token,
        &json!({"reblogs": false, "notify": true, "languages": ["ja", "en"]}),
    )
    .await;

    assert_eq!(response.status, 200, "got: {response:?}");
    let body = body_json(&response);
    assert_eq!(body["following"].as_bool(), Some(true), "got: {body}");
    assert_eq!(
        body["showing_reblogs"].as_bool(),
        Some(false),
        "got: {body}"
    );
    assert_eq!(body["notifying"].as_bool(), Some(true), "got: {body}");
    assert_eq!(
        body["languages"].as_array().cloned().unwrap_or_default(),
        vec![json!("ja"), json!("en")],
        "got: {body}"
    );

    app.cleanup().await;
}

// ---- (4) duplicate follow is idempotent: no duplicate row/Activity ----

#[tokio::test]
async fn follow_is_idempotent_on_duplicate_request_no_duplicate_row_or_delivery() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_test_actor(&app, "fu_dup_viewer").await;
    let actor_uri = "https://remote.example/users/fu_dup_target";
    let target_id = create_test_remote(&app, actor_uri, false).await;

    let token = issue_token(&app, oauth_app_id, viewer_id, &["follow"]).await;
    let path = format!("/api/v1/accounts/{}/follow", target_id.as_i64());

    let first = raw_post_json(app.address, &path, &token, &json!({})).await;
    assert_eq!(first.status, 200, "got: {first:?}");

    let second = raw_post_json(app.address, &path, &token, &json!({})).await;
    assert_eq!(second.status, 200, "got: {second:?}");
    let second_body = body_json(&second);
    assert_eq!(
        second_body["following"].as_bool(),
        Some(true),
        "the duplicate request must still return the current relationship idempotently, got: {second_body}"
    );

    assert_eq!(
        follows_row_count(
            &app,
            ("local", viewer_id.as_i64()),
            ("remote", target_id.as_i64())
        )
        .await,
        1,
        "a duplicate follow must not create a second follows row"
    );
    assert_eq!(
        delivery_job_count_for_type(&app, &remote_inbox(actor_uri), "Follow").await,
        1,
        "a duplicate follow must not enqueue a second Follow delivery job"
    );

    app.cleanup().await;
}

// ---- (5) self-follow rejected with a Mastodon-compatible 422 ----------

#[tokio::test]
async fn follow_self_is_rejected_with_422() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_test_actor(&app, "fu_self_viewer").await;

    let token = issue_token(&app, oauth_app_id, viewer_id, &["follow"]).await;
    let response = raw_post_json(
        app.address,
        &format!("/api/v1/accounts/{}/follow", viewer_id.as_i64()),
        &token,
        &json!({}),
    )
    .await;

    assert_eq!(response.status, 422, "got: {response:?}");
    assert_error_shape(&response);
    assert_eq!(
        follows_row_count(
            &app,
            ("local", viewer_id.as_i64()),
            ("local", viewer_id.as_i64())
        )
        .await,
        0,
        "a self-follow must never persist a follows row"
    );

    app.cleanup().await;
}

// ---- (6) unfollow removes an established follow and delivers Undo -----

#[tokio::test]
async fn unfollow_removes_established_follow_and_delivers_undo() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_test_actor(&app, "fu_undo_viewer").await;
    let actor_uri = "https://remote.example/users/fu_undo_target";
    let target_id = create_test_remote(&app, actor_uri, false).await;

    let token = issue_token(&app, oauth_app_id, viewer_id, &["follow"]).await;
    let follow_path = format!("/api/v1/accounts/{}/follow", target_id.as_i64());
    let follow_response = raw_post_json(app.address, &follow_path, &token, &json!({})).await;
    assert_eq!(follow_response.status, 200, "got: {follow_response:?}");

    let unfollow_path = format!("/api/v1/accounts/{}/unfollow", target_id.as_i64());
    let response = raw_post(
        app.address,
        &unfollow_path,
        &[("Authorization", &bearer_header(&token))],
    )
    .await;
    assert_eq!(response.status, 200, "got: {response:?}");
    let body = body_json(&response);
    assert_eq!(body["following"].as_bool(), Some(false), "got: {body}");

    assert_eq!(
        follows_row_count(
            &app,
            ("local", viewer_id.as_i64()),
            ("remote", target_id.as_i64())
        )
        .await,
        0,
        "unfollow must remove the follows row"
    );
    assert_eq!(
        delivery_job_count_for_type(&app, &remote_inbox(actor_uri), "Undo").await,
        1,
        "unfollow must enqueue exactly one Undo Activity"
    );

    app.cleanup().await;
}

// ---- (7) unfollow with no existing relationship is an idempotent no-op

#[tokio::test]
async fn unfollow_is_idempotent_when_no_relationship_or_pending_request_exists() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_test_actor(&app, "fu_noop_viewer").await;
    let actor_uri = "https://remote.example/users/fu_noop_target";
    let target_id = create_test_remote(&app, actor_uri, false).await;

    let token = issue_token(&app, oauth_app_id, viewer_id, &["follow"]).await;
    let response = raw_post(
        app.address,
        &format!("/api/v1/accounts/{}/unfollow", target_id.as_i64()),
        &[("Authorization", &bearer_header(&token))],
    )
    .await;

    assert_eq!(response.status, 200, "got: {response:?}");
    let body = body_json(&response);
    assert_eq!(body["following"].as_bool(), Some(false), "got: {body}");
    assert_eq!(
        delivery_job_count_for_type(&app, &remote_inbox(actor_uri), "Undo").await,
        0,
        "no Undo must be delivered when there was nothing to unfollow"
    );

    app.cleanup().await;
}

// ---- (8) follow to a locked remote target creates a pending outbound
// request (approval required, no same-server privilege for a remote side),
// and unfollow clears that pending request with an Undo referencing the
// original Follow Activity (Requirement 1.4's "既存のフォロー関係または保留
// 中フォローリクエストを解消") ----

#[tokio::test]
async fn follow_to_locked_remote_target_is_pending_and_unfollow_clears_it_with_undo() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_test_actor(&app, "fu_pending_viewer").await;
    let actor_uri = "https://remote.example/users/fu_pending_locked_target";
    let target_id = create_test_remote(&app, actor_uri, true).await;

    let token = issue_token(&app, oauth_app_id, viewer_id, &["follow"]).await;
    let follow_path = format!("/api/v1/accounts/{}/follow", target_id.as_i64());
    let follow_response = raw_post_json(app.address, &follow_path, &token, &json!({})).await;
    assert_eq!(follow_response.status, 200, "got: {follow_response:?}");
    let follow_body = body_json(&follow_response);
    assert_eq!(
        follow_body["following"].as_bool(),
        Some(false),
        "got: {follow_body}"
    );
    assert_eq!(
        follow_body["requested"].as_bool(),
        Some(true),
        "a locked remote target must yield a pending outbound request, got: {follow_body}"
    );
    assert_eq!(
        follow_requests_row_count(
            &app,
            ("local", viewer_id.as_i64()),
            ("remote", target_id.as_i64())
        )
        .await,
        1
    );
    assert_eq!(
        delivery_job_count_for_type(&app, &remote_inbox(actor_uri), "Follow").await,
        1,
        "the Follow Activity is still delivered even while pending approval"
    );

    let unfollow_path = format!("/api/v1/accounts/{}/unfollow", target_id.as_i64());
    let unfollow_response = raw_post(
        app.address,
        &unfollow_path,
        &[("Authorization", &bearer_header(&token))],
    )
    .await;
    assert_eq!(unfollow_response.status, 200, "got: {unfollow_response:?}");
    let unfollow_body = body_json(&unfollow_response);
    assert_eq!(
        unfollow_body["requested"].as_bool(),
        Some(false),
        "got: {unfollow_body}"
    );

    assert_eq!(
        follow_requests_row_count(
            &app,
            ("local", viewer_id.as_i64()),
            ("remote", target_id.as_i64())
        )
        .await,
        0,
        "unfollow must drop the pending outbound request"
    );
    assert_eq!(
        delivery_job_count_for_type(&app, &remote_inbox(actor_uri), "Undo").await,
        1,
        "unfollow of a pending request must still deliver an Undo(Follow)"
    );

    app.cleanup().await;
}

// ---- (9) 404 for a nonexistent target ----------------------------------

#[tokio::test]
async fn follow_returns_404_for_a_nonexistent_target() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_test_actor(&app, "fu_404_viewer").await;

    let token = issue_token(&app, oauth_app_id, viewer_id, &["follow"]).await;
    let response = raw_post_json(
        app.address,
        "/api/v1/accounts/999999999999/follow",
        &token,
        &json!({}),
    )
    .await;

    assert_eq!(response.status, 404, "got: {response:?}");
    assert_error_shape(&response);

    app.cleanup().await;
}

// ---- (10) scope enforcement: 401 unauthenticated, 403 insufficient
// scope, and `write:follows` accepted as an alternative to `follow`
// (Requirement 1.8, 10.1; `src/social_graph/endpoints.rs`'s own documented
// "Scope-per-endpoint" judgment call) ----

#[tokio::test]
async fn follow_requires_follow_or_write_follows_scope() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_test_actor(&app, "fu_scope_viewer").await;
    let target_id = create_test_actor(&app, "fu_scope_target").await;
    let path = format!("/api/v1/accounts/{}/follow", target_id.as_i64());

    let unauthenticated = raw_post_json(app.address, &path, "not-a-real-token", &json!({})).await;
    assert_eq!(unauthenticated.status, 401, "got: {unauthenticated:?}");
    assert_error_shape(&unauthenticated);

    let wrong_scope_token = issue_token(&app, oauth_app_id, viewer_id, &["read:accounts"]).await;
    let forbidden = raw_post_json(app.address, &path, &wrong_scope_token, &json!({})).await;
    assert_eq!(forbidden.status, 403, "got: {forbidden:?}");
    assert_error_shape(&forbidden);

    let write_follows_token = issue_token(&app, oauth_app_id, viewer_id, &["write:follows"]).await;
    let accepted = raw_post_json(app.address, &path, &write_follows_token, &json!({})).await;
    assert_eq!(
        accepted.status, 200,
        "a write:follows-scoped token must be accepted as an alternative to follow, got: {accepted:?}"
    );

    app.cleanup().await;
}
