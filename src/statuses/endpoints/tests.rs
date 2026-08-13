//! Tests for `endpoints.rs`.
//!
//! [`wire_shape_tests`] covers this module's own pure, DB/network-free
//! wire-shape helpers (`parse_visibility`/`parse_media_ids`/
//! `parse_optional_limit`/`parse_id`), mirroring
//! `accounts::endpoints::tests`'s established convention.
//!
//! The remainder of this file proves the router-level, auth/scope/response-
//! code/`Link`-header behavior this task's own observable-completion
//! condition names ("各エンドポイントが正しい応答コード（200/401/403/404/
//! 422）とスコープ検証で動作し、ブックマーク一覧に Link ヘッダが付く") —
//! against a real, `spawn_test_app`-backed Postgres schema, mirroring
//! `media::endpoints::tests`'s (`tests/media_endpoints_it.rs`'s) established
//! "test-local `Router` + `tower::ServiceExt::oneshot`" pattern for the same
//! "endpoint implemented, full application-router wiring not yet landed"
//! situation (task 7.2's job here). `StatusService`/`InteractionService`/
//! `PollService` are built the same way `status_service/tests.rs`/
//! `interaction_service/tests.rs`/`poll_service/tests.rs` already do (an
//! in-memory `ActorHandleLookup`/`LocalActorLookup`/`RelationshipQuery` plus
//! a `RecordingSink` capturing dispatched Activities) — the real
//! `AccountService`/`LocalFsStore` this crate's `AccountsModule`/
//! `MediaModule` already build for `TestApp` are reused unchanged for
//! account/media rendering, so `StatusRenderInput`'s `account`/
//! `media_attachments` fields are genuinely resolved through
//! already-reviewed code, not faked.

use std::collections::HashSet;
use std::sync::Mutex;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, header};
use axum::routing::{get, post};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::*;
use crate::actor::owner::create_owner;
use crate::actor::{ActorState, ActorType, Handle, NewActor, ResolvedActor};
use crate::config::Secret;
use crate::federation::outbound::target::{DeliveryTarget, RecipientTargetResolver};
use crate::federation::{ActorUrls, CanonicalActivity, DeliveryService};
use crate::oauth::app_repository::{self, NewApp};
use crate::oauth::hash::TokenHashKey;
use crate::oauth::model::ScopeSet as ModelScopeSet;
use crate::oauth::token_repository::{self, NewAccessToken};
use crate::runtime::{IdGenerator, SeqIdGenerator};
use crate::statuses::activity_builder::StatusActivityBuilder;
use crate::statuses::visibility::ViewerRelation;
use crate::test_harness::query_log::{QueryKind, record_queries};
use crate::test_harness::{TestApp, spawn_test_app};

mod wire_shape_tests {
    use super::*;

    #[test]
    fn parse_visibility_accepts_every_canonical_variant() {
        assert_eq!(parse_visibility("public").unwrap(), Visibility::Public);
        assert_eq!(parse_visibility("unlisted").unwrap(), Visibility::Unlisted);
        assert_eq!(parse_visibility("private").unwrap(), Visibility::Private);
        assert_eq!(parse_visibility("direct").unwrap(), Visibility::Direct);
    }

    #[test]
    fn parse_visibility_rejects_unknown_value() {
        let err = parse_visibility("bogus").expect_err("must be rejected");
        assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn parse_media_ids_parses_decimal_strings() {
        let ids = parse_media_ids(&["1".to_string(), "2".to_string()]).unwrap();
        assert_eq!(ids, vec![Id::from_i64(1), Id::from_i64(2)]);
    }

    #[test]
    fn parse_media_ids_rejects_a_non_numeric_value() {
        let err = parse_media_ids(&["abc".to_string()]).expect_err("must be rejected");
        assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn parse_optional_limit_is_none_when_absent() {
        assert_eq!(parse_optional_limit(None).unwrap(), None);
    }

    #[test]
    fn parse_optional_limit_rejects_a_non_numeric_value() {
        let err = parse_optional_limit(Some("abc")).expect_err("must be rejected");
        assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn parse_id_treats_an_unparseable_segment_as_404() {
        let err = parse_id("not-a-number").expect_err("must be rejected");
        assert_eq!(err.status, StatusCode::NOT_FOUND);
    }
}

// ---- Test doubles (mirrors `status_service/tests.rs`'s/
// `interaction_service/tests.rs`'s/`poll_service/tests.rs`'s identical,
// already-reviewed pattern; small duplication across sibling test modules
// is this crate's own documented convention). ----

#[derive(Clone)]
struct MockActorLookup {
    by_id: std::collections::HashMap<i64, Handle>,
    by_handle: std::collections::HashMap<String, Id>,
}

impl MockActorLookup {
    fn with_actors(pairs: &[(Id, &str)]) -> Self {
        let mut by_id = std::collections::HashMap::new();
        let mut by_handle = std::collections::HashMap::new();
        for (id, raw) in pairs {
            let handle = Handle::new(*raw).expect("valid test handle");
            by_id.insert(id.as_i64(), handle.clone());
            by_handle.insert((*raw).to_string(), *id);
        }
        Self { by_id, by_handle }
    }
}

impl ActorHandleLookup for MockActorLookup {
    async fn resolve_handle(&self, actor_id: Id) -> Result<Handle, AppError> {
        self.by_id.get(&actor_id.as_i64()).cloned().ok_or_else(|| {
            AppError::client(
                StatusCode::NOT_FOUND,
                format!("actor id {actor_id:?} not known to MockActorLookup"),
            )
        })
    }
}

impl MentionLookup for MockActorLookup {
    async fn resolve_local_handle(&self, handle: &Handle) -> Result<Option<Id>, AppError> {
        Ok(self.by_handle.get(handle.as_str()).copied())
    }
}

struct MockLocalActorLookup {
    known_handles: HashSet<String>,
}

impl MockLocalActorLookup {
    fn with_handles(handles: &[&str]) -> Self {
        Self {
            known_handles: handles.iter().map(|h| (*h).to_string()).collect(),
        }
    }
}

impl LocalActorLookup for MockLocalActorLookup {
    async fn resolve_actor_by_handle(
        &self,
        handle: &Handle,
    ) -> Result<Option<ResolvedActor>, AppError> {
        if self.known_handles.contains(handle.as_str()) {
            Ok(Some(ResolvedActor {
                id: Id::from_i64(1),
                handle: handle.clone(),
                actor_type: ActorType::Person,
                display_name: "Test Actor".to_string(),
                summary: String::new(),
                state: ActorState::Active,
            }))
        } else {
            Ok(None)
        }
    }
}

struct RecordingSink {
    calls: Mutex<Vec<(DeliveryTarget, CanonicalActivity, Handle)>>,
}

impl RecordingSink {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
        }
    }
}

impl DeliverySink for RecordingSink {
    async fn dispatch(
        &self,
        target: DeliveryTarget,
        activity: &CanonicalActivity,
        sender: &Handle,
    ) -> Result<(), AppError> {
        self.calls
            .lock()
            .unwrap()
            .push((target, activity.clone(), sender.clone()));
        Ok(())
    }
}

impl DeliverySink for Arc<RecordingSink> {
    async fn dispatch(
        &self,
        target: DeliveryTarget,
        activity: &CanonicalActivity,
        sender: &Handle,
    ) -> Result<(), AppError> {
        (**self).dispatch(target, activity, sender).await
    }
}

#[derive(Clone)]
struct MockRelationshipQuery {
    is_follower: bool,
    followers: Vec<crate::federation::Recipient>,
}

impl MockRelationshipQuery {
    fn new(is_follower: bool, followers: Vec<crate::federation::Recipient>) -> Self {
        Self {
            is_follower,
            followers,
        }
    }
}

impl RelationshipQuery for MockRelationshipQuery {
    async fn viewer_relation(
        &self,
        _author: Id,
        viewer: Option<Id>,
    ) -> Result<ViewerRelation, AppError> {
        Ok(ViewerRelation {
            is_follower: viewer.is_some() && self.is_follower,
        })
    }

    async fn followers_of(
        &self,
        _author: Id,
    ) -> Result<Vec<crate::federation::Recipient>, AppError> {
        Ok(self.followers.clone())
    }
}

type TestState = StatusesEndpointsState<
    MockActorLookup,
    MockLocalActorLookup,
    Arc<RecordingSink>,
    Arc<RecordingSink>,
    MockRelationshipQuery,
    MockActorLookup,
>;

fn test_token_hash_key() -> TokenHashKey {
    Secret::new([0x77; 32])
}

/// Builds a real, ready-to-mount [`TestState`] plus the two `RecordingSink`s
/// its embedded `StatusActivityBuilder`s dispatch through. `known_actors`
/// pre-registers every `(Id, handle)` this test needs `ActorHandleLookup`/
/// `LocalActorLookup` to resolve. `is_follower` controls
/// `MockRelationshipQuery`'s `private`-visibility answer.
fn build_state(
    app: &TestApp,
    known_actors: &[(Id, &str)],
    is_follower: bool,
) -> (TestState, Arc<RecordingSink>, Arc<RecordingSink>) {
    let handles: Vec<&str> = known_actors.iter().map(|(_, h)| *h).collect();
    let local_sink = Arc::new(RecordingSink::new());
    let http_sink = Arc::new(RecordingSink::new());
    let urls = ActorUrls::new("kawasemi.example");
    let ids = Arc::new(SeqIdGenerator::new(500_000)) as Arc<dyn IdGenerator>;

    let followers = known_actors
        .first()
        .map(|(_, handle)| {
            vec![crate::federation::Recipient::Local(
                Handle::new(*handle).unwrap(),
            )]
        })
        .unwrap_or_default();
    let relationship = MockRelationshipQuery::new(is_follower, followers);
    let actor_lookup = MockActorLookup::with_actors(known_actors);

    let status_service = Arc::new(StatusService::new(
        app.pool.clone(),
        app.runtime.clone(),
        "kawasemi.example",
        urls.clone(),
        StatusActivityBuilder::new(
            urls.clone(),
            Arc::clone(&ids),
            actor_lookup.clone(),
            Arc::new(DeliveryService::new(
                RecipientTargetResolver::new(MockLocalActorLookup::with_handles(&handles)),
                Arc::clone(&local_sink),
                Arc::clone(&http_sink),
            )),
        ),
        relationship.clone(),
        actor_lookup.clone(),
        crate::statuses::notification_sink::NotificationSinkRegistry::new(),
    ));

    let interaction_service = Arc::new(InteractionService::new(
        app.pool.clone(),
        app.runtime.clone(),
        urls.clone(),
        StatusActivityBuilder::new(
            urls.clone(),
            Arc::clone(&ids),
            actor_lookup.clone(),
            Arc::new(DeliveryService::new(
                RecipientTargetResolver::new(MockLocalActorLookup::with_handles(&handles)),
                Arc::clone(&local_sink),
                Arc::clone(&http_sink),
            )),
        ),
        actor_lookup.clone(),
        relationship.clone(),
        crate::statuses::notification_sink::NotificationSinkRegistry::new(),
    ));

    let poll_service = Arc::new(PollService::new(
        app.pool.clone(),
        app.runtime.clone(),
        urls.clone(),
        StatusActivityBuilder::new(
            urls,
            ids,
            actor_lookup.clone(),
            Arc::new(DeliveryService::new(
                RecipientTargetResolver::new(MockLocalActorLookup::with_handles(&handles)),
                Arc::clone(&local_sink),
                Arc::clone(&http_sink),
            )),
        ),
        actor_lookup,
        relationship,
    ));

    let state = StatusesEndpointsState {
        status_service,
        interaction_service,
        poll_service,
        accounts: app.state.accounts().service(),
        media_store: app.state.media().store().clone(),
        pool: app.pool.clone(),
        runtime: app.runtime.clone(),
        auth: AuthState {
            pool: app.pool.clone(),
            token_hash_key: test_token_hash_key(),
        },
    };
    (state, local_sink, http_sink)
}

fn test_router(state: TestState) -> Router {
    Router::new()
        .route(
            STATUSES_PATH,
            post(
                create_status::<
                    MockActorLookup,
                    MockLocalActorLookup,
                    Arc<RecordingSink>,
                    Arc<RecordingSink>,
                    MockRelationshipQuery,
                    MockActorLookup,
                >,
            ),
        )
        .route(
            STATUS_PATH,
            get(show_status::<
                MockActorLookup,
                MockLocalActorLookup,
                Arc<RecordingSink>,
                Arc<RecordingSink>,
                MockRelationshipQuery,
                MockActorLookup,
            >)
            .delete(
                delete_status::<
                    MockActorLookup,
                    MockLocalActorLookup,
                    Arc<RecordingSink>,
                    Arc<RecordingSink>,
                    MockRelationshipQuery,
                    MockActorLookup,
                >,
            )
            .put(
                edit_status::<
                    MockActorLookup,
                    MockLocalActorLookup,
                    Arc<RecordingSink>,
                    Arc<RecordingSink>,
                    MockRelationshipQuery,
                    MockActorLookup,
                >,
            ),
        )
        .route(
            STATUS_HISTORY_PATH,
            get(status_history::<
                MockActorLookup,
                MockLocalActorLookup,
                Arc<RecordingSink>,
                Arc<RecordingSink>,
                MockRelationshipQuery,
                MockActorLookup,
            >),
        )
        .route(
            STATUS_SOURCE_PATH,
            get(status_source::<
                MockActorLookup,
                MockLocalActorLookup,
                Arc<RecordingSink>,
                Arc<RecordingSink>,
                MockRelationshipQuery,
                MockActorLookup,
            >),
        )
        .route(
            STATUS_CONTEXT_PATH,
            get(status_context::<
                MockActorLookup,
                MockLocalActorLookup,
                Arc<RecordingSink>,
                Arc<RecordingSink>,
                MockRelationshipQuery,
                MockActorLookup,
            >),
        )
        .route(
            STATUS_REBLOG_PATH,
            post(
                reblog_status::<
                    MockActorLookup,
                    MockLocalActorLookup,
                    Arc<RecordingSink>,
                    Arc<RecordingSink>,
                    MockRelationshipQuery,
                    MockActorLookup,
                >,
            ),
        )
        .route(
            STATUS_UNREBLOG_PATH,
            post(
                unreblog_status::<
                    MockActorLookup,
                    MockLocalActorLookup,
                    Arc<RecordingSink>,
                    Arc<RecordingSink>,
                    MockRelationshipQuery,
                    MockActorLookup,
                >,
            ),
        )
        .route(
            STATUS_FAVOURITE_PATH,
            post(
                favourite_status::<
                    MockActorLookup,
                    MockLocalActorLookup,
                    Arc<RecordingSink>,
                    Arc<RecordingSink>,
                    MockRelationshipQuery,
                    MockActorLookup,
                >,
            ),
        )
        .route(
            STATUS_UNFAVOURITE_PATH,
            post(
                unfavourite_status::<
                    MockActorLookup,
                    MockLocalActorLookup,
                    Arc<RecordingSink>,
                    Arc<RecordingSink>,
                    MockRelationshipQuery,
                    MockActorLookup,
                >,
            ),
        )
        .route(
            STATUS_BOOKMARK_PATH,
            post(
                bookmark_status::<
                    MockActorLookup,
                    MockLocalActorLookup,
                    Arc<RecordingSink>,
                    Arc<RecordingSink>,
                    MockRelationshipQuery,
                    MockActorLookup,
                >,
            ),
        )
        .route(
            STATUS_UNBOOKMARK_PATH,
            post(
                unbookmark_status::<
                    MockActorLookup,
                    MockLocalActorLookup,
                    Arc<RecordingSink>,
                    Arc<RecordingSink>,
                    MockRelationshipQuery,
                    MockActorLookup,
                >,
            ),
        )
        .route(
            STATUS_PIN_PATH,
            post(
                pin_status::<
                    MockActorLookup,
                    MockLocalActorLookup,
                    Arc<RecordingSink>,
                    Arc<RecordingSink>,
                    MockRelationshipQuery,
                    MockActorLookup,
                >,
            ),
        )
        .route(
            STATUS_UNPIN_PATH,
            post(
                unpin_status::<
                    MockActorLookup,
                    MockLocalActorLookup,
                    Arc<RecordingSink>,
                    Arc<RecordingSink>,
                    MockRelationshipQuery,
                    MockActorLookup,
                >,
            ),
        )
        .route(
            BOOKMARKS_PATH,
            get(list_bookmarks::<
                MockActorLookup,
                MockLocalActorLookup,
                Arc<RecordingSink>,
                Arc<RecordingSink>,
                MockRelationshipQuery,
                MockActorLookup,
            >),
        )
        .route(
            POLL_PATH,
            get(show_poll::<
                MockActorLookup,
                MockLocalActorLookup,
                Arc<RecordingSink>,
                Arc<RecordingSink>,
                MockRelationshipQuery,
                MockActorLookup,
            >),
        )
        .route(
            POLL_VOTES_PATH,
            post(
                vote_poll::<
                    MockActorLookup,
                    MockLocalActorLookup,
                    Arc<RecordingSink>,
                    Arc<RecordingSink>,
                    MockRelationshipQuery,
                    MockActorLookup,
                >,
            ),
        )
        .with_state(state)
}

// ---- fixtures ----

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
            display_name: "Statuses Endpoints IT Actor".to_string(),
            summary: "a statuses/endpoints test fixture".to_string(),
        })
        .await
        .expect("creating the owner's actor fixture must succeed");

    actor.id
}

async fn register_test_app(pool: &sqlx::PgPool, runtime: &RuntimeContext) -> Id {
    let key = test_token_hash_key();
    let now = runtime.clock.now();
    let registered = app_repository::register_app(
        pool,
        runtime.ids.as_ref(),
        runtime.rng.as_ref(),
        &key,
        now,
        NewApp {
            name: "Statuses Endpoints Test Client".to_string(),
            redirect_uris: vec!["https://client.example/callback".to_string()],
            scopes: ModelScopeSet::new(["read", "write"]),
        },
    )
    .await
    .expect("register_app must succeed");
    registered.id
}

async fn issue_test_token(
    pool: &sqlx::PgPool,
    runtime: &RuntimeContext,
    app_id: Id,
    actor_id: Id,
    scopes: &[&str],
) -> String {
    let key = test_token_hash_key();
    let now = runtime.clock.now();
    let issued = token_repository::issue_token(
        pool,
        runtime.ids.as_ref(),
        runtime.rng.as_ref(),
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

fn get_request(uri: &str, bearer: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method("GET").uri(uri);
    if let Some(token) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    builder.body(Body::empty()).expect("valid test request")
}

fn post_request(uri: &str, bearer: Option<&str>, extra_headers: &[(&str, &str)]) -> Request<Body> {
    let mut builder = Request::builder().method("POST").uri(uri);
    if let Some(token) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    for (name, value) in extra_headers {
        builder = builder.header(*name, *value);
    }
    builder.body(Body::empty()).expect("valid test request")
}

fn json_request(
    method: &str,
    uri: &str,
    bearer: Option<&str>,
    body: &Value,
    extra_headers: &[(&str, &str)],
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(token) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    for (name, value) in extra_headers {
        builder = builder.header(*name, *value);
    }
    builder
        .body(Body::from(body.to_string()))
        .expect("valid test request")
}

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("test response body should be readable");
    serde_json::from_slice(&bytes).expect("test response body should be valid JSON")
}

// ==== POST /api/v1/statuses ====

#[tokio::test]
async fn create_status_without_bearer_is_401() {
    let app = spawn_test_app().await;
    let (state, _local, _http) = build_state(&app, &[], false);
    let router = test_router(state);

    let response = router
        .oneshot(json_request(
            "POST",
            STATUSES_PATH,
            None,
            &json!({"status": "hello"}),
            &[],
        ))
        .await
        .expect("oneshot dispatch must succeed");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    app.cleanup().await;
}

#[tokio::test]
async fn create_status_with_wrong_scope_is_403() {
    let app = spawn_test_app().await;
    let actor_id = create_owner_with_actor(&app, "wrongscope").await;
    let (state, _local, _http) = build_state(&app, &[(actor_id, "wrongscope")], false);
    let router = test_router(state);

    let app_id = register_test_app(&app.pool, &app.runtime).await;
    let token = issue_test_token(&app.pool, &app.runtime, app_id, actor_id, &["read"]).await;

    let response = router
        .oneshot(json_request(
            "POST",
            STATUSES_PATH,
            Some(&token),
            &json!({"status": "hello"}),
            &[],
        ))
        .await
        .expect("oneshot dispatch must succeed");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    app.cleanup().await;
}

#[tokio::test]
async fn create_status_with_empty_body_is_422() {
    let app = spawn_test_app().await;
    let actor_id = create_owner_with_actor(&app, "emptybody").await;
    let (state, _local, _http) = build_state(&app, &[(actor_id, "emptybody")], false);
    let router = test_router(state);

    let app_id = register_test_app(&app.pool, &app.runtime).await;
    let token = issue_test_token(
        &app.pool,
        &app.runtime,
        app_id,
        actor_id,
        &["write:statuses"],
    )
    .await;

    let response = router
        .oneshot(json_request(
            "POST",
            STATUSES_PATH,
            Some(&token),
            &json!({}),
            &[],
        ))
        .await
        .expect("oneshot dispatch must succeed");
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

    app.cleanup().await;
}

#[tokio::test]
async fn create_status_returns_200_with_rendered_account_and_repeats_on_the_same_idempotency_key() {
    let app = spawn_test_app().await;
    let actor_id = create_owner_with_actor(&app, "creator").await;
    let (state, _local, _http) = build_state(&app, &[(actor_id, "creator")], false);
    let router = test_router(state);

    let app_id = register_test_app(&app.pool, &app.runtime).await;
    let token = issue_test_token(
        &app.pool,
        &app.runtime,
        app_id,
        actor_id,
        &["write:statuses"],
    )
    .await;

    let response = router
        .clone()
        .oneshot(json_request(
            "POST",
            STATUSES_PATH,
            Some(&token),
            &json!({"status": "hello world", "visibility": "public"}),
            &[("idempotency-key", "abc-123")],
        ))
        .await
        .expect("oneshot dispatch must succeed");
    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert_eq!(json["content"], "hello world");
    assert_eq!(json["visibility"], "public");
    assert!(
        json["account"]["id"].is_string(),
        "account must be rendered"
    );
    let first_id = json["id"].clone();

    let response2 = router
        .oneshot(json_request(
            "POST",
            STATUSES_PATH,
            Some(&token),
            &json!({"status": "a different body", "visibility": "public"}),
            &[("idempotency-key", "abc-123")],
        ))
        .await
        .expect("oneshot dispatch must succeed");
    assert_eq!(response2.status(), StatusCode::OK);
    let json2 = body_json(response2).await;
    assert_eq!(
        json2["id"], first_id,
        "the same idempotency key must return the same status"
    );

    app.cleanup().await;
}

// ==== GET /api/v1/statuses/:id ====

#[tokio::test]
async fn show_status_is_404_for_an_unknown_id() {
    let app = spawn_test_app().await;
    let (state, _local, _http) = build_state(&app, &[], false);
    let router = test_router(state);

    let response = router
        .oneshot(get_request("/api/v1/statuses/999999999", None))
        .await
        .expect("oneshot dispatch must succeed");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    app.cleanup().await;
}

#[tokio::test]
async fn show_status_returns_200_for_a_public_post_unauthenticated() {
    let app = spawn_test_app().await;
    let actor_id = create_owner_with_actor(&app, "shower").await;
    let (state, _local, _http) = build_state(&app, &[(actor_id, "shower")], false);
    let router = test_router(state.clone());

    let app_id = register_test_app(&app.pool, &app.runtime).await;
    let token = issue_test_token(
        &app.pool,
        &app.runtime,
        app_id,
        actor_id,
        &["write:statuses"],
    )
    .await;
    let created = router
        .clone()
        .oneshot(json_request(
            "POST",
            STATUSES_PATH,
            Some(&token),
            &json!({"status": "public post", "visibility": "public"}),
            &[],
        ))
        .await
        .expect("create must succeed");
    let created_json = body_json(created).await;
    let id = created_json["id"].as_str().unwrap().to_string();

    let response = router
        .oneshot(get_request(&format!("/api/v1/statuses/{id}"), None))
        .await
        .expect("oneshot dispatch must succeed");
    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert_eq!(json["content"], "public post");
    assert_eq!(json["favourited"], false);

    app.cleanup().await;
}

// ==== DELETE /api/v1/statuses/:id ====

#[tokio::test]
async fn delete_status_by_a_non_owner_is_404() {
    let app = spawn_test_app().await;
    let owner_id = create_owner_with_actor(&app, "owner_delete").await;
    let other_id = create_owner_with_actor(&app, "other_delete").await;
    let (state, _local, _http) = build_state(
        &app,
        &[(owner_id, "owner_delete"), (other_id, "other_delete")],
        false,
    );
    let router = test_router(state);

    let app_id = register_test_app(&app.pool, &app.runtime).await;
    let owner_token = issue_test_token(
        &app.pool,
        &app.runtime,
        app_id,
        owner_id,
        &["write:statuses"],
    )
    .await;
    let other_token = issue_test_token(
        &app.pool,
        &app.runtime,
        app_id,
        other_id,
        &["write:statuses"],
    )
    .await;

    let created = router
        .clone()
        .oneshot(json_request(
            "POST",
            STATUSES_PATH,
            Some(&owner_token),
            &json!({"status": "mine", "visibility": "public"}),
            &[],
        ))
        .await
        .expect("create must succeed");
    let created_json = body_json(created).await;
    let id = created_json["id"].as_str().unwrap().to_string();

    let response = router
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/statuses/{id}"))
                .header(header::AUTHORIZATION, format!("Bearer {other_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot dispatch must succeed");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    app.cleanup().await;
}

#[tokio::test]
async fn delete_status_by_the_owner_succeeds() {
    let app = spawn_test_app().await;
    let actor_id = create_owner_with_actor(&app, "self_delete").await;
    let (state, _local, _http) = build_state(&app, &[(actor_id, "self_delete")], false);
    let router = test_router(state);

    let app_id = register_test_app(&app.pool, &app.runtime).await;
    let token = issue_test_token(
        &app.pool,
        &app.runtime,
        app_id,
        actor_id,
        &["write:statuses"],
    )
    .await;

    let created = router
        .clone()
        .oneshot(json_request(
            "POST",
            STATUSES_PATH,
            Some(&token),
            &json!({"status": "delete me", "visibility": "public"}),
            &[],
        ))
        .await
        .expect("create must succeed");
    let created_json = body_json(created).await;
    let id = created_json["id"].as_str().unwrap().to_string();

    let response = router
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/statuses/{id}"))
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot dispatch must succeed");
    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert_eq!(json["content"], "delete me");

    app.cleanup().await;
}

// ==== favourite / bookmark / pin / reblog ====

#[tokio::test]
async fn favourite_then_unfavourite_toggles_state() {
    let app = spawn_test_app().await;
    let actor_id = create_owner_with_actor(&app, "fav_actor").await;
    let (state, _local, _http) = build_state(&app, &[(actor_id, "fav_actor")], false);
    let router = test_router(state);

    let app_id = register_test_app(&app.pool, &app.runtime).await;
    let token = issue_test_token(
        &app.pool,
        &app.runtime,
        app_id,
        actor_id,
        &["write:statuses", "write:favourites"],
    )
    .await;

    let created = router
        .clone()
        .oneshot(json_request(
            "POST",
            STATUSES_PATH,
            Some(&token),
            &json!({"status": "fav me", "visibility": "public"}),
            &[],
        ))
        .await
        .expect("create must succeed");
    let created_json = body_json(created).await;
    let id = created_json["id"].as_str().unwrap().to_string();

    let fav_response = router
        .clone()
        .oneshot(post_request(
            &format!("/api/v1/statuses/{id}/favourite"),
            Some(&token),
            &[],
        ))
        .await
        .expect("favourite dispatch must succeed");
    assert_eq!(fav_response.status(), StatusCode::OK);
    let fav_json = body_json(fav_response).await;
    assert_eq!(fav_json["favourited"], true);

    let unfav_response = router
        .oneshot(post_request(
            &format!("/api/v1/statuses/{id}/unfavourite"),
            Some(&token),
            &[],
        ))
        .await
        .expect("unfavourite dispatch must succeed");
    assert_eq!(unfav_response.status(), StatusCode::OK);
    let unfav_json = body_json(unfav_response).await;
    assert_eq!(unfav_json["favourited"], false);

    app.cleanup().await;
}

#[tokio::test]
async fn favourite_without_the_write_favourites_scope_is_403() {
    let app = spawn_test_app().await;
    let actor_id = create_owner_with_actor(&app, "fav_403").await;
    let (state, _local, _http) = build_state(&app, &[(actor_id, "fav_403")], false);
    let router = test_router(state);

    let app_id = register_test_app(&app.pool, &app.runtime).await;
    let token = issue_test_token(
        &app.pool,
        &app.runtime,
        app_id,
        actor_id,
        &["write:statuses"],
    )
    .await;

    let created = router
        .clone()
        .oneshot(json_request(
            "POST",
            STATUSES_PATH,
            Some(&token),
            &json!({"status": "fav me", "visibility": "public"}),
            &[],
        ))
        .await
        .expect("create must succeed");
    let created_json = body_json(created).await;
    let id = created_json["id"].as_str().unwrap().to_string();

    let response = router
        .oneshot(post_request(
            &format!("/api/v1/statuses/{id}/favourite"),
            Some(&token),
            &[],
        ))
        .await
        .expect("dispatch must succeed");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    app.cleanup().await;
}

#[tokio::test]
async fn pin_rejects_a_direct_visibility_status_with_422() {
    let app = spawn_test_app().await;
    let actor_id = create_owner_with_actor(&app, "pin_direct").await;
    let (state, _local, _http) = build_state(&app, &[(actor_id, "pin_direct")], false);
    let router = test_router(state);

    let app_id = register_test_app(&app.pool, &app.runtime).await;
    let token = issue_test_token(
        &app.pool,
        &app.runtime,
        app_id,
        actor_id,
        &["write:statuses"],
    )
    .await;

    let created = router
        .clone()
        .oneshot(json_request(
            "POST",
            STATUSES_PATH,
            Some(&token),
            &json!({"status": "direct post", "visibility": "direct"}),
            &[],
        ))
        .await
        .expect("create must succeed");
    let created_json = body_json(created).await;
    let id = created_json["id"].as_str().unwrap().to_string();

    let response = router
        .oneshot(post_request(
            &format!("/api/v1/statuses/{id}/pin"),
            Some(&token),
            &[],
        ))
        .await
        .expect("dispatch must succeed");
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

    app.cleanup().await;
}

#[tokio::test]
async fn reblog_then_unreblog_returns_200() {
    let app = spawn_test_app().await;
    let actor_id = create_owner_with_actor(&app, "reblogger").await;
    let (state, _local, _http) = build_state(&app, &[(actor_id, "reblogger")], false);
    let router = test_router(state);

    let app_id = register_test_app(&app.pool, &app.runtime).await;
    let token = issue_test_token(
        &app.pool,
        &app.runtime,
        app_id,
        actor_id,
        &["write:statuses"],
    )
    .await;

    let created = router
        .clone()
        .oneshot(json_request(
            "POST",
            STATUSES_PATH,
            Some(&token),
            &json!({"status": "boost me", "visibility": "public"}),
            &[],
        ))
        .await
        .expect("create must succeed");
    let created_json = body_json(created).await;
    let id = created_json["id"].as_str().unwrap().to_string();

    let reblog_response = router
        .clone()
        .oneshot(post_request(
            &format!("/api/v1/statuses/{id}/reblog"),
            Some(&token),
            &[],
        ))
        .await
        .expect("reblog dispatch must succeed");
    assert_eq!(reblog_response.status(), StatusCode::OK);
    let reblog_json = body_json(reblog_response).await;
    assert_eq!(reblog_json["reblog"]["id"], created_json["id"]);

    let unreblog_response = router
        .oneshot(post_request(
            &format!("/api/v1/statuses/{id}/unreblog"),
            Some(&token),
            &[],
        ))
        .await
        .expect("unreblog dispatch must succeed");
    assert_eq!(unreblog_response.status(), StatusCode::OK);

    app.cleanup().await;
}

// ==== GET /api/v1/bookmarks ====

#[tokio::test]
async fn bookmarks_list_requires_read_bookmarks_scope() {
    let app = spawn_test_app().await;
    let actor_id = create_owner_with_actor(&app, "bm_403").await;
    let (state, _local, _http) = build_state(&app, &[(actor_id, "bm_403")], false);
    let router = test_router(state);

    let app_id = register_test_app(&app.pool, &app.runtime).await;
    let token = issue_test_token(
        &app.pool,
        &app.runtime,
        app_id,
        actor_id,
        &["write:statuses"],
    )
    .await;

    let response = router
        .oneshot(get_request(BOOKMARKS_PATH, Some(&token)))
        .await
        .expect("dispatch must succeed");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    app.cleanup().await;
}

#[tokio::test]
async fn bookmarking_a_status_makes_it_appear_in_the_bookmark_list_with_a_link_header() {
    let app = spawn_test_app().await;
    let actor_id = create_owner_with_actor(&app, "bookmarker").await;
    let (state, _local, _http) = build_state(&app, &[(actor_id, "bookmarker")], false);
    let router = test_router(state);

    let app_id = register_test_app(&app.pool, &app.runtime).await;
    let token = issue_test_token(
        &app.pool,
        &app.runtime,
        app_id,
        actor_id,
        &["write:statuses", "write:bookmarks", "read:bookmarks"],
    )
    .await;

    let mut ids = Vec::new();
    for i in 0..3 {
        let created = router
            .clone()
            .oneshot(json_request(
                "POST",
                STATUSES_PATH,
                Some(&token),
                &json!({"status": format!("bm post {i}"), "visibility": "public"}),
                &[],
            ))
            .await
            .expect("create must succeed");
        let created_json = body_json(created).await;
        ids.push(created_json["id"].as_str().unwrap().to_string());

        let bm_response = router
            .clone()
            .oneshot(post_request(
                &format!("/api/v1/statuses/{}/bookmark", ids.last().unwrap()),
                Some(&token),
                &[],
            ))
            .await
            .expect("bookmark dispatch must succeed");
        assert_eq!(bm_response.status(), StatusCode::OK);
    }

    let response = router
        .oneshot(get_request(
            &format!("{BOOKMARKS_PATH}?limit=2"),
            Some(&token),
        ))
        .await
        .expect("list dispatch must succeed");
    assert_eq!(response.status(), StatusCode::OK);
    let has_link = response.headers().get(header::LINK).is_some();
    let json = body_json(response).await;
    assert_eq!(
        json.as_array().map(Vec::len),
        Some(2),
        "limit=2 must cap the page at two items"
    );
    assert!(
        has_link,
        "a bookmark list with more items than the page limit must carry a Link header"
    );

    app.cleanup().await;
}

// ==== GET /api/v1/polls/:id, POST /api/v1/polls/:id/votes ====

#[tokio::test]
async fn poll_vote_updates_the_tally_and_get_reflects_it() {
    let app = spawn_test_app().await;
    let actor_id = create_owner_with_actor(&app, "voter").await;
    let (state, _local, _http) = build_state(&app, &[(actor_id, "voter")], false);

    // Insert a poll-bearing status directly via the repositories (mirroring
    // `poll_service/tests.rs`'s own "insert fixtures directly" precedent —
    // `StatusService::create_status` always 422-rejects a caller-supplied
    // poll, per that module's own documented boundary decision).
    let now = app.runtime.clock.now();
    let status_id = app.runtime.ids.next_id();
    let poll_id = app.runtime.ids.next_id();
    let uri = format!("https://kawasemi.example/statuses/{}", status_id.as_i64());
    let status = crate::statuses::model::Status {
        id: status_id,
        actor_id,
        uri: uri.clone(),
        url: Some(uri),
        content: "pick one".to_string(),
        visibility: Visibility::Public,
        sensitive: false,
        spoiler_text: String::new(),
        in_reply_to_id: None,
        in_reply_to_account_id: None,
        reblog_of_id: None,
        poll_id: Some(poll_id),
        language: None,
        reblogs_count: 0,
        favourites_count: 0,
        replies_count: 0,
        local: true,
        created_at: now,
        edited_at: None,
    };
    crate::statuses::status_repository::insert_status(&app.pool, &status)
        .await
        .expect("insert_status must succeed");
    crate::statuses::poll_repository::insert_poll(
        &app.pool,
        &crate::statuses::model::Poll {
            id: poll_id,
            status_id,
            expires_at: None,
            multiple: false,
        },
        &[
            crate::statuses::model::PollOption {
                poll_id,
                idx: 0,
                title: "Cats".to_string(),
                votes_count: 0,
            },
            crate::statuses::model::PollOption {
                poll_id,
                idx: 1,
                title: "Dogs".to_string(),
                votes_count: 0,
            },
        ],
    )
    .await
    .expect("insert_poll must succeed");

    let router = test_router(state);

    let app_id = register_test_app(&app.pool, &app.runtime).await;
    let token = issue_test_token(
        &app.pool,
        &app.runtime,
        app_id,
        actor_id,
        &["write:statuses"],
    )
    .await;

    let get_response = router
        .clone()
        .oneshot(get_request(
            &format!("/api/v1/polls/{}", poll_id.as_i64()),
            None,
        ))
        .await
        .expect("get poll dispatch must succeed");
    assert_eq!(get_response.status(), StatusCode::OK);
    let get_json = body_json(get_response).await;
    assert_eq!(get_json["voted"], false);

    let vote_response = router
        .oneshot(json_request(
            "POST",
            &format!("/api/v1/polls/{}/votes", poll_id.as_i64()),
            Some(&token),
            &json!({"choices": [0]}),
            &[],
        ))
        .await
        .expect("vote dispatch must succeed");
    assert_eq!(vote_response.status(), StatusCode::OK);
    let vote_json = body_json(vote_response).await;
    assert_eq!(vote_json["voted"], true);
    assert_eq!(vote_json["own_votes"], json!([0]));

    app.cleanup().await;
}

// ==== `emojis` field resolution (task 10.4) ====

/// Seeds one `custom_emojis` row — mirrors
/// `accounts::emoji_repository::tests`'s own identical seeding helper (this
/// module cannot import that one, it is private to its own `#[cfg(test)]`
/// module, so a small duplicate is this crate's own documented convention
/// for test-local fixtures across sibling test modules).
async fn seed_custom_emoji(app: &TestApp, shortcode: &str) {
    let now = app.runtime.clock.now();
    let url = format!("https://example.test/emoji/{shortcode}.png");
    sqlx::query(
        "INSERT INTO custom_emojis \
             (shortcode, domain, url, static_url, visible_in_picker, category, updated_at) \
         VALUES ($1, '', $2, $2, TRUE, NULL, $3)",
    )
    .bind(shortcode)
    .bind(&url)
    .bind(now)
    .execute(&app.pool)
    .await
    .expect("seeding a custom_emojis row must succeed");
}

#[tokio::test]
async fn create_status_resolves_a_registered_shortcode_into_the_emojis_field() {
    let app = spawn_test_app().await;
    let actor_id = create_owner_with_actor(&app, "emoji_poster").await;
    seed_custom_emoji(&app, "blobcat").await;
    let (state, _local, _http) = build_state(&app, &[(actor_id, "emoji_poster")], false);
    let router = test_router(state);

    let app_id = register_test_app(&app.pool, &app.runtime).await;
    let token = issue_test_token(
        &app.pool,
        &app.runtime,
        app_id,
        actor_id,
        &["write:statuses"],
    )
    .await;

    let response = router
        .oneshot(json_request(
            "POST",
            STATUSES_PATH,
            Some(&token),
            &json!({
                "status": "hello :blobcat: and :not_registered:",
                "visibility": "public"
            }),
            &[("idempotency-key", "emoji-1")],
        ))
        .await
        .expect("oneshot dispatch must succeed");
    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;

    let emojis = json["emojis"].as_array().expect("emojis must be an array");
    assert_eq!(
        emojis.len(),
        1,
        "only the registered shortcode must resolve, unregistered ones are silently omitted: {emojis:?}"
    );
    assert_eq!(emojis[0]["shortcode"], "blobcat");
    assert_eq!(emojis[0]["url"], "https://example.test/emoji/blobcat.png");
    assert_eq!(
        emojis[0]["static_url"],
        "https://example.test/emoji/blobcat.png"
    );
    assert_eq!(emojis[0]["visible_in_picker"], true);

    app.cleanup().await;
}

#[tokio::test]
async fn create_status_with_no_registered_shortcode_has_empty_emojis() {
    let app = spawn_test_app().await;
    let actor_id = create_owner_with_actor(&app, "no_emoji_poster").await;
    let (state, _local, _http) = build_state(&app, &[(actor_id, "no_emoji_poster")], false);
    let router = test_router(state);

    let app_id = register_test_app(&app.pool, &app.runtime).await;
    let token = issue_test_token(
        &app.pool,
        &app.runtime,
        app_id,
        actor_id,
        &["write:statuses"],
    )
    .await;

    let response = router
        .oneshot(json_request(
            "POST",
            STATUSES_PATH,
            Some(&token),
            &json!({"status": "hello :unknown_shortcode:", "visibility": "public"}),
            &[("idempotency-key", "emoji-2")],
        ))
        .await
        .expect("oneshot dispatch must succeed");
    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert_eq!(json["emojis"], json!([]));

    app.cleanup().await;
}

#[tokio::test]
async fn poll_json_resolves_a_registered_shortcode_from_an_option_title() {
    let app = spawn_test_app().await;
    let actor_id = create_owner_with_actor(&app, "poll_emoji_owner").await;
    seed_custom_emoji(&app, "partyparrot").await;
    let (state, _local, _http) = build_state(&app, &[(actor_id, "poll_emoji_owner")], false);

    let now = app.runtime.clock.now();
    let status_id = app.runtime.ids.next_id();
    let poll_id = app.runtime.ids.next_id();
    let uri = format!("https://kawasemi.example/statuses/{}", status_id.as_i64());
    let status = crate::statuses::model::Status {
        id: status_id,
        actor_id,
        uri: uri.clone(),
        url: Some(uri),
        content: "pick one".to_string(),
        visibility: Visibility::Public,
        sensitive: false,
        spoiler_text: String::new(),
        in_reply_to_id: None,
        in_reply_to_account_id: None,
        reblog_of_id: None,
        poll_id: Some(poll_id),
        language: None,
        reblogs_count: 0,
        favourites_count: 0,
        replies_count: 0,
        local: true,
        created_at: now,
        edited_at: None,
    };
    crate::statuses::status_repository::insert_status(&app.pool, &status)
        .await
        .expect("insert_status must succeed");
    crate::statuses::poll_repository::insert_poll(
        &app.pool,
        &crate::statuses::model::Poll {
            id: poll_id,
            status_id,
            expires_at: None,
            multiple: false,
        },
        &[
            crate::statuses::model::PollOption {
                poll_id,
                idx: 0,
                title: "Cats :partyparrot:".to_string(),
                votes_count: 0,
            },
            crate::statuses::model::PollOption {
                poll_id,
                idx: 1,
                title: "Dogs".to_string(),
                votes_count: 0,
            },
        ],
    )
    .await
    .expect("insert_poll must succeed");

    let router = test_router(state);

    let get_response = router
        .oneshot(get_request(
            &format!("/api/v1/polls/{}", poll_id.as_i64()),
            None,
        ))
        .await
        .expect("get poll dispatch must succeed");
    assert_eq!(get_response.status(), StatusCode::OK);
    let get_json = body_json(get_response).await;
    let emojis = get_json["emojis"]
        .as_array()
        .expect("emojis must be an array");
    assert_eq!(emojis.len(), 1, "{emojis:?}");
    assert_eq!(emojis[0]["shortcode"], "partyparrot");

    app.cleanup().await;
}

// ==== Page-shaped rendering: `GET /api/v1/statuses/:id/context` and
//      `GET /api/v1/bookmarks` ====

/// The one `statuses` row shape this module's own HTTP surface cannot
/// create — a row that replies *and* boosts, or carries a poll. Mirrors
/// [`poll_vote_updates_the_tally_and_get_reflects_it`]'s own already-used
/// "insert the fixture directly through the repositories" precedent.
struct SeedStatus<'a> {
    actor_id: Id,
    visibility: Visibility,
    content: &'a str,
    in_reply_to_id: Option<Id>,
    reblog_of_id: Option<Id>,
    poll_id: Option<Id>,
}

async fn seed_status(app: &TestApp, seed: SeedStatus<'_>) -> Id {
    let id = app.runtime.ids.next_id();
    let uri = format!("https://kawasemi.example/statuses/{}", id.as_i64());
    let status = crate::statuses::model::Status {
        id,
        actor_id: seed.actor_id,
        uri: uri.clone(),
        url: Some(uri),
        content: seed.content.to_string(),
        visibility: seed.visibility,
        sensitive: false,
        spoiler_text: String::new(),
        in_reply_to_id: seed.in_reply_to_id,
        in_reply_to_account_id: None,
        reblog_of_id: seed.reblog_of_id,
        poll_id: seed.poll_id,
        language: None,
        reblogs_count: 0,
        favourites_count: 0,
        replies_count: 0,
        local: true,
        created_at: app.runtime.clock.now(),
        edited_at: None,
    };
    crate::statuses::status_repository::insert_status(&app.pool, &status)
        .await
        .expect("insert_status must succeed");
    id
}

/// Attaches a two-option poll to `status_id`, the first option's title
/// carrying a `:shortcode:` of its own (a poll option's title is
/// shortcode-bearing text in its own right, resolved separately from the
/// owning status's `content`).
async fn seed_poll(app: &TestApp, status_id: Id, poll_id: Id) {
    crate::statuses::poll_repository::insert_poll(
        &app.pool,
        &crate::statuses::model::Poll {
            id: poll_id,
            status_id,
            expires_at: None,
            multiple: false,
        },
        &[
            crate::statuses::model::PollOption {
                poll_id,
                idx: 0,
                title: "Cats :partyparrot:".to_string(),
                votes_count: 0,
            },
            crate::statuses::model::PollOption {
                poll_id,
                idx: 1,
                title: "Dogs".to_string(),
                votes_count: 0,
            },
        ],
    )
    .await
    .expect("insert_poll must succeed");
}

/// Registers `name` as a tag and associates it with `status_id`.
async fn attach_tag(app: &TestApp, status_id: Id, name: &str) {
    let tag = crate::statuses::model::Tag {
        id: app.runtime.ids.next_id(),
        name: name.to_string(),
        created_at: app.runtime.clock.now(),
    };
    let tag = crate::statuses::tag_repository::upsert_tag(&app.pool, &tag)
        .await
        .expect("upsert_tag must succeed");
    crate::statuses::tag_repository::associate_tag(&app.pool, status_id, tag.id)
        .await
        .expect("associate_tag must succeed");
}

/// Pulls one field out of every element of a JSON array, tolerating a
/// non-array (a `null` `poll` indexes to `null`, not a panic).
fn field_list(items: &Value, field: &str) -> Vec<Value> {
    items
        .as_array()
        .map(|items| items.iter().map(|item| item[field].clone()).collect())
        .unwrap_or_default()
}

/// Projects exactly what this surface's assembly resolves per status — the
/// author's Account, the tags, the emoji (from `content` and from the poll's
/// own option titles), the poll, the viewer's interaction state, and `muted`
/// — recursing into `reblog`, whose presence or absence is this surface's
/// own `StatusService::show` judgment rather than the assembler's.
fn material_fingerprint(json: &Value) -> Value {
    json!({
        "id": json["id"],
        "account": json["account"]["id"],
        "tags": field_list(&json["tags"], "name"),
        "emojis": field_list(&json["emojis"], "shortcode"),
        "poll_options": field_list(&json["poll"]["options"], "title"),
        "poll_emojis": field_list(&json["poll"]["emojis"], "shortcode"),
        "favourited": json["favourited"],
        "reblogged": json["reblogged"],
        "bookmarked": json["bookmarked"],
        "pinned": json["pinned"],
        "muted": json["muted"],
        "reblog": match json["reblog"] {
            Value::Null => Value::Null,
            ref reblog => material_fingerprint(reblog),
        },
    })
}

fn fingerprints(items: &Value) -> Vec<Value> {
    items
        .as_array()
        .expect("a rendered list must be a JSON array")
        .iter()
        .map(material_fingerprint)
        .collect()
}

fn id_string(id: Id) -> Value {
    Value::String(id.as_i64().to_string())
}

/// The characterization one whole `GET /api/v1/statuses/:id/context`
/// response is measured against, captured from the implementation that
/// resolved and rendered each ancestor and descendant one at a time.
///
/// The thread deliberately carries every shape whose resolution this surface
/// either batches or must deliberately keep out of the batch:
///
/// - two ancestors by the **same author**, so resolving that author once per
///   response cannot be told apart from resolving it twice;
/// - a descendant boosting a target the viewer **can** see, nesting a fully
///   rendered `reblog` — the target is a row in its own right and belongs in
///   the same batch;
/// - a descendant boosting a target the viewer **cannot** see, rendering
///   `reblog: null` — dropped whole rather than partially, decided by
///   `StatusService::show` (this surface's own visibility rule), not by the
///   assembler;
/// - a descendant carrying a **poll** whose option title has a shortcode of
///   its own, distinct from any in the status's `content`;
/// - a descendant **invisible** to the viewer, which `StatusService::context`
///   filters out before this handler sees it;
/// - tags, custom emoji, and a favourite, so no material the assembler
///   resolves is at its default everywhere in the page.
///
/// `muted` is `false` throughout: this surface has no mute context to offer.
#[tokio::test]
async fn status_context_renders_every_ancestor_and_descendant_material_in_thread_order() {
    let app = spawn_test_app().await;
    let viewer = create_owner_with_actor(&app, "ctxviewer").await;
    let other = create_owner_with_actor(&app, "ctxauthor").await;
    let (state, _local, _http) = build_state(
        &app,
        &[(viewer, "ctxviewer"), (other, "ctxauthor")],
        // Not a follower, so `other`'s `private` posts stay invisible.
        false,
    );

    seed_custom_emoji(&app, "blobcat").await;
    seed_custom_emoji(&app, "partyparrot").await;

    // Ancestors: two posts by the same author, the first tagged and
    // carrying a registered shortcode alongside an unregistered one.
    let root = seed_status(
        &app,
        SeedStatus {
            actor_id: other,
            visibility: Visibility::Public,
            content: "root :blobcat: and :not_registered:",
            in_reply_to_id: None,
            reblog_of_id: None,
            poll_id: None,
        },
    )
    .await;
    attach_tag(&app, root, "kawasemi").await;
    let middle = seed_status(
        &app,
        SeedStatus {
            actor_id: other,
            visibility: Visibility::Public,
            content: "second by the same author",
            in_reply_to_id: Some(root),
            reblog_of_id: None,
            poll_id: None,
        },
    )
    .await;

    // The status whose context is requested.
    let focus = seed_status(
        &app,
        SeedStatus {
            actor_id: viewer,
            visibility: Visibility::Public,
            content: "the focus of this context request",
            in_reply_to_id: Some(middle),
            reblog_of_id: None,
            poll_id: None,
        },
    )
    .await;

    // Boost targets, outside the thread: one visible (and favourited by the
    // viewer, so the nested reblog's own interaction state is non-default),
    // one this viewer may not see.
    let visible_target = seed_status(
        &app,
        SeedStatus {
            actor_id: other,
            visibility: Visibility::Public,
            content: "boosted publicly",
            in_reply_to_id: None,
            reblog_of_id: None,
            poll_id: None,
        },
    )
    .await;
    crate::statuses::interaction_repository::add_favourite(
        &app.pool,
        viewer,
        visible_target,
        app.runtime.clock.now(),
    )
    .await
    .expect("add_favourite must succeed");
    let hidden_target = seed_status(
        &app,
        SeedStatus {
            actor_id: other,
            visibility: Visibility::Private,
            content: "boosted privately",
            in_reply_to_id: None,
            reblog_of_id: None,
            poll_id: None,
        },
    )
    .await;

    // Descendants, in `created_at`-then-`id` order.
    let poll_id = app.runtime.ids.next_id();
    let polled = seed_status(
        &app,
        SeedStatus {
            actor_id: other,
            visibility: Visibility::Public,
            content: "pick one",
            in_reply_to_id: Some(focus),
            reblog_of_id: None,
            poll_id: Some(poll_id),
        },
    )
    .await;
    seed_poll(&app, polled, poll_id).await;
    let boost_of_visible = seed_status(
        &app,
        SeedStatus {
            actor_id: viewer,
            visibility: Visibility::Public,
            content: "",
            in_reply_to_id: Some(focus),
            reblog_of_id: Some(visible_target),
            poll_id: None,
        },
    )
    .await;
    let boost_of_hidden = seed_status(
        &app,
        SeedStatus {
            actor_id: viewer,
            visibility: Visibility::Public,
            content: "",
            in_reply_to_id: Some(focus),
            reblog_of_id: Some(hidden_target),
            poll_id: None,
        },
    )
    .await;
    // Filtered out by `StatusService::context` before this handler sees it.
    seed_status(
        &app,
        SeedStatus {
            actor_id: other,
            visibility: Visibility::Private,
            content: "an invisible reply",
            in_reply_to_id: Some(focus),
            reblog_of_id: None,
            poll_id: None,
        },
    )
    .await;

    let router = test_router(state);
    let app_id = register_test_app(&app.pool, &app.runtime).await;
    let token = issue_test_token(&app.pool, &app.runtime, app_id, viewer, &["read"]).await;

    let response = router
        .oneshot(get_request(
            &format!("/api/v1/statuses/{}/context", focus.as_i64()),
            Some(&token),
        ))
        .await
        .expect("context dispatch must succeed");
    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;

    assert_eq!(
        fingerprints(&json["ancestors"]),
        vec![
            json!({
                "id": id_string(root),
                "account": id_string(other),
                "tags": ["kawasemi"],
                "emojis": ["blobcat"],
                "poll_options": [],
                "poll_emojis": [],
                "favourited": false,
                "reblogged": false,
                "bookmarked": false,
                "pinned": false,
                "muted": false,
                "reblog": Value::Null,
            }),
            json!({
                "id": id_string(middle),
                "account": id_string(other),
                "tags": [],
                "emojis": [],
                "poll_options": [],
                "poll_emojis": [],
                "favourited": false,
                "reblogged": false,
                "bookmarked": false,
                "pinned": false,
                "muted": false,
                "reblog": Value::Null,
            }),
        ],
        "ancestors must stay root-first with every material resolved"
    );

    assert_eq!(
        fingerprints(&json["descendants"]),
        vec![
            json!({
                "id": id_string(polled),
                "account": id_string(other),
                "tags": [],
                "emojis": [],
                "poll_options": ["Cats :partyparrot:", "Dogs"],
                "poll_emojis": ["partyparrot"],
                "favourited": false,
                "reblogged": false,
                "bookmarked": false,
                "pinned": false,
                "muted": false,
                "reblog": Value::Null,
            }),
            json!({
                "id": id_string(boost_of_visible),
                "account": id_string(viewer),
                "tags": [],
                "emojis": [],
                "poll_options": [],
                "poll_emojis": [],
                "favourited": false,
                "reblogged": false,
                "bookmarked": false,
                "pinned": false,
                "muted": false,
                "reblog": {
                    "id": id_string(visible_target),
                    "account": id_string(other),
                    "tags": [],
                    "emojis": [],
                    "poll_options": [],
                    "poll_emojis": [],
                    "favourited": true,
                    "reblogged": true,
                    "bookmarked": false,
                    "pinned": false,
                    "muted": false,
                    "reblog": Value::Null,
                },
            }),
            json!({
                "id": id_string(boost_of_hidden),
                "account": id_string(viewer),
                "tags": [],
                "emojis": [],
                "poll_options": [],
                "poll_emojis": [],
                "favourited": false,
                "reblogged": false,
                "bookmarked": false,
                "pinned": false,
                "muted": false,
                "reblog": Value::Null,
            }),
        ],
        "descendants must stay in creation order, exclude the invisible \
         reply, and drop an invisible boost target whole"
    );

    app.cleanup().await;
}

/// The characterization one whole `GET /api/v1/bookmarks` page is measured
/// against, captured from the implementation that rendered its items one at
/// a time.
///
/// Same material mix as the context characterization above, plus the two
/// things only this endpoint has: `bookmarked: true` on every item, and a
/// `Link` header built from the page's own cursors *after* the items are
/// rendered. Order is `bookmarks.id` descending, i.e. the reverse of the
/// order the bookmarks were taken in.
#[tokio::test]
async fn bookmark_list_renders_every_material_newest_bookmark_first() {
    let app = spawn_test_app().await;
    let viewer = create_owner_with_actor(&app, "bmviewer").await;
    let other = create_owner_with_actor(&app, "bmauthor").await;
    let (state, _local, _http) =
        build_state(&app, &[(viewer, "bmviewer"), (other, "bmauthor")], false);

    seed_custom_emoji(&app, "blobcat").await;
    seed_custom_emoji(&app, "partyparrot").await;

    // Two by the same author, the first tagged and shortcode-bearing.
    let first = seed_status(
        &app,
        SeedStatus {
            actor_id: other,
            visibility: Visibility::Public,
            content: "shared author one :blobcat:",
            in_reply_to_id: None,
            reblog_of_id: None,
            poll_id: None,
        },
    )
    .await;
    attach_tag(&app, first, "kawasemi").await;
    let second = seed_status(
        &app,
        SeedStatus {
            actor_id: other,
            visibility: Visibility::Public,
            content: "shared author two",
            in_reply_to_id: None,
            reblog_of_id: None,
            poll_id: None,
        },
    )
    .await;

    let poll_id = app.runtime.ids.next_id();
    let polled = seed_status(
        &app,
        SeedStatus {
            actor_id: other,
            visibility: Visibility::Public,
            content: "pick one",
            in_reply_to_id: None,
            reblog_of_id: None,
            poll_id: Some(poll_id),
        },
    )
    .await;
    seed_poll(&app, polled, poll_id).await;

    let visible_target = seed_status(
        &app,
        SeedStatus {
            actor_id: other,
            visibility: Visibility::Public,
            content: "boosted publicly",
            in_reply_to_id: None,
            reblog_of_id: None,
            poll_id: None,
        },
    )
    .await;
    crate::statuses::interaction_repository::add_favourite(
        &app.pool,
        viewer,
        visible_target,
        app.runtime.clock.now(),
    )
    .await
    .expect("add_favourite must succeed");
    let hidden_target = seed_status(
        &app,
        SeedStatus {
            actor_id: other,
            visibility: Visibility::Private,
            content: "boosted privately",
            in_reply_to_id: None,
            reblog_of_id: None,
            poll_id: None,
        },
    )
    .await;
    let boost_of_visible = seed_status(
        &app,
        SeedStatus {
            actor_id: viewer,
            visibility: Visibility::Public,
            content: "",
            in_reply_to_id: None,
            reblog_of_id: Some(visible_target),
            poll_id: None,
        },
    )
    .await;
    let boost_of_hidden = seed_status(
        &app,
        SeedStatus {
            actor_id: viewer,
            visibility: Visibility::Public,
            content: "",
            in_reply_to_id: None,
            reblog_of_id: Some(hidden_target),
            poll_id: None,
        },
    )
    .await;

    // Bookmarked oldest-first, so the page must come back reversed.
    for status_id in [first, second, polled, boost_of_visible, boost_of_hidden] {
        crate::statuses::interaction_repository::add_bookmark(
            &app.pool,
            app.runtime.ids.next_id(),
            viewer,
            status_id,
            app.runtime.clock.now(),
        )
        .await
        .expect("add_bookmark must succeed");
    }

    let router = test_router(state);
    let app_id = register_test_app(&app.pool, &app.runtime).await;
    let token =
        issue_test_token(&app.pool, &app.runtime, app_id, viewer, &["read:bookmarks"]).await;

    let response = router
        .clone()
        .oneshot(get_request(BOOKMARKS_PATH, Some(&token)))
        .await
        .expect("bookmark list dispatch must succeed");
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers().get(header::LINK).is_some(),
        "a full page still carries the cursor-built Link header"
    );
    let json = body_json(response).await;

    assert_eq!(
        fingerprints(&json),
        vec![
            json!({
                "id": id_string(boost_of_hidden),
                "account": id_string(viewer),
                "tags": [],
                "emojis": [],
                "poll_options": [],
                "poll_emojis": [],
                "favourited": false,
                "reblogged": false,
                "bookmarked": true,
                "pinned": false,
                "muted": false,
                "reblog": Value::Null,
            }),
            json!({
                "id": id_string(boost_of_visible),
                "account": id_string(viewer),
                "tags": [],
                "emojis": [],
                "poll_options": [],
                "poll_emojis": [],
                "favourited": false,
                "reblogged": false,
                "bookmarked": true,
                "pinned": false,
                "muted": false,
                "reblog": {
                    "id": id_string(visible_target),
                    "account": id_string(other),
                    "tags": [],
                    "emojis": [],
                    "poll_options": [],
                    "poll_emojis": [],
                    "favourited": true,
                    "reblogged": true,
                    "bookmarked": false,
                    "pinned": false,
                    "muted": false,
                    "reblog": Value::Null,
                },
            }),
            json!({
                "id": id_string(polled),
                "account": id_string(other),
                "tags": [],
                "emojis": [],
                "poll_options": ["Cats :partyparrot:", "Dogs"],
                "poll_emojis": ["partyparrot"],
                "favourited": false,
                "reblogged": false,
                "bookmarked": true,
                "pinned": false,
                "muted": false,
                "reblog": Value::Null,
            }),
            json!({
                "id": id_string(second),
                "account": id_string(other),
                "tags": [],
                "emojis": [],
                "poll_options": [],
                "poll_emojis": [],
                "favourited": false,
                "reblogged": false,
                "bookmarked": true,
                "pinned": false,
                "muted": false,
                "reblog": Value::Null,
            }),
            json!({
                "id": id_string(first),
                "account": id_string(other),
                "tags": ["kawasemi"],
                "emojis": ["blobcat"],
                "poll_options": [],
                "poll_emojis": [],
                "favourited": false,
                "reblogged": false,
                "bookmarked": true,
                "pinned": false,
                "muted": false,
                "reblog": Value::Null,
            }),
        ],
        "the bookmark page must stay newest-bookmark-first with every \
         material resolved"
    );

    // The `Link` header is still built from the page's own cursors, and
    // `limit` still caps the page.
    let limited = router
        .oneshot(get_request(
            &format!("{BOOKMARKS_PATH}?limit=2"),
            Some(&token),
        ))
        .await
        .expect("limited bookmark list dispatch must succeed");
    assert_eq!(limited.status(), StatusCode::OK);
    assert!(
        limited.headers().get(header::LINK).is_some(),
        "a truncated bookmark page must carry a Link header"
    );
    let limited_json = body_json(limited).await;
    assert_eq!(
        field_list(&limited_json, "id"),
        vec![id_string(boost_of_hidden), id_string(boost_of_visible)],
        "limit=2 must cap the page at the two newest bookmarks"
    );

    app.cleanup().await;
}

// ==== Query counts for `GET /api/v1/bookmarks` ====

/// Seeds one bookmarked status carrying every batched per-status material
/// except the viewer's pin, and hands back its id.
async fn seed_bookmarked_status(app: &TestApp, author: Id, viewer: Id) -> Id {
    let poll_id = app.runtime.ids.next_id();
    let status_id = seed_status(
        app,
        SeedStatus {
            actor_id: author,
            visibility: Visibility::Public,
            content: "bookmark me :blobcat:",
            in_reply_to_id: None,
            reblog_of_id: None,
            poll_id: Some(poll_id),
        },
    )
    .await;
    seed_poll(app, status_id, poll_id).await;
    attach_tag(app, status_id, "kawasemi").await;
    crate::statuses::interaction_repository::add_bookmark(
        &app.pool,
        app.runtime.ids.next_id(),
        viewer,
        status_id,
        app.runtime.clock.now(),
    )
    .await
    .expect("add_bookmark must succeed");
    status_id
}

/// Page-sized material lookups through the real HTTP surface, with one
/// qualification: on `GET /api/v1/bookmarks` the media, tag, emoji and
/// interaction-state lookups are page-sized — but the **poll lookups are
/// still per poll**, and that is a known, accepted residual rather than an
/// oversight.
///
/// The reason: this endpoint's `PollServiceResolver` goes through
/// `PollService::poll`, which applies its own per-poll
/// `visible_poll_and_status` check, and batching it needs a
/// visibility-checking multi-poll entry point `PollService` does not have.
/// The residual is asserted here as an exact multiple of the page's poll
/// count rather than excluded from the measurement, so that (a) nobody
/// reading this suite concludes the whole endpoint is batched, and (b)
/// closing the gap later fails this test and forces the note to be struck.
///
/// Measured through `router.oneshot` rather than against the handler
/// directly: the per-status loop this replaced was in the handler, and a
/// page assembled through the real request path is the thing that matters.
#[tokio::test]
async fn the_bookmark_page_batches_every_material_except_its_polls() {
    let app = spawn_test_app().await;
    let viewer = create_owner_with_actor(&app, "bmqcviewer").await;
    let author = create_owner_with_actor(&app, "bmqcauthor").await;
    let (state, _local, _http) = build_state(
        &app,
        &[(viewer, "bmqcviewer"), (author, "bmqcauthor")],
        false,
    );
    seed_custom_emoji(&app, "blobcat").await;
    seed_custom_emoji(&app, "partyparrot").await;

    let router = test_router(state);
    let app_id = register_test_app(&app.pool, &app.runtime).await;
    let token =
        issue_test_token(&app.pool, &app.runtime, app_id, viewer, &["read:bookmarks"]).await;

    let fetch_page = |page_size: usize| {
        let router = router.clone();
        let token = token.clone();
        async move {
            let response = router
                .oneshot(get_request(BOOKMARKS_PATH, Some(&token)))
                .await
                .expect("bookmark list dispatch must succeed");
            assert_eq!(response.status(), StatusCode::OK);
            let json = body_json(response).await;
            assert_eq!(
                json.as_array().map(Vec::len),
                Some(page_size),
                "the fixture must render {page_size} bookmarks"
            );
        }
    };

    seed_bookmarked_status(&app, author, viewer).await;
    let ((), one_log) = record_queries(&app.pool, fetch_page(1)).await;

    // Nineteen more, filling exactly one default-limit page.
    for _ in 0..19 {
        seed_bookmarked_status(&app, author, viewer).await;
    }
    let ((), twenty_log) = record_queries(&app.pool, fetch_page(20)).await;

    one_log.require_kinds(&[
        QueryKind::Media,
        QueryKind::Tags,
        QueryKind::Emoji,
        QueryKind::Interaction,
    ]);
    for kind in [
        QueryKind::Media,
        QueryKind::Tags,
        QueryKind::Emoji,
        QueryKind::Interaction,
    ] {
        assert_eq!(
            one_log.count(kind),
            twenty_log.count(kind),
            "{kind:?} queries must not depend on the page's length, but a \
             1-item page issued {} and a 20-item page issued {}.\n\
             1-item page: {:#?}\n20-item page: {:#?}",
            one_log.count(kind),
            twenty_log.count(kind),
            one_log.per_statement(),
            twenty_log.per_statement(),
        );
    }

    // The residual. Every status on this page carries a poll, so the
    // per-poll cost is the one-item page's own count and the twenty-item
    // page pays it twenty times over.
    let per_poll = one_log.count(QueryKind::PollPerPoll);
    assert!(
        per_poll > 0,
        "the fixture must actually carry polls for this residual to be \
         measurable.\nobserved: {:#?}",
        one_log.per_statement(),
    );
    assert_eq!(
        twenty_log.count(QueryKind::PollPerPoll),
        20 * per_poll,
        "`PollServiceResolver` still resolves one poll at a time — an \
         accepted residual: on this module's three list routes, polls alone \
         stay proportional to the page's length. Batching it must strike \
         this assertion together with the note in the module doc comment."
    );
    assert_eq!(
        twenty_log.count(QueryKind::Poll),
        0,
        "and it reaches none of the batched poll lookups"
    );

    app.cleanup().await;
}
