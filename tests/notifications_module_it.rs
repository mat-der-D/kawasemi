//! Composition-root wiring tests for `kawasemi::notifications` — that
//! [`kawasemi::notifications::build_notification_module`]'s assembly is
//! actually reachable through the real, fully-booted application: the
//! notification routes are mounted and authenticated, and a favourite
//! travelling the whole wired chain (`StatusService::favourite`'s local
//! emit -> `NotificationSinkRegistry` -> `StatusesEventSinkAdapter` ->
//! `GeneratorEventSink` -> `NotificationGenerator::generate` ->
//! `NotificationRepository::insert_dedup` -> `NotificationService::list` ->
//! the mounted `list_notifications` handler) becomes visible, including
//! through the real `ActorDirectory`-backed `account_id` resolution.
//!
//! These tests were moved here from `src/notifications/tests.rs` by
//! `.kiro/specs/test-placement-migration` task 4.2, so that steering
//! `structure.md`'s test layout rule ("DB込みの実起動インスタンスを要する検証
//! は `tests/` 直下の `*_it.rs` に置く") holds in fact and not only on paper.
//! Both tests require a live database via `spawn_test_app`. Every assertion
//! is unchanged from before the move; only import qualification changed
//! (`crate::` -> `kawasemi::`). The move emptied the source file, which was
//! deleted along with its `#[cfg(test)] mod tests;` declaration; the pointer
//! to this file now lives in `src/notifications.rs`'s own module doc.
//!
//! ## Relationship to the other notification integration tests
//! `tests/notification_list_it.rs`, `tests/notification_generation_it.rs`,
//! `tests/notification_filter_it.rs` and `tests/notification_contract_it.rs`
//! drive the same production router to assert *behaviour* — every kind's
//! generation, dedup, suppression, list semantics, JSON shape. This file
//! asserts the *wiring* instead: that the route is mounted at all (an
//! unmounted route would 404, never 401) and that one event traverses every
//! seam `build_notification_module` connects. `tests/module_wiring_it.rs`
//! is the same kind of check for `compose_modules`' account-ports ordering.
//! Both sets are kept as-is; neither is a re-verification of the other's
//! subject.
//!
//! The fixture plumbing below mirrors `src/social_graph/tests.rs`'s own
//! established helpers. Each `tests/*.rs` file is its own compiled crate, so
//! such plumbing is duplicated rather than imported — this crate's own
//! documented convention.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor};
use kawasemi::domain::Id;
use kawasemi::oauth::app_repository::{self, NewApp};
use kawasemi::oauth::model::ScopeSet as ModelScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::server;
use kawasemi::test_harness::{TestApp, spawn_test_app};

// ---- Fixture plumbing (mirrors `src/social_graph/tests.rs`'s own
// established helpers). ----

async fn create_test_actor(app: &TestApp, handle: &str) -> Id {
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
            handle: Handle::new(handle).expect("test handle must be valid"),
            actor_type: ActorType::Person,
            display_name: "Notifications Wiring Test Actor".to_string(),
            summary: "an actor used by notifications' own task 4.2 wiring tests".to_string(),
        })
        .await
        .expect("create_actor (with signing key provisioning) must succeed");

    actor.id
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
            name: "Notifications Wiring Test Client".to_string(),
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

/// Requirement 9.1: proves the router is actually mounted (a not-yet-mounted
/// route 404s, never 401) and that `AuthState` is correctly derived for it.
#[tokio::test]
async fn notification_endpoints_require_bearer_auth() {
    let app = spawn_test_app().await;
    let router = real_router(&app);

    let (status, body) = send(&router, req("GET", "/api/v1/notifications", None, None)).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "an unmounted route would 404, not 401: {body:?}"
    );

    app.cleanup().await;
}

/// Requirements 2.3, 5.1, 5.2, 5.4, 5.5, 6.1, 9.1: the full wired chain, end
/// to end — see this module's own doc comment.
#[tokio::test]
async fn favourite_flows_through_the_wired_sink_to_the_generator_and_is_visible_via_real_account_resolution()
 {
    let app = spawn_test_app().await;
    let router = real_router(&app);

    let author = create_test_actor(&app, "notif_wiring_author").await;
    let favouriter = create_test_actor(&app, "notif_wiring_favouriter").await;
    let app_id = register_test_app(&app).await;
    let author_token = issue_test_token(
        &app,
        app_id,
        author,
        &["write:statuses", "read:notifications"],
    )
    .await;
    let favouriter_token = issue_test_token(&app, app_id, favouriter, &["write:favourites"]).await;

    let post = create_status(&router, &author_token, json!({"status": "notify me"})).await;
    let post_id = post["id"]
        .as_str()
        .expect("post id must be a string")
        .to_string();

    let (status, favourited) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/statuses/{post_id}/favourite"),
            Some(&favouriter_token),
            None,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "favourite must succeed: {favourited:?}"
    );

    // Requirements 5.1, 5.2, 5.4, 5.5, 6.1: the favourite event reached the
    // single generation point through this task's own wiring and produced a
    // persisted, retrievable notification for the post's author.
    let (status, list) = send(
        &router,
        req("GET", "/api/v1/notifications", Some(&author_token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {list:?}");
    let items = list
        .as_array()
        .expect("notification list must be a JSON array");
    assert_eq!(
        items.len(),
        1,
        "exactly one notification must have been generated: {items:?}"
    );
    assert_eq!(items[0]["type"], "favourite");
    assert_eq!(items[0]["account"]["id"], favouriter.as_i64().to_string());

    // Requirement 2.3: `account_id` narrows through the real
    // ActorDirectory/RemoteAccountRepository resolution this task injects —
    // the favouriter's own real id resolves and still matches the same
    // notification.
    let (status, filtered) = send(
        &router,
        req(
            "GET",
            &format!("/api/v1/notifications?account_id={}", favouriter.as_i64()),
            Some(&author_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {filtered:?}");
    assert_eq!(
        filtered
            .as_array()
            .expect("notification list must be a JSON array")
            .len(),
        1
    );

    // Requirement 2.3's other half: an `account_id` that resolves to
    // neither a local actor nor a known remote account is 200 + [], never
    // 404, and `NotificationRepository::list` is never even reached.
    let unknown_id = favouriter.as_i64() + 1_000_000_000;
    let (status, empty) = send(
        &router,
        req(
            "GET",
            &format!("/api/v1/notifications?account_id={unknown_id}"),
            Some(&author_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {empty:?}");
    assert_eq!(
        empty
            .as_array()
            .expect("notification list must be a JSON array"),
        &Vec::<Value>::new(),
        "an unresolvable account_id must be 200 + [], never 404"
    );

    app.cleanup().await;
}
