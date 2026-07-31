//! Integration tests for task 6.2's own observable completion condition
//! (`.kiro/specs/social-graph/tasks.md`, "6.2 (P) 受信処理・署名拒否・プロバ
//! イダ統合テスト", `_Boundary: InboundHandler, BlockPolicyImpl,
//! RelProviderImpl_`) — this file covers the `RelProviderImpl` half
//! (`tests/social_graph_inbound_it.rs`'s own doc comment covers
//! `InboundHandler`/`BlockPolicyImpl`): design.md's own "Integration Tests
//! （`spawn_test_app` 上）" Testing Strategy bullet "RelationshipStateProvider:
//! 本 spec 登録後に accounts-and-instance の relationships が実フラグを返す
//! （8.2, 8.3）".
//!
//! `tests/relationships_it.rs` (accounts-and-instance's own task 7.1) already
//! proves this endpoint's cross-cutting HTTP contract (scope enforcement,
//! multi-id array ordering, an *unregistered*-provider default, and a
//! hand-built `FixedTrueProvider` test double actually taking effect once
//! swapped in). This file's own, different job (Requirements 8.2, 8.3):
//! prove that `spawn_test_app()`'s own *default*, already-registered
//! provider -- this spec's real `RelProviderImpl`, registered by
//! `social_graph::build_social_graph_module` every time a `TestApp` boots
//! (`src/test_harness.rs`, matching design.md's task 5.2 "既定差し替え"
//! wiring) -- genuinely reflects this spec's own real follow/mute/block
//! state, not a fixed/default value, once that state is established through
//! this spec's own real API (mirrors `tests/mute_block_it.rs`/
//! `tests/follow_unfollow_it.rs`'s own established fixture conventions).
//!
//! ## No HTTP client dependency: raw sockets (this crate's established
//! `tests/*_it.rs` convention).

use std::net::SocketAddr;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use kawasemi::accounts::model::{ProfileField, ProfilePatch, RemoteAccount};
use kawasemi::accounts::profile_repository::upsert_profile;
use kawasemi::accounts::remote_repository::upsert_remote;
use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor};
use kawasemi::domain::{AccountRef, Id};
use kawasemi::oauth::app_repository::{self, NewApp};
use kawasemi::oauth::model::ScopeSet as PlaceholderScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::social_graph::repository::upsert_request;
use kawasemi::social_graph::{FollowRequest, FollowRequestDirection};
use kawasemi::test_harness::{TestApp, spawn_test_app};

// ==========================================================================
// raw HTTP plumbing (mirrors `tests/mute_block_it.rs`/`tests/relationships_it.rs`)
// ==========================================================================

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

async fn raw_get(addr: SocketAddr, path: &str, token: &str) -> RawResponse {
    raw_request(
        addr,
        "GET",
        path,
        &[("Authorization", &bearer_header(token))],
        b"",
    )
    .await
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

fn bearer_header(token: &str) -> String {
    format!("Bearer {token}")
}

// ==========================================================================
// fixtures
// ==========================================================================

async fn register_test_app(app: &TestApp) -> Id {
    let now = app.runtime.clock.now();
    let registered = app_repository::register_app(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        app.state.oauth().token_hash_key(),
        now,
        NewApp {
            name: "social_graph_relationship_provider_it Client".to_string(),
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
            display_name: "Relationship Provider IT Actor".to_string(),
            summary: "a social_graph_relationship_provider_it integration test fixture".to_string(),
        })
        .await
        .expect("creating the owner's actor fixture must succeed");

    actor.id
}

async fn create_test_remote(app: &TestApp, actor_uri: &str, locked: bool) -> Id {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    upsert_remote(
        &app.pool,
        &RemoteAccount {
            id,
            actor_uri: actor_uri.to_string(),
            username: actor_uri.rsplit('/').next().unwrap_or("remote").to_string(),
            domain: "remote.example".to_string(),
            display_name: "Relationship Provider IT Remote Actor".to_string(),
            note: String::new(),
            url: actor_uri.to_string(),
            avatar_url: None,
            header_url: None,
            fields: Vec::<ProfileField>::new(),
            bot: false,
            locked,
            fetched_at: now,
        },
    )
    .await
    .expect("upsert_remote must succeed");
    id
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

/// Seeds a pending follow_requests row directly (test setup only, mirrors
/// `tests/follow_request_it.rs::seed_pending_inbound_request`/
/// `tests/mute_block_it.rs::seed_pending_request`'s identical rationale) --
/// needed for the `requested_by` direction the public API alone cannot
/// produce from `viewer`'s own side (a real inbound Follow received while
/// `viewer` is locked would produce the identical row shape; this spec's own
/// `SocialGraphInboundHandler` is `tests/social_graph_inbound_it.rs`'s own
/// job to prove end to end -- this file's job is only that `RelProviderImpl`
/// surfaces whatever the `follow_requests` table already holds).
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

// ==========================================================================
// (1) Requirements 8.2, 8.3: after real follow/mute/block state is
// established through this spec's own real API, the default-registered
// `RelProviderImpl` (registered by `spawn_test_app`'s own bootstrap wiring,
// task 5.2) is genuinely consulted by accounts-and-instance's
// `GET /api/v1/accounts/relationships` and returns real flags per target,
// in request order.
// ==========================================================================

#[tokio::test]
async fn relationships_reflects_real_follow_mute_and_block_state_via_the_registered_provider() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_test_actor(&app, "sgp_viewer").await;
    let followee_id = create_test_actor(&app, "sgp_followee").await;
    let follower_id = create_test_actor(&app, "sgp_follower").await;
    let blocked_id = create_test_actor(&app, "sgp_blocked").await;
    let muted_id = create_test_actor(&app, "sgp_muted").await;
    let stranger_id = create_test_actor(&app, "sgp_stranger").await;

    let token = issue_token(&app, oauth_app_id, viewer_id, &["follow", "read:follows"]).await;
    let follower_token = issue_token(&app, oauth_app_id, follower_id, &["follow"]).await;

    // viewer -> followee (real follow endpoint; both local+unlocked, so
    // this establishes immediately, no approval needed).
    let follow_response = raw_post(
        app.address,
        &format!("/api/v1/accounts/{}/follow", followee_id.as_i64()),
        &token,
    )
    .await;
    assert_eq!(follow_response.status, 200, "got: {follow_response:?}");

    // follower -> viewer (real follow endpoint, driven by follower's own
    // token -- proves the reverse direction through the real API too, not
    // just a seeded row).
    let followed_by_response = raw_post(
        app.address,
        &format!("/api/v1/accounts/{}/follow", viewer_id.as_i64()),
        &follower_token,
    )
    .await;
    assert_eq!(
        followed_by_response.status, 200,
        "got: {followed_by_response:?}"
    );

    // viewer blocks blocked_id (real block endpoint).
    let block_response = raw_post(
        app.address,
        &format!("/api/v1/accounts/{}/block", blocked_id.as_i64()),
        &token,
    )
    .await;
    assert_eq!(block_response.status, 200, "got: {block_response:?}");

    // viewer mutes muted_id, with notifications muted too (real mute
    // endpoint).
    let mute_response = raw_post_json(
        app.address,
        &format!("/api/v1/accounts/{}/mute", muted_id.as_i64()),
        &token,
        &json!({"notifications": true}),
    )
    .await;
    assert_eq!(mute_response.status, 200, "got: {mute_response:?}");

    let path = format!(
        "/api/v1/accounts/relationships?id={}&id={}&id={}&id={}&id={}",
        followee_id.as_i64(),
        follower_id.as_i64(),
        blocked_id.as_i64(),
        muted_id.as_i64(),
        stranger_id.as_i64(),
    );
    let response = raw_get(app.address, &path, &token).await;
    assert_eq!(response.status, 200, "got: {response:?}");
    let body = body_json(&response);
    let array = body
        .as_array()
        .expect("relationships must return a JSON array");
    assert_eq!(array.len(), 5, "got: {body}");

    // followee: viewer follows them, not followed back.
    assert_eq!(array[0]["following"].as_bool(), Some(true), "got: {body}");
    assert_eq!(
        array[0]["followed_by"].as_bool(),
        Some(false),
        "got: {body}"
    );
    assert_eq!(array[0]["blocking"].as_bool(), Some(false), "got: {body}");

    // follower: follows viewer, not followed back.
    assert_eq!(array[1]["followed_by"].as_bool(), Some(true), "got: {body}");
    assert_eq!(array[1]["following"].as_bool(), Some(false), "got: {body}");

    // blocked: viewer blocks them.
    assert_eq!(array[2]["blocking"].as_bool(), Some(true), "got: {body}");
    assert_eq!(array[2]["blocked_by"].as_bool(), Some(false), "got: {body}");

    // muted: viewer mutes them, with notifications.
    assert_eq!(array[3]["muting"].as_bool(), Some(true), "got: {body}");
    assert_eq!(
        array[3]["muting_notifications"].as_bool(),
        Some(true),
        "got: {body}"
    );

    // stranger: no relationship at all -- every flag this spec owns stays
    // at its documented default (Requirement 8.4, already unit-tested by
    // `relationship_mapper/tests.rs`; asserted here only as this batch
    // query's own baseline, not a re-proof of that unit coverage).
    for field in [
        "following",
        "followed_by",
        "blocking",
        "blocked_by",
        "muting",
        "muting_notifications",
        "requested",
        "requested_by",
        "domain_blocking",
    ] {
        assert_eq!(
            array[4][field].as_bool(),
            Some(false),
            "stranger's '{field}' must stay at its default, got: {body}"
        );
    }

    app.cleanup().await;
}

// ==========================================================================
// (2) Requirements 8.2, 8.3: `requested`/`requested_by` -- the pending
// follow-request-specific flags -- are also genuinely supplied by
// `RelProviderImpl` from real `follow_requests` state (one direction
// produced through the real API against a locked target, the other seeded
// directly to represent the inbound-received shape -- see
// `seed_pending_request`'s own doc comment for why).
// ==========================================================================

#[tokio::test]
async fn relationships_shows_requested_and_requested_by_from_pending_follow_requests() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_test_actor(&app, "sgp_reqviewer").await;
    let locked_target_id =
        create_test_remote(&app, "https://remote.example/users/sgp_reqtarget", true).await;
    let requester_id = create_test_actor(&app, "sgp_requester").await;
    lock_actor(&app, viewer_id).await;

    let token = issue_token(&app, oauth_app_id, viewer_id, &["follow", "read:follows"]).await;

    // viewer -> locked *remote* target: real follow endpoint, pending
    // outbound. Must be remote, not local -- `FollowApprovalPolicy`'s own
    // same-server privilege (Requirement 3) would otherwise establish a
    // locked *local* target's follow immediately regardless of the lock,
    // never producing the pending state this test needs.
    let follow_response = raw_post(
        app.address,
        &format!("/api/v1/accounts/{}/follow", locked_target_id.as_i64()),
        &token,
    )
    .await;
    assert_eq!(follow_response.status, 200, "got: {follow_response:?}");
    assert_eq!(
        body_json(&follow_response)["requested"].as_bool(),
        Some(true),
        "the follow response itself must already report requested=true, got: {follow_response:?}"
    );

    // requester -> viewer: pending inbound (the shape a real inbound Follow
    // to locked `viewer` would produce -- seeded directly, see
    // `seed_pending_request`'s own doc comment).
    seed_pending_request(
        &app,
        AccountRef::Local(requester_id),
        AccountRef::Local(viewer_id),
        FollowRequestDirection::Inbound,
        "https://local.example/activities/sgp-seed-pending-inbound",
    )
    .await;

    let path = format!(
        "/api/v1/accounts/relationships?id={}&id={}",
        locked_target_id.as_i64(),
        requester_id.as_i64(),
    );
    let response = raw_get(app.address, &path, &token).await;
    assert_eq!(response.status, 200, "got: {response:?}");
    let body = body_json(&response);
    let array = body
        .as_array()
        .expect("relationships must return a JSON array");
    assert_eq!(array.len(), 2, "got: {body}");

    assert_eq!(
        array[0]["requested"].as_bool(),
        Some(true),
        "viewer's own pending outbound request to the locked target must show requested=true, got: {body}"
    );
    assert_eq!(
        array[1]["requested_by"].as_bool(),
        Some(true),
        "the requester's pending inbound request to viewer must show requested_by=true, got: {body}"
    );

    app.cleanup().await;
}
