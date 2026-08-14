//! Router-level integration tests for `kawasemi::notifications::endpoints`'s
//! four handlers (Requirements 2.1, 2.2, 2.3, 2.5, 3.1, 3.2, 4.1, 4.2, 4.3,
//! 9.1, 9.2, 9.3), driven through a real, test-only axum `Router` dispatched
//! via `tower::ServiceExt::oneshot` against a real, `spawn_test_app`-backed
//! Postgres schema.
//!
//! These tests were moved here from `src/notifications/endpoints/tests.rs`
//! by `.kiro/specs/test-placement-migration` task 4.1, so that steering
//! `structure.md`'s test layout rule ("DB込みの実起動インスタンスを要する検証
//! は `tests/` 直下の `*_it.rs` に置く") holds in fact and not only on paper.
//! Every assertion is unchanged from before the move; only import
//! qualification changed (`crate::` -> `kawasemi::`, and the glob over the
//! production module expanded into explicit imports).
//!
//! Coverage is deliberately scoped to what is genuinely new at this HTTP
//! layer — auth/scope enforcement, response codes, `Link`-header attachment,
//! `types[]`/`exclude_types[]`/`account_id` query-parameter wiring, and that
//! module's own `account_id` resolution (`resolve_account_id_filter`,
//! including the "unresolved -> 200 + empty array, not 404" branch,
//! Requirement 2.3) — not `NotificationService`'s own list/single/dismiss/
//! clear business logic, which `tests/notifications_service_it.rs` covers.
//!
//! ## Relationship to the production-router notification integration tests
//! `tests/notification_list_it.rs`, `tests/notification_show_dismiss_it.rs`
//! and `tests/notification_contract_it.rs` drive the same four handlers
//! through the *real, fully-wired* production router
//! (`kawasemi::server::build_router`). This file instead mounts the handlers
//! onto a router it builds itself, which is a different entry point: it is
//! the component-level check of the handlers themselves, not of the module
//! wiring. Both sets are kept as-is; neither subsumes the other.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::routing::{get, post};
use tower::ServiceExt;

use kawasemi::accounts::model::{ProfileField, RemoteAccount};
use kawasemi::accounts::remote_repository::upsert_remote;
use kawasemi::actor::owner::create_owner;
use kawasemi::actor::repository::insert_actor;
use kawasemi::actor::{ActorState, ActorType, Handle};
use kawasemi::domain::{AccountRef, Id};
use kawasemi::notifications::endpoints::{
    NOTIFICATION_DISMISS_PATH, NOTIFICATION_SHOW_PATH, NOTIFICATIONS_CLEAR_PATH,
    NOTIFICATIONS_LIST_PATH, NotificationEndpointsState, clear_notifications, dismiss_notification,
    list_notifications, show_notification,
};
use kawasemi::notifications::model::{Notification, NotificationType};
use kawasemi::notifications::repository::insert_dedup;
use kawasemi::notifications::service::NotificationService;
use kawasemi::oauth::app_repository::{self, NewApp};
use kawasemi::oauth::middleware::AuthState;
use kawasemi::oauth::model::ScopeSet as ModelScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::test_harness::{TestApp, spawn_test_app};

// ---- Fixture plumbing (mirrors `tests/notifications_service_it.rs`'s own
// established `create_test_actor`/`sample_notification`/`seed_notification`
// helpers) ------------------------------------------------------------------

async fn create_test_actor(app: &TestApp, handle: &str) -> Id {
    let now = app.runtime.clock.now();
    let owner_id = app.runtime.ids.next_id();
    create_owner(&app.pool, owner_id, now)
        .await
        .expect("creating the owner must succeed");

    let actor_id = app.runtime.ids.next_id();
    let actor = kawasemi::actor::model::LocalActor {
        id: actor_id,
        owner_id,
        handle: Handle::new(handle).expect("test handle must be valid"),
        actor_type: ActorType::Person,
        display_name: "Notification Endpoints Test Actor".to_string(),
        summary: "an actor used by the notification endpoints test".to_string(),
        state: ActorState::Active,
        created_at: now,
        updated_at: now,
    };
    let mut tx = app
        .pool
        .begin()
        .await
        .expect("opening a transaction must succeed");
    insert_actor(&mut tx, &actor)
        .await
        .expect("insert_actor must succeed");
    tx.commit().await.expect("committing must succeed");

    actor_id
}

/// Inserts a known remote account row, returning its `Id` — used by the
/// `account_id=<known remote>` filter tests (Requirement 2.3's "既知リモート"
/// branch).
async fn create_remote_account(app: &TestApp, username: &str) -> Id {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    let account = RemoteAccount {
        id,
        actor_uri: format!("https://notification-endpoints-it.example/users/{username}"),
        username: username.to_string(),
        domain: "notification-endpoints-it.example".to_string(),
        display_name: "Notification Endpoints IT Remote".to_string(),
        note: String::new(),
        url: format!("https://notification-endpoints-it.example/@{username}"),
        avatar_url: None,
        header_url: None,
        fields: Vec::<ProfileField>::new(),
        bot: false,
        locked: false,
        fetched_at: now,
    };
    let persisted = upsert_remote(&app.pool, &account)
        .await
        .expect("upsert_remote fixture must succeed");
    persisted.id
}

#[allow(clippy::too_many_arguments)]
fn sample_notification(
    id: Id,
    recipient_id: Id,
    kind: NotificationType,
    origin: AccountRef,
    status_id: Option<Id>,
    created_at: time::OffsetDateTime,
) -> Notification {
    Notification {
        id,
        recipient_id,
        kind,
        origin,
        status_id,
        dismissed: false,
        created_at,
    }
}

async fn seed_notification(
    app: &TestApp,
    recipient_id: Id,
    kind: NotificationType,
    origin: AccountRef,
) -> Id {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    let notification = sample_notification(id, recipient_id, kind, origin, None, now);
    insert_dedup(&app.pool, &notification)
        .await
        .expect("insert_dedup must succeed");
    id
}

/// Builds the `NotificationEndpointsState` these tests drive requests
/// through — mirrors `tests/notifications_service_it.rs::build_service`'s own
/// construction of `NotificationService` from `TestApp`'s already-wired
/// `AccountService`/`LocalFsStore`, plus this module's own two additional
/// `account_id`-resolution collaborators (`ActorDirectory`/`PgPool`, see
/// `endpoints.rs`'s own doc comment).
fn build_state(app: &TestApp) -> NotificationEndpointsState {
    let service = NotificationService::new(
        app.pool.clone(),
        app.runtime.clone(),
        app.state.config().server.domain.clone(),
        app.state.accounts().service(),
        app.state.media().store().clone(),
    );
    let auth = AuthState {
        pool: app.pool.clone(),
        token_hash_key: app.state.config().oauth.token_hash_key.clone(),
    };
    NotificationEndpointsState {
        service: Arc::new(service),
        actor_directory: Arc::clone(app.state.actor().directory()),
        pool: app.pool.clone(),
        auth,
    }
}

/// Mounts this module's four handlers onto a test-only router — this task's
/// own boundary explicitly forbids mounting them onto the real production
/// router (task 4.2's job), so every test in this file dispatches through
/// this router directly via `tower::ServiceExt::oneshot`.
fn build_router(state: NotificationEndpointsState) -> Router {
    Router::new()
        .route(NOTIFICATIONS_LIST_PATH, get(list_notifications))
        .route(NOTIFICATION_SHOW_PATH, get(show_notification))
        .route(NOTIFICATIONS_CLEAR_PATH, post(clear_notifications))
        .route(NOTIFICATION_DISMISS_PATH, post(dismiss_notification))
        .with_state(state)
}

/// Registers a real `oauth_applications` row, returning its `Id` — mirrors
/// `tests/social_graph_endpoints_it.rs::register_test_app`/
/// `tests/timelines_endpoints_handler_it.rs::register_test_app`.
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
            name: "Notification Endpoints Test Client".to_string(),
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
/// `tests/social_graph_endpoints_it.rs::issue_test_token`/
/// `tests/timelines_endpoints_handler_it.rs::issue_test_token`. Never
/// hand-constructs a `RequestActorContext`.
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

async fn dispatch(
    router: &Router,
    method: &str,
    path: &str,
    token: Option<&str>,
) -> axum::http::Response<Body> {
    let mut builder = Request::builder().method(method).uri(path);
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

// ---- list_notifications: auth/scope ---------------------------------------

#[tokio::test]
async fn list_notifications_without_a_bearer_token_is_401() {
    let app = spawn_test_app().await;
    let router = build_router(build_state(&app));

    let response = dispatch(&router, "GET", NOTIFICATIONS_LIST_PATH, None).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    app.cleanup().await;
}

#[tokio::test]
async fn list_notifications_with_insufficient_scope_is_403() {
    let app = spawn_test_app().await;
    let router = build_router(build_state(&app));

    let app_id = register_test_app(&app).await;
    let viewer = create_test_actor(&app, "list_403_viewer").await;
    // `read:accounts` deliberately omits `read:notifications`.
    let token = issue_test_token(&app, app_id, viewer, &["read:accounts"]).await;

    let response = dispatch(&router, "GET", NOTIFICATIONS_LIST_PATH, Some(&token)).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    app.cleanup().await;
}

// ---- list_notifications: happy path + Link header --------------------------

#[tokio::test]
async fn list_notifications_returns_recipient_notifications_newest_first() {
    let app = spawn_test_app().await;
    let router = build_router(build_state(&app));

    let app_id = register_test_app(&app).await;
    let recipient = create_test_actor(&app, "list_ok_recipient").await;
    // Two *distinct* origins: `insert_dedup`'s `ON CONFLICT` target is
    // (`recipient_id`, `kind`, `origin_kind`, `origin_id`,
    // `COALESCE(status_id, 0)`), so two `Follow` notifications sharing one
    // origin collapse into a single row and this test's own newest-first
    // assertion would have nothing to order.
    let older_origin = create_test_actor(&app, "list_ok_origin_older").await;
    let newer_origin = create_test_actor(&app, "list_ok_origin_newer").await;

    let older = seed_notification(
        &app,
        recipient,
        NotificationType::Follow,
        AccountRef::Local(older_origin),
    )
    .await;
    let newer = seed_notification(
        &app,
        recipient,
        NotificationType::Follow,
        AccountRef::Local(newer_origin),
    )
    .await;

    let token = issue_test_token(&app, app_id, recipient, &["read:notifications"]).await;
    let response = dispatch(&router, "GET", NOTIFICATIONS_LIST_PATH, Some(&token)).await;
    assert_eq!(response.status(), StatusCode::OK);

    let body = body_json(response).await;
    let returned_ids = ids_of(&body);
    assert_eq!(
        returned_ids,
        vec![newer.as_i64().to_string(), older.as_i64().to_string()],
        "newest-first ordering (Requirement 2.1)"
    );

    app.cleanup().await;
}

#[tokio::test]
async fn list_notifications_filters_by_types() {
    let app = spawn_test_app().await;
    let router = build_router(build_state(&app));

    let app_id = register_test_app(&app).await;
    let recipient = create_test_actor(&app, "list_types_recipient").await;
    let origin_actor = create_test_actor(&app, "list_types_origin").await;

    let follow_id = seed_notification(
        &app,
        recipient,
        NotificationType::Follow,
        AccountRef::Local(origin_actor),
    )
    .await;
    seed_notification(
        &app,
        recipient,
        NotificationType::FollowRequest,
        AccountRef::Local(origin_actor),
    )
    .await;

    let token = issue_test_token(&app, app_id, recipient, &["read:notifications"]).await;
    let response = dispatch(
        &router,
        "GET",
        &format!("{NOTIFICATIONS_LIST_PATH}?types[]=follow"),
        Some(&token),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let body = body_json(response).await;
    assert_eq!(ids_of(&body), vec![follow_id.as_i64().to_string()]);

    app.cleanup().await;
}

#[tokio::test]
async fn list_notifications_with_unknown_type_is_422() {
    let app = spawn_test_app().await;
    let router = build_router(build_state(&app));

    let app_id = register_test_app(&app).await;
    let recipient = create_test_actor(&app, "list_bad_type_recipient").await;
    let token = issue_test_token(&app, app_id, recipient, &["read:notifications"]).await;

    let response = dispatch(
        &router,
        "GET",
        &format!("{NOTIFICATIONS_LIST_PATH}?types[]=bogus"),
        Some(&token),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

    app.cleanup().await;
}

// ---- list_notifications: account_id resolution (Requirement 2.3) ----------

#[tokio::test]
async fn list_notifications_with_unresolved_account_id_returns_empty_array_not_404() {
    let app = spawn_test_app().await;
    let router = build_router(build_state(&app));

    let app_id = register_test_app(&app).await;
    let recipient = create_test_actor(&app, "list_unresolved_recipient").await;
    let origin_actor = create_test_actor(&app, "list_unresolved_origin").await;
    seed_notification(
        &app,
        recipient,
        NotificationType::Follow,
        AccountRef::Local(origin_actor),
    )
    .await;

    let token = issue_test_token(&app, app_id, recipient, &["read:notifications"]).await;
    // A numeric id matching neither a local actor nor a known remote account.
    let unknown_id = app.runtime.ids.next_id().as_i64();
    let response = dispatch(
        &router,
        "GET",
        &format!("{NOTIFICATIONS_LIST_PATH}?account_id={unknown_id}"),
        Some(&token),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "unresolved account_id must be 200, not 404 (Requirement 2.3)"
    );

    let body = body_json(response).await;
    assert_eq!(
        body.as_array().expect("body must be a JSON array").len(),
        0,
        "unresolved account_id must yield an empty array, not the recipient's other notifications"
    );

    app.cleanup().await;
}

#[tokio::test]
async fn list_notifications_with_non_numeric_account_id_returns_empty_array_not_404() {
    let app = spawn_test_app().await;
    let router = build_router(build_state(&app));

    let app_id = register_test_app(&app).await;
    let recipient = create_test_actor(&app, "list_nonnumeric_recipient").await;
    let token = issue_test_token(&app, app_id, recipient, &["read:notifications"]).await;

    let response = dispatch(
        &router,
        "GET",
        &format!("{NOTIFICATIONS_LIST_PATH}?account_id=not-a-number"),
        Some(&token),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body.as_array().expect("body must be a JSON array").len(), 0);

    app.cleanup().await;
}

#[tokio::test]
async fn list_notifications_with_known_local_account_id_filters_to_that_origin() {
    let app = spawn_test_app().await;
    let router = build_router(build_state(&app));

    let app_id = register_test_app(&app).await;
    let recipient = create_test_actor(&app, "list_local_filter_recipient").await;
    let matching_origin = create_test_actor(&app, "list_local_filter_match").await;
    let other_origin = create_test_actor(&app, "list_local_filter_other").await;

    let matching_id = seed_notification(
        &app,
        recipient,
        NotificationType::Follow,
        AccountRef::Local(matching_origin),
    )
    .await;
    seed_notification(
        &app,
        recipient,
        NotificationType::Follow,
        AccountRef::Local(other_origin),
    )
    .await;

    let token = issue_test_token(&app, app_id, recipient, &["read:notifications"]).await;
    let response = dispatch(
        &router,
        "GET",
        &format!(
            "{NOTIFICATIONS_LIST_PATH}?account_id={}",
            matching_origin.as_i64()
        ),
        Some(&token),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(ids_of(&body), vec![matching_id.as_i64().to_string()]);

    app.cleanup().await;
}

#[tokio::test]
async fn list_notifications_with_known_remote_account_id_filters_to_that_origin() {
    let app = spawn_test_app().await;
    let router = build_router(build_state(&app));

    let app_id = register_test_app(&app).await;
    let recipient = create_test_actor(&app, "list_remote_filter_recipient").await;
    let remote_origin = create_remote_account(&app, "list_remote_filter_match").await;

    let matching_id = seed_notification(
        &app,
        recipient,
        NotificationType::Follow,
        AccountRef::Remote(remote_origin),
    )
    .await;

    let token = issue_test_token(&app, app_id, recipient, &["read:notifications"]).await;
    let response = dispatch(
        &router,
        "GET",
        &format!(
            "{NOTIFICATIONS_LIST_PATH}?account_id={}",
            remote_origin.as_i64()
        ),
        Some(&token),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(ids_of(&body), vec![matching_id.as_i64().to_string()]);

    app.cleanup().await;
}

#[tokio::test]
async fn list_notifications_link_header_present_when_more_pages_remain() {
    let app = spawn_test_app().await;
    let router = build_router(build_state(&app));

    let app_id = register_test_app(&app).await;
    let recipient = create_test_actor(&app, "list_link_recipient").await;
    let origin_actor = create_test_actor(&app, "list_link_origin").await;
    for _ in 0..3 {
        seed_notification(
            &app,
            recipient,
            NotificationType::Follow,
            AccountRef::Local(origin_actor),
        )
        .await;
    }

    let token = issue_test_token(&app, app_id, recipient, &["read:notifications"]).await;
    let response = dispatch(
        &router,
        "GET",
        &format!("{NOTIFICATIONS_LIST_PATH}?limit=1"),
        Some(&token),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers().get(header::LINK).is_some(),
        "Link header must be present when more pages remain (Requirement 9.3)"
    );

    app.cleanup().await;
}

// ---- show_notification ------------------------------------------------------

#[tokio::test]
async fn show_notification_without_a_bearer_token_is_401() {
    let app = spawn_test_app().await;
    let router = build_router(build_state(&app));

    let response = dispatch(&router, "GET", "/api/v1/notifications/1", None).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    app.cleanup().await;
}

#[tokio::test]
async fn show_notification_with_insufficient_scope_is_403() {
    let app = spawn_test_app().await;
    let router = build_router(build_state(&app));

    let app_id = register_test_app(&app).await;
    let recipient = create_test_actor(&app, "show_403_recipient").await;
    let token = issue_test_token(&app, app_id, recipient, &["write:notifications"]).await;

    let response = dispatch(&router, "GET", "/api/v1/notifications/1", Some(&token)).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    app.cleanup().await;
}

#[tokio::test]
async fn show_notification_returns_200_for_own_notification() {
    let app = spawn_test_app().await;
    let router = build_router(build_state(&app));

    let app_id = register_test_app(&app).await;
    let recipient = create_test_actor(&app, "show_ok_recipient").await;
    let origin_actor = create_test_actor(&app, "show_ok_origin").await;
    let notification_id = seed_notification(
        &app,
        recipient,
        NotificationType::Follow,
        AccountRef::Local(origin_actor),
    )
    .await;

    let token = issue_test_token(&app, app_id, recipient, &["read:notifications"]).await;
    let response = dispatch(
        &router,
        "GET",
        &format!("/api/v1/notifications/{}", notification_id.as_i64()),
        Some(&token),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["id"], notification_id.as_i64().to_string());
    assert_eq!(body["type"], "follow");

    app.cleanup().await;
}

#[tokio::test]
async fn show_notification_for_another_actors_notification_is_404() {
    let app = spawn_test_app().await;
    let router = build_router(build_state(&app));

    let app_id = register_test_app(&app).await;
    let owner = create_test_actor(&app, "show_404_owner").await;
    let intruder = create_test_actor(&app, "show_404_intruder").await;
    let origin_actor = create_test_actor(&app, "show_404_origin").await;
    let notification_id = seed_notification(
        &app,
        owner,
        NotificationType::Follow,
        AccountRef::Local(origin_actor),
    )
    .await;

    let token = issue_test_token(&app, app_id, intruder, &["read:notifications"]).await;
    let response = dispatch(
        &router,
        "GET",
        &format!("/api/v1/notifications/{}", notification_id.as_i64()),
        Some(&token),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    app.cleanup().await;
}

#[tokio::test]
async fn show_notification_for_nonexistent_id_is_404() {
    let app = spawn_test_app().await;
    let router = build_router(build_state(&app));

    let app_id = register_test_app(&app).await;
    let recipient = create_test_actor(&app, "show_404_nonexistent_recipient").await;
    let token = issue_test_token(&app, app_id, recipient, &["read:notifications"]).await;

    let never_used_id = app.runtime.ids.next_id().as_i64();
    let response = dispatch(
        &router,
        "GET",
        &format!("/api/v1/notifications/{never_used_id}"),
        Some(&token),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    app.cleanup().await;
}

#[tokio::test]
async fn show_notification_with_non_numeric_id_is_404() {
    let app = spawn_test_app().await;
    let router = build_router(build_state(&app));

    let app_id = register_test_app(&app).await;
    let recipient = create_test_actor(&app, "show_404_bad_id_recipient").await;
    let token = issue_test_token(&app, app_id, recipient, &["read:notifications"]).await;

    let response = dispatch(
        &router,
        "GET",
        "/api/v1/notifications/not-a-number",
        Some(&token),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    app.cleanup().await;
}

// ---- clear_notifications ----------------------------------------------------

#[tokio::test]
async fn clear_notifications_without_a_bearer_token_is_401() {
    let app = spawn_test_app().await;
    let router = build_router(build_state(&app));

    let response = dispatch(&router, "POST", NOTIFICATIONS_CLEAR_PATH, None).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    app.cleanup().await;
}

#[tokio::test]
async fn clear_notifications_with_insufficient_scope_is_403() {
    let app = spawn_test_app().await;
    let router = build_router(build_state(&app));

    let app_id = register_test_app(&app).await;
    let recipient = create_test_actor(&app, "clear_403_recipient").await;
    let token = issue_test_token(&app, app_id, recipient, &["read:notifications"]).await;

    let response = dispatch(&router, "POST", NOTIFICATIONS_CLEAR_PATH, Some(&token)).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    app.cleanup().await;
}

#[tokio::test]
async fn clear_notifications_dismisses_everything_and_returns_empty_object() {
    let app = spawn_test_app().await;
    let router = build_router(build_state(&app));

    let app_id = register_test_app(&app).await;
    let recipient = create_test_actor(&app, "clear_ok_recipient").await;
    let origin_actor = create_test_actor(&app, "clear_ok_origin").await;
    seed_notification(
        &app,
        recipient,
        NotificationType::Follow,
        AccountRef::Local(origin_actor),
    )
    .await;

    let token = issue_test_token(&app, app_id, recipient, &["write:notifications"]).await;
    let response = dispatch(&router, "POST", NOTIFICATIONS_CLEAR_PATH, Some(&token)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body, serde_json::json!({}));

    // A subsequent list must now be empty (Requirement 4.4, exercised
    // end-to-end through this handler).
    let list_token = issue_test_token(&app, app_id, recipient, &["read:notifications"]).await;
    let list_response = dispatch(&router, "GET", NOTIFICATIONS_LIST_PATH, Some(&list_token)).await;
    let list_body = body_json(list_response).await;
    assert_eq!(list_body.as_array().unwrap().len(), 0);

    app.cleanup().await;
}

// ---- dismiss_notification ----------------------------------------------------

#[tokio::test]
async fn dismiss_notification_without_a_bearer_token_is_401() {
    let app = spawn_test_app().await;
    let router = build_router(build_state(&app));

    let response = dispatch(&router, "POST", "/api/v1/notifications/1/dismiss", None).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    app.cleanup().await;
}

#[tokio::test]
async fn dismiss_notification_with_insufficient_scope_is_403() {
    let app = spawn_test_app().await;
    let router = build_router(build_state(&app));

    let app_id = register_test_app(&app).await;
    let recipient = create_test_actor(&app, "dismiss_403_recipient").await;
    let token = issue_test_token(&app, app_id, recipient, &["read:notifications"]).await;

    let response = dispatch(
        &router,
        "POST",
        "/api/v1/notifications/1/dismiss",
        Some(&token),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    app.cleanup().await;
}

#[tokio::test]
async fn dismiss_notification_dismisses_and_returns_empty_object() {
    let app = spawn_test_app().await;
    let router = build_router(build_state(&app));

    let app_id = register_test_app(&app).await;
    let recipient = create_test_actor(&app, "dismiss_ok_recipient").await;
    let origin_actor = create_test_actor(&app, "dismiss_ok_origin").await;
    let notification_id = seed_notification(
        &app,
        recipient,
        NotificationType::Follow,
        AccountRef::Local(origin_actor),
    )
    .await;

    let token = issue_test_token(&app, app_id, recipient, &["write:notifications"]).await;
    let response = dispatch(
        &router,
        "POST",
        &format!("/api/v1/notifications/{}/dismiss", notification_id.as_i64()),
        Some(&token),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body, serde_json::json!({}));

    // A subsequent show must now be 404 (Requirement 4.4).
    let show_token = issue_test_token(&app, app_id, recipient, &["read:notifications"]).await;
    let show_response = dispatch(
        &router,
        "GET",
        &format!("/api/v1/notifications/{}", notification_id.as_i64()),
        Some(&show_token),
    )
    .await;
    assert_eq!(show_response.status(), StatusCode::NOT_FOUND);

    app.cleanup().await;
}

#[tokio::test]
async fn dismiss_notification_for_another_actors_notification_is_404() {
    let app = spawn_test_app().await;
    let router = build_router(build_state(&app));

    let app_id = register_test_app(&app).await;
    let owner = create_test_actor(&app, "dismiss_404_owner").await;
    let intruder = create_test_actor(&app, "dismiss_404_intruder").await;
    let origin_actor = create_test_actor(&app, "dismiss_404_origin").await;
    let notification_id = seed_notification(
        &app,
        owner,
        NotificationType::Follow,
        AccountRef::Local(origin_actor),
    )
    .await;

    let token = issue_test_token(&app, app_id, intruder, &["write:notifications"]).await;
    let response = dispatch(
        &router,
        "POST",
        &format!("/api/v1/notifications/{}/dismiss", notification_id.as_i64()),
        Some(&token),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    app.cleanup().await;
}

#[tokio::test]
async fn dismiss_notification_for_nonexistent_id_is_404() {
    let app = spawn_test_app().await;
    let router = build_router(build_state(&app));

    let app_id = register_test_app(&app).await;
    let recipient = create_test_actor(&app, "dismiss_404_nonexistent_recipient").await;
    let token = issue_test_token(&app, app_id, recipient, &["write:notifications"]).await;

    let never_used_id = app.runtime.ids.next_id().as_i64();
    let response = dispatch(
        &router,
        "POST",
        &format!("/api/v1/notifications/{never_used_id}/dismiss"),
        Some(&token),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    app.cleanup().await;
}
