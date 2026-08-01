//! Router-level tests for `endpoints.rs`'s three handlers (Requirements
//! 1.1, 1.6, 2.1, 2.3, 3.1, 4.1, 7.2, 9.1, 9.2, 9.3, 9.4), driven through a
//! real, test-only axum `Router` dispatched via `tower::ServiceExt::oneshot`
//! against a real, `spawn_test_app`-backed Postgres schema — mirrors
//! `crate::social_graph::endpoints::tests`'s own established "real router,
//! real DB, no mocked auth" precedent (see `endpoints.rs`'s own doc comment,
//! "Testing strategy", for why this is the chosen approach over a new
//! `tests/*_it.rs` integration test).
//!
//! Coverage is deliberately scoped to what is genuinely new at this HTTP
//! layer — auth/scope enforcement, response codes, `Link`-header
//! attachment, and this module's own query-parameter wiring (`local`
//! selecting `TimelineKind::Local`, the tag path parameter, `any[]`
//! narrowing) — not `TimelineService::timeline`'s own aggregation/filter
//! behavior, which `tests/timeline_service_it.rs` (task 4.2) already proves
//! exhaustively end to end.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::routing::get;
use tower::ServiceExt;

use super::*;
use crate::actor::owner::create_owner;
use crate::actor::{ActorType, Handle, NewActor};
use crate::domain::{Id, Visibility};
use crate::oauth::app_repository::{self, NewApp};
use crate::oauth::model::ScopeSet as ModelScopeSet;
use crate::oauth::token_repository::{self, NewAccessToken};
use crate::social_graph::model::Follow;
use crate::social_graph::repository as sg_repository;
use crate::statuses::{Status, Tag, status_repository, tag_repository};
use crate::test_harness::{TestApp, spawn_test_app};
use crate::timelines::hydrator::StatusHydrator;
use crate::timelines::service::TimelineService;

// ---- Fixture plumbing (mirrors `tests/timeline_service_it.rs`'s own
// established conventions for this crate's timeline fixtures) -------------

async fn actor_fixture(app: &TestApp, handle_str: &str) -> Id {
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
            display_name: format!("Timeline Endpoints IT {handle_str}"),
            summary: "an actor used by the timeline endpoints test".to_string(),
        })
        .await
        .expect("create_actor (with signing key provisioning) must succeed");

    actor.id
}

async fn insert_status_fixture(
    app: &TestApp,
    actor_id: Id,
    visibility: Visibility,
    local: bool,
) -> Status {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    let uri = format!(
        "https://timeline-endpoints-it.example/statuses/{}",
        id.as_i64()
    );
    let status = Status {
        id,
        actor_id,
        uri: uri.clone(),
        url: Some(uri),
        content: "a fixture post inserted directly by timeline_endpoints tests".to_string(),
        visibility,
        sensitive: false,
        spoiler_text: String::new(),
        in_reply_to_id: None,
        in_reply_to_account_id: None,
        reblog_of_id: None,
        poll_id: None,
        language: None,
        reblogs_count: 0,
        favourites_count: 0,
        replies_count: 0,
        local,
        created_at: now,
        edited_at: None,
    };
    status_repository::insert_status(&app.pool, &status)
        .await
        .expect("insert status fixture must succeed");
    status
}

async fn tag_status(app: &TestApp, status_id: Id, tag_name: &str) {
    let tag_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    let tag = tag_repository::upsert_tag(
        &app.pool,
        &Tag {
            id: tag_id,
            name: tag_name.to_string(),
            created_at: now,
        },
    )
    .await
    .expect("upsert tag fixture must succeed");
    tag_repository::associate_tag(&app.pool, status_id, tag.id)
        .await
        .expect("associate tag fixture must succeed");
}

async fn upsert_follow(app: &TestApp, follower: Id, followee: Id) {
    sg_repository::upsert_follow(
        &app.pool,
        app.runtime.ids.next_id(),
        &Follow {
            follower: crate::domain::AccountRef::Local(follower),
            followee: crate::domain::AccountRef::Local(followee),
            reblogs: true,
            notify: false,
            languages: Vec::new(),
            activity_id: format!(
                "https://timeline-endpoints-it.example/activities/follow-{}",
                app.runtime.ids.next_id().as_i64()
            ),
            created_at: app.runtime.clock.now(),
        },
    )
    .await
    .expect("upsert_follow fixture must succeed");
}

/// Builds the `TimelineEndpointsState` these tests drive requests through —
/// mirrors `tests/timeline_service_it.rs::service`'s own construction of
/// `TimelineService`/`StatusHydrator` from `TestApp`'s already-wired
/// `AccountService`/`LocalFsStore`.
fn build_state(app: &TestApp) -> TimelineEndpointsState {
    let hydrator = StatusHydrator::new(
        app.pool.clone(),
        app.state.accounts().service(),
        app.state.media().store().clone(),
    );
    let service = TimelineService::new(app.pool.clone(), app.runtime.clone(), hydrator);
    let auth = AuthState {
        pool: app.pool.clone(),
        token_hash_key: app.state.config().oauth.token_hash_key.clone(),
    };
    TimelineEndpointsState {
        service: Arc::new(service),
        auth,
    }
}

/// Mounts this module's three handlers onto a test-only router — this task's
/// own boundary explicitly forbids mounting them onto the real production
/// router (task 5.2's job), so every test in this file dispatches through
/// this router directly via `tower::ServiceExt::oneshot`.
fn build_router(state: TimelineEndpointsState) -> Router {
    Router::new()
        .route(HOME_TIMELINE_PATH, get(home_timeline))
        .route(PUBLIC_TIMELINE_PATH, get(public_timeline))
        .route(TAG_TIMELINE_PATH, get(tag_timeline))
        .with_state(state)
}

/// Registers a real `oauth_applications` row, returning its `Id` — mirrors
/// `social_graph::endpoints::tests::register_test_app`.
async fn register_test_app(app: &TestApp) -> Id {
    let key = app.state.config().oauth.token_hash_key.clone();
    let now = app.runtime.clock.now();
    let registered = app_repository::register_app(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        &key,
        now,
        NewApp {
            name: "Timeline Endpoints Test Client".to_string(),
            redirect_uris: vec!["https://client.example/callback".to_string()],
            scopes: ModelScopeSet::new(["read", "write"]),
        },
    )
    .await
    .expect("register_app must succeed");
    registered.id
}

/// Issues a real access token bound to `actor_id` with `scopes`, returning
/// its plaintext bearer value — mirrors
/// `social_graph::endpoints::tests::issue_test_token`. Never hand-constructs
/// a `RequestActorContext`.
async fn issue_test_token(app: &TestApp, app_id: Id, actor_id: Id, scopes: &[&str]) -> String {
    let key = app.state.config().oauth.token_hash_key.clone();
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

async fn dispatch(router: &Router, path: &str, token: Option<&str>) -> axum::http::Response<Body> {
    let mut builder = Request::builder().method("GET").uri(path);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let request = builder
        .body(Body::empty())
        .expect("building the test request must succeed");
    router
        .clone()
        .oneshot(request)
        .await
        .expect("dispatching the test request must succeed")
}

async fn body_json(response: axum::http::Response<Body>) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("reading the response body must succeed");
    serde_json::from_slice(&bytes).expect("response body must be valid JSON")
}

fn ids_of(items: &serde_json::Value) -> Vec<String> {
    items
        .as_array()
        .expect("response body must be a JSON array")
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_string())
        .collect()
}

// ---- home: auth/scope ----------------------------------------------------

#[tokio::test]
async fn home_timeline_without_a_bearer_token_is_401() {
    let app = spawn_test_app().await;
    let router = build_router(build_state(&app));

    let response = dispatch(&router, HOME_TIMELINE_PATH, None).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    app.cleanup().await;
}

#[tokio::test]
async fn home_timeline_with_insufficient_scope_is_403() {
    let app = spawn_test_app().await;
    let router = build_router(build_state(&app));

    let app_id = register_test_app(&app).await;
    let viewer = actor_fixture(&app, "home_403_viewer").await;
    // `read:accounts` deliberately omits `read:statuses`.
    let token = issue_test_token(&app, app_id, viewer, &["read:accounts"]).await;

    let response = dispatch(&router, HOME_TIMELINE_PATH, Some(&token)).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    app.cleanup().await;
}

#[tokio::test]
async fn home_timeline_returns_followed_and_self_posts_with_link_header() {
    let app = spawn_test_app().await;
    let router = build_router(build_state(&app));

    let app_id = register_test_app(&app).await;
    let viewer = actor_fixture(&app, "home_ok_viewer").await;
    let followed = actor_fixture(&app, "home_ok_followed").await;
    let stranger = actor_fixture(&app, "home_ok_stranger").await;
    upsert_follow(&app, viewer, followed).await;

    let own_post = insert_status_fixture(&app, viewer, Visibility::Public, true).await;
    let followed_post = insert_status_fixture(&app, followed, Visibility::Public, true).await;
    let stranger_post = insert_status_fixture(&app, stranger, Visibility::Public, true).await;

    let token = issue_test_token(&app, app_id, viewer, &["read:statuses"]).await;
    let response = dispatch(&router, HOME_TIMELINE_PATH, Some(&token)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers().get(header::LINK).is_some(),
        "a non-empty page must carry a Link header (Requirement 7.2)"
    );

    let body = body_json(response).await;
    let ids = ids_of(&body);
    assert!(ids.contains(&own_post.id.as_i64().to_string()));
    assert!(ids.contains(&followed_post.id.as_i64().to_string()));
    assert!(!ids.contains(&stranger_post.id.as_i64().to_string()));

    app.cleanup().await;
}

// ---- public/local ---------------------------------------------------------

#[tokio::test]
async fn public_timeline_unauthenticated_returns_only_public_posts() {
    let app = spawn_test_app().await;
    let router = build_router(build_state(&app));

    let author = actor_fixture(&app, "public_unauth_author").await;
    let public_post = insert_status_fixture(&app, author, Visibility::Public, true).await;
    let private_post = insert_status_fixture(&app, author, Visibility::Private, true).await;

    let response = dispatch(&router, PUBLIC_TIMELINE_PATH, None).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "unauthenticated public timeline requests must be allowed (Requirement 9.2)"
    );
    assert!(response.headers().get(header::LINK).is_some());

    let body = body_json(response).await;
    let ids = ids_of(&body);
    assert!(ids.contains(&public_post.id.as_i64().to_string()));
    assert!(!ids.contains(&private_post.id.as_i64().to_string()));

    app.cleanup().await;
}

#[tokio::test]
async fn public_timeline_local_true_excludes_remote_posts() {
    let app = spawn_test_app().await;
    let router = build_router(build_state(&app));

    let local_author = actor_fixture(&app, "public_local_author").await;
    let local_post = insert_status_fixture(&app, local_author, Visibility::Public, true).await;
    let remote_author_id = app.runtime.ids.next_id();
    let remote_post =
        insert_status_fixture(&app, remote_author_id, Visibility::Public, false).await;

    let response = dispatch(&router, "/api/v1/timelines/public?local=true", None).await;
    assert_eq!(response.status(), StatusCode::OK);

    let body = body_json(response).await;
    let ids = ids_of(&body);
    assert!(
        ids.contains(&local_post.id.as_i64().to_string()),
        "a local public post must be included when local=true (Requirement 3.1)"
    );
    assert!(
        !ids.contains(&remote_post.id.as_i64().to_string()),
        "a remote post must be excluded when local=true (Requirement 3.1)"
    );

    app.cleanup().await;
}

// ---- tag --------------------------------------------------------------

#[tokio::test]
async fn tag_timeline_matches_the_path_hashtag_case_insensitively() {
    let app = spawn_test_app().await;
    let router = build_router(build_state(&app));

    let author = actor_fixture(&app, "tag_match_author").await;
    let tagged_post = insert_status_fixture(&app, author, Visibility::Public, true).await;
    // Mirrors `tests/timeline_candidate_repository_it.rs`'s own fixture
    // convention: `tags.name` is inserted already-normalized (lower-case),
    // matching statuses-core's own real extraction-time normalization
    // (`candidate_repository.rs`'s documented precondition) — this test
    // instead proves case-insensitivity of the *path segment the client
    // sends* (`RuSt`), which is this endpoint's own responsibility to fold
    // via `fold_tag` before matching.
    tag_status(&app, tagged_post.id, "rust").await;
    let untagged_post = insert_status_fixture(&app, author, Visibility::Public, true).await;

    let response = dispatch(&router, "/api/v1/timelines/tag/RuSt", None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().get(header::LINK).is_some());

    let body = body_json(response).await;
    let ids = ids_of(&body);
    assert!(ids.contains(&tagged_post.id.as_i64().to_string()));
    assert!(!ids.contains(&untagged_post.id.as_i64().to_string()));

    app.cleanup().await;
}

#[tokio::test]
async fn tag_timeline_any_filter_requires_at_least_one_additional_tag() {
    let app = spawn_test_app().await;
    let router = build_router(build_state(&app));

    let author = actor_fixture(&app, "tag_any_author").await;

    let matching_post = insert_status_fixture(&app, author, Visibility::Public, true).await;
    tag_status(&app, matching_post.id, "rust").await;
    tag_status(&app, matching_post.id, "rustlang").await;

    let non_matching_post = insert_status_fixture(&app, author, Visibility::Public, true).await;
    tag_status(&app, non_matching_post.id, "rust").await;

    let response = dispatch(
        &router,
        "/api/v1/timelines/tag/rust?any[]=rustlang&any[]=programming",
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let body = body_json(response).await;
    let ids = ids_of(&body);
    assert!(
        ids.contains(&matching_post.id.as_i64().to_string()),
        "a post carrying an any[] tag must be included"
    );
    assert!(
        !ids.contains(&non_matching_post.id.as_i64().to_string()),
        "a post missing every any[] tag must be excluded"
    );

    app.cleanup().await;
}
