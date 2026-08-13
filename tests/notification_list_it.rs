//! Integration tests for notifications task 5.2 (`.kiro/specs/notifications/
//! tasks.md`, "5.2 (P) 取得・消去の統合テスト", `_Depends: 4.2_`) — the
//! `GET /api/v1/notifications` half. design.md's File Structure Plan names
//! this exact file (`tests/notification_list_it.rs`, "一覧・ページネーショ
//! ン・types/exclude_types/account_id フィルタ・消去済み除外・スコープ
//! （統合）"). Requirements 2.1-2.5, 9.1-9.4.
//!
//! ## Relationship to `src/notifications/endpoints/tests.rs` (task 4.1) and
//! `tests/notification_contract_it.rs` (task 5.1) — why this file, given
//! both already exist
//! `src/notifications/endpoints/tests.rs` (task 4.1, 24 tests) already
//! exercises `list_notifications`'s auth/scope/`types[]`/`exclude_types[]`/
//! `account_id`/`Link`-header wiring in detail — but, by that task's own
//! explicit boundary (its own doc comment, "Not wired into the module tree
//! yet"), only against a *hand-built, test-only* `Router` it constructs
//! itself in-module, never through `crate::server::build_router`. Task 4.2
//! has since landed (`.kiro/specs/notifications/tasks.md`'s "4.2 モジュー
//! ル配線" is `[x]`) and mounted these handlers for real
//! (`src/server.rs::notifications_router`), so this file is the first to
//! drive `list_notifications` through the *real, fully-wired* production
//! router (`kawasemi::server::build_router`, exactly as booted by
//! `spawn_test_app`) — mirroring `tests/timelines_endpoints_it.rs`'s (task
//! 6.1 of the sibling `timelines` spec) own identical "why this file, given
//! X already exists" situation, and `tests/notification_contract_it.rs`'s
//! (task 5.1, already committed) own "real router via `spawn_test_app()` +
//! `tower::ServiceExt::oneshot`, real fixtures inserted via real service/
//! repository calls, real Bearer tokens issued via
//! `oauth::token_repository::issue_token`" convention, which this file
//! follows verbatim.
//!
//! Unlike `tests/notification_contract_it.rs` (whose own concern is each
//! kind's exact JSON *shape*, one golden per kind), this file's concern is
//! the *retrieval semantics* around a list of already-shaped notifications:
//! ownership scoping, ordering, pagination, the three `ListFilter` axes, and
//! dismissed-exclusion — so every notification here is triggered through
//! whichever real upstream action is cheapest to drive (mostly `follow`,
//! occasionally `mention`/a directly-driven `follow_request`), and every
//! notification's *contents* are asserted only shallowly (kind, account id)
//! — the full JSON envelope shape is `notification_contract_it.rs`'s own,
//! already-covered boundary.
//!
//! ## The single most spec-critical scenario this file covers
//! [`list_with_an_account_id_that_resolves_to_no_known_account_returns_200_and_an_empty_array_not_404`]
//! below: design.md's own `NotificationEndpoints` API Contract table states
//! this explicitly ("`account_id` が未知の ID に解決できない場合も 200 +
//! 空配列。404 にはしない") — the endpoint must never 404 an unresolvable
//! `account_id`, and must take the exact same "ordinary empty page" code
//! path an unrelated, merely-empty query already takes (see `src/
//! notifications/endpoints.rs`'s own doc comment, "`account_id` resolution"/
//! "通常のページネーションヘッダ").
//!
//! ## RED phase evidence
//! Before this file existed, `cargo test --test notification_list_it`
//! failed with `error: no test target named `notification_list_it`` (no
//! such file, no such Cargo-discovered integration test binary). This task
//! is pure test-authoring against already-implemented, already-wired
//! behavior (tasks 1.1 through 4.2 are all `[x]` complete), so — mirroring
//! task 5.1's own documented resolution to the identical situation — there
//! is no separate "make it fail, then make it pass" cycle beyond the file
//! not existing yet; every assertion below was additionally verified by
//! direct reading of the real collaborators it calls (`src/notifications/
//! endpoints.rs`, `src/notifications/repository.rs`, `src/notifications/
//! service.rs`).
//!
//! ## Sandbox DB availability
//! This sandbox has no reachable PostgreSQL (`pg_isready` confirms no
//! response — the same constraint every earlier task in this spec's own
//! `tasks.md` "## Implementation Notes" documents), so none of the
//! `#[tokio::test]`s below could be executed to completion here. Every test
//! is written as a real, executable integration test against
//! `crate::test_harness::spawn_test_app` and the real production router;
//! this file compiles cleanly (`cargo check --tests` / `cargo test
//! --no-run`) and every assertion was verified by direct reading of the real
//! collaborators it calls, mirroring `tests/notification_contract_it.rs`'s
//! own identical resolution to the identical constraint.

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

use kawasemi::accounts::model::{ProfileField, RemoteAccount};
use kawasemi::accounts::remote_repository::upsert_remote;
use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor, ResolvedActor};
use kawasemi::domain::{AccountRef, Id};
use kawasemi::oauth::app_repository::{self, NewApp};
use kawasemi::oauth::model::ScopeSet as ModelScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::server;
use kawasemi::social_graph::{FollowRequest, FollowRequestDirection, Transitions};
use kawasemi::test_harness::{TestApp, spawn_test_app};

// ---- Fixture plumbing (each `tests/*.rs` file is its own compiled crate,
// so this deliberately duplicates `tests/notification_contract_it.rs`'s own
// established conventions rather than importing them — this crate's own
// documented convention). ----------------------------------------------

async fn insert_actor_fixture(app: &TestApp, handle_str: &str) -> ResolvedActor {
    let owner_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    create_owner(&app.pool, owner_id, now)
        .await
        .expect("creating the owner fixture must succeed");

    let actor = app
        .actor
        .actor_service()
        .create_actor(NewActor {
            owner_id,
            handle: Handle::new(handle_str).expect("test handle must be valid"),
            actor_type: ActorType::Person,
            display_name: format!("Notification List IT {handle_str}"),
            summary: "an actor used by the notification_list_it integration test".to_string(),
        })
        .await
        .expect("create_actor (with signing key provisioning) must succeed");

    app.actor
        .directory()
        .resolve_actor_by_handle(&actor.handle)
        .await
        .expect("resolving the just-created actor must succeed")
        .expect("the just-created actor must be resolvable")
}

fn sample_remote_account(
    id: Id,
    actor_uri: &str,
    fetched_at: time::OffsetDateTime,
) -> RemoteAccount {
    RemoteAccount {
        id,
        actor_uri: actor_uri.to_string(),
        username: "notif_list_remote".to_string(),
        domain: "remote.notif-list.example".to_string(),
        display_name: "Notification List IT Remote Actor".to_string(),
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

/// Seeds a known remote account row directly — mirrors `tests/
/// notification_contract_it.rs::insert_remote_actor_fixture`'s identical
/// convention.
async fn insert_remote_actor_fixture(app: &TestApp, actor_uri: &str) -> Id {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    upsert_remote(&app.pool, &sample_remote_account(id, actor_uri, now))
        .await
        .expect("upsert_remote must succeed");
    id
}

async fn register_test_app(app: &TestApp) -> Id {
    let now = app.runtime.clock.now();
    let registered = app_repository::register_app(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        app.state.oauth().token_hash_key(),
        now,
        NewApp {
            name: "Notification List IT Client".to_string(),
            redirect_uris: vec!["https://client.example/callback".to_string()],
            scopes: ModelScopeSet::new(["read", "write", "follow"]),
        },
    )
    .await
    .expect("register_app must succeed");
    registered.id
}

async fn issue_test_token(app: &TestApp, app_id: Id, actor_id: Id, scopes: &[&str]) -> String {
    let now = app.runtime.clock.now();
    let issued = token_repository::issue_token(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        app.state.oauth().token_hash_key(),
        now,
        NewAccessToken {
            app_id,
            actor_id,
            scopes: ModelScopeSet::new(scopes.iter().copied()),
        },
    )
    .await
    .expect("issue_token must succeed");
    issued.plaintext.expose_secret().to_string()
}

fn real_router(app: &TestApp) -> Router {
    server::build_router(app.state.clone())
}

fn req(method: &str, path: &str, token: Option<&str>, body: Option<Value>) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    match body {
        Some(value) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::to_vec(&value).expect("serialize body"),
            ))
            .expect("build request"),
        None => builder.body(Body::empty()).expect("build request"),
    }
}

async fn send(router: &Router, request: Request<Body>) -> (StatusCode, HeaderMap, Value) {
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("router must not fail to produce a response");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read response body");
    let value: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("response body must be valid JSON")
    };
    (status, headers, value)
}

fn assert_error_shape(body: &Value) {
    assert!(
        body.get("error").and_then(Value::as_str).is_some(),
        "expected a Mastodon-compatible {{\"error\": ...}} body, got: {body}"
    );
}

fn ids_of(body: &Value) -> Vec<String> {
    body.as_array()
        .expect("response body must be a JSON array")
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_string())
        .collect()
}

/// Parses a `Link` response header's `rel="next"`/`rel="prev"` target URLs —
/// mirrors `tests/timelines_endpoints_it.rs::link_targets`.
fn link_targets(headers: &HeaderMap) -> (Option<String>, Option<String>) {
    let Some(raw) = headers.get(header::LINK) else {
        return (None, None);
    };
    let raw = raw.to_str().expect("Link header must be valid UTF-8");
    let mut next = None;
    let mut prev = None;
    for part in raw.split(',') {
        let part = part.trim();
        let Some(url_end) = part.find('>') else {
            continue;
        };
        let url = part[1..url_end].to_string();
        if part.contains("rel=\"next\"") {
            next = Some(url);
        } else if part.contains("rel=\"prev\"") {
            prev = Some(url);
        }
    }
    (next, prev)
}

/// Extracts the path+query portion of an absolute `Link` target URL —
/// mirrors `tests/timelines_endpoints_it.rs::path_and_query`, so a follow-up
/// request can be dispatched through the same in-process router without a
/// real socket.
fn path_and_query(url: &str) -> String {
    let (_scheme, after_scheme) = url
        .split_once("://")
        .expect("Link target must be an absolute URL");
    let slash = after_scheme
        .find('/')
        .expect("Link target must carry a path after the origin");
    after_scheme[slash..].to_string()
}

/// `GET /api/v1/notifications<query>` — returns `(status, headers, body)`.
async fn list_raw(
    router: &Router,
    token: Option<&str>,
    query: &str,
) -> (StatusCode, HeaderMap, Value) {
    let path = if query.is_empty() {
        "/api/v1/notifications".to_string()
    } else {
        format!("/api/v1/notifications{query}")
    };
    send(router, req("GET", &path, token, None)).await
}

/// `GET /api/v1/notifications<query>` asserting `200`, returning the parsed
/// item array.
async fn list_ok(router: &Router, token: &str, query: &str) -> Vec<Value> {
    let (status, _headers, body) = list_raw(router, Some(token), query).await;
    assert_eq!(status, StatusCode::OK, "got: {body:?}");
    body.as_array()
        .expect("notification list must be a JSON array")
        .clone()
}

async fn create_status(router: &Router, token: &str, body: Value) -> Value {
    let (status, _headers, resp) = send(
        router,
        req("POST", "/api/v1/statuses", Some(token), Some(body)),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "status creation must succeed: {resp:?}"
    );
    resp
}

/// Drives a real follow: `follower` -> `target` (real `POST /api/v1/
/// accounts/{id}/follow`, real `SocialGraph`/`Transitions` emit path,
/// real, already-wired chain into the notification generator, Requirement
/// 6.4). Returns nothing — callers fetch the resulting notification through
/// the real list endpoint themselves (Requirement-scoped: this file's own
/// concern is retrieval, not generation).
async fn trigger_follow(router: &Router, follower_token: &str, target_id: Id) {
    let (status, _headers, body) = send(
        router,
        req(
            "POST",
            &format!("/api/v1/accounts/{}/follow", target_id.as_i64()),
            Some(follower_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "follow must succeed: {body:?}");
}

/// Drives a real mention: `author` posts a status mentioning `mention_handle`
/// (real `POST /api/v1/statuses`, real mention-resolution + `Mention`
/// `NotificationEvent` emit loop, Requirement 6.3). Returns the created
/// status's own id (unused by most callers, kept for parity with `tests/
/// notification_contract_it.rs::build_mention_notification`).
async fn trigger_mention(router: &Router, author_token: &str, mention_handle: &str) -> String {
    let created = create_status(
        router,
        author_token,
        json!({"status": format!("hey @{mention_handle}, from notification_list_it")}),
    )
    .await;
    created["id"]
        .as_str()
        .expect("created status id must be a string")
        .to_string()
}

/// Drives a real favourite: `favouriter` favourites `status_id` (real `POST
/// /api/v1/statuses/{id}/favourite`, Requirement 6.1).
async fn trigger_favourite(router: &Router, favouriter_token: &str, status_id: &str) {
    let (status, _headers, body) = send(
        router,
        req(
            "POST",
            &format!("/api/v1/statuses/{status_id}/favourite"),
            Some(favouriter_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "favourite must succeed: {body:?}");
}

/// Drives a real, single generation-point `follow_request` for a *local*
/// recipient without needing the full signed-federation inbound pipeline —
/// mirrors `tests/notification_contract_it.rs`'s own identical, already-
/// reviewed convention (see that file's own doc comment, "follow_request")
/// for exactly why `Transitions::record_pending` is the correct thing to
/// drive directly here.
async fn trigger_follow_request(app: &TestApp, requester_remote_id: Id, target_id: Id) {
    let transitions = Transitions::new(
        app.pool.clone(),
        app.runtime.clone(),
        app.state.statuses().notification_sink_registry(),
    );
    let now = app.runtime.clock.now();
    transitions
        .record_pending(&FollowRequest {
            requester: AccountRef::Remote(requester_remote_id),
            target: AccountRef::Local(target_id),
            direction: FollowRequestDirection::Inbound,
            activity_id: format!(
                "https://remote.notif-list.example/activities/follow-request-{}",
                app.runtime.ids.next_id().as_i64()
            ),
            created_at: now,
        })
        .await
        .expect("record_pending must succeed and emit a real FollowRequest NotificationEvent");
}

// ==========================================================================
// (1) Ownership scoping + newest-first ordering (Requirement 2.1).
// ==========================================================================

/// An authenticated actor's list contains only their own notifications,
/// newest-first — mirrors `notification_contract_it.rs`'s per-kind
/// "exactly one" checks generalized to a multi-recipient, multi-notification
/// scene.
#[tokio::test]
async fn list_returns_only_the_authenticated_actors_own_notifications_newest_first() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let app_id = register_test_app(&app).await;

    let alice = insert_actor_fixture(&app, "notif_list_own_alice").await;
    let bob = insert_actor_fixture(&app, "notif_list_own_bob").await;
    let carol = insert_actor_fixture(&app, "notif_list_own_carol").await;

    let alice_token = issue_test_token(
        &app,
        app_id,
        alice.id,
        &["write:statuses", "read:notifications", "follow"],
    )
    .await;
    let bob_token = issue_test_token(&app, app_id, bob.id, &["read:notifications"]).await;
    let carol_token =
        issue_test_token(&app, app_id, carol.id, &["write:favourites", "follow"]).await;

    // alice gets exactly one notification of her own (carol favourites her
    // post) — used below to prove bob's list never leaks it.
    let posted = create_status(
        &router,
        &alice_token,
        json!({"status": "own-scoping check"}),
    )
    .await;
    trigger_favourite(&router, &carol_token, posted["id"].as_str().unwrap()).await;

    // bob gets two follow notifications, alice's (older) then carol's
    // (newer).
    trigger_follow(&router, &alice_token, bob.id).await;
    trigger_follow(&router, &carol_token, bob.id).await;

    let bob_items = list_ok(&router, &bob_token, "").await;
    assert_eq!(
        bob_items.len(),
        2,
        "bob must see exactly his own two follow notifications: {bob_items:?}"
    );
    assert!(
        bob_items.iter().all(|n| n["type"] == "follow"),
        "bob's list must contain only follow notifications: {bob_items:?}"
    );
    // Newest-first: carol's follow (issued second) must precede alice's.
    assert_eq!(
        bob_items[0]["account"]["id"].as_str(),
        Some(carol.id.as_i64().to_string().as_str()),
        "newest-first ordering (Requirement 2.1): carol's later follow must come first"
    );
    assert_eq!(
        bob_items[1]["account"]["id"].as_str(),
        Some(alice.id.as_i64().to_string().as_str())
    );

    let alice_items = list_ok(&router, &alice_token, "").await;
    assert_eq!(
        alice_items.len(),
        1,
        "alice must see only her own favourite notification, never bob's follow \
         notifications: {alice_items:?}"
    );
    assert_eq!(alice_items[0]["type"], "favourite");
    let bob_ids: Vec<_> = bob_items.iter().map(|n| n["id"].clone()).collect();
    assert!(
        !bob_ids.contains(&alice_items[0]["id"]),
        "alice's and bob's notification id sets must never overlap"
    );

    app.cleanup().await;
}

// ==========================================================================
// (2) types[] / exclude_types[] filtering (Requirement 2.2).
// ==========================================================================

#[tokio::test]
async fn list_types_and_exclude_types_filter_by_notification_kind() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let app_id = register_test_app(&app).await;

    let alice = insert_actor_fixture(&app, "notif_list_kind_alice").await;
    let bob = insert_actor_fixture(&app, "notif_list_kind_bob").await;
    let carol = insert_actor_fixture(&app, "notif_list_kind_carol").await;

    let alice_token = issue_test_token(&app, app_id, alice.id, &["write:statuses"]).await;
    let bob_token = issue_test_token(&app, app_id, bob.id, &["read:notifications"]).await;
    let carol_token = issue_test_token(&app, app_id, carol.id, &["follow"]).await;

    // bob gets one mention (from alice) and one follow (from carol).
    trigger_mention(&router, &alice_token, "notif_list_kind_bob").await;
    trigger_follow(&router, &carol_token, bob.id).await;

    let mention_only = list_ok(&router, &bob_token, "?types[]=mention").await;
    assert_eq!(
        mention_only.len(),
        1,
        "types[]=mention must return exactly the mention: {mention_only:?}"
    );
    assert_eq!(mention_only[0]["type"], "mention");

    let follow_only = list_ok(&router, &bob_token, "?types[]=follow").await;
    assert_eq!(follow_only.len(), 1);
    assert_eq!(follow_only[0]["type"], "follow");

    let both = list_ok(&router, &bob_token, "?types[]=mention&types[]=follow").await;
    assert_eq!(
        both.len(),
        2,
        "types[] with two values must include both kinds: {both:?}"
    );

    let exclude_mention = list_ok(&router, &bob_token, "?exclude_types[]=mention").await;
    assert_eq!(
        exclude_mention.len(),
        1,
        "exclude_types[]=mention must exclude the mention, leaving the follow: \
         {exclude_mention:?}"
    );
    assert_eq!(exclude_mention[0]["type"], "follow");

    app.cleanup().await;
}

// ==========================================================================
// (3) account_id filtering, local and known-remote origin (Requirement 2.3).
// ==========================================================================

#[tokio::test]
async fn list_account_id_filter_narrows_to_local_and_known_remote_origin() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let app_id = register_test_app(&app).await;

    let alice = insert_actor_fixture(&app, "notif_list_acct_alice").await;
    let bob = insert_actor_fixture(&app, "notif_list_acct_bob").await;
    let bob_token = issue_test_token(&app, app_id, bob.id, &["read:notifications"]).await;
    let alice_token = issue_test_token(&app, app_id, alice.id, &["follow"]).await;

    let remote_id = insert_remote_actor_fixture(
        &app,
        "https://remote.notif-list.example/actors/notif_list_acct_remote",
    )
    .await;

    // bob gets a local-origin follow (alice) and a remote-origin
    // follow_request (remote_id).
    trigger_follow(&router, &alice_token, bob.id).await;
    trigger_follow_request(&app, remote_id, bob.id).await;

    let by_local = list_ok(
        &router,
        &bob_token,
        &format!("?account_id={}", alice.id.as_i64()),
    )
    .await;
    assert_eq!(
        by_local.len(),
        1,
        "account_id=alice must return only alice's follow: {by_local:?}"
    );
    assert_eq!(by_local[0]["type"], "follow");
    assert_eq!(
        by_local[0]["account"]["id"].as_str(),
        Some(alice.id.as_i64().to_string().as_str())
    );

    let by_remote = list_ok(
        &router,
        &bob_token,
        &format!("?account_id={}", remote_id.as_i64()),
    )
    .await;
    assert_eq!(
        by_remote.len(),
        1,
        "account_id=<known remote> must return only the follow_request: {by_remote:?}"
    );
    assert_eq!(by_remote[0]["type"], "follow_request");
    assert_eq!(
        by_remote[0]["account"]["id"].as_str(),
        Some(remote_id.as_i64().to_string().as_str())
    );

    app.cleanup().await;
}

// ==========================================================================
// (4) The single most spec-critical scenario: an unresolvable account_id is
// 200 + empty array, never 404 (Requirement 2.3).
// ==========================================================================

#[tokio::test]
async fn list_with_an_account_id_that_resolves_to_no_known_account_returns_200_and_an_empty_array_not_404()
 {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let app_id = register_test_app(&app).await;

    let bob = insert_actor_fixture(&app, "notif_list_unresolved_bob").await;
    let alice = insert_actor_fixture(&app, "notif_list_unresolved_alice").await;
    let bob_token = issue_test_token(&app, app_id, bob.id, &["read:notifications"]).await;
    let alice_token = issue_test_token(&app, app_id, alice.id, &["follow"]).await;

    // bob genuinely has a notification — proves the empty result is
    // specifically about the unresolved filter, not an accidentally-empty
    // recipient.
    trigger_follow(&router, &alice_token, bob.id).await;

    // A numeric id that matches neither a local actor nor a known remote
    // account.
    let unknown_numeric_id = app.runtime.ids.next_id().as_i64();
    let (status, headers, body) = list_raw(
        &router,
        Some(&bob_token),
        &format!("?account_id={unknown_numeric_id}"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "an unresolvable account_id must be 200, never 404 (Requirement 2.3): {body:?}"
    );
    assert_eq!(
        body.as_array().expect("body must be a JSON array").len(),
        0,
        "an unresolvable account_id must yield an empty array, not bob's other notifications"
    );
    assert!(
        headers.get(header::LINK).is_none(),
        "an empty page must build its Link header through the exact same ordinary path any \
         other empty result takes (no special-cased response) — see `src/notifications/\
         endpoints.rs`'s own doc comment, \"通常のページネーションヘッダ\""
    );

    // A non-numeric account_id value takes the identical "cannot resolve"
    // branch (Requirement 2.3's own two-case wording covers both a
    // non-existent internal id and a value that never even parses as one).
    let (status_nn, _headers_nn, body_nn) =
        list_raw(&router, Some(&bob_token), "?account_id=not-a-number").await;
    assert_eq!(status_nn, StatusCode::OK, "got: {body_nn:?}");
    assert_eq!(
        body_nn.as_array().expect("body must be a JSON array").len(),
        0
    );

    app.cleanup().await;
}

// ==========================================================================
// (5) Dismissed/cleared notifications never appear in list results
// (Requirement 2.4).
// ==========================================================================

#[tokio::test]
async fn list_excludes_dismissed_and_cleared_notifications() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let app_id = register_test_app(&app).await;

    let bob = insert_actor_fixture(&app, "notif_list_dismiss_bob").await;
    let alice = insert_actor_fixture(&app, "notif_list_dismiss_alice").await;
    let carol = insert_actor_fixture(&app, "notif_list_dismiss_carol").await;
    let bob_read_token = issue_test_token(&app, app_id, bob.id, &["read:notifications"]).await;
    let bob_write_token = issue_test_token(&app, app_id, bob.id, &["write:notifications"]).await;
    let alice_token = issue_test_token(&app, app_id, alice.id, &["follow"]).await;
    let carol_token = issue_test_token(&app, app_id, carol.id, &["follow"]).await;

    trigger_follow(&router, &alice_token, bob.id).await;
    trigger_follow(&router, &carol_token, bob.id).await;

    let before = list_ok(&router, &bob_read_token, "").await;
    assert_eq!(before.len(), 2);
    let to_dismiss = before
        .iter()
        .find(|n| n["account"]["id"].as_str() == Some(&alice.id.as_i64().to_string()))
        .expect("alice's follow notification must be present")["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (dismiss_status, _headers, dismiss_body) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/notifications/{to_dismiss}/dismiss"),
            Some(&bob_write_token),
            None,
        ),
    )
    .await;
    assert_eq!(dismiss_status, StatusCode::OK, "got: {dismiss_body:?}");

    let after_dismiss = list_ok(&router, &bob_read_token, "").await;
    assert_eq!(
        after_dismiss.len(),
        1,
        "a dismissed notification must never appear in list results: {after_dismiss:?}"
    );
    assert!(!ids_of(&json!(after_dismiss)).contains(&to_dismiss));

    let (clear_status, _headers, clear_body) = send(
        &router,
        req(
            "POST",
            "/api/v1/notifications/clear",
            Some(&bob_write_token),
            None,
        ),
    )
    .await;
    assert_eq!(clear_status, StatusCode::OK, "got: {clear_body:?}");

    let after_clear = list_ok(&router, &bob_read_token, "").await;
    assert_eq!(
        after_clear.len(),
        0,
        "cleared notifications must never appear in list results: {after_clear:?}"
    );

    app.cleanup().await;
}

// ==========================================================================
// (6) Auth/scope discipline (Requirements 2.5, 9.1, 9.2).
// ==========================================================================

#[tokio::test]
async fn list_requires_a_bearer_token_and_read_notifications_scope() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let app_id = register_test_app(&app).await;

    let (status_no_token, _headers, body_no_token) = list_raw(&router, None, "").await;
    assert_eq!(
        status_no_token,
        StatusCode::UNAUTHORIZED,
        "a missing Bearer token must be rejected: {body_no_token:?}"
    );
    assert_error_shape(&body_no_token);

    let viewer = insert_actor_fixture(&app, "notif_list_scope_viewer").await;
    // `read:accounts` deliberately omits `read:notifications`.
    let token = issue_test_token(&app, app_id, viewer.id, &["read:accounts"]).await;
    let (status_bad_scope, _headers, body_bad_scope) = list_raw(&router, Some(&token), "").await;
    assert_eq!(
        status_bad_scope,
        StatusCode::FORBIDDEN,
        "insufficient scope must be rejected: {body_bad_scope:?}"
    );
    assert_error_shape(&body_bad_scope);

    app.cleanup().await;
}

// ==========================================================================
// (7) Pagination: Link header + notification-id cursor next-page round trip
// (Requirements 2.1, 9.3).
// ==========================================================================

#[tokio::test]
async fn pagination_link_header_and_next_page_round_trip() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let app_id = register_test_app(&app).await;

    let bob = insert_actor_fixture(&app, "notif_list_page_bob").await;
    let bob_token = issue_test_token(&app, app_id, bob.id, &["read:notifications"]).await;

    let mut followers = Vec::new();
    for n in 0..3 {
        let handle = format!("notif_list_page_follower_{n}");
        let follower = insert_actor_fixture(&app, &handle).await;
        let token = issue_test_token(&app, app_id, follower.id, &["follow"]).await;
        trigger_follow(&router, &token, bob.id).await;
        followers.push(follower.id);
    }

    let (status, headers1, body1) = list_raw(&router, Some(&bob_token), "?limit=1").await;
    assert_eq!(status, StatusCode::OK, "got: {body1:?}");
    let page1 = ids_of(&body1);
    assert_eq!(page1.len(), 1, "limit=1 must return exactly one item");
    let (next1, _prev1) = link_targets(&headers1);
    let next1 = next1.expect("a partial page must carry a Link next target (Requirement 9.3)");

    let (status2, headers2, body2) = send(
        &router,
        req("GET", &path_and_query(&next1), Some(&bob_token), None),
    )
    .await;
    assert_eq!(status2, StatusCode::OK, "got: {body2:?}");
    let page2 = ids_of(&body2);
    assert_eq!(page2.len(), 1);
    assert_ne!(
        page2, page1,
        "the next page must not repeat the first page's item"
    );
    let (next2, _prev2) = link_targets(&headers2);
    let next2 = next2.expect("page 2 of 3 must still carry a Link next target");

    let (status3, _headers3, body3) = send(
        &router,
        req("GET", &path_and_query(&next2), Some(&bob_token), None),
    )
    .await;
    assert_eq!(status3, StatusCode::OK, "got: {body3:?}");
    let page3 = ids_of(&body3);
    assert_eq!(page3.len(), 1);

    let mut all_ids: Vec<String> = page1.into_iter().chain(page2).chain(page3).collect();
    all_ids.sort();
    all_ids.dedup();
    assert_eq!(
        all_ids.len(),
        3,
        "walking the Link next chain across all 3 pages must visit exactly the 3 distinct \
         follow notifications, with no duplicates or omissions"
    );

    app.cleanup().await;
}
