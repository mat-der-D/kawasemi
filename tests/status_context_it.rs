//! Integration tests for task 8.1 (`.kiro/specs/statuses-core/tasks.md`,
//! "8.1 統合テスト（CRUD・冪等・context・操作・投票）を整備する"): thread
//! `context` — ancestor/descendant traversal ordering and visibility-based
//! exclusion, including the unauthenticated "public only" boundary —
//! driven as real HTTP requests through the fully-wired application router
//! booted by `spawn_test_app` (Requirements 6.2, 6.3, 6.4).
//!
//! See `tests/status_crud_it.rs`'s own doc comment for this file's shared
//! conventions (in-process `tower::ServiceExt::oneshot` against
//! `crate::server::build_router(app.state.clone())`, fixture-plumbing
//! duplication across sibling test files).

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor, ResolvedActor};
use kawasemi::domain::Id;
use kawasemi::oauth::app_repository::{self, NewApp};
use kawasemi::oauth::model::ScopeSet as ModelScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::server;
use kawasemi::test_harness::{TestApp, spawn_test_app};

// ---- Fixture plumbing (duplicated per sibling test-file convention — see
// `tests/status_crud_it.rs`'s own doc comment). ----

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
            display_name: format!("Status Context IT {handle_str}"),
            summary: "an actor used by the status_context_it integration test".to_string(),
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
    let now = app.runtime.clock.now();
    let registered = app_repository::register_app(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        app.state.oauth().token_hash_key(),
        now,
        NewApp {
            name: "Status Context IT Client".to_string(),
            redirect_uris: vec!["https://client.example/callback".to_string()],
            scopes: ModelScopeSet::new(["read", "write"]),
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

async fn send(router: &Router, request: Request<Body>) -> (StatusCode, Value) {
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

async fn create_status(router: &Router, token: &str, body: Value) -> Value {
    let (status, resp) = send(
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

fn id_of(v: &Value) -> String {
    v["id"].as_str().expect("id must be a string").to_string()
}

// ==== Ancestor/descendant ordering (Requirement 6.2) ====

#[tokio::test]
async fn context_returns_ancestors_root_first_and_descendants_in_creation_order() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "alice_ctx").await;
    let app_id = register_test_app(&app).await;
    let token = issue_test_token(&app, app_id, alice.id, &["write:statuses"]).await;

    let root = create_status(&router, &token, json!({"status": "root"})).await;
    let root_id = id_of(&root);
    let reply1 = create_status(
        &router,
        &token,
        json!({"status": "reply1", "in_reply_to_id": root_id}),
    )
    .await;
    let reply1_id = id_of(&reply1);
    let reply1b = create_status(
        &router,
        &token,
        json!({"status": "reply1b", "in_reply_to_id": root_id}),
    )
    .await;
    let reply1b_id = id_of(&reply1b);
    let reply2 = create_status(
        &router,
        &token,
        json!({"status": "reply2", "in_reply_to_id": reply1_id}),
    )
    .await;
    let reply2_id = id_of(&reply2);

    let (status, ctx) = send(
        &router,
        req(
            "GET",
            &format!("/api/v1/statuses/{reply2_id}/context"),
            Some(&token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {ctx:?}");
    let ancestor_ids: Vec<&str> = ctx["ancestors"]
        .as_array()
        .expect("ancestors array")
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ancestor_ids,
        vec![root_id.as_str(), reply1_id.as_str()],
        "ancestors must be ordered root-first: {ctx:?}"
    );

    let (status, ctx) = send(
        &router,
        req(
            "GET",
            &format!("/api/v1/statuses/{root_id}/context"),
            Some(&token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {ctx:?}");
    let descendant_ids: Vec<&str> = ctx["descendants"]
        .as_array()
        .expect("descendants array")
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        descendant_ids,
        vec![reply1_id.as_str(), reply1b_id.as_str(), reply2_id.as_str()],
        "the full descendant tree must be flattened in creation order: {ctx:?}"
    );

    app.cleanup().await;
}

// ==== Invisible-post exclusion (Requirement 6.3) ====

#[tokio::test]
async fn context_excludes_replies_invisible_to_the_viewer() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "alice_ctx_vis").await;
    let bob = insert_actor_fixture(&app, "bob_ctx_vis").await;
    let charlie = insert_actor_fixture(&app, "charlie_ctx_vis").await;
    let app_id = register_test_app(&app).await;
    let alice_token = issue_test_token(&app, app_id, alice.id, &["write:statuses"]).await;
    let bob_token = issue_test_token(&app, app_id, bob.id, &["write:statuses"]).await;
    let charlie_token = issue_test_token(&app, app_id, charlie.id, &["write:statuses"]).await;

    let root = create_status(&router, &alice_token, json!({"status": "public root"})).await;
    let root_id = id_of(&root);
    let private_reply = create_status(
        &router,
        &bob_token,
        json!({
            "status": "bob's private reply", "in_reply_to_id": root_id, "visibility": "private"
        }),
    )
    .await;
    let private_reply_id = id_of(&private_reply);

    // charlie is not a follower of bob (no social-graph wired — the default
    // `NoRelationshipQuery` treats every viewer as a non-follower), so
    // bob's private reply must be excluded from charlie's view of context.
    let (status, ctx) = send(
        &router,
        req(
            "GET",
            &format!("/api/v1/statuses/{root_id}/context"),
            Some(&charlie_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {ctx:?}");
    let descendant_ids: Vec<&str> = ctx["descendants"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert!(
        !descendant_ids.contains(&private_reply_id.as_str()),
        "a private reply must be excluded from a non-follower's context: {ctx:?}"
    );

    // bob himself always sees his own private reply.
    let (status, ctx) = send(
        &router,
        req(
            "GET",
            &format!("/api/v1/statuses/{root_id}/context"),
            Some(&bob_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {ctx:?}");
    let descendant_ids: Vec<&str> = ctx["descendants"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert!(
        descendant_ids.contains(&private_reply_id.as_str()),
        "the author must still see their own private reply: {ctx:?}"
    );

    app.cleanup().await;
}

// ==== Unauthenticated "public only" boundary (Requirement 6.4) ====

#[tokio::test]
async fn context_unauthenticated_only_includes_public_replies() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "alice_ctx_anon").await;
    let app_id = register_test_app(&app).await;
    let token = issue_test_token(&app, app_id, alice.id, &["write:statuses"]).await;

    let root = create_status(&router, &token, json!({"status": "root"})).await;
    let root_id = id_of(&root);
    let unlisted_reply = create_status(
        &router,
        &token,
        json!({"status": "unlisted reply", "in_reply_to_id": root_id, "visibility": "unlisted"}),
    )
    .await;
    let unlisted_id = id_of(&unlisted_reply);
    let public_reply = create_status(
        &router,
        &token,
        json!({"status": "public reply", "in_reply_to_id": root_id}),
    )
    .await;
    let public_id = id_of(&public_reply);

    let (status, ctx) = send(
        &router,
        req(
            "GET",
            &format!("/api/v1/statuses/{root_id}/context"),
            None,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {ctx:?}");
    let descendant_ids: Vec<&str> = ctx["descendants"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert!(descendant_ids.contains(&public_id.as_str()));
    assert!(
        !descendant_ids.contains(&unlisted_id.as_str()),
        "an unauthenticated viewer must not see an unlisted reply: {ctx:?}"
    );

    let (status, ctx) = send(
        &router,
        req(
            "GET",
            &format!("/api/v1/statuses/{root_id}/context"),
            Some(&token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {ctx:?}");
    let descendant_ids: Vec<&str> = ctx["descendants"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert!(
        descendant_ids.contains(&unlisted_id.as_str()),
        "an authenticated viewer must see the unlisted reply: {ctx:?}"
    );

    app.cleanup().await;
}

#[tokio::test]
async fn context_of_an_unknown_or_invisible_status_is_404() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "alice_ctx_404").await;
    let bob = insert_actor_fixture(&app, "bob_ctx_404").await;
    let app_id = register_test_app(&app).await;
    let alice_token = issue_test_token(&app, app_id, alice.id, &["write:statuses"]).await;
    let bob_token = issue_test_token(&app, app_id, bob.id, &["read:statuses"]).await;

    let (status, _) = send(
        &router,
        req("GET", "/api/v1/statuses/999999999999/context", None, None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let private = create_status(
        &router,
        &alice_token,
        json!({"status": "private root", "visibility": "private"}),
    )
    .await;
    let private_id = id_of(&private);
    let (status, _) = send(
        &router,
        req(
            "GET",
            &format!("/api/v1/statuses/{private_id}/context"),
            Some(&bob_token),
            None,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "context of an invisible root must be 404"
    );

    app.cleanup().await;
}
