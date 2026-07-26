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
            DeliveryService::new(
                RecipientTargetResolver::new(MockLocalActorLookup::with_handles(&handles)),
                Arc::clone(&local_sink),
                Arc::clone(&http_sink),
            ),
        ),
        relationship.clone(),
        actor_lookup.clone(),
    ));

    let interaction_service = Arc::new(InteractionService::new(
        app.pool.clone(),
        app.runtime.clone(),
        urls.clone(),
        StatusActivityBuilder::new(
            urls.clone(),
            Arc::clone(&ids),
            actor_lookup.clone(),
            DeliveryService::new(
                RecipientTargetResolver::new(MockLocalActorLookup::with_handles(&handles)),
                Arc::clone(&local_sink),
                Arc::clone(&http_sink),
            ),
        ),
        actor_lookup.clone(),
        relationship.clone(),
    ));

    let poll_service = Arc::new(PollService::new(
        app.pool.clone(),
        app.runtime.clone(),
        urls.clone(),
        StatusActivityBuilder::new(
            urls,
            ids,
            actor_lookup.clone(),
            DeliveryService::new(
                RecipientTargetResolver::new(MockLocalActorLookup::with_handles(&handles)),
                Arc::clone(&local_sink),
                Arc::clone(&http_sink),
            ),
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
