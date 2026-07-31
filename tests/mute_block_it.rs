//! Integration tests for social-graph's mute/block HTTP surface
//! (`.kiro/specs/social-graph/tasks.md`, task 6.1 "関係操作の統合テスト",
//! `_Boundary: SocialGraphEndpoints, FollowService, FollowRequestService,
//! BlockService, MuteService_`), driven against the real, `spawn_test_app`-
//! booted application router.
//!
//! design.md's File Structure Plan names this exact filename
//! (`mute_block_it.rs`: "mute/unmute（通知・期限）・block/unblock（関係解消
//! ・配送）（統合）") and its own Testing Strategy bullet ("mute/block: 通知
//! ミュート・期限付きミュート・期限後の解除扱い、block で双方向関係解消 +
//! Block 配送、unblock で Undo 配送（4.1, 4.2, 4.3, 5.1, 5.2, 5.4）").
//!
//! Covers Requirements 4.1, 4.2, 4.3, 5.1, 5.2, 5.4 (this task's own assigned
//! subset).
//!
//! ## Mute expiry (Requirement 4.3) is observed through the real
//! `relationships` read path, with `expires_at` pushed into the past
//! directly rather than a real-time wait
//! `MuteService::mute`/`unmute` only ever *write* (Requirement 4.1/4.4) —
//! neither returns a relationship that has since "aged out" mid-request, so
//! observing automatic expiry needs a *subsequent, independent* read.
//! `RelationshipMapper`'s query-time expiry check (design.md, "期限切れミュ
//! ートは...偽として扱う", already unit-tested at task 2.4) is exercised here
//! through accounts-and-instance's real, already-wired `GET
//! /api/v1/accounts/relationships` endpoint (which consumes this spec's own
//! registered `RelationshipStateProvider`, task 4.3/5.2, already reviewed)
//! -- the same read-only oracle `tests/relationships_it.rs` itself uses.
//! `spawn_test_app`'s own `RuntimeContext` is `RuntimeContext::deterministic`
//! (a `FixedClock`, see `src/runtime.rs`), so a real `tokio::time::sleep`
//! would never move `Clock::now()` forward and could never make a mute
//! naturally expire; this test instead directly rewrites the seeded
//! `mutes.expires_at` row to a timestamp already in the past relative to
//! this instance's own fixed `now`, exercising exactly the same query-time
//! comparison `RelationshipMapper`/`repository::load_states` perform,
//! without depending on wall-clock time at all.
//!
//! ## No HTTP client dependency: raw sockets (this crate's established
//! per-file-duplicated convention).

use std::collections::HashMap;
use std::time::Duration;

use serde_json::{Value, json};
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

async fn raw_post_empty(
    addr: std::net::SocketAddr,
    path: &str,
    extra_headers: &[(&str, &str)],
) -> RawResponse {
    raw_request(addr, "POST", path, None, extra_headers, "").await
}

async fn raw_get(
    addr: std::net::SocketAddr,
    path: &str,
    extra_headers: &[(&str, &str)],
) -> RawResponse {
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
            name: "Mute Block IT Client".to_string(),
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
            display_name: "Mute Block IT Actor".to_string(),
            summary: "a mute_block_it integration test fixture".to_string(),
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

async fn create_remote_account_with_id(app: &TestApp, id: Id, actor_uri: &str, username: &str) {
    upsert_remote(
        &app.pool,
        &RemoteAccount {
            id,
            actor_uri: actor_uri.to_string(),
            username: username.to_string(),
            domain: "remote.example".to_string(),
            display_name: format!("Remote {username}"),
            note: String::new(),
            url: actor_uri.to_string(),
            avatar_url: None,
            header_url: None,
            fields: Vec::<ProfileField>::new(),
            bot: false,
            locked: false,
            fetched_at: app.runtime.clock.now(),
        },
    )
    .await
    .expect("seeding the cached remote account must succeed");
}

async fn delivery_job_count(app: &TestApp, target_inbox: &str, activity_type: &str) -> i64 {
    let row: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM delivery_jobs WHERE target_inbox = $1 AND activity->>'type' = $2",
    )
    .bind(target_inbox)
    .bind(activity_type)
    .fetch_one(&app.pool)
    .await
    .expect("querying delivery_jobs must succeed");
    row.0
}

async fn relationship_to(app: &TestApp, viewer_token: &str, target_id: Id) -> Value {
    let response = raw_get(
        app.address,
        &format!("/api/v1/accounts/relationships?id={}", target_id.as_i64()),
        &[("Authorization", &bearer_header(viewer_token))],
    )
    .await;
    assert_eq!(response.status, 200, "got: {response:?}");
    let body = body_json(&response);
    body.as_array()
        .expect("relationships must return an array")
        .first()
        .cloned()
        .expect("relationships must return exactly one entry for one requested id")
}

async fn mute(app: &TestApp, token: &str, target_id: Id, opts: Option<Value>) -> RawResponse {
    let path = format!("/api/v1/accounts/{}/mute", target_id.as_i64());
    match opts {
        Some(v) => {
            let payload = v.to_string();
            raw_request(
                app.address,
                "POST",
                &path,
                Some("application/json"),
                &[("Authorization", &bearer_header(token))],
                &payload,
            )
            .await
        }
        None => {
            raw_post_empty(
                app.address,
                &path,
                &[("Authorization", &bearer_header(token))],
            )
            .await
        }
    }
}

async fn unmute(app: &TestApp, token: &str, target_id: Id) -> RawResponse {
    let path = format!("/api/v1/accounts/{}/unmute", target_id.as_i64());
    raw_post_empty(
        app.address,
        &path,
        &[("Authorization", &bearer_header(token))],
    )
    .await
}

async fn follow(app: &TestApp, token: &str, target_id: Id) -> RawResponse {
    let path = format!("/api/v1/accounts/{}/follow", target_id.as_i64());
    raw_post_empty(
        app.address,
        &path,
        &[("Authorization", &bearer_header(token))],
    )
    .await
}

async fn block(app: &TestApp, token: &str, target_id: Id) -> RawResponse {
    let path = format!("/api/v1/accounts/{}/block", target_id.as_i64());
    raw_post_empty(
        app.address,
        &path,
        &[("Authorization", &bearer_header(token))],
    )
    .await
}

async fn unblock(app: &TestApp, token: &str, target_id: Id) -> RawResponse {
    let path = format!("/api/v1/accounts/{}/unblock", target_id.as_i64());
    raw_post_empty(
        app.address,
        &path,
        &[("Authorization", &bearer_header(token))],
    )
    .await
}

// ==========================================================================
// (1) Requirement 4.1 / 4.2: mute records muting + muting_notifications
// ==========================================================================

#[tokio::test]
async fn mute_records_muting_and_notification_flag() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_owner_with_actor(&app, "mb_mute_viewer").await;
    let target_id = create_owner_with_actor(&app, "mb_mute_target").await;
    let token = issue_token(&app, oauth_app_id, viewer_id, &["follow"]).await;

    let response = mute(
        &app,
        &token,
        target_id,
        Some(json!({ "notifications": true })),
    )
    .await;
    assert_eq!(response.status, 200, "got: {response:?}");
    let body = body_json(&response);
    assert_eq!(body["muting"].as_bool(), Some(true), "got: {body}");
    assert_eq!(
        body["muting_notifications"].as_bool(),
        Some(true),
        "got: {body}"
    );

    // A mute is a local-only relationship (Requirement 4.5): no federation
    // Activity is ever built/delivered for it -- the whole `delivery_jobs`
    // table (not merely one inbox) must still be empty after a mute-only
    // scenario with no other social-graph operation involved.
    let total_delivery_jobs: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM delivery_jobs")
        .fetch_one(&app.pool)
        .await
        .expect("querying delivery_jobs must succeed");
    assert_eq!(
        total_delivery_jobs.0, 0,
        "mute must never enqueue any federation delivery"
    );

    app.cleanup().await;
}

/// `notifications: false` is respected distinctly from the (also-true-by-
/// default) implicit case above, proving this flag is genuinely read from
/// the request body rather than hardcoded true.
#[tokio::test]
async fn mute_with_notifications_false_does_not_set_notification_mute() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_owner_with_actor(&app, "mb_mute_nonotif_viewer").await;
    let target_id = create_owner_with_actor(&app, "mb_mute_nonotif_target").await;
    let token = issue_token(&app, oauth_app_id, viewer_id, &["follow"]).await;

    let response = mute(
        &app,
        &token,
        target_id,
        Some(json!({ "notifications": false })),
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

    app.cleanup().await;
}

// ==========================================================================
// (2) Requirement 4.3: a duration-bound mute expires automatically
// ==========================================================================

#[tokio::test]
async fn mute_with_duration_automatically_expires() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_owner_with_actor(&app, "mb_expiry_viewer").await;
    let target_id = create_owner_with_actor(&app, "mb_expiry_target").await;
    let token = issue_token(&app, oauth_app_id, viewer_id, &["follow"]).await;
    let read_token = issue_token(&app, oauth_app_id, viewer_id, &["read:follows"]).await;

    let muted = mute(&app, &token, target_id, Some(json!({ "duration": 60 }))).await;
    assert_eq!(muted.status, 200, "got: {muted:?}");
    assert_eq!(body_json(&muted)["muting"].as_bool(), Some(true));

    // Immediately after muting, a fresh read must still show `muting: true`
    // (the duration has not elapsed yet).
    let still_muted = relationship_to(&app, &read_token, target_id).await;
    assert_eq!(
        still_muted["muting"].as_bool(),
        Some(true),
        "got: {still_muted}"
    );

    // `spawn_test_app`'s `RuntimeContext` uses a `FixedClock` (see this
    // module's doc comment), so push `expires_at` into the past directly
    // rather than waiting on real wall-clock time.
    let past = app.runtime.clock.now() - time::Duration::seconds(3600);
    sqlx::query("UPDATE mutes SET expires_at = $1 WHERE muter_id = $2 AND muted_id = $3")
        .bind(past)
        .bind(viewer_id.as_i64())
        .bind(target_id.as_i64())
        .execute(&app.pool)
        .await
        .expect("rewriting the seeded mute's expires_at must succeed");

    let expired = relationship_to(&app, &read_token, target_id).await;
    assert_eq!(
        expired["muting"].as_bool(),
        Some(false),
        "a mute past its expiry must no longer report `muting: true`, got: {expired}"
    );
    assert_eq!(
        expired["muting_notifications"].as_bool(),
        Some(false),
        "got: {expired}"
    );

    app.cleanup().await;
}

// ==========================================================================
// (3) Requirement 4.4/4.1's idempotency: unmute clears the relationship
// ==========================================================================

#[tokio::test]
async fn unmute_clears_muting() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_owner_with_actor(&app, "mb_unmute_viewer").await;
    let target_id = create_owner_with_actor(&app, "mb_unmute_target").await;
    let token = issue_token(&app, oauth_app_id, viewer_id, &["follow"]).await;

    let muted = mute(&app, &token, target_id, None).await;
    assert_eq!(muted.status, 200, "got: {muted:?}");
    assert_eq!(body_json(&muted)["muting"].as_bool(), Some(true));

    let unmuted = unmute(&app, &token, target_id).await;
    assert_eq!(unmuted.status, 200, "got: {unmuted:?}");
    let body = body_json(&unmuted);
    assert_eq!(body["muting"].as_bool(), Some(false), "got: {body}");

    app.cleanup().await;
}

// ==========================================================================
// (4) Requirement 5.1 / 5.2: block clears bidirectional follows and records
// `blocking`/`blocked_by`
// ==========================================================================

#[tokio::test]
async fn block_clears_mutual_follows_and_sets_blocking_and_blocked_by() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let alice_id = create_owner_with_actor(&app, "mb_block_alice").await;
    let bob_id = create_owner_with_actor(&app, "mb_block_bob").await;
    let alice_token = issue_token(&app, oauth_app_id, alice_id, &["follow"]).await;
    let bob_token = issue_token(&app, oauth_app_id, bob_id, &["follow"]).await;
    // `GET /api/v1/accounts/relationships` (accounts-and-instance's own
    // endpoint, used here as a read-only oracle) requires exactly
    // `read:follows` -- unlike social-graph's own endpoints, it does not
    // accept the coarser `follow` scope as an alternative (see
    // `src/accounts/endpoints.rs`'s own scope-per-endpoint documentation),
    // so a separate read-scoped token is needed for each viewer.
    let alice_read_token = issue_token(&app, oauth_app_id, alice_id, &["read:follows"]).await;
    let bob_read_token = issue_token(&app, oauth_app_id, bob_id, &["read:follows"]).await;

    // Establish a mutual follow first (both are local, so both establish
    // immediately).
    assert_eq!(follow(&app, &alice_token, bob_id).await.status, 200);
    assert_eq!(follow(&app, &bob_token, alice_id).await.status, 200);

    let alice_sees_bob_before = relationship_to(&app, &alice_read_token, bob_id).await;
    assert_eq!(alice_sees_bob_before["following"].as_bool(), Some(true));
    assert_eq!(alice_sees_bob_before["followed_by"].as_bool(), Some(true));

    // Alice blocks Bob.
    let blocked = block(&app, &alice_token, bob_id).await;
    assert_eq!(blocked.status, 200, "got: {blocked:?}");
    let blocked_body = body_json(&blocked);
    assert_eq!(
        blocked_body["blocking"].as_bool(),
        Some(true),
        "got: {blocked_body}"
    );
    assert_eq!(
        blocked_body["following"].as_bool(),
        Some(false),
        "blocking must clear the blocker's own follow of the target, got: {blocked_body}"
    );
    assert_eq!(
        blocked_body["followed_by"].as_bool(),
        Some(false),
        "blocking must clear the target's follow of the blocker too (bidirectional), \
         got: {blocked_body}"
    );

    // From Bob's own perspective: he is now blocked by Alice, and no longer
    // follows or is followed by her.
    let bob_sees_alice = relationship_to(&app, &bob_read_token, alice_id).await;
    assert_eq!(
        bob_sees_alice["blocked_by"].as_bool(),
        Some(true),
        "got: {bob_sees_alice}"
    );
    assert_eq!(bob_sees_alice["following"].as_bool(), Some(false));
    assert_eq!(bob_sees_alice["followed_by"].as_bool(), Some(false));

    app.cleanup().await;
}

/// Blocking a remote target enqueues a Block Activity delivery
/// (Requirement 5.1's own delivery aspect, called out by this task's own
/// text: "配送").
#[tokio::test]
async fn block_remote_target_enqueues_block_delivery() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_owner_with_actor(&app, "mb_block_remote_viewer").await;
    let token = issue_token(&app, oauth_app_id, viewer_id, &["follow"]).await;

    let remote_id = app.runtime.ids.next_id();
    let actor_uri = "https://remote.example/users/mb_block_remote_target";
    create_remote_account_with_id(&app, remote_id, actor_uri, "mb_block_remote_target").await;

    let response = block(&app, &token, remote_id).await;
    assert_eq!(response.status, 200, "got: {response:?}");
    assert_eq!(body_json(&response)["blocking"].as_bool(), Some(true));

    let inbox = format!("{actor_uri}/inbox");
    assert_eq!(
        delivery_job_count(&app, &inbox, "Block").await,
        1,
        "blocking a remote target must enqueue a Block Activity delivery"
    );

    app.cleanup().await;
}

// ==========================================================================
// (5) Requirement 5.4: unblock clears the block and delivers Undo(Block)
// ==========================================================================

#[tokio::test]
async fn unblock_clears_blocking_and_enqueues_undo_delivery() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_owner_with_actor(&app, "mb_unblock_viewer").await;
    let token = issue_token(&app, oauth_app_id, viewer_id, &["follow"]).await;

    let remote_id = app.runtime.ids.next_id();
    let actor_uri = "https://remote.example/users/mb_unblock_target";
    create_remote_account_with_id(&app, remote_id, actor_uri, "mb_unblock_target").await;

    let blocked = block(&app, &token, remote_id).await;
    assert_eq!(blocked.status, 200, "got: {blocked:?}");
    assert_eq!(body_json(&blocked)["blocking"].as_bool(), Some(true));

    let unblocked = unblock(&app, &token, remote_id).await;
    assert_eq!(unblocked.status, 200, "got: {unblocked:?}");
    let body = body_json(&unblocked);
    assert_eq!(body["blocking"].as_bool(), Some(false), "got: {body}");

    let inbox = format!("{actor_uri}/inbox");
    assert_eq!(
        delivery_job_count(&app, &inbox, "Undo").await,
        1,
        "unblock must deliver an Undo(Block) Activity to the target's inbox"
    );

    app.cleanup().await;
}

/// Unblocking a target that was never blocked is an idempotent no-op (no
/// Undo delivered) -- mirrors `FollowService::unfollow`'s identical
/// idempotency discipline (Requirement 1.6's principle applied here).
#[tokio::test]
async fn unblock_with_no_existing_block_is_an_idempotent_no_op() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_owner_with_actor(&app, "mb_unblock_noop_viewer").await;
    let token = issue_token(&app, oauth_app_id, viewer_id, &["follow"]).await;

    let remote_id = app.runtime.ids.next_id();
    let actor_uri = "https://remote.example/users/mb_unblock_noop_target";
    create_remote_account_with_id(&app, remote_id, actor_uri, "mb_unblock_noop_target").await;

    let response = unblock(&app, &token, remote_id).await;
    assert_eq!(response.status, 200, "got: {response:?}");
    assert_eq!(body_json(&response)["blocking"].as_bool(), Some(false));

    let inbox = format!("{actor_uri}/inbox");
    assert_eq!(delivery_job_count(&app, &inbox, "Undo").await, 0);

    app.cleanup().await;
}
