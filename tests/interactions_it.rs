//! Integration tests for task 8.1 (`.kiro/specs/statuses-core/tasks.md`,
//! "8.1 統合テスト（CRUD・冪等・context・操作・投票）を整備する"): reblog/
//! favourite/bookmark/pin register/unregister, duplicate prevention,
//! counters, scoping, and the bookmark list — driven as real HTTP requests
//! through the fully-wired application router booted by `spawn_test_app`
//! (Requirements 9.1, 9.3, 9.4, 10.1, 10.3, 10.4, 11.1, 11.2, 11.3, 11.4,
//! 12.1, 12.2, 12.3, 12.4).
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
            display_name: format!("Interactions IT {handle_str}"),
            summary: "an actor used by the interactions_it integration test".to_string(),
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
            name: "Interactions IT Client".to_string(),
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

// ==== Reblog (Requirements 9.1, 9.3, 9.4) ====

#[tokio::test]
async fn reblog_round_trips_with_duplicate_prevention_and_counter_updates() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "alice_reblog").await;
    let bob = insert_actor_fixture(&app, "bob_reblog").await;
    let app_id = register_test_app(&app).await;
    let alice_token = issue_test_token(&app, app_id, alice.id, &["write:statuses"]).await;
    let bob_token = issue_test_token(&app, app_id, bob.id, &["write:statuses"]).await;

    let original = create_status(&router, &alice_token, json!({"status": "boost me"})).await;
    let original_id = id_of(&original);

    let (status, boost1) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/statuses/{original_id}/reblog"),
            Some(&bob_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {boost1:?}");
    assert_eq!(boost1["reblog"]["id"].as_str(), Some(original_id.as_str()));
    let boost_id = id_of(&boost1);

    let (status, refreshed) = send(
        &router,
        req(
            "GET",
            &format!("/api/v1/statuses/{original_id}"),
            Some(&alice_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {refreshed:?}");
    assert_eq!(refreshed["reblogs_count"], 1);

    // A duplicate reblog by the same actor must not create a second row or
    // double the counter (Requirement 9.3).
    let (status, boost2) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/statuses/{original_id}/reblog"),
            Some(&bob_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {boost2:?}");
    assert_eq!(
        id_of(&boost2),
        boost_id,
        "a duplicate reblog must return the existing boost row, not a new one"
    );

    let (status, refreshed) = send(
        &router,
        req(
            "GET",
            &format!("/api/v1/statuses/{original_id}"),
            Some(&alice_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        refreshed["reblogs_count"], 1,
        "a duplicate reblog must not double the counter"
    );

    // Unreblog (Requirement 9.4).
    let (status, unreblogged) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/statuses/{original_id}/unreblog"),
            Some(&bob_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {unreblogged:?}");
    assert_eq!(unreblogged["reblogs_count"], 0);

    let (status, _) = send(
        &router,
        req(
            "GET",
            &format!("/api/v1/statuses/{boost_id}"),
            Some(&alice_token),
            None,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "the boost row itself must be gone after unreblog"
    );

    // Unreblogging again is a no-op, not an error.
    let (status, unreblogged_again) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/statuses/{original_id}/unreblog"),
            Some(&bob_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {unreblogged_again:?}");
    assert_eq!(unreblogged_again["reblogs_count"], 0);

    app.cleanup().await;
}

#[tokio::test]
async fn reblog_requires_write_statuses_scope_and_rejects_an_invisible_target() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "alice_reblog_scope").await;
    let bob = insert_actor_fixture(&app, "bob_reblog_scope").await;
    let app_id = register_test_app(&app).await;
    let alice_token = issue_test_token(&app, app_id, alice.id, &["write:statuses"]).await;

    let private = create_status(
        &router,
        &alice_token,
        json!({"status": "private post", "visibility": "private"}),
    )
    .await;
    let private_id = id_of(&private);

    let (status, body) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/statuses/{private_id}/reblog"),
            None,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "got: {body:?}");

    let bob_token = issue_test_token(&app, app_id, bob.id, &["write:statuses"]).await;
    let (status, body) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/statuses/{private_id}/reblog"),
            Some(&bob_token),
            None,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "an invisible target must be rejected: {body:?}"
    );

    let wrong_scope_token = issue_test_token(&app, app_id, bob.id, &["read:statuses"]).await;
    let public = create_status(&router, &alice_token, json!({"status": "public post"})).await;
    let public_id = id_of(&public);
    let (status, body) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/statuses/{public_id}/reblog"),
            Some(&wrong_scope_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "got: {body:?}");

    app.cleanup().await;
}

// ==== Favourite (Requirements 10.1, 10.3, 10.4) ====

#[tokio::test]
async fn favourite_round_trips_with_duplicate_prevention_and_counter_updates() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "alice_fav2").await;
    let bob = insert_actor_fixture(&app, "bob_fav2").await;
    let app_id = register_test_app(&app).await;
    let alice_token = issue_test_token(&app, app_id, alice.id, &["write:statuses"]).await;
    let bob_token = issue_test_token(&app, app_id, bob.id, &["write:favourites"]).await;

    let post = create_status(&router, &alice_token, json!({"status": "fav me"})).await;
    let post_id = id_of(&post);

    let (status, favourited) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/statuses/{post_id}/favourite"),
            Some(&bob_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {favourited:?}");
    assert_eq!(favourited["favourited"], true);
    assert_eq!(favourited["favourites_count"], 1);

    let (status, favourited_again) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/statuses/{post_id}/favourite"),
            Some(&bob_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {favourited_again:?}");
    assert_eq!(
        favourited_again["favourites_count"], 1,
        "a duplicate favourite must not double the counter"
    );

    let (status, unfavourited) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/statuses/{post_id}/unfavourite"),
            Some(&bob_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {unfavourited:?}");
    assert_eq!(unfavourited["favourited"], false);
    assert_eq!(unfavourited["favourites_count"], 0);

    let (status, unfavourited_again) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/statuses/{post_id}/unfavourite"),
            Some(&bob_token),
            None,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "unfavouriting a non-favourited post must be a no-op, not an error: {unfavourited_again:?}"
    );
    assert_eq!(unfavourited_again["favourites_count"], 0);

    app.cleanup().await;
}

// ==== Bookmark (Requirements 11.1, 11.2, 11.3, 11.4) ====

#[tokio::test]
async fn bookmarking_toggles_state_and_listing_is_scoped_and_newest_first() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "alice_bm").await;
    let bob = insert_actor_fixture(&app, "bob_bm").await;
    let app_id = register_test_app(&app).await;
    let alice_token = issue_test_token(&app, app_id, alice.id, &["write:statuses"]).await;
    let bob_token =
        issue_test_token(&app, app_id, bob.id, &["write:bookmarks", "read:bookmarks"]).await;

    let post1 = create_status(&router, &alice_token, json!({"status": "post 1"})).await;
    let post2 = create_status(&router, &alice_token, json!({"status": "post 2"})).await;
    let post3 = create_status(&router, &alice_token, json!({"status": "post 3"})).await;
    let (id1, id2, id3) = (id_of(&post1), id_of(&post2), id_of(&post3));

    for id in [&id1, &id2, &id3] {
        let (status, body) = send(
            &router,
            req(
                "POST",
                &format!("/api/v1/statuses/{id}/bookmark"),
                Some(&bob_token),
                None,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "got: {body:?}");
        assert_eq!(body["bookmarked"], true);
    }

    let (status, listing) = send(
        &router,
        req("GET", "/api/v1/bookmarks", Some(&bob_token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {listing:?}");
    let ids: Vec<&str> = listing
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec![id3.as_str(), id2.as_str(), id1.as_str()],
        "bookmarks must list newest-bookmarked-first: {listing:?}"
    );

    let (status, unbookmarked) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/statuses/{id2}/unbookmark"),
            Some(&bob_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {unbookmarked:?}");
    assert_eq!(unbookmarked["bookmarked"], false);

    let (status, listing) = send(
        &router,
        req("GET", "/api/v1/bookmarks", Some(&bob_token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {listing:?}");
    let ids: Vec<&str> = listing
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![id3.as_str(), id1.as_str()]);

    let (status, body) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/statuses/{id1}/bookmark"),
            None,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "got: {body:?}");

    let (status, body) = send(&router, req("GET", "/api/v1/bookmarks", None, None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "got: {body:?}");

    // A page smaller than the total bookmark count must carry a Link header
    // (Requirement 11.3's pagination-convention application).
    let response = router
        .clone()
        .oneshot(req(
            "GET",
            "/api/v1/bookmarks?limit=1",
            Some(&bob_token),
            None,
        ))
        .await
        .expect("router must not fail to produce a response");
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers().get(header::LINK).is_some(),
        "a page smaller than the total bookmark count must carry a Link header"
    );

    app.cleanup().await;
}

// ==== Pin (Requirements 12.1, 12.2, 12.3, 12.4) ====

#[tokio::test]
async fn pinning_enforces_ownership_and_rejects_direct_visibility() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "alice_pin").await;
    let bob = insert_actor_fixture(&app, "bob_pin").await;
    let app_id = register_test_app(&app).await;
    let alice_token = issue_test_token(&app, app_id, alice.id, &["write:statuses"]).await;
    let bob_token = issue_test_token(&app, app_id, bob.id, &["write:statuses"]).await;

    let own_post = create_status(&router, &alice_token, json!({"status": "pin me"})).await;
    let own_id = id_of(&own_post);
    let bobs_post = create_status(&router, &bob_token, json!({"status": "not alice's"})).await;
    let bobs_id = id_of(&bobs_post);
    let direct_post = create_status(
        &router,
        &alice_token,
        json!({"status": "shh", "visibility": "direct"}),
    )
    .await;
    let direct_id = id_of(&direct_post);

    let (status, pinned) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/statuses/{own_id}/pin"),
            Some(&alice_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {pinned:?}");
    assert_eq!(pinned["pinned"], true);

    let (status, unpinned) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/statuses/{own_id}/unpin"),
            Some(&alice_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {unpinned:?}");
    assert_eq!(unpinned["pinned"], false);

    let (status, body) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/statuses/{bobs_id}/pin"),
            Some(&alice_token),
            None,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "pinning someone else's post must be rejected: {body:?}"
    );

    let (status, body) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/statuses/{direct_id}/pin"),
            Some(&alice_token),
            None,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "pinning a direct-visibility post must be rejected: {body:?}"
    );

    app.cleanup().await;
}

// ==== Unbookmark / unpin dedicated endpoint tests (task 10.5) ====

/// Task 10.5's own "7.1 で指摘済みの未テストハンドラ...に...専用テストを
/// 追加する" for `unbookmark_status`. `bookmarking_toggles_state_...` above
/// already proves the success-path toggle-off; this test proves
/// `design.md`'s API Contract row for `POST /api/v1/statuses/:id/unbookmark`
/// (`write:bookmarks`, errors "401, 403, 404") on its remaining, previously-
/// unverified edges (Requirement 11.2): unauthenticated is rejected, a
/// missing `write:bookmarks` scope is rejected, an unknown target 404s (
/// `InteractionService::bookmark`'s `find_by_id` runs unconditionally, even
/// for `on == false` — see that method's own doc comment), and — unlike
/// `pin`/`unpin` — revoking a bookmark the caller never held is a no-op
/// success rather than an error (the same doc comment's "revoking a private
/// state the actor already holds is always allowed" rule).
#[tokio::test]
async fn unbookmark_status_endpoint_requires_auth_and_scope_and_rejects_unknown_targets() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "alice_unbm").await;
    let bob = insert_actor_fixture(&app, "bob_unbm").await;
    let app_id = register_test_app(&app).await;
    let alice_token = issue_test_token(&app, app_id, alice.id, &["write:statuses"]).await;
    let bob_full_token =
        issue_test_token(&app, app_id, bob.id, &["write:bookmarks", "read:bookmarks"]).await;
    let bob_no_scope_token = issue_test_token(&app, app_id, bob.id, &["write:statuses"]).await;

    let post = create_status(&router, &alice_token, json!({"status": "unbookmark me"})).await;
    let id = id_of(&post);

    // Unauthenticated is rejected.
    let (status, body) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/statuses/{id}/unbookmark"),
            None,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "got: {body:?}");

    // A token without `write:bookmarks` is rejected.
    let (status, body) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/statuses/{id}/unbookmark"),
            Some(&bob_no_scope_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "got: {body:?}");

    // An unknown target 404s.
    let (status, body) = send(
        &router,
        req(
            "POST",
            "/api/v1/statuses/999999999999/unbookmark",
            Some(&bob_full_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "got: {body:?}");

    // Unbookmarking a post bob never bookmarked is an idempotent success,
    // not an error (Requirement 11.2's "bookmarked=false を反映").
    let (status, body) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/statuses/{id}/unbookmark"),
            Some(&bob_full_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {body:?}");
    assert_eq!(body["bookmarked"], false);

    app.cleanup().await;
}

/// Task 10.5's own "...に...専用テストを追加する" for `unpin_status`.
/// `pinning_enforces_ownership_and_rejects_direct_visibility` above only
/// exercises *pin*'s ownership rejection, never unpin's — but
/// `InteractionService::pin`'s ownership check (`target.actor_id !=
/// actor_id`) runs identically for both `on == true` and `on == false`, and
/// `design.md`'s API Contract row for `POST /api/v1/statuses/:id/unpin`
/// lists the same "401, 403, 404" error set as `pin`. This test proves the
/// previously-unverified `unpin` half: unauthenticated is rejected, a
/// missing `write:statuses` scope is rejected, and — the real gap — a
/// non-owner attempting to unpin someone else's post 404s exactly like a
/// non-owner attempting to pin it (Requirement 12.2's counterpart to 12.3).
#[tokio::test]
async fn unpin_status_endpoint_enforces_ownership_scope_and_authentication() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "alice_unpin").await;
    let bob = insert_actor_fixture(&app, "bob_unpin").await;
    let app_id = register_test_app(&app).await;
    let alice_token = issue_test_token(&app, app_id, alice.id, &["write:statuses"]).await;
    let bob_token = issue_test_token(&app, app_id, bob.id, &["write:statuses"]).await;
    let bob_no_scope_token = issue_test_token(&app, app_id, bob.id, &["read:statuses"]).await;

    let (status, pinned_post) = send(
        &router,
        req(
            "POST",
            "/api/v1/statuses",
            Some(&alice_token),
            Some(json!({"status": "alice's pin"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {pinned_post:?}");
    let alice_post_id = id_of(&pinned_post);
    let (status, pinned) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/statuses/{alice_post_id}/pin"),
            Some(&alice_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {pinned:?}");
    assert_eq!(pinned["pinned"], true);

    // Unauthenticated is rejected.
    let (status, body) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/statuses/{alice_post_id}/unpin"),
            None,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "got: {body:?}");

    // A token without `write:statuses` is rejected.
    let (status, body) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/statuses/{alice_post_id}/unpin"),
            Some(&bob_no_scope_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "got: {body:?}");

    // A non-owner may not unpin someone else's post (the real gap this test
    // closes: only *pin*'s ownership rejection had a test before task 10.5).
    let (status, body) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/statuses/{alice_post_id}/unpin"),
            Some(&bob_token),
            None,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a non-owner must not be able to unpin another actor's post: {body:?}"
    );

    // The owner can unpin.
    let (status, unpinned) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/statuses/{alice_post_id}/unpin"),
            Some(&alice_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {unpinned:?}");
    assert_eq!(unpinned["pinned"], false);

    app.cleanup().await;
}
