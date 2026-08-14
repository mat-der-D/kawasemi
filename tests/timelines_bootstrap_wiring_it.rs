//! Integration test proving timelines' task 5.2 own observable completion
//! condition (`.kiro/specs/timelines/tasks.md`, "5.2 モジュール配線と
//! Matcher シーム公開を行う", `_Boundary: TimelinesModule, server, bootstrap,
//! state_`): "アプリ起動時に home/public/tag のルートが有効になり、
//! `TimelineMatcher` が `AppState` から参照可能になる（起動・配線テストで
//! E2E にタイムライン取得が一気通貫で動く）" (Requirements 8.1, 8.3).
//!
//! Mirrors `tests/statuses_bootstrap_wiring_it.rs`'s/
//! `tests/accounts_module_wiring_it.rs`'s own established "prove the
//! composition-root wiring itself" precedent for a same-shaped task: drives
//! the real, fully-assembled router (`crate::server::build_router(app.state.clone())`,
//! the exact `AppState` `spawn_test_app` itself serves) in-process via
//! `tower::ServiceExt::oneshot`, rather than a test-local router the way
//! `tests/timelines_endpoints_handler_it.rs` (task 5.1) necessarily had to, since
//! nothing mounted `TimelinesModule` onto the real application until this
//! task.
//!
//! Four things proven here:
//!
//! 1. `GET /api/v1/timelines/home` unauthenticated returns `401` — proving
//!    the route is actually mounted on the live router (a not-yet-mounted
//!    route would 404, not 401) and that `TimelineEndpointsState`'s
//!    `AuthState` is correctly derived from the real `AppState` via
//!    `FromRef` (task 5.2's own `src/server.rs` addition), not merely that
//!    *some* router exists.
//! 2. `GET /api/v1/timelines/public` unauthenticated returns `200` with an
//!    empty array before anything is posted — proving the route is live and
//!    the whole `TimelineService::timeline` -> `TimelineFilter`/`FilterQuery`
//!    -> `paginate` chain runs without error against a freshly booted,
//!    empty database (Requirement 9.2's "未認証は公開のみ" doesn't error, it
//!    degrades).
//! 3. A real post created via `POST /api/v1/statuses` (statuses-core,
//!    already-mounted) becomes visible through both `GET
//!    /api/v1/timelines/home` (authenticated as its own author) and `GET
//!    /api/v1/timelines/public` (unauthenticated) — the full "一気通貫"
//!    (end-to-end) proof this task's own observable-completion text names:
//!    candidate query -> visibility/relationship filter -> hydration via the
//!    real `AccountService`/`LocalFsStore` handles `TimelinesModule` was
//!    wired with (`accounts_module.service()`/`media_module.store()`,
//!    `src/bootstrap.rs`'s own `build_timelines_module` call) -> Status JSON
//!    response, through the real mounted route.
//! 4. `TimelineMatcher` is reachable from `AppState` via
//!    `state.timelines().matcher()` and is directly usable (Requirement
//!    8.3) — a compile-time-plus-runtime check, sufficient per this task's
//!    own instruction ("a simple compile-time/unit check is enough since the
//!    type itself is a stateless unit struct").

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
use kawasemi::timelines::model::{TimelineKind, TimelineParams};

// ---- Fixture plumbing (each `tests/*.rs` file is its own compiled crate,
// so this deliberately duplicates `tests/statuses_bootstrap_wiring_it.rs`'s
// own established conventions rather than importing them — this crate's own
// documented convention). ----

/// Creates a real owner + a real local actor via `ActorService::create_actor`
/// (real RSA-2048 signing key provisioning), resolved back through
/// `ActorDirectory` — mirrors
/// `tests/statuses_bootstrap_wiring_it.rs::insert_actor_fixture` exactly.
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
            display_name: format!("Timelines Wiring IT {handle_str}"),
            summary: "an actor used by the timelines bootstrap-wiring integration test".to_string(),
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

/// Registers a throwaway OAuth app, using this instance's own real
/// `token_hash_key` — mirrors
/// `tests/statuses_bootstrap_wiring_it.rs::register_test_app` exactly.
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
            name: "Timelines Wiring IT Client".to_string(),
            redirect_uris: vec!["https://client.example/callback".to_string()],
            scopes: ModelScopeSet::new(["read", "write"]),
        },
    )
    .await
    .expect("register_app must succeed");
    registered.id
}

/// Issues a real access token bound to `actor_id` with `scopes` — mirrors
/// `tests/statuses_bootstrap_wiring_it.rs::issue_test_token` exactly.
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

/// Builds the real, fully-assembled router (`crate::server::build_router`)
/// from `app`'s own `AppState` — the exact same value `spawn_test_app` itself
/// serves over its bound TCP listener — so this file's requests observe the
/// real `TimelinesModule` wiring task 5.2 adds, not a test-local substitute
/// (unlike task 5.1's own `tests/timelines_endpoints_handler_it.rs`, which
/// necessarily built its own router since nothing mounted this module onto
/// the real one before this task).
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

/// Requirement 1.6/9.1: `home` is actually mounted (a `401`, not a `404`,
/// proves the route exists and Bearer/scope enforcement runs through the
/// real `AppState`-derived `AuthState`), and Requirement 9.2/5.2: `public`
/// is actually mounted and degrades to an empty, error-free `200` when
/// unauthenticated against a freshly booted (empty) database — both before
/// this task, neither route existed on the live router at all.
#[tokio::test]
async fn home_and_public_timeline_routes_are_live_on_the_real_router() {
    let app = spawn_test_app().await;
    let router = real_router(&app);

    let (status, body) = get_json(&router, "/api/v1/timelines/home", None).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "unauthenticated GET /api/v1/timelines/home must 401 (proving the route is mounted \
         and Bearer enforcement runs), not 404 (which would mean the route is still absent \
         from the live router): {body:?}"
    );

    let (status, body) = get_json(&router, "/api/v1/timelines/public", None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "unauthenticated GET /api/v1/timelines/public must succeed once task 5.2 mounts the \
         route: {body:?}"
    );
    assert_eq!(
        body.as_array().map(Vec::len),
        Some(0),
        "an empty, freshly booted database must yield an empty page, not an error: {body:?}"
    );

    app.cleanup().await;
}

/// Requirement 8.1/8.3's own "一気通貫" (end-to-end) proof: a real post
/// created through the already-mounted statuses-core route becomes visible
/// through the newly-mounted home (authenticated, as its own author) and
/// public (unauthenticated) timeline routes — exercising the complete
/// `TimelineService::timeline` pipeline (candidate query -> visibility/
/// relationship filter -> hydration via the real `AccountService`/
/// `LocalFsStore` handles `build_timelines_module` was wired with) through
/// the real, live router.
#[tokio::test]
async fn a_real_post_is_visible_through_the_live_home_and_public_timeline_routes() {
    let app = spawn_test_app().await;
    let router = real_router(&app);

    let alice = insert_actor_fixture(&app, "alice_tl_wiring").await;
    let app_id = register_test_app(&app).await;
    let token =
        issue_test_token(&app, app_id, alice.id, &["write:statuses", "read:statuses"]).await;

    let (status, body) = post_json(
        &router,
        "/api/v1/statuses",
        &token,
        serde_json::json!({ "status": "hello from the timelines wiring test" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "alice's post creation: {body:?}");
    let status_id = body["id"]
        .as_str()
        .expect("id must be a string")
        .to_string();

    let (status, body) = get_json(&router, "/api/v1/timelines/home", Some(&token)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "authenticated GET /api/v1/timelines/home: {body:?}"
    );
    let home_ids: Vec<&str> = body
        .as_array()
        .expect("home timeline body must be a JSON array")
        .iter()
        .map(|item| item["id"].as_str().expect("id must be a string"))
        .collect();
    assert!(
        home_ids.contains(&status_id.as_str()),
        "alice's own post must appear on her home timeline through the fully-wired stack: \
         {body:?}"
    );

    let (status, body) = get_json(&router, "/api/v1/timelines/public", None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "unauthenticated GET /api/v1/timelines/public: {body:?}"
    );
    let public_ids: Vec<&str> = body
        .as_array()
        .expect("public timeline body must be a JSON array")
        .iter()
        .map(|item| item["id"].as_str().expect("id must be a string"))
        .collect();
    assert!(
        public_ids.contains(&status_id.as_str()),
        "alice's public post must appear on the unauthenticated public timeline through the \
         fully-wired stack: {body:?}"
    );

    app.cleanup().await;
}

/// Requirement 4.1/4.4: `tag` is actually mounted and reachable end-to-end —
/// a real post carrying `#kawasemi` becomes visible through `GET
/// /api/v1/timelines/tag/kawasemi`, unauthenticated.
#[tokio::test]
async fn tag_timeline_route_is_live_and_returns_a_tagged_post() {
    let app = spawn_test_app().await;
    let router = real_router(&app);

    let alice = insert_actor_fixture(&app, "alice_tl_tag").await;
    let app_id = register_test_app(&app).await;
    let token =
        issue_test_token(&app, app_id, alice.id, &["write:statuses", "read:statuses"]).await;

    let (status, body) = post_json(
        &router,
        "/api/v1/statuses",
        &token,
        serde_json::json!({ "status": "tagged post #kawasemi" }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "alice's tagged post creation: {body:?}"
    );
    let status_id = body["id"]
        .as_str()
        .expect("id must be a string")
        .to_string();

    let (status, body) = get_json(&router, "/api/v1/timelines/tag/kawasemi", None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "unauthenticated GET /api/v1/timelines/tag/:hashtag must succeed once task 5.2 mounts \
         the route: {body:?}"
    );
    let tag_ids: Vec<&str> = body
        .as_array()
        .expect("tag timeline body must be a JSON array")
        .iter()
        .map(|item| item["id"].as_str().expect("id must be a string"))
        .collect();
    assert!(
        tag_ids.contains(&status_id.as_str()),
        "alice's tagged post must appear on the tag timeline through the fully-wired stack: \
         {body:?}"
    );

    app.cleanup().await;
}

/// Requirement 8.3: `TimelineMatcher` is reachable from `AppState` as the
/// public seam a downstream `streaming` spec reuses
/// (`state.timelines().matcher()`), and is directly usable — proven here by
/// actually invoking `candidate_spec` against the real, live `AppState`
/// (not merely asserting the accessor compiles). Per this task's own
/// instruction, a simple reachability/usability check is sufficient: the
/// type itself is a stateless `Copy` unit struct with no runtime behavior
/// beyond what `matcher.rs`'s own already-reviewed unit tests already prove.
#[tokio::test]
async fn timeline_matcher_is_reachable_and_usable_from_app_state() {
    let app = spawn_test_app().await;

    let matcher = app.state.timelines().matcher();
    let ctx = kawasemi::timelines::model::FilterContext {
        viewer: None,
        blocked: Default::default(),
        blocked_by: Default::default(),
        muted: Default::default(),
        following: Default::default(),
        reblogs_hidden: Default::default(),
        now: app.runtime.clock.now(),
    };
    let params = TimelineParams {
        local: false,
        remote: false,
        only_media: false,
        tag: None,
        page: Default::default(),
    };
    let spec = matcher.candidate_spec(TimelineKind::Public, &params, &ctx);
    assert_eq!(
        spec.kind,
        TimelineKind::Public,
        "the matcher reached through AppState must be the real, usable TimelineMatcher"
    );

    app.cleanup().await;
}
