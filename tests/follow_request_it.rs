//! Integration tests for task 6.1's own observable completion condition
//! (`.kiro/specs/social-graph/tasks.md`, "6.1 (P) 関係操作の統合テスト") —
//! this file covers exactly the `follow_requests` bullet of design.md's own
//! "Integration Tests（`spawn_test_app` 上）" Testing Strategy section:
//! "follow_requests: ロック済み宛で保留生成、一覧のページネーション、
//! authorize で Accept 配送 + 確立、reject で Reject 配送 + 削除（2.1, 2.2,
//! 2.3, 2.4）" — driven through the real, mounted `GET
//! /api/v1/follow_requests` / `POST /api/v1/follow_requests/:id/authorize` /
//! `.../reject` handlers (`src/social_graph/endpoints.rs`, wired by
//! `src/server.rs`'s `social_graph_router`).
//!
//! ## Why every pending row here is *seeded*, not produced by a real
//! inbound Follow POST
//! Requirement 3.1/3.4 (`tests/same_server_skip_it.rs`'s own boundary)
//! establishes that two *local* actors on this same instance can never
//! produce a genuinely pending **inbound** follow request via the public
//! API: `FollowApprovalPolicy::requires_approval` always resolves same-
//! server (both `AccountRef::Local`) pairs to immediate establishment
//! regardless of lock state (design.md, "同一サーバー承認スキップ"). The
//! only way a local actor ever actually accumulates a pending *inbound*
//! request through this app's own real request pipeline is a genuinely
//! signed Follow Activity POSTed to that actor's inbox by a remote peer —
//! exercising that whole receive pipeline is `InboundHandler`'s own
//! boundary (task 4.1) and design.md's Testing Strategy places it in a
//! *separate* Integration Tests bullet ("受信 Activity: 受信 Follow...") from
//! this file's own ("follow_requests: ..."), matching task 6.2's separate
//! `_Boundary: InboundHandler, BlockPolicyImpl, RelProviderImpl_` (not named
//! by this task's own `_Boundary: SocialGraphEndpoints, FollowService,
//! FollowRequestService, BlockService, MuteService_`).
//!
//! So this file seeds the pending-request *precondition* directly via
//! [`kawasemi::social_graph::repository::upsert_request`] — the exact same
//! `RelationshipRepository` free function `FollowRequestService` itself
//! calls (`follow_request_service.rs`'s own `list_requests`/
//! `authorize_request`/`reject_request` all read/consume rows this same
//! function writes) — then exercises the actual **endpoint layer** under
//! test (`list_follow_requests`/`authorize_follow_request`/
//! `reject_follow_request`) against that seeded state. This mirrors this
//! crate's own established precedent for seeding a repository-level
//! precondition directly rather than re-deriving it through a whole
//! upstream subsystem (e.g. `tests/federation_bootstrap_it.rs::
//! seed_remote_public_key` seeds a cached remote public key directly rather
//! than performing a real HTTP key fetch first).
//!
//! ## No HTTP client dependency: raw sockets (this crate's established
//! `tests/*_it.rs` convention; duplicated per file since each integration
//! test is its own compiled crate).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

use serde_json::Value;
use time::OffsetDateTime;
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

// ---- raw HTTP plumbing -----------------------------------------------------

#[derive(Debug)]
struct RawResponse {
    status: u16,
    headers: HashMap<String, String>,
    body: String,
}

impl RawResponse {
    /// Parses this response's `Link` header (if present) into its
    /// `rel="next"` target URL (mirrors `tests/pagination_it.rs`'s own
    /// `RawResponse::link_targets`, next-only since this file's own
    /// pagination scenario only ever follows forward).
    fn link_next(&self) -> Option<String> {
        let raw = self.headers.get("link")?;
        for part in raw.split(',') {
            let part = part.trim();
            if let Some(url_end) = part.find('>')
                && part.contains("rel=\"next\"")
            {
                return Some(part[1..url_end].to_string());
            }
        }
        None
    }
}

fn path_and_query(url: &str) -> String {
    let (_scheme, after_scheme) = url
        .split_once("://")
        .expect("Link target must be an absolute URL");
    let slash = after_scheme
        .find('/')
        .expect("Link target must carry a path after the origin");
    after_scheme[slash..].to_string()
}

async fn raw_request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
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
            name: "follow_request_it Client".to_string(),
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
            display_name: "Follow Request IT Actor".to_string(),
            summary: "a follow_request_it integration test fixture".to_string(),
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

fn sample_remote_account(id: Id, actor_uri: &str, fetched_at: OffsetDateTime) -> RemoteAccount {
    RemoteAccount {
        id,
        actor_uri: actor_uri.to_string(),
        username: actor_uri.rsplit('/').next().unwrap_or("remote").to_string(),
        domain: "remote.example".to_string(),
        display_name: "Remote Requester".to_string(),
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

/// Seeds a pending **inbound** follow request from `requester` (remote) to
/// `target` (local) — see this file's own doc comment for why this is a
/// direct repository seed rather than a real signed inbound Follow POST.
async fn seed_pending_inbound_request(app: &TestApp, requester: Id, target: Id, activity_id: &str) {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    upsert_request(
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
    .expect("seeding a pending inbound follow request must succeed");
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

// ---- (1) list_follow_requests: pending inbound requesters, paginated --

#[tokio::test]
async fn list_follow_requests_returns_pending_requesters_with_pagination() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let owner_id = create_test_actor(&app, "fr_list_owner").await;
    lock_actor(&app, owner_id).await;

    let mut requester_ids = Vec::new();
    for n in 0..3 {
        let actor_uri = format!("https://remote.example/users/fr_list_requester_{n}");
        let requester_id = create_test_remote(&app, &actor_uri).await;
        seed_pending_inbound_request(
            &app,
            requester_id,
            owner_id,
            &format!("https://remote.example/activities/follow-{n}"),
        )
        .await;
        requester_ids.push(requester_id);
    }

    let token = issue_token(&app, oauth_app_id, owner_id, &["follow"]).await;
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
        .expect("follow_requests must return a JSON array");
    assert_eq!(first_items.len(), 2, "got: {first_body}");

    let next = first_page
        .link_next()
        .expect("a 3rd pending request must yield a rel=\"next\" Link header");
    let second_page = raw_request(
        app.address,
        "GET",
        &path_and_query(&next),
        &[("Authorization", &bearer_header(&token))],
    )
    .await;
    assert_eq!(second_page.status, 200, "got: {second_page:?}");
    let second_body = body_json(&second_page);
    let second_items = second_body
        .as_array()
        .expect("follow_requests page 2 must return a JSON array");
    assert_eq!(second_items.len(), 1, "got: {second_body}");

    let mut seen_ids: Vec<i64> = first_items
        .iter()
        .chain(second_items.iter())
        .map(|item| {
            item["id"]
                .as_str()
                .expect("Account JSON id must be a string")
                .parse::<i64>()
                .expect("Account JSON id must be numeric")
        })
        .collect();
    seen_ids.sort_unstable();
    let mut expected_ids: Vec<i64> = requester_ids.iter().map(Id::as_i64).collect();
    expected_ids.sort_unstable();
    assert_eq!(
        seen_ids, expected_ids,
        "every seeded pending requester must appear exactly once across both pages"
    );

    app.cleanup().await;
}

// ---- (2) authorize: establishes the follow and delivers Accept --------

#[tokio::test]
async fn authorize_follow_request_establishes_follow_and_delivers_accept() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let owner_id = create_test_actor(&app, "fr_auth_owner").await;
    lock_actor(&app, owner_id).await;
    let actor_uri = "https://remote.example/users/fr_auth_requester";
    let requester_id = create_test_remote(&app, actor_uri).await;
    seed_pending_inbound_request(
        &app,
        requester_id,
        owner_id,
        "https://remote.example/activities/follow-auth",
    )
    .await;

    let token = issue_token(&app, oauth_app_id, owner_id, &["follow"]).await;
    let response = raw_request(
        app.address,
        "POST",
        &format!(
            "/api/v1/follow_requests/{}/authorize",
            requester_id.as_i64()
        ),
        &[("Authorization", &bearer_header(&token))],
    )
    .await;

    assert_eq!(response.status, 200, "got: {response:?}");
    let body = body_json(&response);
    assert_eq!(
        body["followed_by"].as_bool(),
        Some(true),
        "the requester must now follow the owner, got: {body}"
    );
    assert_eq!(
        body["requested_by"].as_bool(),
        Some(false),
        "the pending request must no longer be pending, got: {body}"
    );

    assert_eq!(
        follows_row_count(
            &app,
            ("remote", requester_id.as_i64()),
            ("local", owner_id.as_i64())
        )
        .await,
        1,
        "authorize must establish a follows row (requester -> owner)"
    );
    assert_eq!(
        follow_requests_row_count(
            &app,
            ("remote", requester_id.as_i64()),
            ("local", owner_id.as_i64())
        )
        .await,
        0,
        "the pending request row must be consumed"
    );
    assert_eq!(
        delivery_job_count_for_type(&app, &remote_inbox(actor_uri), "Accept").await,
        1,
        "authorize must deliver exactly one Accept(Follow) to the requester's inbox"
    );

    app.cleanup().await;
}

// ---- (3) reject: drops the pending request and delivers Reject --------

#[tokio::test]
async fn reject_follow_request_drops_pending_and_delivers_reject() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let owner_id = create_test_actor(&app, "fr_reject_owner").await;
    lock_actor(&app, owner_id).await;
    let actor_uri = "https://remote.example/users/fr_reject_requester";
    let requester_id = create_test_remote(&app, actor_uri).await;
    seed_pending_inbound_request(
        &app,
        requester_id,
        owner_id,
        "https://remote.example/activities/follow-reject",
    )
    .await;

    let token = issue_token(&app, oauth_app_id, owner_id, &["follow"]).await;
    let response = raw_request(
        app.address,
        "POST",
        &format!("/api/v1/follow_requests/{}/reject", requester_id.as_i64()),
        &[("Authorization", &bearer_header(&token))],
    )
    .await;

    assert_eq!(response.status, 200, "got: {response:?}");
    let body = body_json(&response);
    assert_eq!(body["followed_by"].as_bool(), Some(false), "got: {body}");
    assert_eq!(body["requested_by"].as_bool(), Some(false), "got: {body}");

    assert_eq!(
        follows_row_count(
            &app,
            ("remote", requester_id.as_i64()),
            ("local", owner_id.as_i64())
        )
        .await,
        0,
        "reject must never establish a follow"
    );
    assert_eq!(
        follow_requests_row_count(
            &app,
            ("remote", requester_id.as_i64()),
            ("local", owner_id.as_i64())
        )
        .await,
        0,
        "the pending request row must be dropped"
    );
    assert_eq!(
        delivery_job_count_for_type(&app, &remote_inbox(actor_uri), "Reject").await,
        1,
        "reject must deliver exactly one Reject(Follow) to the requester's inbox"
    );

    // A second reject of the now-consumed request has nothing pending left
    // to act on -- 404, per `follow_request_service.rs`'s own documented
    // "404 for 'nothing pending'" convention.
    let second = raw_request(
        app.address,
        "POST",
        &format!("/api/v1/follow_requests/{}/reject", requester_id.as_i64()),
        &[("Authorization", &bearer_header(&token))],
    )
    .await;
    assert_eq!(second.status, 404, "got: {second:?}");
    assert_error_shape(&second);

    app.cleanup().await;
}

// ---- (4) authorize with nothing pending is a 404 -----------------------

#[tokio::test]
async fn authorize_follow_request_returns_404_when_nothing_pending() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let owner_id = create_test_actor(&app, "fr_404_owner").await;
    let actor_uri = "https://remote.example/users/fr_404_requester";
    let requester_id = create_test_remote(&app, actor_uri).await;

    let token = issue_token(&app, oauth_app_id, owner_id, &["follow"]).await;
    let response = raw_request(
        app.address,
        "POST",
        &format!(
            "/api/v1/follow_requests/{}/authorize",
            requester_id.as_i64()
        ),
        &[("Authorization", &bearer_header(&token))],
    )
    .await;

    assert_eq!(response.status, 404, "got: {response:?}");
    assert_error_shape(&response);

    app.cleanup().await;
}

// ---- (5) scope enforcement: list requires follow/read:follows, and
// authorize/reject require exactly `follow` (Requirement 2.7, 10.1) -----

#[tokio::test]
async fn list_follow_requests_requires_follow_or_read_follows_scope() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let owner_id = create_test_actor(&app, "fr_scope_owner").await;

    let unauthenticated = raw_request(app.address, "GET", "/api/v1/follow_requests", &[]).await;
    assert_eq!(unauthenticated.status, 401, "got: {unauthenticated:?}");
    assert_error_shape(&unauthenticated);

    let wrong_scope_token = issue_token(&app, oauth_app_id, owner_id, &["read:accounts"]).await;
    let forbidden = raw_request(
        app.address,
        "GET",
        "/api/v1/follow_requests",
        &[("Authorization", &bearer_header(&wrong_scope_token))],
    )
    .await;
    assert_eq!(forbidden.status, 403, "got: {forbidden:?}");
    assert_error_shape(&forbidden);

    let read_follows_token = issue_token(&app, oauth_app_id, owner_id, &["read:follows"]).await;
    let accepted = raw_request(
        app.address,
        "GET",
        "/api/v1/follow_requests",
        &[("Authorization", &bearer_header(&read_follows_token))],
    )
    .await;
    assert_eq!(
        accepted.status, 200,
        "a read:follows-scoped token must be accepted, got: {accepted:?}"
    );

    app.cleanup().await;
}

#[tokio::test]
async fn authorize_follow_request_requires_exactly_follow_scope() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let owner_id = create_test_actor(&app, "fr_authscope_owner").await;
    let actor_uri = "https://remote.example/users/fr_authscope_requester";
    let requester_id = create_test_remote(&app, actor_uri).await;
    seed_pending_inbound_request(
        &app,
        requester_id,
        owner_id,
        "https://remote.example/activities/follow-authscope",
    )
    .await;

    // Requirement 2.7 narrows authorize/reject to `follow` alone --
    // `read:follows` (sufficient for the list endpoint) must NOT be
    // accepted here.
    let read_follows_token = issue_token(&app, oauth_app_id, owner_id, &["read:follows"]).await;
    let forbidden = raw_request(
        app.address,
        "POST",
        &format!(
            "/api/v1/follow_requests/{}/authorize",
            requester_id.as_i64()
        ),
        &[("Authorization", &bearer_header(&read_follows_token))],
    )
    .await;
    assert_eq!(
        forbidden.status, 403,
        "authorize must reject a read:follows-only token, got: {forbidden:?}"
    );
    assert_error_shape(&forbidden);

    let follow_token = issue_token(&app, oauth_app_id, owner_id, &["follow"]).await;
    let accepted = raw_request(
        app.address,
        "POST",
        &format!(
            "/api/v1/follow_requests/{}/authorize",
            requester_id.as_i64()
        ),
        &[("Authorization", &bearer_header(&follow_token))],
    )
    .await;
    assert_eq!(accepted.status, 200, "got: {accepted:?}");

    app.cleanup().await;
}
