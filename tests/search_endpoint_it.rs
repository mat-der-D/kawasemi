//! Integration tests for [`kawasemi::search::endpoint::search`], the
//! `GET /api/v2/search` HTTP handler (task 5.2 of the search spec;
//! Requirements 2.1, 2.3, 2.4, 2.5, 9.1, 9.2, 9.3).
//!
//! These tests were moved here from `src/search/endpoint/tests.rs` by
//! `.kiro/specs/test-placement-migration` task 3.3, so that steering
//! `structure.md`'s test layout rule ("DB込みの実起動インスタンスを要する検証
//! は `tests/` 直下の `*_it.rs` に置く") holds in fact and not only on paper.
//! Each one drives a real, test-only axum `Router` via
//! `tower::ServiceExt::oneshot` against a real Postgres schema spawned by
//! `kawasemi::test_harness::spawn_test_app` -- "real router, real DB, no
//! mocked auth", mirroring `tests/search_resolve_it.rs`'s and
//! `tests/search_backend_swap_it.rs`'s identical hand-built-router
//! precedent. Mounting the handler onto the *production* router is task
//! 5.3's job and is proven separately by `tests/search_module_it.rs`.
//!
//! The pure, DB-independent unit tests for `endpoint.rs`'s
//! `resolve_search_limit`/`resolve_search_offset`/`parse_search_type`/
//! `parse_optional_bool_query`/`parse_optional_account_id` helpers stay
//! where they were, in `src/search/endpoint/tests.rs` -- they need no
//! running instance. Four of the five are private to `endpoint.rs`, so
//! moving them would require widening those helpers' visibility, which
//! Requirement 3.1 forbids; `parse_optional_bool_query` is the exception,
//! being `kawasemi::api::query`'s public helper that `endpoint.rs` imports.
//!
//! Fixture plumbing (`create_test_actor`/`build_hydrator`/
//! `FakeRemoteActors`/`remote_resolver_for`/`register_test_app`/
//! `issue_test_token`) is a direct, intentional per-file copy of
//! `tests/search_service_it.rs`'s own identically named helpers -- this
//! crate's established "small, documented, intentional duplication across
//! sibling test modules" convention (see `kawasemi::search::service`'s own
//! doc comment for the same rationale).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::routing::get;
use tower::ServiceExt;

use kawasemi::accounts::DEFAULT_REMOTE_ACCOUNT_CACHE_TTL;
use kawasemi::accounts::remote_fetcher::RemoteAccountFetcher;
use kawasemi::actor::ActorDirectory;
use kawasemi::actor::owner::create_owner;
use kawasemi::actor::repository::insert_actor;
use kawasemi::actor::{ActorState, ActorType, Handle};
use kawasemi::domain::{AccountRef, Id};
use kawasemi::error::AppError;
use kawasemi::federation::signatures::{HttpResponse, MockFederationHttpClient};
use kawasemi::oauth::app_repository::{self, NewApp};
use kawasemi::oauth::middleware::AuthState;
use kawasemi::oauth::model::ScopeSet as ModelScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::search::endpoint::{SEARCH_PATH, SearchEndpointsState, search};
use kawasemi::search::hydrator::SearchHydrator;
use kawasemi::search::ports::StubSearchBackend;
use kawasemi::search::result_serializer::SearchResultSerializer;
use kawasemi::search::service::SearchService;
use kawasemi::statuses::inbound_handlers::RemoteActorResolver;
use kawasemi::statuses::ingest_service::StatusIngestService;
use kawasemi::test_harness::{TestApp, spawn_test_app};

// ==== DB-backed HTTP integration tests ======================================

type TestSearchService =
    SearchService<StubSearchBackend, MockFederationHttpClient, FakeRemoteActors, ActorDirectory>;
type TestSearchState = SearchEndpointsState<
    StubSearchBackend,
    MockFederationHttpClient,
    FakeRemoteActors,
    ActorDirectory,
>;

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
        display_name: "Search Endpoint Test Actor".to_string(),
        summary: "an actor used by the search endpoint tests".to_string(),
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

fn build_hydrator(app: &TestApp) -> SearchHydrator {
    SearchHydrator::new(
        app.pool.clone(),
        app.state.accounts().service(),
        app.state.accounts().ports(),
        app.state.media().store().clone(),
        app.state.statuses().relationship_query_registry(),
        app.runtime.clone(),
        app.state.config().server.domain.clone(),
    )
}

/// An in-memory [`RemoteActorResolver`] double -- mirrors
/// `tests/search_service_it.rs`'s `FakeRemoteActors`' identical
/// per-test-module-owned precedent.
struct FakeRemoteActors {
    runtime: kawasemi::runtime::RuntimeContext,
    by_uri: Mutex<HashMap<String, Id>>,
}

impl FakeRemoteActors {
    fn new(runtime: kawasemi::runtime::RuntimeContext) -> Self {
        Self {
            runtime,
            by_uri: Mutex::new(HashMap::new()),
        }
    }
}

impl RemoteActorResolver for FakeRemoteActors {
    async fn resolve_remote_actor(&self, actor_uri: &str) -> Result<Id, AppError> {
        let mut map = self.by_uri.lock().unwrap();
        if let Some(id) = map.get(actor_uri) {
            return Ok(*id);
        }
        let id = self.runtime.ids.next_id();
        map.insert(actor_uri.to_string(), id);
        Ok(id)
    }
}

const TEST_DOMAIN: &str = "kawasemi.example";

/// Builds a [`RemoteResolver`] against `app`'s real pool and `mock`'s
/// queued federation responses -- mirrors `tests/search_service_it.rs`'s
/// `resolver_for`.
fn remote_resolver_for(
    app: &TestApp,
    mock: Arc<MockFederationHttpClient>,
) -> kawasemi::search::remote_resolver::RemoteResolver<
    MockFederationHttpClient,
    FakeRemoteActors,
    ActorDirectory,
> {
    let account_fetcher = Arc::new(RemoteAccountFetcher::new(
        app.pool.clone(),
        Arc::clone(&mock),
        app.runtime.clone(),
        DEFAULT_REMOTE_ACCOUNT_CACHE_TTL,
    ));
    let status_ingest = Arc::new(StatusIngestService::new(
        app.pool.clone(),
        Arc::clone(&mock),
        app.runtime.clone(),
        Arc::new(FakeRemoteActors::new(app.runtime.clone())),
        TEST_DOMAIN,
        ActorDirectory::new(app.pool.clone()),
    ));
    kawasemi::search::remote_resolver::RemoteResolver::new(mock, account_fetcher, status_ingest)
}

/// Builds the [`SearchEndpointsState`] these tests drive requests through.
fn build_state(
    app: &TestApp,
    mock: Arc<MockFederationHttpClient>,
    backend: StubSearchBackend,
) -> TestSearchState {
    let service: TestSearchService = SearchService::new(
        backend,
        remote_resolver_for(app, mock),
        build_hydrator(app),
        SearchResultSerializer::new(),
    );
    let auth = AuthState {
        pool: app.pool.clone(),
        token_hash_key: app.state.config().oauth.token_hash_key.clone(),
    };
    SearchEndpointsState {
        search_service: Arc::new(service),
        auth,
    }
}

/// Mounts [`search`] onto a test-only router -- search spec task 5.2's own
/// boundary explicitly forbade mounting it onto the real production router
/// (task 5.3's job, proven by `tests/search_module_it.rs`), so every test in
/// this file dispatches through this router directly via
/// `tower::ServiceExt::oneshot`.
fn build_router(state: TestSearchState) -> Router {
    Router::new()
        .route(
            SEARCH_PATH,
            get(search::<
                StubSearchBackend,
                MockFederationHttpClient,
                FakeRemoteActors,
                ActorDirectory,
            >),
        )
        .with_state(state)
}

/// Registers a real `oauth_applications` row, returning its `Id` -- mirrors
/// `notifications::endpoints::tests::register_test_app`.
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
            name: "Search Endpoint Test Client".to_string(),
            redirect_uris: vec!["https://client.example/callback".to_string()],
            scopes: ModelScopeSet::new(["read", "write"]),
        },
    )
    .await
    .expect("register_app must succeed");
    registered.id
}

/// Issues a real access token bound to `actor_id` with `scopes`, returning
/// its plaintext bearer value -- mirrors `notifications::endpoints::tests::
/// issue_test_token`. Never hand-constructs a `RequestActorContext`.
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
    path_and_query: &str,
    token: Option<&str>,
) -> axum::http::Response<Body> {
    let mut builder = Request::builder().method("GET").uri(path_and_query);
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

fn empty_ok_response() -> HttpResponse {
    HttpResponse {
        status: StatusCode::OK,
        headers: HeaderMap::new(),
        body: b"{}".to_vec(),
    }
}

// ---- 9.1, 2.4: auth/scope -------------------------------------------------

/// Requirement 9.1/2.4: a request with no bearer token at all is `401`,
/// before any scope check or `SearchParams` construction runs.
#[tokio::test]
async fn search_without_a_bearer_token_is_401() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    let router = build_router(build_state(&app, mock, StubSearchBackend::new()));

    let response = dispatch(&router, &format!("{SEARCH_PATH}?q=hello"), None).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    app.cleanup().await;
}

/// Requirement 9.1/2.4: an authenticated request whose token lacks
/// `read:search` is `403`.
#[tokio::test]
async fn search_with_insufficient_scope_is_403() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    let router = build_router(build_state(&app, mock, StubSearchBackend::new()));

    let app_id = register_test_app(&app).await;
    let viewer = create_test_actor(&app, "scope_403_viewer").await;
    // `read:accounts` deliberately omits `read:search`.
    let token = issue_test_token(&app, app_id, viewer, &["read:accounts"]).await;

    let response = dispatch(&router, &format!("{SEARCH_PATH}?q=hello"), Some(&token)).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    app.cleanup().await;
}

// ---- 2.3: empty query -------------------------------------------------

/// Requirement 2.3: an authenticated, correctly-scoped request with an
/// empty `q` is `422` (delegated to, not reimplemented by, this handler --
/// `SearchService::search`'s own `parse_query` rejection).
#[tokio::test]
async fn search_with_empty_query_is_422() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    let router = build_router(build_state(&app, mock, StubSearchBackend::new()));

    let app_id = register_test_app(&app).await;
    let viewer = create_test_actor(&app, "empty_422_viewer").await;
    let token = issue_test_token(&app, app_id, viewer, &["read:search"]).await;

    let response = dispatch(&router, &format!("{SEARCH_PATH}?q="), Some(&token)).await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

    // Omitting `q` entirely behaves identically -- an absent query param
    // defaults to an empty string, not a distinct "missing" rejection.
    let response = dispatch(&router, SEARCH_PATH, Some(&token)).await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

    app.cleanup().await;
}

// ---- 2.1, 9.2: happy path + SearchResults shape ----------------------------

/// Requirement 2.1/9.2: an authenticated, correctly-scoped request with a
/// non-matching `q` still returns `200` and a well-formed SearchResults
/// envelope (`accounts`/`statuses`/`hashtags`, each `[]` not `null` --
/// Requirement 1.4, already proven by `search::result_serializer`, this
/// test proves the endpoint wires the whole pipeline together, not that
/// requirement's own JSON shape rules again).
#[tokio::test]
async fn search_returns_search_results_shape_for_an_authenticated_request() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    let router = build_router(build_state(&app, mock, StubSearchBackend::new()));

    let app_id = register_test_app(&app).await;
    let viewer = create_test_actor(&app, "shape_ok_viewer").await;
    let token = issue_test_token(&app, app_id, viewer, &["read:search"]).await;

    let response = dispatch(
        &router,
        &format!("{SEARCH_PATH}?q=nothing-will-match-this"),
        Some(&token),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["accounts"], serde_json::json!([]));
    assert_eq!(body["statuses"], serde_json::json!([]));
    assert_eq!(body["hashtags"], serde_json::json!([]));

    app.cleanup().await;
}

/// Requirement 2.1: a query that genuinely matches an account (via
/// `StubSearchBackend`) is threaded all the way through to a populated
/// `accounts` array in the response -- proves `SearchParams`/`viewer` are
/// built correctly from the authenticated request, not just that an empty
/// result renders correctly.
#[tokio::test]
async fn search_returns_a_matching_account_end_to_end() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    let target = create_test_actor(&app, "target_match").await;
    let backend = StubSearchBackend::new().with_account(
        AccountRef::Local(target),
        "Target Match target_match@kawasemi.example",
    );
    let router = build_router(build_state(&app, mock, backend));

    let app_id = register_test_app(&app).await;
    let viewer = create_test_actor(&app, "match_e2e_viewer").await;
    let token = issue_test_token(&app, app_id, viewer, &["read:search"]).await;

    let response = dispatch(
        &router,
        &format!("{SEARCH_PATH}?q=target_match&type=accounts"),
        Some(&token),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let accounts = body["accounts"]
        .as_array()
        .expect("accounts must be an array");
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0]["id"], target.as_i64().to_string());
    assert_eq!(body["statuses"], serde_json::json!([]));
    assert_eq!(body["hashtags"], serde_json::json!([]));

    app.cleanup().await;
}

// ---- 9.3, 2.5: type/limit/offset wiring -----------------------------------

/// Requirement 2.2: `type=hashtags` scopes the search away from an
/// otherwise-matching account -- proves the `type` query parameter is
/// actually parsed and threaded into `SearchParams.kind`.
#[tokio::test]
async fn search_type_hashtags_excludes_a_matching_account() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    let target = create_test_actor(&app, "type_scoped_target").await;
    let backend = StubSearchBackend::new().with_account(
        AccountRef::Local(target),
        "Type Scoped Target type_scoped@kawasemi.example",
    );
    let router = build_router(build_state(&app, mock, backend));

    let app_id = register_test_app(&app).await;
    let viewer = create_test_actor(&app, "type_scoped_viewer").await;
    let token = issue_test_token(&app, app_id, viewer, &["read:search"]).await;

    let response = dispatch(
        &router,
        &format!("{SEARCH_PATH}?q=type_scoped&type=hashtags"),
        Some(&token),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["accounts"], serde_json::json!([]));

    app.cleanup().await;
}

/// Requirement 2.5/9.3: an unrecognized `type` value is `422`.
#[tokio::test]
async fn search_with_unrecognized_type_is_422() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    let router = build_router(build_state(&app, mock, StubSearchBackend::new()));

    let app_id = register_test_app(&app).await;
    let viewer = create_test_actor(&app, "bad_type_viewer").await;
    let token = issue_test_token(&app, app_id, viewer, &["read:search"]).await;

    let response = dispatch(
        &router,
        &format!("{SEARCH_PATH}?q=hello&type=bogus"),
        Some(&token),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

    app.cleanup().await;
}

/// Requirement 2.5/9.3: `limit`/`offset` query parameters are parsed and
/// threaded through to the `SearchBackend` call -- reuses
/// `StubSearchBackend`'s own already-proven `limit`/`offset` pagination
/// arithmetic (task 1.4, reviewed) as the observable effect: two matching
/// accounts registered in order, `limit=1&offset=1` returns only the
/// second one.
#[tokio::test]
async fn search_limit_and_offset_are_extracted_and_threaded_through() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    let first = create_test_actor(&app, "paginated_first").await;
    let second = create_test_actor(&app, "paginated_second").await;
    let backend = StubSearchBackend::new()
        .with_account(
            AccountRef::Local(first),
            "Paginated Target paginated_one@kawasemi.example",
        )
        .with_account(
            AccountRef::Local(second),
            "Paginated Target paginated_two@kawasemi.example",
        );
    let router = build_router(build_state(&app, mock, backend));

    let app_id = register_test_app(&app).await;
    let viewer = create_test_actor(&app, "pagination_viewer").await;
    let token = issue_test_token(&app, app_id, viewer, &["read:search"]).await;

    let response = dispatch(
        &router,
        &format!("{SEARCH_PATH}?q=Paginated+Target&type=accounts&limit=1&offset=1"),
        Some(&token),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let accounts = body["accounts"]
        .as_array()
        .expect("accounts must be an array");
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0]["id"], second.as_i64().to_string());

    app.cleanup().await;
}

// ---- account_id / boolean malformed values ---------------------------------

/// A malformed `account_id` (non-numeric) is `422` (see this module's own
/// doc comment, "`account_id`").
#[tokio::test]
async fn search_with_malformed_account_id_is_422() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    let router = build_router(build_state(&app, mock, StubSearchBackend::new()));

    let app_id = register_test_app(&app).await;
    let viewer = create_test_actor(&app, "bad_account_id_viewer").await;
    let token = issue_test_token(&app, app_id, viewer, &["read:search"]).await;

    let response = dispatch(
        &router,
        &format!("{SEARCH_PATH}?q=hello&account_id=not-an-id"),
        Some(&token),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

    app.cleanup().await;
}

/// A malformed `resolve` boolean is `422` (see this module's own doc
/// comment, "Boolean parsing").
#[tokio::test]
async fn search_with_malformed_resolve_boolean_is_422() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    let router = build_router(build_state(&app, mock, StubSearchBackend::new()));

    let app_id = register_test_app(&app).await;
    let viewer = create_test_actor(&app, "bad_resolve_viewer").await;
    let token = issue_test_token(&app, app_id, viewer, &["read:search"]).await;

    let response = dispatch(
        &router,
        &format!("{SEARCH_PATH}?q=hello&resolve=maybe"),
        Some(&token),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

    app.cleanup().await;
}

// Silences an "unused function" warning in configurations where the
// `empty_ok_response` fixture ends up unused (kept for parity with
// `tests/search_service_it.rs`'s own fixture surface, available for a future
// test needing a queued federation response without recreating it).
#[allow(dead_code)]
fn _touch_empty_ok_response() -> HttpResponse {
    empty_ok_response()
}
