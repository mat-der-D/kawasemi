//! Integration tests for social-graph's follow/unfollow HTTP surface
//! (`.kiro/specs/social-graph/tasks.md`, task 6.1 "関係操作の統合テスト",
//! `_Boundary: SocialGraphEndpoints, FollowService, FollowRequestService,
//! BlockService, MuteService_`), driven against the real, `spawn_test_app`-
//! booted application router (never a test-local router — task 5.2 already
//! mounted every social-graph endpoint onto the real one, `src/server.rs`).
//!
//! design.md's File Structure Plan names this exact filename
//! (`follow_unfollow_it.rs`: "follow/unfollow（ローカル/リモート・冪等・自
//! 己フォロー拒否・オプション反映・Relationship 応答・follow 通知イベント
//! emit）（統合）") and its own Testing Strategy bullet ("follow/unfollow:
//! ローカル/リモートで Relationship 応答、重複フォロー冪等、自己フォロー
//! 拒否、オプション（reblogs/notify/languages）反映（1.1, 1.5, 1.6,
//! 1.7）").
//!
//! Covers Requirements 1.1, 1.5, 1.6, 1.7 (this task's own assigned subset;
//! 1.2/1.3/1.4/1.8/10.x are covered elsewhere — unit-level `ActivityBuilder`
//! coverage for 1.2/1.3's "identical logical Activity" claim already exists
//! from task 2.3, and 1.8/10.x scope/auth enforcement is already covered by
//! `src/social_graph/endpoints/tests.rs`, task 5.1's own boundary).
//!
//! ## Remote-target delivery: observed via a `delivery_jobs` row, never a
//! real network send
//! Mirrors `tests/federation_bootstrap_it.rs`'s own established pattern
//! (`delivery_job_exists`/`delivery_job_count`): a remote target's inbox
//! (`{actor_uri}/inbox`, task 3.1's own documented interim convention — see
//! `src/social_graph/follow_service.rs`'s doc comment, "Remote delivery's
//! `inbox`") points at an unreachable host in this test environment, so
//! these tests only assert that `DeliveryService::deliver()` durably
//! enqueued a `delivery_jobs` row for the expected Activity `type` — never
//! that a real HTTP send to `remote.example` succeeded.
//!
//! ## No HTTP client dependency: raw sockets (this crate's established
//! per-file-duplicated convention; see e.g. `tests/oauth_flow_it.rs`'s own
//! doc comment for the rationale).

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

// ---- fixtures (mirrors tests/relationships_it.rs's own established
// pattern) ----

async fn register_test_app(app: &TestApp) -> Id {
    let now = app.runtime.clock.now();
    let registered = app_repository::register_app(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        app.state.oauth().token_hash_key(),
        now,
        NewApp {
            name: "Follow Unfollow IT Client".to_string(),
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
            display_name: "Follow Unfollow IT Actor".to_string(),
            summary: "a follow_unfollow_it integration test fixture".to_string(),
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

/// Seeds a cached remote account row (`RemoteAccount`, accounts-and-instance's
/// own cache table) with an id this test controls, so `FollowService`/
/// `BlockService`'s own "resolve numeric id -> local-first-then-remote-cache"
/// discipline (see `src/social_graph/follow_service.rs`'s doc comment,
/// "`target: &str` resolution") finds it without a live network fetch —
/// mirrors `tests/federation_bootstrap_it.rs`'s own `upsert_remote` fixture
/// pattern.
async fn create_remote_account_with_id(
    app: &TestApp,
    id: Id,
    actor_uri: &str,
    username: &str,
    locked: bool,
) {
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
            locked,
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

async fn follow(app: &TestApp, token: &str, target_id: Id, body: Option<Value>) -> RawResponse {
    let path = format!("/api/v1/accounts/{}/follow", target_id.as_i64());
    match body {
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
            raw_request(
                app.address,
                "POST",
                &path,
                None,
                &[("Authorization", &bearer_header(token))],
                "",
            )
            .await
        }
    }
}

async fn unfollow(app: &TestApp, token: &str, target_id: Id) -> RawResponse {
    let path = format!("/api/v1/accounts/{}/unfollow", target_id.as_i64());
    raw_request(
        app.address,
        "POST",
        &path,
        None,
        &[("Authorization", &bearer_header(token))],
        "",
    )
    .await
}

// ==========================================================================
// (1) Requirement 1.1: follow a local target -> Relationship with
// following=true
// ==========================================================================

#[tokio::test]
async fn follow_local_target_establishes_relationship_and_returns_relationship_json() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_owner_with_actor(&app, "fu_local_viewer").await;
    let target_id = create_owner_with_actor(&app, "fu_local_target").await;
    let token = issue_token(&app, oauth_app_id, viewer_id, &["follow"]).await;

    let response = follow(&app, &token, target_id, None).await;
    assert_eq!(response.status, 200, "got: {response:?}");
    let body = body_json(&response);
    assert_eq!(body["following"].as_bool(), Some(true), "got: {body}");
    assert_eq!(body["requested"].as_bool(), Some(false), "got: {body}");
    assert_eq!(
        body["id"].as_str(),
        Some(target_id.as_i64().to_string()).as_deref()
    );

    app.cleanup().await;
}

// ==========================================================================
// (2) Requirement 1.1 / 1.2: follow an unlocked remote target -> established
// immediately, Follow Activity delivered (delivery_jobs row enqueued)
// ==========================================================================

#[tokio::test]
async fn follow_remote_target_establishes_and_enqueues_a_follow_delivery_job() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_owner_with_actor(&app, "fu_remote_viewer").await;
    let token = issue_token(&app, oauth_app_id, viewer_id, &["follow"]).await;

    let remote_id = app.runtime.ids.next_id();
    let actor_uri = "https://remote.example/users/fu_remote_target";
    create_remote_account_with_id(&app, remote_id, actor_uri, "fu_remote_target", false).await;

    let response = follow(&app, &token, remote_id, None).await;
    assert_eq!(response.status, 200, "got: {response:?}");
    let body = body_json(&response);
    assert_eq!(body["following"].as_bool(), Some(true), "got: {body}");

    let inbox = format!("{actor_uri}/inbox");
    assert_eq!(
        delivery_job_count(&app, &inbox, "Follow").await,
        1,
        "a Follow Activity must have been enqueued for the remote target's inbox"
    );

    app.cleanup().await;
}

// ==========================================================================
// (3) Requirement 1.6: duplicate follow is idempotent -- no second
// Relationship mutation, no second Activity delivered
// ==========================================================================

#[tokio::test]
async fn duplicate_follow_is_idempotent_and_does_not_double_enqueue_delivery() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_owner_with_actor(&app, "fu_dup_viewer").await;
    let token = issue_token(&app, oauth_app_id, viewer_id, &["follow"]).await;

    let remote_id = app.runtime.ids.next_id();
    let actor_uri = "https://remote.example/users/fu_dup_target";
    create_remote_account_with_id(&app, remote_id, actor_uri, "fu_dup_target", false).await;

    let first = follow(&app, &token, remote_id, None).await;
    assert_eq!(first.status, 200, "got: {first:?}");
    assert_eq!(body_json(&first)["following"].as_bool(), Some(true));

    let second = follow(&app, &token, remote_id, None).await;
    assert_eq!(second.status, 200, "got: {second:?}");
    let second_body = body_json(&second);
    assert_eq!(
        second_body["following"].as_bool(),
        Some(true),
        "a duplicate follow must idempotently return the current relationship, got: {second_body}"
    );

    let inbox = format!("{actor_uri}/inbox");
    assert_eq!(
        delivery_job_count(&app, &inbox, "Follow").await,
        1,
        "a duplicate follow must not enqueue a second Follow Activity delivery"
    );

    app.cleanup().await;
}

// ==========================================================================
// (4) Requirement 1.7: self-follow is rejected (422), no relationship
// created
// ==========================================================================

#[tokio::test]
async fn self_follow_is_rejected_with_422() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_owner_with_actor(&app, "fu_self_follow").await;
    let token = issue_token(&app, oauth_app_id, viewer_id, &["follow"]).await;

    let response = follow(&app, &token, viewer_id, None).await;
    assert_eq!(response.status, 422, "got: {response:?}");
    let body = body_json(&response);
    assert!(
        body.get("error").and_then(Value::as_str).is_some(),
        "expected a Mastodon-compatible error body, got: {body}"
    );

    app.cleanup().await;
}

// ==========================================================================
// (5) Requirement 1.5: follow options (reblogs/notify/languages) reflected
// on the returned Relationship
// ==========================================================================

#[tokio::test]
async fn follow_options_are_reflected_on_the_relationship() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_owner_with_actor(&app, "fu_opts_viewer").await;
    let target_id = create_owner_with_actor(&app, "fu_opts_target").await;
    let token = issue_token(&app, oauth_app_id, viewer_id, &["follow"]).await;

    let opts = json!({
        "reblogs": false,
        "notify": true,
        "languages": ["en", "fr"],
    });
    let response = follow(&app, &token, target_id, Some(opts)).await;
    assert_eq!(response.status, 200, "got: {response:?}");
    let body = body_json(&response);
    assert_eq!(body["following"].as_bool(), Some(true), "got: {body}");
    assert_eq!(
        body["showing_reblogs"].as_bool(),
        Some(false),
        "got: {body}"
    );
    assert_eq!(body["notifying"].as_bool(), Some(true), "got: {body}");
    let langs: Vec<String> = body["languages"]
        .as_array()
        .expect("languages must be an array")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(langs, vec!["en".to_string(), "fr".to_string()]);

    app.cleanup().await;
}

/// A default (empty-body) follow request maps to Mastodon's own real
/// per-field defaults (`reblogs: true, notify: false, languages: []`,
/// `src/social_graph/endpoints.rs`'s own `parse_follow_options`) --
/// covers Requirement 1.5's "no options sent" branch alongside the
/// preceding, explicit-options test.
#[tokio::test]
async fn follow_with_no_body_uses_mastodon_default_options() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_owner_with_actor(&app, "fu_defaults_viewer").await;
    let target_id = create_owner_with_actor(&app, "fu_defaults_target").await;
    let token = issue_token(&app, oauth_app_id, viewer_id, &["follow"]).await;

    let response = follow(&app, &token, target_id, None).await;
    assert_eq!(response.status, 200, "got: {response:?}");
    let body = body_json(&response);
    assert_eq!(body["showing_reblogs"].as_bool(), Some(true), "got: {body}");
    assert_eq!(body["notifying"].as_bool(), Some(false), "got: {body}");
    assert_eq!(body["languages"].as_array().map(Vec::len), Some(0));

    app.cleanup().await;
}

// ==========================================================================
// (6) Requirement 1.4: unfollow removes the relationship and delivers
// Undo(Follow) for a remote target
// ==========================================================================

#[tokio::test]
async fn unfollow_removes_relationship_and_enqueues_undo_delivery_for_remote_target() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_owner_with_actor(&app, "fu_unfollow_viewer").await;
    let token = issue_token(&app, oauth_app_id, viewer_id, &["follow"]).await;

    let remote_id = app.runtime.ids.next_id();
    let actor_uri = "https://remote.example/users/fu_unfollow_target";
    create_remote_account_with_id(&app, remote_id, actor_uri, "fu_unfollow_target", false).await;

    let followed = follow(&app, &token, remote_id, None).await;
    assert_eq!(followed.status, 200, "got: {followed:?}");
    assert_eq!(body_json(&followed)["following"].as_bool(), Some(true));

    let unfollowed = unfollow(&app, &token, remote_id).await;
    assert_eq!(unfollowed.status, 200, "got: {unfollowed:?}");
    let body = body_json(&unfollowed);
    assert_eq!(body["following"].as_bool(), Some(false), "got: {body}");

    let inbox = format!("{actor_uri}/inbox");
    assert_eq!(
        delivery_job_count(&app, &inbox, "Undo").await,
        1,
        "unfollow must deliver an Undo(Follow) Activity to the remote target's inbox"
    );

    app.cleanup().await;
}

// ==========================================================================
// (7) Requirement 1.6's idempotency principle applied to unfollow: no
// existing relationship -> no-op success, no Undo delivered
// ==========================================================================

#[tokio::test]
async fn unfollow_with_no_existing_relationship_is_an_idempotent_no_op() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_owner_with_actor(&app, "fu_noop_unfollow_viewer").await;
    let token = issue_token(&app, oauth_app_id, viewer_id, &["follow"]).await;

    let remote_id = app.runtime.ids.next_id();
    let actor_uri = "https://remote.example/users/fu_noop_unfollow_target";
    create_remote_account_with_id(&app, remote_id, actor_uri, "fu_noop_unfollow_target", false)
        .await;

    let response = unfollow(&app, &token, remote_id).await;
    assert_eq!(response.status, 200, "got: {response:?}");
    let body = body_json(&response);
    assert_eq!(body["following"].as_bool(), Some(false), "got: {body}");

    let inbox = format!("{actor_uri}/inbox");
    assert_eq!(
        delivery_job_count(&app, &inbox, "Undo").await,
        0,
        "unfollowing a target never followed must not deliver any Undo Activity"
    );

    app.cleanup().await;
}
