//! Integration test proving statuses-core's task 9.1 own observable
//! completion condition (`.kiro/specs/statuses-core/tasks.md`, "9.1 accounts
//! 委譲ポートの実装を供給する", `_Boundary: AccountStatusesProviderImpl,
//! AccountCountsContribution_`): "配線後に `GET /accounts/:id/statuses` が
//! 投稿ページを返し、Account の `statuses_count` が実値になる" (Requirements
//! 6.1, 7.1).
//!
//! Drives the real, fully-assembled router (`crate::server::build_router`)
//! from `spawn_test_app`'s own `AppState` — the same "prove the composition-
//! root wiring itself" precedent `tests/statuses_bootstrap_wiring_it.rs`
//! already established for task 7.2 — so this file's requests observe the
//! real `crate::statuses::register_account_ports` wiring this task adds
//! (`src/test_harness.rs`'s own call site), not a test-local substitute.
//!
//! Four things proven here:
//! 1. `GET /accounts/:id/statuses` returns a real, non-empty page containing
//!    a post actually created via `POST /api/v1/statuses` — proof
//!    `AccountStatusesProviderImpl` is live, not the built-in
//!    `EmptyStatusesProvider` default.
//! 2. `GET /accounts/:id`'s `statuses_count` reflects the real number of
//!    posts the account authored — proof `AccountCountsContribution` is
//!    live, not the built-in `ZeroCountsProvider` default.
//! 3. Visibility filtering (Requirement 6.1) is applied: an unauthenticated
//!    request to `GET /accounts/:id/statuses` sees only the account's
//!    `public`/`unlisted` posts, never its `private`/`direct` ones.
//! 4. An account that has authored nothing still gets the safe, empty/zero
//!    shape (mirrors the built-in defaults' own contract for the case where
//!    there is genuinely nothing to report).

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::Value;
use tower::ServiceExt;

use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor, ResolvedActor};
use kawasemi::domain::Id;
use kawasemi::oauth::app_repository::{self, NewApp};
use kawasemi::oauth::model::ScopeSet as ModelScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::server;
use kawasemi::test_harness::{TestApp, spawn_test_app};

// ---- Fixture plumbing (each `tests/*.rs` file is its own compiled crate,
// so this deliberately duplicates `tests/statuses_bootstrap_wiring_it.rs`'s
// own established conventions rather than importing them — this crate's own
// documented convention). ----

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
            display_name: format!("Account Statuses Provider IT {handle_str}"),
            summary: "an actor used by the account-statuses-provider integration test".to_string(),
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

async fn register_test_app(app: &TestApp) -> Id {
    let key = app.state.oauth().token_hash_key().clone();
    let now = app.runtime.clock.now();
    let registered = app_repository::register_app(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        &key,
        now,
        NewApp {
            name: "Account Statuses Provider IT Client".to_string(),
            redirect_uris: vec!["https://client.example/callback".to_string()],
            scopes: ModelScopeSet::new(["read", "write"]),
        },
    )
    .await
    .expect("register_app must succeed");
    registered.id
}

async fn issue_test_token(app: &TestApp, app_id: Id, actor_id: Id, scopes: &[&str]) -> String {
    let key = app.state.oauth().token_hash_key().clone();
    let now = app.runtime.clock.now();
    let issued = token_repository::issue_token(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        &key,
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

async fn post_json(router: &Router, path: &str, token: &str, body: Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("POST")
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&body).expect("serialize body"),
        ))
        .expect("build request");
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("router must not fail to produce a response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read response body");
    let value: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("response body must be valid JSON")
    };
    (status, value)
}

async fn get_json(router: &Router, path: &str, token: Option<&str>) -> (StatusCode, Value) {
    let mut builder = Request::builder().method("GET").uri(path);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let request = builder.body(Body::empty()).expect("build request");
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("router must not fail to produce a response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read response body");
    let value: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("response body must be valid JSON")
    };
    (status, value)
}

/// Requirements 6.1, 7.1: once `crate::statuses::register_account_ports`
/// wires the real implementation in, `GET /accounts/:id/statuses` returns
/// the account's actual posts and `GET /accounts/:id`'s `statuses_count`
/// reflects the real total — not the built-in `EmptyStatusesProvider`/
/// `ZeroCountsProvider` defaults' empty/zero shape.
#[tokio::test]
async fn account_statuses_and_counts_reflect_real_posts_once_wired() {
    let app = spawn_test_app().await;
    let router = real_router(&app);

    let alice = insert_actor_fixture(&app, "alice_provider").await;
    let app_id = register_test_app(&app).await;
    let token =
        issue_test_token(&app, app_id, alice.id, &["write:statuses", "read:statuses"]).await;

    // Before posting anything: an empty page / zero counts, the same safe
    // shape the built-in defaults would also produce for an account with
    // nothing to report (proof #4).
    let (status, body) = get_json(
        &router,
        &format!("/api/v1/accounts/{}/statuses", alice.id.as_i64()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {body:?}");
    assert_eq!(body.as_array().map(Vec::len), Some(0));

    let (status, body) = get_json(
        &router,
        &format!("/api/v1/accounts/{}", alice.id.as_i64()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {body:?}");
    assert_eq!(body["statuses_count"].as_i64(), Some(0));
    assert!(body["last_status_at"].is_null());

    // Alice posts a public status and a private one.
    let (status, body) = post_json(
        &router,
        "/api/v1/statuses",
        &token,
        serde_json::json!({ "status": "alice's public post", "visibility": "public" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "public post creation: {body:?}");
    let public_id = body["id"]
        .as_str()
        .expect("id must be a string")
        .to_string();

    let (status, body) = post_json(
        &router,
        "/api/v1/statuses",
        &token,
        serde_json::json!({ "status": "alice's private post", "visibility": "private" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "private post creation: {body:?}");

    // Proof #2: statuses_count is now the real total (2), not 0.
    let (status, body) = get_json(
        &router,
        &format!("/api/v1/accounts/{}", alice.id.as_i64()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {body:?}");
    assert_eq!(
        body["statuses_count"].as_i64(),
        Some(2),
        "statuses_count must reflect both real posts once AccountCountsContribution is wired, \
         got: {body}"
    );
    assert!(
        !body["last_status_at"].is_null(),
        "last_status_at must be populated once a real post exists, got: {body}"
    );

    // Proof #1 + #3: an unauthenticated request sees only the public post,
    // never the private one (Requirement 6.1's visibility filter).
    let (status, body) = get_json(
        &router,
        &format!("/api/v1/accounts/{}/statuses", alice.id.as_i64()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {body:?}");
    let items = body.as_array().expect("statuses page must be a JSON array");
    assert_eq!(
        items.len(),
        1,
        "an unauthenticated caller must see only the public post, got: {body}"
    );
    assert_eq!(items[0]["id"], public_id);
    assert_eq!(items[0]["content"], "alice's public post");

    // The author herself sees both.
    let (status, body) = get_json(
        &router,
        &format!("/api/v1/accounts/{}/statuses", alice.id.as_i64()),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {body:?}");
    assert_eq!(
        body.as_array().map(Vec::len),
        Some(2),
        "the author herself must see both her own public and private posts, got: {body}"
    );

    app.cleanup().await;
}

/// Task 10.5's own "`GET /accounts/:id/statuses` が `create_status` 経由で
/// 作成された投票付き投稿を正しく描画することを証明する
/// `account_statuses_provider_it.rs` 側のテストを追加する" (Requirements 6.1,
/// 7.1, 13.1). `AccountStatusesProviderImpl::page` (`src/statuses/
/// account_provider.rs`) has its own `poll_json` rendering helper, but every
/// existing test in this file only ever posts plain-text statuses — this
/// test proves that helper actually surfaces real poll data for a status
/// that went through the full `POST /api/v1/statuses` create pipeline (task
/// 9.2-追補's poll-persistence remediation), not a hand-inserted DB row like
/// `tests/polls_it.rs`'s own established fixture technique uses.
#[tokio::test]
async fn account_statuses_endpoint_renders_a_poll_created_via_the_real_create_status_flow() {
    let app = spawn_test_app().await;
    let router = real_router(&app);

    let alice = insert_actor_fixture(&app, "alice_provider_poll").await;
    let app_id = register_test_app(&app).await;
    let token = issue_test_token(&app, app_id, alice.id, &["write:statuses"]).await;

    let (status, created) = post_json(
        &router,
        "/api/v1/statuses",
        &token,
        serde_json::json!({
            "status": "which color?",
            "poll": {"options": ["red", "green", "blue"], "multiple": false}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "poll status creation: {created:?}");
    let created_id = created["id"].as_str().unwrap().to_string();
    let created_poll_id = created["poll"]["id"]
        .as_str()
        .expect("create response must embed a poll id")
        .to_string();

    let (status, page) = get_json(
        &router,
        &format!("/api/v1/accounts/{}/statuses", alice.id.as_i64()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {page:?}");
    let items = page.as_array().expect("statuses page must be a JSON array");
    assert_eq!(items.len(), 1, "got: {page:?}");
    let rendered = &items[0];
    assert_eq!(rendered["id"], created_id);

    let poll = rendered
        .get("poll")
        .expect("AccountStatusesProviderImpl must render the poll field for a poll-bearing status");
    assert!(
        !poll.is_null(),
        "the poll field must not be null for a status created with a poll: {rendered}"
    );
    assert_eq!(poll["id"], created_poll_id);
    assert_eq!(poll["multiple"], false);
    let option_titles: Vec<&str> = poll["options"]
        .as_array()
        .expect("poll.options must be an array")
        .iter()
        .map(|option| option["title"].as_str().expect("option.title"))
        .collect();
    assert_eq!(
        option_titles,
        vec!["red", "green", "blue"],
        "the rendered poll must retain the real options from the create request: {poll}"
    );
    for option in poll["options"].as_array().unwrap() {
        assert_eq!(option["votes_count"], 0);
    }

    app.cleanup().await;
}
