//! Integration tests for social-graph's follow-request (approval) HTTP
//! surface (`.kiro/specs/social-graph/tasks.md`, task 6.1 "関係操作の統合
//! テスト", `_Boundary: SocialGraphEndpoints, FollowService,
//! FollowRequestService, BlockService, MuteService_`), driven against the
//! real, `spawn_test_app`-booted application router.
//!
//! design.md's File Structure Plan names this exact filename
//! (`follow_request_it.rs`: "ロック済み宛保留・一覧ページネーション・
//! authorize/reject と Accept/Reject 配送・follow_request 通知イベント
//! emit（統合）") and its own Testing Strategy bullet ("follow_requests:
//! ロック済み宛で保留生成、一覧のページネーション、authorize で Accept 配送
//! + 確立、reject で Reject 配送 + 削除（2.1, 2.2, 2.3, 2.4）").
//!
//! Covers Requirements 2.1, 2.2, 2.3, 2.4 (this task's own assigned subset).
//!
//! ## Requirement 2.1's "pending creation" is seeded directly, not re-derived
//! through `InboundActivityHandler`
//! Requirement 2.1 describes a Follow arriving at a **locked local actor**
//! becoming a pending request. On a single kawasemi instance, the *only*
//! path that can reach this literal shape (destination-locked local actor,
//! non-privileged source) is a **remote** requester's Follow processed
//! through `SocialGraphInboundHandler` (task 4.1) — same-server local/local
//! Follows always bypass approval entirely (Requirement 3.1, exercised
//! instead by `tests/same_server_skip_it.rs`), and this HTTP surface's own
//! `follow` endpoint can only ever act as a **local** caller (`ctx.actor_id`,
//! see `src/social_graph/endpoints.rs`'s own doc comment), never as a
//! simulated remote one. `InboundActivityHandler`'s own receive-path
//! behavior is task 6.2's boundary (`_Boundary: InboundHandler,
//! BlockPolicyImpl, RelProviderImpl_`), not this task's (`_Boundary:
//! SocialGraphEndpoints, FollowService, FollowRequestService, BlockService,
//! MuteService_`) — so this file does not re-derive it. Instead, this file
//! seeds the *already-recorded* pending state an inbound Follow would have
//! produced directly via `social_graph::repository::upsert_request`
//! (`direction: Inbound`) — the exact row shape `SocialGraphInboundHandler::
//! handle`'s own `record_pending` call would have persisted — then exercises
//! this task's own boundary (`list_follow_requests`/`authorize_follow_request`/
//! `reject_follow_request`, `FollowRequestService`) against it. This mirrors
//! `tests/federation_bootstrap_it.rs`'s own established "seed a cached row
//! directly to bypass an out-of-boundary upstream step" precedent
//! (`seed_remote_public_key`).
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
use kawasemi::domain::{AccountRef, Id};
use kawasemi::oauth::app_repository::{self, NewApp};
use kawasemi::oauth::model::ScopeSet as PlaceholderScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::social_graph::model::{FollowRequest, FollowRequestDirection};
use kawasemi::social_graph::repository;
use kawasemi::test_harness::{TestApp, spawn_test_app};

// ---- raw HTTP plumbing (duplicated per-file; see this module's doc
// comment) ----

#[derive(Debug)]
struct RawResponse {
    status: u16,
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
            name: "Follow Request IT Client".to_string(),
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
            display_name: "Follow Request IT Actor".to_string(),
            summary: "a follow_request_it integration test fixture".to_string(),
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

/// Seeds an already-recorded *inbound* pending follow request (see this
/// module's doc comment for why this bypasses `InboundActivityHandler`
/// itself) from `requester` (remote) to `target` (local, locked).
async fn seed_inbound_pending_request(app: &TestApp, requester: Id, target: Id, activity_id: &str) {
    let now = app.runtime.clock.now();
    let id = app.runtime.ids.next_id();
    repository::upsert_request(
        &app.pool,
        id,
        &FollowRequest {
            requester: AccountRef::Remote(requester),
            target: AccountRef::Local(target),
            direction: FollowRequestDirection::Inbound,
            activity_id: activity_id.to_string(),
            created_at: now,
        },
    )
    .await
    .expect("seeding an inbound pending follow request must succeed");
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

fn parse_link_header(headers: &HashMap<String, String>) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Some(raw) = headers.get("link") else {
        return out;
    };
    for entry in raw.split(',') {
        let entry = entry.trim();
        let Some((url_part, rel_part)) = entry.split_once(';') else {
            continue;
        };
        let url = url_part
            .trim()
            .trim_start_matches('<')
            .trim_end_matches('>');
        if let Some(rel_value) = rel_part
            .trim()
            .strip_prefix("rel=\"")
            .and_then(|s| s.strip_suffix('"'))
        {
            out.insert(rel_value.to_string(), url.to_string());
        }
    }
    out
}

// ==========================================================================
// (1) Requirement 2.1 / 2.2: a pending inbound request appears in
// list_follow_requests
// ==========================================================================

#[tokio::test]
async fn pending_inbound_request_appears_in_list_follow_requests() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let owner_id = create_owner_with_actor(&app, "fr_list_owner").await;
    lock_actor(&app, owner_id).await;
    let token = issue_token(&app, oauth_app_id, owner_id, &["follow"]).await;

    let remote_id = app.runtime.ids.next_id();
    let actor_uri = "https://remote.example/users/fr_list_requester";
    create_remote_account_with_id(&app, remote_id, actor_uri, "fr_list_requester").await;
    seed_inbound_pending_request(
        &app,
        remote_id,
        owner_id,
        "https://remote.example/activities/fr-list-1",
    )
    .await;

    let response = raw_request(
        app.address,
        "GET",
        "/api/v1/follow_requests",
        &[("Authorization", &bearer_header(&token))],
    )
    .await;
    assert_eq!(response.status, 200, "got: {response:?}");
    let body = body_json(&response);
    let items = body.as_array().expect("follow_requests must be an array");
    assert_eq!(items.len(), 1, "got: {body}");
    assert_eq!(
        items[0]["id"].as_str(),
        Some(remote_id.as_i64().to_string()).as_deref()
    );

    app.cleanup().await;
}

// ==========================================================================
// (2) Requirement 2.2: list pagination (Link header, limit)
// ==========================================================================

#[tokio::test]
async fn list_follow_requests_paginates_with_link_header() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let owner_id = create_owner_with_actor(&app, "fr_page_owner").await;
    lock_actor(&app, owner_id).await;
    let token = issue_token(&app, oauth_app_id, owner_id, &["follow"]).await;

    for i in 0..3 {
        let remote_id = app.runtime.ids.next_id();
        let actor_uri = format!("https://remote.example/users/fr_page_requester_{i}");
        create_remote_account_with_id(
            &app,
            remote_id,
            &actor_uri,
            &format!("fr_page_requester_{i}"),
        )
        .await;
        seed_inbound_pending_request(
            &app,
            remote_id,
            owner_id,
            &format!("https://remote.example/activities/fr-page-{i}"),
        )
        .await;
    }

    let first_page = raw_request(
        app.address,
        "GET",
        "/api/v1/follow_requests?limit=2",
        &[("Authorization", &bearer_header(&token))],
    )
    .await;
    assert_eq!(first_page.status, 200, "got: {first_page:?}");
    let first_body = body_json(&first_page);
    let first_items = first_body
        .as_array()
        .expect("follow_requests must be an array");
    assert_eq!(first_items.len(), 2, "got: {first_body}");

    let links = parse_link_header(&first_page.headers);
    let next_url = links
        .get("next")
        .expect("a first page with more remaining items must carry a next Link relation");

    // The `next` Link is an absolute URL; extract just the path+query for
    // our raw-socket client.
    let next_path = next_url
        .splitn(4, '/')
        .nth(3)
        .map(|rest| format!("/{rest}"))
        .expect("next Link must carry a path");

    let second_page = raw_request(
        app.address,
        "GET",
        &next_path,
        &[("Authorization", &bearer_header(&token))],
    )
    .await;
    assert_eq!(second_page.status, 200, "got: {second_page:?}");
    let second_body = body_json(&second_page);
    let second_items = second_body
        .as_array()
        .expect("follow_requests must be an array");
    assert_eq!(second_items.len(), 1, "got: {second_body}");

    // Every item across both pages is distinct (no duplication/omission).
    let mut all_ids: Vec<String> = first_items
        .iter()
        .chain(second_items.iter())
        .map(|item| item["id"].as_str().unwrap().to_string())
        .collect();
    all_ids.sort();
    all_ids.dedup();
    assert_eq!(
        all_ids.len(),
        3,
        "expected 3 distinct requesters across pages"
    );

    app.cleanup().await;
}

// ==========================================================================
// (3) Requirement 2.3: authorize establishes the follow and delivers
// Accept(Follow)
// ==========================================================================

#[tokio::test]
async fn authorize_establishes_follow_and_delivers_accept() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let owner_id = create_owner_with_actor(&app, "fr_auth_owner").await;
    lock_actor(&app, owner_id).await;
    let token = issue_token(&app, oauth_app_id, owner_id, &["follow"]).await;

    let remote_id = app.runtime.ids.next_id();
    let actor_uri = "https://remote.example/users/fr_auth_requester";
    create_remote_account_with_id(&app, remote_id, actor_uri, "fr_auth_requester").await;
    seed_inbound_pending_request(
        &app,
        remote_id,
        owner_id,
        "https://remote.example/activities/fr-auth-1",
    )
    .await;

    let path = format!("/api/v1/follow_requests/{}/authorize", remote_id.as_i64());
    let response = raw_request(
        app.address,
        "POST",
        &path,
        &[("Authorization", &bearer_header(&token))],
    )
    .await;
    assert_eq!(response.status, 200, "got: {response:?}");
    let body = body_json(&response);
    // owner_id's own relationship *to* the requester: the requester now
    // follows the owner (Requirement 2.3's establishment), so from the
    // owner's viewpoint `followed_by` is true and `requested_by` is false
    // (no longer pending).
    assert_eq!(body["followed_by"].as_bool(), Some(true), "got: {body}");
    assert_eq!(body["requested_by"].as_bool(), Some(false), "got: {body}");

    let inbox = format!("{actor_uri}/inbox");
    assert_eq!(
        delivery_job_count(&app, &inbox, "Accept").await,
        1,
        "authorize must deliver an Accept(Follow) Activity to the requester's inbox"
    );

    // The pending request must no longer be listed.
    let list_response = raw_request(
        app.address,
        "GET",
        "/api/v1/follow_requests",
        &[("Authorization", &bearer_header(&token))],
    )
    .await;
    let list_body = body_json(&list_response);
    assert_eq!(
        list_body.as_array().map(Vec::len),
        Some(0),
        "an authorized request must no longer be pending, got: {list_body}"
    );

    app.cleanup().await;
}

/// Authorizing a nonexistent pending request is a 404 (Requirement 10.5,
/// exercised here as part of this task's own `authorize_request` coverage
/// since `FollowRequestService::authorize_request`'s own 404 falls
/// naturally out of a no-op `promote_pending`).
#[tokio::test]
async fn authorize_with_no_pending_request_is_404() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let owner_id = create_owner_with_actor(&app, "fr_auth_404_owner").await;
    let token = issue_token(&app, oauth_app_id, owner_id, &["follow"]).await;

    let remote_id = app.runtime.ids.next_id();
    create_remote_account_with_id(
        &app,
        remote_id,
        "https://remote.example/users/fr_auth_404_requester",
        "fr_auth_404_requester",
    )
    .await;

    let path = format!("/api/v1/follow_requests/{}/authorize", remote_id.as_i64());
    let response = raw_request(
        app.address,
        "POST",
        &path,
        &[("Authorization", &bearer_header(&token))],
    )
    .await;
    assert_eq!(response.status, 404, "got: {response:?}");

    app.cleanup().await;
}

// ==========================================================================
// (4) Requirement 2.4: reject drops the pending request and delivers
// Reject(Follow)
// ==========================================================================

#[tokio::test]
async fn reject_drops_pending_request_and_delivers_reject() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let owner_id = create_owner_with_actor(&app, "fr_reject_owner").await;
    lock_actor(&app, owner_id).await;
    let token = issue_token(&app, oauth_app_id, owner_id, &["follow"]).await;

    let remote_id = app.runtime.ids.next_id();
    let actor_uri = "https://remote.example/users/fr_reject_requester";
    create_remote_account_with_id(&app, remote_id, actor_uri, "fr_reject_requester").await;
    seed_inbound_pending_request(
        &app,
        remote_id,
        owner_id,
        "https://remote.example/activities/fr-reject-1",
    )
    .await;

    let path = format!("/api/v1/follow_requests/{}/reject", remote_id.as_i64());
    let response = raw_request(
        app.address,
        "POST",
        &path,
        &[("Authorization", &bearer_header(&token))],
    )
    .await;
    assert_eq!(response.status, 200, "got: {response:?}");
    let body = body_json(&response);
    assert_eq!(
        body["followed_by"].as_bool(),
        Some(false),
        "a rejected request must never establish a follow, got: {body}"
    );
    assert_eq!(body["requested_by"].as_bool(), Some(false), "got: {body}");

    let inbox = format!("{actor_uri}/inbox");
    assert_eq!(
        delivery_job_count(&app, &inbox, "Reject").await,
        1,
        "reject must deliver a Reject(Follow) Activity to the requester's inbox"
    );

    let list_response = raw_request(
        app.address,
        "GET",
        "/api/v1/follow_requests",
        &[("Authorization", &bearer_header(&token))],
    )
    .await;
    let list_body = body_json(&list_response);
    assert_eq!(
        list_body.as_array().map(Vec::len),
        Some(0),
        "a rejected request must be removed from the pending list, got: {list_body}"
    );

    app.cleanup().await;
}
