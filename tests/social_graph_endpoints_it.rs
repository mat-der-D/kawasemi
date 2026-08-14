//! Router-level integration tests for `social_graph::endpoints`'s nine
//! handlers (Requirements 1.8, 2.7, 4.6, 5.6, 10.1, 10.2, 10.4, 10.5),
//! driven through a real, test-only axum `Router` dispatched via
//! `tower::ServiceExt::oneshot` against a real, `spawn_test_app`-backed
//! Postgres schema -- mirrors `kawasemi::oauth::middleware`'s own
//! established "real router, real DB, no mocked auth" precedent, and reuses
//! the `build_service`/`RecordingSink`/real-actor-row fixture conventions of
//! the social-graph service tests for constructing the four business
//! services this module's handlers close over.
//!
//! Relocated from `src/social_graph/endpoints/tests.rs` (spec
//! `test-placement-migration`, task 5.1): every verification here needs a
//! real running instance, so it belongs under `tests/` per steering
//! `structure.md`'s test-layout rule. The pure wire-shape helper unit tests
//! (`parse_follow_options` / `parse_mute_options` / `parse_optional_limit`)
//! stay at the unit-test position, where they need no instance at all.
//!
//! Coverage: a success path for each of the nine endpoints, a missing-Bearer
//! 401 and an insufficient-scope 403 (one representative case each, since
//! every handler shares the identical `RequiredActor`/`require_scope`
//! machinery under test), a target-not-found 404 (one representative case,
//! since every handler shares the identical service-layer `resolve_target`
//! 404 shape), the `follow_requests` list's `Link` header + Account JSON
//! shape (Requirement 10.4), and the self-follow 422 idempotency/edge case
//! (Requirement 1.7, confirming the endpoint surfaces the service's own
//! guarantee correctly -- not re-testing the service's own logic).

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::routing::{get, post};
use tower::ServiceExt;

use kawasemi::actor::owner::create_owner;
use kawasemi::actor::repository::insert_actor;
use kawasemi::actor::{ActorDirectory, ActorState, ActorType, Handle};
use kawasemi::domain::Id;
use kawasemi::error::AppError;
use kawasemi::federation::outbound::target::RecipientTargetResolver;
use kawasemi::federation::{CanonicalActivity, DeliveryService, DeliverySink, DeliveryTarget};
use kawasemi::oauth::app_repository::{self, NewApp};
use kawasemi::oauth::middleware::AuthState;
use kawasemi::oauth::model::ScopeSet as ModelScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::runtime::SeqIdGenerator;
use kawasemi::social_graph::activity_builder::{ActivityBuilder, PgRemoteActorLookup};
use kawasemi::social_graph::block_service::BlockService;
use kawasemi::social_graph::endpoints::{
    SocialGraphEndpointsState, authorize_follow_request, block, follow, list_follow_requests, mute,
    reject_follow_request, unblock, unfollow, unmute,
};
use kawasemi::social_graph::follow_request_service::FollowRequestService;
use kawasemi::social_graph::follow_service::FollowService;
use kawasemi::social_graph::mute_service::MuteService;
use kawasemi::social_graph::transitions::Transitions;
use kawasemi::statuses::notification_sink::NotificationSinkRegistry;
use kawasemi::test_harness::{TestApp, spawn_test_app};

// --- Fixtures ----------------------------------------------------------

/// Creates a real owner + local actor row, returning the actor's `Id` --
/// mirrors `follow_service/tests.rs::create_test_actor` exactly.
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
        display_name: "Test Actor".to_string(),
        summary: "a test actor".to_string(),
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

fn sample_remote_account(
    id: Id,
    actor_uri: &str,
    username: &str,
    fetched_at: time::OffsetDateTime,
) -> kawasemi::accounts::model::RemoteAccount {
    kawasemi::accounts::model::RemoteAccount {
        id,
        actor_uri: actor_uri.to_string(),
        username: username.to_string(),
        domain: "remote.example".to_string(),
        display_name: "Remote Requester".to_string(),
        note: String::new(),
        url: actor_uri.to_string(),
        avatar_url: None,
        header_url: None,
        fields: Vec::<kawasemi::accounts::model::ProfileField>::new(),
        bot: false,
        locked: false,
        fetched_at,
    }
}

/// Creates a real `remote_accounts` row, returning its `Id` -- mirrors
/// `tests/social_graph_follow_request_service_it.rs::create_test_remote`.
/// `promote_pending`/
/// `drop_pending` (task 3.2, `transitions.rs`'s own documented convention)
/// derive the pending row's `FollowRequestDirection` from the `requester`'s
/// own `AccountRef` variant (`Remote` -> `Inbound`) rather than accepting it
/// as a parameter, so every inbound-pending fixture in this file must use a
/// *remote* requester to match the only direction/requester-kind
/// combination `FollowRequestService::authorize_request`/`reject_request`
/// can ever actually consume in production.
async fn create_test_remote(app: &TestApp, actor_uri: &str, username: &str) -> Id {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    kawasemi::accounts::remote_repository::upsert_remote(
        &app.pool,
        &sample_remote_account(id, actor_uri, username, now),
    )
    .await
    .expect("upsert_remote must succeed");
    id
}

/// A no-op `DeliverySink` double -- these tests only need real delivery
/// dispatch to *succeed* (Follow/Undo/Accept/Reject/Block delivery is
/// already proven by the task-3.x service tests), never inspect what was
/// dispatched, so a bare counting stub is enough (unlike
/// `follow_service/tests.rs::RecordingSink`, which callers there inspect).
struct NoopSink {
    calls: Mutex<usize>,
}

impl NoopSink {
    fn new() -> Self {
        Self {
            calls: Mutex::new(0),
        }
    }
}

impl DeliverySink for NoopSink {
    async fn dispatch(
        &self,
        _target: DeliveryTarget,
        _activity: &CanonicalActivity,
        _sender: &Handle,
    ) -> Result<(), AppError> {
        *self.calls.lock().unwrap() += 1;
        Ok(())
    }
}

/// The shared handle every service in [`build_state`] receives -- a
/// test-local newtype around `Arc<NoopSink>` that simply forwards
/// `dispatch` to the inner [`NoopSink`].
///
/// The unit-test-position original wrote `impl DeliverySink for
/// Arc<NoopSink>` directly. From this integration-test crate that is an
/// orphan-rule violation (E0117): both `DeliverySink` and `Arc` are foreign
/// here, and `Arc` is not a fundamental type, so only a local type can carry
/// the impl. Wrapping is the smallest change that keeps the double's
/// behavior -- one shared, cloneable counting sink per service -- exactly as
/// it was; nothing about the visibility of any production item is involved.
#[derive(Clone)]
struct SharedNoopSink(Arc<NoopSink>);

impl DeliverySink for SharedNoopSink {
    async fn dispatch(
        &self,
        target: DeliveryTarget,
        activity: &CanonicalActivity,
        sender: &Handle,
    ) -> Result<(), AppError> {
        self.0.dispatch(target, activity, sender).await
    }
}

type TestState = SocialGraphEndpointsState<
    ActorDirectory,
    PgRemoteActorLookup,
    ActorDirectory,
    SharedNoopSink,
    SharedNoopSink,
>;

/// Builds a real [`SocialGraphEndpointsState`] bound to `app`'s isolated
/// schema -- mirrors `follow_service/tests.rs::build_service` (and its
/// siblings') established construction shape for every task-3.x service at
/// once, plus [`AuthState`] (this module's own addition, needed for the
/// `RequiredActor` extraction none of the task-3.x service tests exercise).
fn build_state(app: &TestApp) -> TestState {
    let local_sink = SharedNoopSink(Arc::new(NoopSink::new()));
    let http_sink = SharedNoopSink(Arc::new(NoopSink::new()));
    let delivery = Arc::new(DeliveryService::new(
        RecipientTargetResolver::new(ActorDirectory::new(app.pool.clone())),
        local_sink.clone(),
        http_sink.clone(),
    ));
    let ids = Arc::new(SeqIdGenerator::new(120_000)) as Arc<dyn kawasemi::runtime::IdGenerator>;
    // `ActivityBuilder` is not `Clone` -- build one independent instance per
    // service, mirroring `follow_service/tests.rs` et al.'s own established
    // pattern of never sharing one `ActivityBuilder` value across services.
    let build_activity_builder = || {
        ActivityBuilder::new(
            kawasemi::federation::urls::ActorUrls::new("kawasemi.example"),
            Arc::clone(&ids),
            ActorDirectory::new(app.pool.clone()),
            PgRemoteActorLookup::new(app.pool.clone()),
        )
    };
    let transitions = || {
        Transitions::new(
            app.pool.clone(),
            app.runtime.clone(),
            NotificationSinkRegistry::new(),
        )
    };

    let follow = FollowService::new(
        app.pool.clone(),
        app.runtime.clone(),
        ActorDirectory::new(app.pool.clone()),
        build_activity_builder(),
        transitions(),
        Arc::clone(&delivery),
    );
    let follow_requests = FollowRequestService::new(
        app.pool.clone(),
        app.runtime.clone(),
        ActorDirectory::new(app.pool.clone()),
        build_activity_builder(),
        transitions(),
        Arc::clone(&delivery),
        app.state.accounts().service(),
        app.state.config().server.domain.clone(),
    );
    let mute = MuteService::new(
        app.pool.clone(),
        app.runtime.clone(),
        ActorDirectory::new(app.pool.clone()),
    );
    let block = BlockService::new(
        app.pool.clone(),
        app.runtime.clone(),
        ActorDirectory::new(app.pool.clone()),
        build_activity_builder(),
        transitions(),
        Arc::clone(&delivery),
    );

    let auth = AuthState {
        pool: app.pool.clone(),
        token_hash_key: app.state.config().oauth.token_hash_key.clone(),
    };

    SocialGraphEndpointsState {
        follow: Arc::new(follow),
        follow_requests: Arc::new(follow_requests),
        mute: Arc::new(mute),
        block: Arc::new(block),
        auth,
    }
}

/// Mounts this module's nine handlers onto a test-only router bound to
/// `state` -- the concrete monomorphization every test in this file drives
/// requests through via `tower::ServiceExt::oneshot`.
fn build_router(state: TestState) -> Router {
    Router::new()
        .route(
            "/api/v1/accounts/{id}/follow",
            post(
                follow::<
                    ActorDirectory,
                    PgRemoteActorLookup,
                    ActorDirectory,
                    SharedNoopSink,
                    SharedNoopSink,
                >,
            ),
        )
        .route(
            "/api/v1/accounts/{id}/unfollow",
            post(
                unfollow::<
                    ActorDirectory,
                    PgRemoteActorLookup,
                    ActorDirectory,
                    SharedNoopSink,
                    SharedNoopSink,
                >,
            ),
        )
        .route(
            "/api/v1/follow_requests",
            get(list_follow_requests::<
                ActorDirectory,
                PgRemoteActorLookup,
                ActorDirectory,
                SharedNoopSink,
                SharedNoopSink,
            >),
        )
        .route(
            "/api/v1/follow_requests/{id}/authorize",
            post(
                authorize_follow_request::<
                    ActorDirectory,
                    PgRemoteActorLookup,
                    ActorDirectory,
                    SharedNoopSink,
                    SharedNoopSink,
                >,
            ),
        )
        .route(
            "/api/v1/follow_requests/{id}/reject",
            post(
                reject_follow_request::<
                    ActorDirectory,
                    PgRemoteActorLookup,
                    ActorDirectory,
                    SharedNoopSink,
                    SharedNoopSink,
                >,
            ),
        )
        .route(
            "/api/v1/accounts/{id}/mute",
            post(
                mute::<
                    ActorDirectory,
                    PgRemoteActorLookup,
                    ActorDirectory,
                    SharedNoopSink,
                    SharedNoopSink,
                >,
            ),
        )
        .route(
            "/api/v1/accounts/{id}/unmute",
            post(
                unmute::<
                    ActorDirectory,
                    PgRemoteActorLookup,
                    ActorDirectory,
                    SharedNoopSink,
                    SharedNoopSink,
                >,
            ),
        )
        .route(
            "/api/v1/accounts/{id}/block",
            post(
                block::<
                    ActorDirectory,
                    PgRemoteActorLookup,
                    ActorDirectory,
                    SharedNoopSink,
                    SharedNoopSink,
                >,
            ),
        )
        .route(
            "/api/v1/accounts/{id}/unblock",
            post(
                unblock::<
                    ActorDirectory,
                    PgRemoteActorLookup,
                    ActorDirectory,
                    SharedNoopSink,
                    SharedNoopSink,
                >,
            ),
        )
        .with_state(state)
}

/// Registers a real `oauth_applications` row, returning its `Id` --
/// mirrors `tests/oauth_middleware_it.rs::register_test_app`.
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
            name: "Social Graph Endpoints Test Client".to_string(),
            redirect_uris: vec!["https://client.example/callback".to_string()],
            scopes: ModelScopeSet::new(["read", "write", "follow"]),
        },
    )
    .await
    .expect("register_app must succeed");
    registered.id
}

/// Issues a real access token bound to `actor_id` with `scopes`, returning
/// its plaintext bearer value -- mirrors
/// `tests/oauth_middleware_it.rs::issue_test_token`. Never hand-constructs a
/// `RequestActorContext`: every token these tests present was actually
/// persisted and hashed through `token_repository::issue_token`.
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

async fn bearer_request(
    router: &Router,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<&str>,
) -> axum::http::Response<Body> {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let request = match body {
        Some(body) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .expect("building the test request must succeed"),
        None => builder
            .body(Body::empty())
            .expect("building the test request must succeed"),
    };
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

fn as_bool(value: &serde_json::Value, key: &str) -> bool {
    value
        .get(key)
        .unwrap_or_else(|| panic!("relationship JSON missing '{key}'"))
        .as_bool()
        .unwrap_or_else(|| panic!("relationship JSON '{key}' was not a bool"))
}

// --- follow --------------------------------------------------------------

#[tokio::test]
async fn follow_succeeds_with_follow_scope_and_returns_relationship() {
    let app = spawn_test_app().await;
    let state = build_state(&app);
    let router = build_router(state);

    let app_id = register_test_app(&app).await;
    let viewer = create_test_actor(&app, "ep_follower1").await;
    let target = create_test_actor(&app, "ep_target1").await;
    let token = issue_test_token(&app, app_id, viewer, &["follow"]).await;

    let response = bearer_request(
        &router,
        "POST",
        &format!("/api/v1/accounts/{}/follow", target.as_i64()),
        Some(&token),
        None,
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert!(as_bool(&body, "following"));

    app.cleanup().await;
}

#[tokio::test]
async fn follow_without_a_bearer_token_is_401() {
    let app = spawn_test_app().await;
    let state = build_state(&app);
    let router = build_router(state);

    let target = create_test_actor(&app, "ep_target_noauth").await;

    let response = bearer_request(
        &router,
        "POST",
        &format!("/api/v1/accounts/{}/follow", target.as_i64()),
        None,
        None,
    )
    .await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = body_json(response).await;
    assert!(
        body.get("error").is_some(),
        "must be a mastodon-compatible error body"
    );

    app.cleanup().await;
}

#[tokio::test]
async fn follow_with_insufficient_scope_is_403() {
    let app = spawn_test_app().await;
    let state = build_state(&app);
    let router = build_router(state);

    let app_id = register_test_app(&app).await;
    let viewer = create_test_actor(&app, "ep_follower_scopeless").await;
    let target = create_test_actor(&app, "ep_target_scopeless").await;
    // `read:accounts` satisfies neither `follow` nor any of its granular
    // children -- Requirement 1.8/10.1's 403 case.
    let token = issue_test_token(&app, app_id, viewer, &["read:accounts"]).await;

    let response = bearer_request(
        &router,
        "POST",
        &format!("/api/v1/accounts/{}/follow", target.as_i64()),
        Some(&token),
        None,
    )
    .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    app.cleanup().await;
}

#[tokio::test]
async fn follow_of_a_nonexistent_account_is_404() {
    let app = spawn_test_app().await;
    let state = build_state(&app);
    let router = build_router(state);

    let app_id = register_test_app(&app).await;
    let viewer = create_test_actor(&app, "ep_follower_404").await;
    let token = issue_test_token(&app, app_id, viewer, &["follow"]).await;

    let response = bearer_request(
        &router,
        "POST",
        "/api/v1/accounts/999999999/follow",
        Some(&token),
        None,
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    app.cleanup().await;
}

#[tokio::test]
async fn follow_applies_a_json_body_options_override() {
    let app = spawn_test_app().await;
    let state = build_state(&app);
    let router = build_router(state);

    let app_id = register_test_app(&app).await;
    let viewer = create_test_actor(&app, "ep_follower_opts").await;
    let target = create_test_actor(&app, "ep_target_opts").await;
    let token = issue_test_token(&app, app_id, viewer, &["follow"]).await;

    let response = bearer_request(
        &router,
        "POST",
        &format!("/api/v1/accounts/{}/follow", target.as_i64()),
        Some(&token),
        Some(r#"{"reblogs": false, "notify": true, "languages": ["en"]}"#),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert!(as_bool(&body, "following"));
    assert!(!as_bool(&body, "showing_reblogs"));
    assert!(as_bool(&body, "notifying"));

    app.cleanup().await;
}

#[tokio::test]
async fn follow_succeeds_with_write_follows_scope_and_returns_relationship() {
    // `write:follows` alone (no `follow`) must also satisfy `follow`'s scope
    // requirement -- requirements.md 10.1's literal "`follow` または
    // `write:follows`" wording for follow/unfollow/mute/unmute/block/unblock.
    let app = spawn_test_app().await;
    let state = build_state(&app);
    let router = build_router(state);

    let app_id = register_test_app(&app).await;
    let viewer = create_test_actor(&app, "ep_follower_wf").await;
    let target = create_test_actor(&app, "ep_target_wf").await;
    let token = issue_test_token(&app, app_id, viewer, &["write:follows"]).await;

    let response = bearer_request(
        &router,
        "POST",
        &format!("/api/v1/accounts/{}/follow", target.as_i64()),
        Some(&token),
        None,
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert!(as_bool(&body, "following"));

    app.cleanup().await;
}

#[tokio::test]
async fn follow_self_is_422_unprocessable_entity() {
    // Requirement 1.7's self-follow rejection, surfaced correctly through
    // the endpoint layer (the underlying rejection itself is
    // `FollowService::follow`'s own already-tested behavior; this only
    // confirms the endpoint does not swallow or remap it).
    let app = spawn_test_app().await;
    let state = build_state(&app);
    let router = build_router(state);

    let app_id = register_test_app(&app).await;
    let viewer = create_test_actor(&app, "ep_self_follower").await;
    let token = issue_test_token(&app, app_id, viewer, &["follow"]).await;

    let response = bearer_request(
        &router,
        "POST",
        &format!("/api/v1/accounts/{}/follow", viewer.as_i64()),
        Some(&token),
        None,
    )
    .await;

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

    app.cleanup().await;
}

// --- unfollow --------------------------------------------------------------

#[tokio::test]
async fn unfollow_succeeds_and_returns_relationship() {
    let app = spawn_test_app().await;
    let state = build_state(&app);
    let router = build_router(state);

    let app_id = register_test_app(&app).await;
    let viewer = create_test_actor(&app, "ep_unfollower1").await;
    let target = create_test_actor(&app, "ep_unfollow_target1").await;
    let token = issue_test_token(&app, app_id, viewer, &["follow"]).await;

    let follow_response = bearer_request(
        &router,
        "POST",
        &format!("/api/v1/accounts/{}/follow", target.as_i64()),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(follow_response.status(), StatusCode::OK);

    let response = bearer_request(
        &router,
        "POST",
        &format!("/api/v1/accounts/{}/unfollow", target.as_i64()),
        Some(&token),
        None,
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert!(!as_bool(&body, "following"));

    app.cleanup().await;
}

#[tokio::test]
async fn unfollow_succeeds_with_write_follows_scope_and_returns_relationship() {
    // `write:follows` alone (no `follow`) must also satisfy `unfollow`'s
    // scope requirement -- requirements.md 10.1.
    let app = spawn_test_app().await;
    let state = build_state(&app);
    let router = build_router(state);

    let app_id = register_test_app(&app).await;
    let viewer = create_test_actor(&app, "ep_unfollower_wf").await;
    let target = create_test_actor(&app, "ep_unfollow_target_wf").await;
    let follow_token = issue_test_token(&app, app_id, viewer, &["follow"]).await;
    let unfollow_token = issue_test_token(&app, app_id, viewer, &["write:follows"]).await;

    let follow_response = bearer_request(
        &router,
        "POST",
        &format!("/api/v1/accounts/{}/follow", target.as_i64()),
        Some(&follow_token),
        None,
    )
    .await;
    assert_eq!(follow_response.status(), StatusCode::OK);

    let response = bearer_request(
        &router,
        "POST",
        &format!("/api/v1/accounts/{}/unfollow", target.as_i64()),
        Some(&unfollow_token),
        None,
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert!(!as_bool(&body, "following"));

    app.cleanup().await;
}

// --- follow_requests ---------------------------------------------------

#[tokio::test]
async fn list_follow_requests_requires_read_follows_scope() {
    let app = spawn_test_app().await;
    let state = build_state(&app);
    let router = build_router(state);

    let app_id = register_test_app(&app).await;
    let owner = create_test_actor(&app, "ep_owner_scope").await;
    // `read:accounts` satisfies neither `follow` nor `read:follows` -- a
    // genuinely insufficient scope (mirrors `follow_with_insufficient_scope_is_403`'s
    // own established negative-scope literal in this same file). `follow`
    // itself is *not* a valid negative case here any more: requirements.md
    // 10.1's general "`follow` または ... `read:follows` 相当" pattern applies
    // to `follow_requests` too (see [`list_follow_requests_succeeds_with_follow_scope`]).
    let token = issue_test_token(&app, app_id, owner, &["read:accounts"]).await;

    let response = bearer_request(
        &router,
        "GET",
        "/api/v1/follow_requests",
        Some(&token),
        None,
    )
    .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    app.cleanup().await;
}

#[tokio::test]
async fn list_follow_requests_succeeds_with_follow_scope() {
    // `follow` alone (no `read:follows`) must also satisfy
    // `list_follow_requests`'s scope requirement -- requirements.md 10.1's
    // general "follow / ... / follow_requests 操作に対し...スコープ（`follow`
    // または ... `read:follows` 相当）" pattern; Requirement 2.7 only narrows
    // authorize/reject to `follow`-only, not this list endpoint. Mirrors
    // `list_follow_requests_returns_account_json_with_link_header`'s own
    // Account JSON + `Link` header assertions, adapted for a `follow`-scoped
    // token.
    let app = spawn_test_app().await;
    let state = build_state(&app);
    let router = build_router(state);

    let app_id = register_test_app(&app).await;
    let owner = create_test_actor(&app, "ep_owner_reqs_follow_scope").await;
    let requester = create_test_remote(
        &app,
        "https://remote.example/users/ep_reqs_follow_scope",
        "ep_reqs_follow_scope",
    )
    .await;
    let token = issue_test_token(&app, app_id, owner, &["follow"]).await;

    let transitions = Transitions::new(
        app.pool.clone(),
        app.runtime.clone(),
        NotificationSinkRegistry::new(),
    );
    let req = kawasemi::social_graph::model::FollowRequest {
        requester: kawasemi::domain::AccountRef::Remote(requester),
        target: kawasemi::domain::AccountRef::Local(owner),
        direction: kawasemi::social_graph::model::FollowRequestDirection::Inbound,
        activity_id: "https://kawasemi.example/activities/follow/ep_follow_scope".to_string(),
        created_at: app.runtime.clock.now(),
    };
    transitions
        .record_pending(&req)
        .await
        .expect("record_pending must succeed");

    let response = bearer_request(
        &router,
        "GET",
        "/api/v1/follow_requests?limit=1",
        Some(&token),
        None,
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let link_header = response
        .headers()
        .get(header::LINK)
        .map(|value| value.to_str().unwrap().to_string());

    let body = body_json(response).await;
    let items = body.as_array().expect("body must be a JSON array");
    assert_eq!(items.len(), 1);
    assert_eq!(
        items[0].get("id").and_then(|v| v.as_str()),
        Some(requester.as_i64().to_string()).as_deref()
    );
    assert!(
        link_header.is_some_and(|link| link.contains("max_id")),
        "a full page must carry a next-page Link header"
    );

    app.cleanup().await;
}

#[tokio::test]
async fn list_follow_requests_returns_account_json_with_link_header() {
    let app = spawn_test_app().await;
    let state = build_state(&app);
    let router = build_router(state);

    let app_id = register_test_app(&app).await;
    let owner = create_test_actor(&app, "ep_owner_reqs").await;
    let requester =
        create_test_remote(&app, "https://remote.example/users/ep_reqs", "ep_reqs").await;
    let token = issue_test_token(&app, app_id, owner, &["read:follows"]).await;

    // Seed a real pending inbound request directly via `Transitions`
    // (mirrors `tests/social_graph_follow_request_service_it.rs`'s own
    // established fixture
    // technique -- there is no live inbound handler yet to produce this
    // state through the wire). The requester must be `Remote` -- see
    // `create_test_remote`'s own doc comment.
    let transitions = Transitions::new(
        app.pool.clone(),
        app.runtime.clone(),
        NotificationSinkRegistry::new(),
    );
    let req = kawasemi::social_graph::model::FollowRequest {
        requester: kawasemi::domain::AccountRef::Remote(requester),
        target: kawasemi::domain::AccountRef::Local(owner),
        direction: kawasemi::social_graph::model::FollowRequestDirection::Inbound,
        activity_id: "https://kawasemi.example/activities/follow/ep1".to_string(),
        created_at: app.runtime.clock.now(),
    };
    transitions
        .record_pending(&req)
        .await
        .expect("record_pending must succeed");

    let response = bearer_request(
        &router,
        "GET",
        "/api/v1/follow_requests?limit=1",
        Some(&token),
        None,
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let link_header = response
        .headers()
        .get(header::LINK)
        .map(|value| value.to_str().unwrap().to_string());

    let body = body_json(response).await;
    let items = body.as_array().expect("body must be a JSON array");
    assert_eq!(items.len(), 1);
    assert_eq!(
        items[0].get("id").and_then(|v| v.as_str()),
        Some(requester.as_i64().to_string()).as_deref()
    );
    assert!(
        link_header.is_some_and(|link| link.contains("max_id")),
        "a full page must carry a next-page Link header"
    );

    app.cleanup().await;
}

// --- authorize / reject -------------------------------------------------

#[tokio::test]
async fn authorize_follow_request_establishes_a_follow() {
    let app = spawn_test_app().await;
    let state = build_state(&app);
    let router = build_router(state);

    let app_id = register_test_app(&app).await;
    let owner = create_test_actor(&app, "ep_owner_authz").await;
    let requester =
        create_test_remote(&app, "https://remote.example/users/ep_authz", "ep_authz").await;
    let token = issue_test_token(&app, app_id, owner, &["follow"]).await;

    let transitions = Transitions::new(
        app.pool.clone(),
        app.runtime.clone(),
        NotificationSinkRegistry::new(),
    );
    let req = kawasemi::social_graph::model::FollowRequest {
        requester: kawasemi::domain::AccountRef::Remote(requester),
        target: kawasemi::domain::AccountRef::Local(owner),
        direction: kawasemi::social_graph::model::FollowRequestDirection::Inbound,
        activity_id: "https://kawasemi.example/activities/follow/ep2".to_string(),
        created_at: app.runtime.clock.now(),
    };
    transitions
        .record_pending(&req)
        .await
        .expect("record_pending must succeed");

    let response = bearer_request(
        &router,
        "POST",
        &format!("/api/v1/follow_requests/{}/authorize", requester.as_i64()),
        Some(&token),
        None,
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert!(as_bool(&body, "followed_by"));

    app.cleanup().await;
}

#[tokio::test]
async fn reject_follow_request_drops_the_pending_request() {
    let app = spawn_test_app().await;
    let state = build_state(&app);
    let router = build_router(state);

    let app_id = register_test_app(&app).await;
    let owner = create_test_actor(&app, "ep_owner_reject").await;
    let requester =
        create_test_remote(&app, "https://remote.example/users/ep_reject", "ep_reject").await;
    let token = issue_test_token(&app, app_id, owner, &["follow"]).await;

    let transitions = Transitions::new(
        app.pool.clone(),
        app.runtime.clone(),
        NotificationSinkRegistry::new(),
    );
    let req = kawasemi::social_graph::model::FollowRequest {
        requester: kawasemi::domain::AccountRef::Remote(requester),
        target: kawasemi::domain::AccountRef::Local(owner),
        direction: kawasemi::social_graph::model::FollowRequestDirection::Inbound,
        activity_id: "https://kawasemi.example/activities/follow/ep3".to_string(),
        created_at: app.runtime.clock.now(),
    };
    transitions
        .record_pending(&req)
        .await
        .expect("record_pending must succeed");

    let response = bearer_request(
        &router,
        "POST",
        &format!("/api/v1/follow_requests/{}/reject", requester.as_i64()),
        Some(&token),
        None,
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert!(!as_bool(&body, "followed_by"));
    assert!(!as_bool(&body, "requested_by"));

    app.cleanup().await;
}

#[tokio::test]
async fn authorize_follow_request_with_no_pending_request_is_404() {
    let app = spawn_test_app().await;
    let state = build_state(&app);
    let router = build_router(state);

    let app_id = register_test_app(&app).await;
    let owner = create_test_actor(&app, "ep_owner_authz_404").await;
    let requester = create_test_actor(&app, "ep_requester_authz_404").await;
    let token = issue_test_token(&app, app_id, owner, &["follow"]).await;

    let response = bearer_request(
        &router,
        "POST",
        &format!("/api/v1/follow_requests/{}/authorize", requester.as_i64()),
        Some(&token),
        None,
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    app.cleanup().await;
}

// --- mute / unmute -------------------------------------------------------

#[tokio::test]
async fn mute_succeeds_with_write_mutes_scope_and_returns_relationship() {
    let app = spawn_test_app().await;
    let state = build_state(&app);
    let router = build_router(state);

    let app_id = register_test_app(&app).await;
    let viewer = create_test_actor(&app, "ep_muter1").await;
    let target = create_test_actor(&app, "ep_muted1").await;
    // `write:mutes` alone (no `follow`) must also satisfy `mute`'s scope
    // requirement -- Requirement 4.6/10.1's "or" case.
    let token = issue_test_token(&app, app_id, viewer, &["write:mutes"]).await;

    let response = bearer_request(
        &router,
        "POST",
        &format!("/api/v1/accounts/{}/mute", target.as_i64()),
        Some(&token),
        Some(r#"{"notifications": true, "duration": 3600}"#),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert!(as_bool(&body, "muting"));
    assert!(as_bool(&body, "muting_notifications"));

    app.cleanup().await;
}

#[tokio::test]
async fn mute_succeeds_with_write_follows_scope_and_returns_relationship() {
    // `write:follows` alone (no `follow`, no `write:mutes`) must also
    // satisfy `mute`'s scope requirement -- requirements.md 10.1's literal
    // "`follow` または `write:follows`" wording.
    let app = spawn_test_app().await;
    let state = build_state(&app);
    let router = build_router(state);

    let app_id = register_test_app(&app).await;
    let viewer = create_test_actor(&app, "ep_muter_wf").await;
    let target = create_test_actor(&app, "ep_muted_wf").await;
    let token = issue_test_token(&app, app_id, viewer, &["write:follows"]).await;

    let response = bearer_request(
        &router,
        "POST",
        &format!("/api/v1/accounts/{}/mute", target.as_i64()),
        Some(&token),
        None,
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert!(as_bool(&body, "muting"));

    app.cleanup().await;
}

#[tokio::test]
async fn unmute_succeeds_and_returns_relationship() {
    let app = spawn_test_app().await;
    let state = build_state(&app);
    let router = build_router(state);

    let app_id = register_test_app(&app).await;
    let viewer = create_test_actor(&app, "ep_unmuter1").await;
    let target = create_test_actor(&app, "ep_unmuted1").await;
    let token = issue_test_token(&app, app_id, viewer, &["follow"]).await;

    let mute_response = bearer_request(
        &router,
        "POST",
        &format!("/api/v1/accounts/{}/mute", target.as_i64()),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(mute_response.status(), StatusCode::OK);

    let response = bearer_request(
        &router,
        "POST",
        &format!("/api/v1/accounts/{}/unmute", target.as_i64()),
        Some(&token),
        None,
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert!(!as_bool(&body, "muting"));

    app.cleanup().await;
}

// --- block / unblock ------------------------------------------------------

#[tokio::test]
async fn block_succeeds_with_write_blocks_scope_and_returns_relationship() {
    let app = spawn_test_app().await;
    let state = build_state(&app);
    let router = build_router(state);

    let app_id = register_test_app(&app).await;
    let viewer = create_test_actor(&app, "ep_blocker1").await;
    let target = create_test_actor(&app, "ep_blocked1").await;
    // `write:blocks` alone (no `follow`) must also satisfy `block`'s scope
    // requirement -- Requirement 5.6/10.1's "or" case.
    let token = issue_test_token(&app, app_id, viewer, &["write:blocks"]).await;

    let response = bearer_request(
        &router,
        "POST",
        &format!("/api/v1/accounts/{}/block", target.as_i64()),
        Some(&token),
        None,
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert!(as_bool(&body, "blocking"));

    app.cleanup().await;
}

#[tokio::test]
async fn block_succeeds_with_write_follows_scope_and_returns_relationship() {
    // `write:follows` alone (no `follow`, no `write:blocks`) must also
    // satisfy `block`'s scope requirement -- requirements.md 10.1's literal
    // "`follow` または `write:follows`" wording.
    let app = spawn_test_app().await;
    let state = build_state(&app);
    let router = build_router(state);

    let app_id = register_test_app(&app).await;
    let viewer = create_test_actor(&app, "ep_blocker_wf").await;
    let target = create_test_actor(&app, "ep_blocked_wf").await;
    let token = issue_test_token(&app, app_id, viewer, &["write:follows"]).await;

    let response = bearer_request(
        &router,
        "POST",
        &format!("/api/v1/accounts/{}/block", target.as_i64()),
        Some(&token),
        None,
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert!(as_bool(&body, "blocking"));

    app.cleanup().await;
}

#[tokio::test]
async fn unblock_succeeds_and_returns_relationship() {
    let app = spawn_test_app().await;
    let state = build_state(&app);
    let router = build_router(state);

    let app_id = register_test_app(&app).await;
    let viewer = create_test_actor(&app, "ep_unblocker1").await;
    let target = create_test_actor(&app, "ep_unblocked1").await;
    let token = issue_test_token(&app, app_id, viewer, &["follow"]).await;

    let block_response = bearer_request(
        &router,
        "POST",
        &format!("/api/v1/accounts/{}/block", target.as_i64()),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(block_response.status(), StatusCode::OK);

    let response = bearer_request(
        &router,
        "POST",
        &format!("/api/v1/accounts/{}/unblock", target.as_i64()),
        Some(&token),
        None,
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert!(!as_bool(&body, "blocking"));

    app.cleanup().await;
}

#[tokio::test]
async fn block_of_a_nonexistent_account_is_404() {
    let app = spawn_test_app().await;
    let state = build_state(&app);
    let router = build_router(state);

    let app_id = register_test_app(&app).await;
    let viewer = create_test_actor(&app, "ep_blocker_404").await;
    let token = issue_test_token(&app, app_id, viewer, &["follow"]).await;

    let response = bearer_request(
        &router,
        "POST",
        "/api/v1/accounts/999999998/block",
        Some(&token),
        None,
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    app.cleanup().await;
}
