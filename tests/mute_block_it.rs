//! Integration tests for task 6.1's own observable completion condition
//! (`.kiro/specs/social-graph/tasks.md`, "6.1 (P) 関係操作の統合テスト") —
//! this file covers the "mute/block" bullet of design.md's own "Integration
//! Tests（`spawn_test_app` 上）" Testing Strategy section: "mute/block: 通知
//! ミュート・期限付きミュート・期限後の解除扱い、block で双方向関係解消 +
//! Block 配送、unblock で Undo 配送（4.1, 4.2, 4.3, 5.1, 5.2, 5.4）", driven
//! through the real, mounted `POST /api/v1/accounts/:id/mute` /
//! `.../unmute` / `.../block` / `.../unblock` handlers
//! (`src/social_graph/endpoints.rs`, wired by `src/server.rs`'s
//! `social_graph_router`).
//!
//! ## Testing "期限後の解除扱い" (post-expiry release) with a *deterministic*
//! clock, without waiting on real time
//! `spawn_test_app` always injects `RuntimeContext::deterministic`'s
//! `FixedClock` (`src/test_harness.rs`'s own doc comment, "Deterministic
//! injection"; `src/runtime/clock.rs::FixedClock::now` always returns the
//! same constructed instant, never advancing) — this instance's `now()`
//! genuinely never moves forward during a test. `MuteService::mute`
//! resolves `opts.duration` (a relative *seconds* count) into an absolute
//! `expires_at = now + Duration::seconds(seconds)`
//! (`src/social_graph/mute_service.rs`), and `repository::load_states`'s own
//! mute query excludes any row where `expires_at <= now`
//! (`src/social_graph/repository.rs`: `expires_at IS NULL OR expires_at >
//! $now`). A **negative** `duration` therefore deterministically produces
//! an `expires_at` that is already in the past relative to this same fixed
//! `now` the instant the mute is recorded — the one genuine way to observe
//! "the mute's expiry has elapsed" against a clock that cannot itself
//! advance, and a direct, real consequence of this already-implemented
//! expiry-filter logic (not a special case bolted on for the test). The
//! `mutes` row itself is still persisted with that past `expires_at`
//! (Requirement 4.3's "期限指定が記録される"); only the *derived*
//! `muting`/`muting_notifications` Relationship flags are excluded once
//! expired (Requirement 9.3, unit-tested in isolation by
//! `relationship_mapper/tests.rs`; this file's own job is proving that
//! exclusion is genuinely reachable end to end through the real `mute`
//! endpoint's own Relationship response, not re-deriving the unit-level
//! proof).
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
use kawasemi::domain::{AccountRef, Id};
use kawasemi::oauth::app_repository::{self, NewApp};
use kawasemi::oauth::model::ScopeSet as PlaceholderScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::social_graph::repository::{upsert_follow, upsert_request};
use kawasemi::social_graph::{Follow, FollowRequest, FollowRequestDirection};
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

async fn raw_post(addr: SocketAddr, path: &str, token: &str) -> RawResponse {
    raw_request(
        addr,
        "POST",
        path,
        &[("Authorization", &bearer_header(token))],
        b"",
    )
    .await
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
            name: "mute_block_it Client".to_string(),
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
            display_name: "Mute Block IT Actor".to_string(),
            summary: "a mute_block_it integration test fixture".to_string(),
        })
        .await
        .expect("creating the owner's actor fixture must succeed");

    actor.id
}

fn sample_remote_account(id: Id, actor_uri: &str, fetched_at: OffsetDateTime) -> RemoteAccount {
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
        locked: false,
        fetched_at,
    }
}

async fn create_test_remote(app: &TestApp, actor_uri: &str) -> Id {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    upsert_remote(&app.pool, &sample_remote_account(id, actor_uri, now))
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

async fn mutes_row_count(app: &TestApp, muter: (&str, i64), muted: (&str, i64)) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM mutes \
         WHERE muter_kind = $1 AND muter_id = $2 AND muted_kind = $3 AND muted_id = $4",
    )
    .bind(muter.0)
    .bind(muter.1)
    .bind(muted.0)
    .bind(muted.1)
    .fetch_one(&app.pool)
    .await
    .expect("counting mutes rows must succeed")
}

async fn mute_expires_at(
    app: &TestApp,
    muter: (&str, i64),
    muted: (&str, i64),
) -> Option<OffsetDateTime> {
    sqlx::query_scalar(
        "SELECT expires_at FROM mutes \
         WHERE muter_kind = $1 AND muter_id = $2 AND muted_kind = $3 AND muted_id = $4",
    )
    .bind(muter.0)
    .bind(muter.1)
    .bind(muted.0)
    .bind(muted.1)
    .fetch_one(&app.pool)
    .await
    .expect("reading the seeded mutes row's expires_at must succeed")
}

async fn blocks_row_count(app: &TestApp, blocker: (&str, i64), blocked: (&str, i64)) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM blocks \
         WHERE blocker_kind = $1 AND blocker_id = $2 AND blocked_kind = $3 AND blocked_id = $4",
    )
    .bind(blocker.0)
    .bind(blocker.1)
    .bind(blocked.0)
    .bind(blocked.1)
    .fetch_one(&app.pool)
    .await
    .expect("counting blocks rows must succeed")
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
    format!("{actor_uri}/inbox")
}

// ==========================================================================
// mute / unmute (Requirements 4.1-4.6)
// ==========================================================================

// ---- (1) mute records muting + reflects the notifications flag --------

#[tokio::test]
async fn mute_records_muting_true_and_reflects_notifications_flag() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_test_actor(&app, "mb_mute_viewer").await;
    let target_id = create_test_actor(&app, "mb_mute_target").await;

    let token = issue_token(&app, oauth_app_id, viewer_id, &["follow"]).await;
    let response = raw_post_json(
        app.address,
        &format!("/api/v1/accounts/{}/mute", target_id.as_i64()),
        &token,
        &json!({"notifications": false}),
    )
    .await;

    assert_eq!(response.status, 200, "got: {response:?}");
    let body = body_json(&response);
    assert_eq!(body["muting"].as_bool(), Some(true), "got: {body}");
    assert_eq!(
        body["muting_notifications"].as_bool(),
        Some(false),
        "got: {body}"
    );

    assert_eq!(
        mutes_row_count(
            &app,
            ("local", viewer_id.as_i64()),
            ("local", target_id.as_i64())
        )
        .await,
        1
    );

    app.cleanup().await;
}

// ---- (2) empty body: Mastodon defaults (notifications=true, unbounded) -

#[tokio::test]
async fn mute_with_empty_body_defaults_to_notifications_true_and_unbounded_duration() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_test_actor(&app, "mb_muteempty_viewer").await;
    let target_id = create_test_actor(&app, "mb_muteempty_target").await;

    let token = issue_token(&app, oauth_app_id, viewer_id, &["follow"]).await;
    let response = raw_post(
        app.address,
        &format!("/api/v1/accounts/{}/mute", target_id.as_i64()),
        &token,
    )
    .await;

    assert_eq!(response.status, 200, "got: {response:?}");
    let body = body_json(&response);
    assert_eq!(body["muting"].as_bool(), Some(true), "got: {body}");
    assert_eq!(
        body["muting_notifications"].as_bool(),
        Some(true),
        "an empty body must default notifications to true, got: {body}"
    );

    let expires_at = mute_expires_at(
        &app,
        ("local", viewer_id.as_i64()),
        ("local", target_id.as_i64()),
    )
    .await;
    assert_eq!(
        expires_at, None,
        "an omitted duration must record an unbounded (NULL) expires_at"
    );

    app.cleanup().await;
}

// ---- (3) duration is recorded, and an already-elapsed duration is
// excluded from the returned Relationship's `muting` flag (Requirements
// 4.3, 9.3 -- see this file's own doc comment for the deterministic-clock
// rationale) --------------------------------------------------------------

#[tokio::test]
async fn mute_with_an_already_elapsed_duration_is_recorded_but_excluded_from_muting() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_test_actor(&app, "mb_muteexpired_viewer").await;
    let target_id = create_test_actor(&app, "mb_muteexpired_target").await;

    let token = issue_token(&app, oauth_app_id, viewer_id, &["follow"]).await;
    let response = raw_post_json(
        app.address,
        &format!("/api/v1/accounts/{}/mute", target_id.as_i64()),
        &token,
        &json!({"duration": -3600}),
    )
    .await;

    assert_eq!(response.status, 200, "got: {response:?}");
    let body = body_json(&response);
    assert_eq!(
        body["muting"].as_bool(),
        Some(false),
        "an expires_at already in the past relative to `now` must be excluded from `muting`, got: {body}"
    );

    // The row itself must still exist (recorded, per Requirement 4.3),
    // carrying a genuinely past expires_at -- only its *derived*
    // Relationship flag is excluded, not the persisted fact of the mute.
    let now = app.runtime.clock.now();
    let expires_at = mute_expires_at(
        &app,
        ("local", viewer_id.as_i64()),
        ("local", target_id.as_i64()),
    )
    .await
    .expect("the mute row itself must still be recorded with a past expires_at");
    assert!(
        expires_at < now,
        "expires_at ({expires_at}) must be strictly before now ({now})"
    );

    app.cleanup().await;
}

// ---- (4) unmute removes the mute and is idempotent ---------------------

#[tokio::test]
async fn unmute_removes_mute_and_is_idempotent() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_test_actor(&app, "mb_unmute_viewer").await;
    let target_id = create_test_actor(&app, "mb_unmute_target").await;

    let token = issue_token(&app, oauth_app_id, viewer_id, &["follow"]).await;
    let mute_path = format!("/api/v1/accounts/{}/mute", target_id.as_i64());
    let mute_response = raw_post(app.address, &mute_path, &token).await;
    assert_eq!(mute_response.status, 200, "got: {mute_response:?}");

    let unmute_path = format!("/api/v1/accounts/{}/unmute", target_id.as_i64());
    let response = raw_post(app.address, &unmute_path, &token).await;
    assert_eq!(response.status, 200, "got: {response:?}");
    let body = body_json(&response);
    assert_eq!(body["muting"].as_bool(), Some(false), "got: {body}");
    assert_eq!(
        mutes_row_count(
            &app,
            ("local", viewer_id.as_i64()),
            ("local", target_id.as_i64())
        )
        .await,
        0
    );

    // Idempotent: unmuting again with nothing to remove is still a 200.
    let second = raw_post(app.address, &unmute_path, &token).await;
    assert_eq!(second.status, 200, "got: {second:?}");
    assert_eq!(body_json(&second)["muting"].as_bool(), Some(false));

    app.cleanup().await;
}

// ---- (5) 404 for a nonexistent mute target -----------------------------

#[tokio::test]
async fn mute_returns_404_for_a_nonexistent_target() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_test_actor(&app, "mb_mute404_viewer").await;

    let token = issue_token(&app, oauth_app_id, viewer_id, &["follow"]).await;
    let response = raw_post(app.address, "/api/v1/accounts/999999999999/mute", &token).await;

    assert_eq!(response.status, 404, "got: {response:?}");
    assert_error_shape(&response);

    app.cleanup().await;
}

// ---- (6) scope enforcement: follow/write:follows/write:mutes all
// accepted (Requirement 4.6, 10.1) ----------------------------------------

#[tokio::test]
async fn mute_requires_follow_write_follows_or_write_mutes_scope() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_test_actor(&app, "mb_mutescope_viewer").await;
    let target_id = create_test_actor(&app, "mb_mutescope_target").await;
    let path = format!("/api/v1/accounts/{}/mute", target_id.as_i64());

    let unauthenticated = raw_post(app.address, &path, "not-a-real-token").await;
    assert_eq!(unauthenticated.status, 401, "got: {unauthenticated:?}");
    assert_error_shape(&unauthenticated);

    let wrong_scope_token = issue_token(&app, oauth_app_id, viewer_id, &["read:accounts"]).await;
    let forbidden = raw_post(app.address, &path, &wrong_scope_token).await;
    assert_eq!(forbidden.status, 403, "got: {forbidden:?}");
    assert_error_shape(&forbidden);

    let write_mutes_token = issue_token(&app, oauth_app_id, viewer_id, &["write:mutes"]).await;
    let accepted = raw_post(app.address, &path, &write_mutes_token).await;
    assert_eq!(
        accepted.status, 200,
        "a write:mutes-scoped token must be accepted, got: {accepted:?}"
    );

    app.cleanup().await;
}

// ==========================================================================
// block / unblock (Requirements 5.1-5.6)
// ==========================================================================

/// Seeds an established follow row directly (test setup only, mirrors
/// `tests/follow_request_it.rs::seed_pending_inbound_request`'s identical
/// rationale) — needed so a block's bidirectional-clearing behavior
/// (Requirement 5.2) can be observed against a *pre-existing* reverse-
/// direction follow the public API alone cannot produce symmetrically in
/// one request (the forward direction is instead established for real,
/// through the actual `follow` endpoint, immediately below each call site).
async fn seed_follow(app: &TestApp, follower: AccountRef, followee: AccountRef, activity_id: &str) {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    upsert_follow(
        &app.pool,
        id,
        &Follow {
            follower,
            followee,
            reblogs: true,
            notify: false,
            languages: Vec::new(),
            activity_id: activity_id.to_string(),
            created_at: now,
        },
    )
    .await
    .expect("seeding a follows row must succeed");
}

async fn seed_pending_request(
    app: &TestApp,
    requester: AccountRef,
    target: AccountRef,
    direction: FollowRequestDirection,
    activity_id: &str,
) {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    upsert_request(
        &app.pool,
        id,
        &FollowRequest {
            requester,
            target,
            direction,
            activity_id: activity_id.to_string(),
            created_at: now,
        },
    )
    .await
    .expect("seeding a follow_requests row must succeed");
}

// ---- (7) block clears both-direction follows + pending requests, sets
// blocking=true, and delivers Block to a remote target (Requirements 5.1,
// 5.2, 5.3) -----------------------------------------------------------------

#[tokio::test]
async fn block_clears_bidirectional_follows_and_pending_requests_and_delivers_block() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_test_actor(&app, "mb_block_viewer").await;
    let actor_uri = "https://remote.example/users/mb_block_target";
    let target_id = create_test_remote(&app, actor_uri).await;
    let viewer_ref = AccountRef::Local(viewer_id);
    let target_ref = AccountRef::Remote(target_id);

    // Establish viewer -> target for real, through the actual follow
    // endpoint.
    let token = issue_token(&app, oauth_app_id, viewer_id, &["follow"]).await;
    let follow_response = raw_post_json(
        app.address,
        &format!("/api/v1/accounts/{}/follow", target_id.as_i64()),
        &token,
        &json!({}),
    )
    .await;
    assert_eq!(follow_response.status, 200, "got: {follow_response:?}");

    // Seed the reverse-direction follow (target already follows viewer
    // back) and one pending request in each direction -- see
    // `seed_follow`'s own doc comment for why this precondition is seeded
    // rather than produced by a second real API call.
    seed_follow(
        &app,
        target_ref,
        viewer_ref,
        "https://remote.example/activities/reverse-follow",
    )
    .await;
    seed_pending_request(
        &app,
        viewer_ref,
        target_ref,
        FollowRequestDirection::Outbound,
        "https://remote.example/activities/pending-outbound",
    )
    .await;
    seed_pending_request(
        &app,
        target_ref,
        viewer_ref,
        FollowRequestDirection::Inbound,
        "https://remote.example/activities/pending-inbound",
    )
    .await;

    let response = raw_post(
        app.address,
        &format!("/api/v1/accounts/{}/block", target_id.as_i64()),
        &token,
    )
    .await;
    assert_eq!(response.status, 200, "got: {response:?}");
    let body = body_json(&response);
    assert_eq!(body["blocking"].as_bool(), Some(true), "got: {body}");

    assert_eq!(
        follows_row_count(
            &app,
            ("local", viewer_id.as_i64()),
            ("remote", target_id.as_i64())
        )
        .await,
        0,
        "the viewer -> target follow must be cleared"
    );
    assert_eq!(
        follows_row_count(
            &app,
            ("remote", target_id.as_i64()),
            ("local", viewer_id.as_i64())
        )
        .await,
        0,
        "the target -> viewer follow must also be cleared"
    );
    assert_eq!(
        follow_requests_row_count(
            &app,
            ("local", viewer_id.as_i64()),
            ("remote", target_id.as_i64())
        )
        .await,
        0,
        "the outbound pending request must be cleared"
    );
    assert_eq!(
        follow_requests_row_count(
            &app,
            ("remote", target_id.as_i64()),
            ("local", viewer_id.as_i64())
        )
        .await,
        0,
        "the inbound pending request must also be cleared"
    );
    assert_eq!(
        blocks_row_count(
            &app,
            ("local", viewer_id.as_i64()),
            ("remote", target_id.as_i64())
        )
        .await,
        1
    );
    assert_eq!(
        delivery_job_count_for_type(&app, &remote_inbox(actor_uri), "Block").await,
        1,
        "block must enqueue exactly one Block Activity to the target's inbox"
    );

    app.cleanup().await;
}

// ---- (8) block is idempotent: no duplicate Block Activity -------------

#[tokio::test]
async fn block_is_idempotent_on_duplicate_request() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_test_actor(&app, "mb_blockdup_viewer").await;
    let actor_uri = "https://remote.example/users/mb_blockdup_target";
    let target_id = create_test_remote(&app, actor_uri).await;

    let token = issue_token(&app, oauth_app_id, viewer_id, &["follow"]).await;
    let path = format!("/api/v1/accounts/{}/block", target_id.as_i64());

    let first = raw_post(app.address, &path, &token).await;
    assert_eq!(first.status, 200, "got: {first:?}");
    let second = raw_post(app.address, &path, &token).await;
    assert_eq!(second.status, 200, "got: {second:?}");
    assert_eq!(body_json(&second)["blocking"].as_bool(), Some(true));

    assert_eq!(
        blocks_row_count(
            &app,
            ("local", viewer_id.as_i64()),
            ("remote", target_id.as_i64())
        )
        .await,
        1,
        "a duplicate block must not create a second blocks row"
    );
    assert_eq!(
        delivery_job_count_for_type(&app, &remote_inbox(actor_uri), "Block").await,
        1,
        "a duplicate block must not enqueue a second Block Activity"
    );

    app.cleanup().await;
}

// ---- (9) unblock removes the block and delivers Undo(Block) -----------

#[tokio::test]
async fn unblock_removes_block_and_delivers_undo() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_test_actor(&app, "mb_unblock_viewer").await;
    let actor_uri = "https://remote.example/users/mb_unblock_target";
    let target_id = create_test_remote(&app, actor_uri).await;

    let token = issue_token(&app, oauth_app_id, viewer_id, &["follow"]).await;
    let block_path = format!("/api/v1/accounts/{}/block", target_id.as_i64());
    let block_response = raw_post(app.address, &block_path, &token).await;
    assert_eq!(block_response.status, 200, "got: {block_response:?}");

    let unblock_path = format!("/api/v1/accounts/{}/unblock", target_id.as_i64());
    let response = raw_post(app.address, &unblock_path, &token).await;
    assert_eq!(response.status, 200, "got: {response:?}");
    let body = body_json(&response);
    assert_eq!(body["blocking"].as_bool(), Some(false), "got: {body}");

    assert_eq!(
        blocks_row_count(
            &app,
            ("local", viewer_id.as_i64()),
            ("remote", target_id.as_i64())
        )
        .await,
        0
    );
    assert_eq!(
        delivery_job_count_for_type(&app, &remote_inbox(actor_uri), "Undo").await,
        1,
        "unblock must enqueue exactly one Undo(Block) to the target's inbox"
    );

    // Idempotent: unblocking again delivers no second Undo.
    let second = raw_post(app.address, &unblock_path, &token).await;
    assert_eq!(second.status, 200, "got: {second:?}");
    assert_eq!(
        delivery_job_count_for_type(&app, &remote_inbox(actor_uri), "Undo").await,
        1,
        "a second unblock with nothing to remove must not enqueue another Undo"
    );

    app.cleanup().await;
}

// ---- (10) self-block rejected with 422 ----------------------------------

#[tokio::test]
async fn block_self_is_rejected_with_422() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_test_actor(&app, "mb_blockself_viewer").await;

    let token = issue_token(&app, oauth_app_id, viewer_id, &["follow"]).await;
    let response = raw_post(
        app.address,
        &format!("/api/v1/accounts/{}/block", viewer_id.as_i64()),
        &token,
    )
    .await;

    assert_eq!(response.status, 422, "got: {response:?}");
    assert_error_shape(&response);

    app.cleanup().await;
}

// ---- (11) 404 for a nonexistent block target ----------------------------

#[tokio::test]
async fn block_returns_404_for_a_nonexistent_target() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_test_actor(&app, "mb_block404_viewer").await;

    let token = issue_token(&app, oauth_app_id, viewer_id, &["follow"]).await;
    let response = raw_post(app.address, "/api/v1/accounts/999999999999/block", &token).await;

    assert_eq!(response.status, 404, "got: {response:?}");
    assert_error_shape(&response);

    app.cleanup().await;
}

// ---- (12) scope enforcement: follow/write:follows/write:blocks all
// accepted (Requirement 5.6, 10.1) -----------------------------------------

#[tokio::test]
async fn block_requires_follow_write_follows_or_write_blocks_scope() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_test_actor(&app, "mb_blockscope_viewer").await;
    let target_id = create_test_actor(&app, "mb_blockscope_target").await;
    let path = format!("/api/v1/accounts/{}/block", target_id.as_i64());

    let unauthenticated = raw_post(app.address, &path, "not-a-real-token").await;
    assert_eq!(unauthenticated.status, 401, "got: {unauthenticated:?}");
    assert_error_shape(&unauthenticated);

    let wrong_scope_token = issue_token(&app, oauth_app_id, viewer_id, &["read:accounts"]).await;
    let forbidden = raw_post(app.address, &path, &wrong_scope_token).await;
    assert_eq!(forbidden.status, 403, "got: {forbidden:?}");
    assert_error_shape(&forbidden);

    let write_blocks_token = issue_token(&app, oauth_app_id, viewer_id, &["write:blocks"]).await;
    let accepted = raw_post(app.address, &path, &write_blocks_token).await;
    assert_eq!(
        accepted.status, 200,
        "a write:blocks-scoped token must be accepted, got: {accepted:?}"
    );

    app.cleanup().await;
}
