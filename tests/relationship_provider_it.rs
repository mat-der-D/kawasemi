//! Integration tests proving social-graph's `RelationshipStateProvider`
//! delegation-boundary implementation (`RelProviderImpl`, task 4.3) actually
//! supplies real relationship flags through accounts-and-instance's live
//! `GET /api/v1/accounts/relationships` endpoint, once registered (task
//! 5.2's bootstrap wiring) -- not merely the always-"no relationship"
//! default (`.kiro/specs/social-graph/tasks.md`, task 6.2 "受信処理・署名拒
//! 否・プロバイダ統合テスト", `_Boundary: InboundHandler, BlockPolicyImpl,
//! RelProviderImpl_`).
//!
//! design.md's File Structure Plan names this exact filename
//! (`relationship_provider_it.rs`: "RelationshipStateProvider /
//! AccountCountsProvider 供給で accounts-and-instance の relationships/
//! counts が実値を返す（統合）") and its own Testing Strategy bullet
//! ("RelationshipStateProvider: 本 spec 登録後に accounts-and-instance の
//! relationships が実フラグを返す（8.2, 8.3）").
//!
//! Covers Requirements 8.2, 8.3.
//!
//! ## Goes deeper than `tests/relationships_it.rs`/`tests/follow_request_it.rs`/
//! `tests/mute_block_it.rs`'s own incidental use of this same oracle
//! Task 6.1's own Implementation Note (`tasks.md`) already used `GET
//! /api/v1/accounts/relationships` as a read-only oracle for its own,
//! different assertions. This file's own job (task 6.2's explicit brief) is
//! to prove the provider registration itself, not merely read through it:
//! 1. The **unregistered** default (`NoRelationshipProvider`, Requirement
//!    5.4 of accounts-and-instance) really does answer "no relationship"
//!    for every flag -- proven directly against a bare, freshly constructed
//!    `AccountPortsRegistry` (never bootstrap-wired to social-graph), the
//!    literal "before" baseline this task's own brief asks for.
//! 2. Contrasted against the **live, real** `spawn_test_app` instance (whose
//!    `AccountPortsRegistry` already has `RelProviderImpl` registered via
//!    task 5.2's bootstrap wiring, by construction) genuinely returning
//!    non-default, real flags for established follow/block/mute
//!    relationships, for **several target ids in one batched request** --
//!    including one untouched target correctly still reporting "no
//!    relationship" *within that same real, registered response* (proving
//!    the real provider reduces to the same baseline for an account that
//!    genuinely has no relationship, rather than the baseline only ever
//!    being observable pre-registration).
//!
//! ## No HTTP client dependency: raw sockets (this crate's established
//! per-file-duplicated convention).

use std::collections::HashMap;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use kawasemi::accounts::AccountPortsRegistry;
use kawasemi::accounts::model::{ProfileField, RemoteAccount};
use kawasemi::accounts::remote_repository::upsert_remote;
use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor};
use kawasemi::domain::{AccountRef, Id};
use kawasemi::oauth::app_repository::{self, NewApp};
use kawasemi::oauth::model::ScopeSet as PlaceholderScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::test_harness::{TestApp, spawn_test_app};

// ==========================================================================
// Raw HTTP plumbing (duplicated per-file; see this module's doc comment).
// ==========================================================================

#[derive(Debug)]
struct RawResponse {
    status: u16,
    #[allow(dead_code)]
    headers: HashMap<String, String>,
    body: String,
}

async fn raw_get(addr: std::net::SocketAddr, path: &str, headers: &[(&str, &str)]) -> RawResponse {
    let mut stream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr))
        .await
        .expect("connecting to the test listener must not time out")
        .expect("connect");

    let mut request = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n");
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

async fn raw_post_empty(
    addr: std::net::SocketAddr,
    path: &str,
    extra_headers: &[(&str, &str)],
) -> RawResponse {
    let mut stream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr))
        .await
        .expect("connecting to the test listener must not time out")
        .expect("connect");

    let mut request = format!("POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n");
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

// ==========================================================================
// (1) Requirement 8.2's own "before" baseline: an unregistered
// `RelationshipStateProvider` (a bare `AccountPortsRegistry`, never wired to
// social-graph) answers accounts-and-instance's own Requirement-5.4 "no
// relationship" default for every flag, ignoring `viewer` entirely, and
// touching no DB at all -- this test spawns no `TestApp`, needs none.
// ==========================================================================

#[tokio::test]
async fn unregistered_relationship_state_provider_answers_the_no_relationship_baseline() {
    let registry = AccountPortsRegistry::new();
    let viewer = Id::from_i64(1);
    let targets = vec![
        AccountRef::Local(Id::from_i64(2)),
        AccountRef::Remote(Id::from_i64(3)),
    ];

    let views = registry
        .relationships(viewer, &targets)
        .await
        .expect("the default NoRelationshipProvider must never fail");

    assert_eq!(
        views.len(),
        2,
        "one view per requested target, got: {views:?}"
    );
    for view in &views {
        assert!(!view.following, "got: {view:?}");
        assert!(!view.showing_reblogs, "got: {view:?}");
        assert!(!view.notifying, "got: {view:?}");
        assert!(view.languages.is_empty(), "got: {view:?}");
        assert!(!view.followed_by, "got: {view:?}");
        assert!(!view.blocking, "got: {view:?}");
        assert!(!view.blocked_by, "got: {view:?}");
        assert!(!view.muting, "got: {view:?}");
        assert!(!view.muting_notifications, "got: {view:?}");
        assert!(!view.requested, "got: {view:?}");
        assert!(!view.requested_by, "got: {view:?}");
        assert!(!view.domain_blocking, "got: {view:?}");
        assert!(!view.endorsed, "got: {view:?}");
        assert!(view.note.is_empty(), "got: {view:?}");
    }
}

// ==========================================================================
// Fixtures for the "after registration, real values" half.
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
            name: "Relationship Provider IT Client".to_string(),
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
            display_name: "Relationship Provider IT Actor".to_string(),
            summary: "a relationship_provider_it integration test fixture".to_string(),
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

async fn mute(app: &TestApp, token: &str, target_id: Id) -> RawResponse {
    let path = format!("/api/v1/accounts/{}/mute", target_id.as_i64());
    raw_post_empty(
        app.address,
        &path,
        &[("Authorization", &bearer_header(token))],
    )
    .await
}

// ==========================================================================
// (2) Requirements 8.2, 8.3: after registration (already wired via the real
// bootstrap, task 5.2), the live `GET /api/v1/accounts/relationships`
// endpoint returns real, non-default flags for several targets at once --
// including one untouched target still correctly reporting "no
// relationship" within that same registered, real response.
// ==========================================================================

#[tokio::test]
async fn registered_provider_returns_real_flags_for_multiple_targets_in_one_batched_request() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let viewer_id = create_owner_with_actor(&app, "rp_viewer").await;
    let viewer_token = issue_token(&app, oauth_app_id, viewer_id, &["follow"]).await;
    let viewer_read_token = issue_token(&app, oauth_app_id, viewer_id, &["read:follows"]).await;

    // Target 1: an established follow.
    let followed_id = app.runtime.ids.next_id();
    create_remote_account_with_id(
        &app,
        followed_id,
        "https://remote.example/users/rp_followed",
        "rp_followed",
        false,
    )
    .await;
    let follow_response = follow(&app, &viewer_token, followed_id).await;
    assert_eq!(follow_response.status, 200, "got: {follow_response:?}");
    assert_eq!(
        body_json(&follow_response)["following"].as_bool(),
        Some(true)
    );

    // Target 2: a block.
    let blocked_id = app.runtime.ids.next_id();
    create_remote_account_with_id(
        &app,
        blocked_id,
        "https://remote.example/users/rp_blocked",
        "rp_blocked",
        false,
    )
    .await;
    let block_response = block(&app, &viewer_token, blocked_id).await;
    assert_eq!(block_response.status, 200, "got: {block_response:?}");
    assert_eq!(body_json(&block_response)["blocking"].as_bool(), Some(true));

    // Target 3: a mute (default options: notifications muted too).
    let muted_id = app.runtime.ids.next_id();
    create_remote_account_with_id(
        &app,
        muted_id,
        "https://remote.example/users/rp_muted",
        "rp_muted",
        false,
    )
    .await;
    let mute_response = mute(&app, &viewer_token, muted_id).await;
    assert_eq!(mute_response.status, 200, "got: {mute_response:?}");
    assert_eq!(body_json(&mute_response)["muting"].as_bool(), Some(true));

    // Target 4: an untouched account with no relationship at all.
    let untouched_id = app.runtime.ids.next_id();
    create_remote_account_with_id(
        &app,
        untouched_id,
        "https://remote.example/users/rp_untouched",
        "rp_untouched",
        false,
    )
    .await;

    // One batched request across all four targets, in this exact order.
    let path = format!(
        "/api/v1/accounts/relationships?id={}&id={}&id={}&id={}",
        followed_id.as_i64(),
        blocked_id.as_i64(),
        muted_id.as_i64(),
        untouched_id.as_i64(),
    );
    let response = raw_get(
        app.address,
        &path,
        &[("Authorization", &bearer_header(&viewer_read_token))],
    )
    .await;
    assert_eq!(response.status, 200, "got: {response:?}");
    let body = body_json(&response);
    let items = body.as_array().expect("relationships must return an array");
    assert_eq!(items.len(), 4, "got: {body}");

    let by_id = |id: Id| -> &Value {
        items
            .iter()
            .find(|item| item["id"].as_str() == Some(id.as_i64().to_string()).as_deref())
            .unwrap_or_else(|| panic!("expected an entry for id {id:?} in {body}"))
    };

    let followed_view = by_id(followed_id);
    assert_eq!(
        followed_view["following"].as_bool(),
        Some(true),
        "got: {followed_view}"
    );
    assert_eq!(
        followed_view["blocking"].as_bool(),
        Some(false),
        "got: {followed_view}"
    );
    assert_eq!(
        followed_view["muting"].as_bool(),
        Some(false),
        "got: {followed_view}"
    );
    assert_eq!(followed_view["domain_blocking"].as_bool(), Some(false));

    let blocked_view = by_id(blocked_id);
    assert_eq!(
        blocked_view["blocking"].as_bool(),
        Some(true),
        "got: {blocked_view}"
    );
    assert_eq!(
        blocked_view["following"].as_bool(),
        Some(false),
        "got: {blocked_view}"
    );
    assert_eq!(
        blocked_view["muting"].as_bool(),
        Some(false),
        "got: {blocked_view}"
    );

    let muted_view = by_id(muted_id);
    assert_eq!(
        muted_view["muting"].as_bool(),
        Some(true),
        "got: {muted_view}"
    );
    assert_eq!(
        muted_view["muting_notifications"].as_bool(),
        Some(true),
        "the default mute options mute notifications too, got: {muted_view}"
    );
    assert_eq!(
        muted_view["following"].as_bool(),
        Some(false),
        "got: {muted_view}"
    );
    assert_eq!(
        muted_view["blocking"].as_bool(),
        Some(false),
        "got: {muted_view}"
    );

    // The untouched target, resolved through the very same *registered*
    // provider in the very same batched call, still correctly reports "no
    // relationship" for every flag -- the real provider reduces to the same
    // baseline test (1) proved for the unregistered default, but this time
    // sitting alongside genuinely non-default entries in one response.
    let untouched_view = by_id(untouched_id);
    assert_eq!(
        untouched_view["following"].as_bool(),
        Some(false),
        "got: {untouched_view}"
    );
    assert_eq!(
        untouched_view["followed_by"].as_bool(),
        Some(false),
        "got: {untouched_view}"
    );
    assert_eq!(
        untouched_view["blocking"].as_bool(),
        Some(false),
        "got: {untouched_view}"
    );
    assert_eq!(
        untouched_view["blocked_by"].as_bool(),
        Some(false),
        "got: {untouched_view}"
    );
    assert_eq!(
        untouched_view["muting"].as_bool(),
        Some(false),
        "got: {untouched_view}"
    );
    assert_eq!(
        untouched_view["requested"].as_bool(),
        Some(false),
        "got: {untouched_view}"
    );
    assert_eq!(
        untouched_view["requested_by"].as_bool(),
        Some(false),
        "got: {untouched_view}"
    );
    assert_eq!(
        untouched_view["domain_blocking"].as_bool(),
        Some(false),
        "got: {untouched_view}"
    );

    app.cleanup().await;
}
