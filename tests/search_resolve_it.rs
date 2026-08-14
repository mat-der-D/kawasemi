//! Full-HTTP-pipeline integration tests for `acct:`/URL remote resolution
//! (search spec task 6.3, `.kiro/specs/search/tasks.md`, "6.3 (P) リモート解
//! 決・type/スコープの統合テスト", `_Depends: 5.3_`), Requirements 2.4, 6.1,
//! 6.2, 6.3, 6.4, 6.5, 9.1, 9.2. design.md's File Structure Plan names this
//! exact file (`tests/search_resolve_it.rs`, "acct:/URL リモート解決
//! （resolve=true 認証時のみ・WebFinger モック・取得失敗除外）（統合,
//! FederationHttpClient モック）").
//!
//! ## Why this file does not drive through `crate::server::build_router`
//! (unlike `tests/search_contract_it.rs`/`tests/search_accounts_it.rs`/
//! `tests/search_statuses_it.rs`/`tests/search_hashtags_it.rs`)
//! `crate::search::build_search_module` (task 5.3, `src/search.rs`) wires
//! the real, production `AppState`'s one `SearchService` instantiation to a
//! real `ReqwestFederationHttpClient` unconditionally -- confirmed by
//! reading `src/search.rs`'s own `ProdSearchService` type alias
//! (`SearchService<PgSearchBackend, ReqwestFederationHttpClient,
//! ProdRemoteActorResolver, ActorDirectory>`) and `src/server.rs`'s own
//! `type SeH = ReqwestFederationHttpClient;` -- with no test-time swap point
//! anywhere in `crate::test_harness::spawn_test_app` either: that function
//! calls this exact same production `search::build_search_module` wiring
//! (`src/test_harness.rs`, "Assembles the search module bundle (task 5.3)
//! the same way `bootstrap()`'s production path does"). Driving a
//! `resolve=true` request through `server::build_router(app.state.clone())`
//! would therefore issue a genuine outbound HTTPS request to whatever
//! `acct:`/URL domain a test supplies -- not deterministic, not
//! sandbox-safe, and not what this task's own dispatch brief asks for
//! ("`FederationHttpClient` モック").
//!
//! This file therefore follows `tests/search_endpoint_it.rs`'s/
//! `tests/search_service_it.rs`'s/`src/search/remote_resolver/tests.rs`'s own
//! already-reviewed, established convention exactly (the first two were
//! themselves moved out of `src/search/endpoint/tests.rs`/
//! `src/search/service/tests.rs` by
//! `.kiro/specs/test-placement-migration` tasks 3.1/3.2):
//! a small, test-only axum `Router` mounts the real
//! `crate::search::endpoint::search` handler directly, closing over a
//! `SearchEndpointsState<PgSearchBackend, MockFederationHttpClient,
//! FakeRemoteActors, ActorDirectory>` built from `spawn_test_app`'s real
//! pool/runtime/accounts/media/statuses collaborators plus a
//! `MockFederationHttpClient` this file controls. Every layer this test
//! exercises is still the real production code -- real Bearer/`read:search`
//! enforcement (`crate::oauth::middleware`), real query-string parsing
//! (`crate::search::endpoint`), the real `crate::search::service::
//! SearchService::search` pipeline, the real default `PgSearchBackend`
//! (task 3.1, so a locally-known match and a remotely-resolved one are
//! proven to travel through the identical response, not a stub), the real
//! `RemoteResolver` (task 4.3), and the real `SearchHydrator` (task 4.2) --
//! only the one non-deterministic network boundary
//! (`FederationHttpClient::fetch`) is swapped for a queued, deterministic
//! double, exactly `src/search/remote_resolver/tests.rs`'s own established
//! reasoning for testing this exact seam.
//!
//! ## Fixture plumbing duplication
//! `FakeRemoteActors`/`ok_response`/`jrd_document`/`actor_document`/
//! `note_document`/`register_test_app`/`issue_test_token` are direct,
//! intentional per-file copies of `src/search/remote_resolver/tests.rs`'s/
//! `tests/search_service_it.rs`'s/`tests/search_endpoint_it.rs`'s own
//! identically named helpers -- each `tests/*.rs` file is its own compiled
//! crate (cannot import another test file's private items), and this
//! crate's own established convention is exactly this kind of small,
//! documented, intentional duplication across sibling test modules (see any
//! of those three files' own doc comments for the identical rationale).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::routing::get;
use serde_json::{Value, json};
use tower::ServiceExt;

use kawasemi::accounts::DEFAULT_REMOTE_ACCOUNT_CACHE_TTL;
use kawasemi::accounts::remote_fetcher::RemoteAccountFetcher;
use kawasemi::accounts::remote_repository::find_remote_by_uri;
use kawasemi::actor::ActorDirectory;
use kawasemi::domain::Id;
use kawasemi::error::AppError;
use kawasemi::federation::signatures::{HttpResponse, MockFederationHttpClient};
use kawasemi::oauth::app_repository::{self, NewApp};
use kawasemi::oauth::middleware::AuthState;
use kawasemi::oauth::model::ScopeSet as ModelScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::search::endpoint::{SEARCH_PATH, SearchEndpointsState, search};
use kawasemi::search::hydrator::SearchHydrator;
use kawasemi::search::pg_backend::PgSearchBackend;
use kawasemi::search::remote_resolver::RemoteResolver;
use kawasemi::search::result_serializer::SearchResultSerializer;
use kawasemi::search::service::SearchService;
use kawasemi::statuses::inbound_handlers::RemoteActorResolver;
use kawasemi::statuses::ingest_service::StatusIngestService;
use kawasemi::statuses::status_repository::find_by_uri;
use kawasemi::test_harness::{TestApp, spawn_test_app};

// ---- Fixture plumbing (per-file duplication, see this file's own doc
// comment) --------------------------------------------------------------

/// An in-memory-cached [`RemoteActorResolver`] double -- unlike `crate::
/// search::remote_resolver::tests::FakeRemoteActors`'s own identical-named
/// precedent (which never persists anything, sufficient for that module's
/// own tests since they only assert against `status_repository`/
/// `remote_repository` directly), this file's copy additionally persists a
/// minimal, real `remote_accounts` row for every `actor_uri` it resolves
/// (mirroring `tests/search_accounts_it.rs::insert_remote_account`'s own
/// established minimal-fixture-row convention). This is required here,
/// specifically, because this file's own tests drive requests through the
/// real `SearchHydrator` (unlike `remote_resolver::tests`'s own narrower
/// scope): a `Note`'s `attributedTo` author resolved by this double must be
/// a genuine, renderable Account row, or the real Account-rendering path a
/// hydrated Status's `account` field depends on 404s downstream.
struct FakeRemoteActors {
    runtime: kawasemi::runtime::RuntimeContext,
    pool: sqlx::PgPool,
    by_uri: Mutex<HashMap<String, Id>>,
}

impl FakeRemoteActors {
    fn new(runtime: kawasemi::runtime::RuntimeContext, pool: sqlx::PgPool) -> Self {
        Self {
            runtime,
            pool,
            by_uri: Mutex::new(HashMap::new()),
        }
    }
}

impl RemoteActorResolver for FakeRemoteActors {
    async fn resolve_remote_actor(&self, actor_uri: &str) -> Result<Id, AppError> {
        if let Some(id) = self.by_uri.lock().unwrap().get(actor_uri) {
            return Ok(*id);
        }
        let id = self.runtime.ids.next_id();
        sqlx::query(
            "INSERT INTO remote_accounts (id, actor_uri, username, domain, display_name, url, \
             fetched_at) VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(id.as_i64())
        .bind(actor_uri)
        .bind(format!("fakeactor{}", id.as_i64()))
        .bind("remote.example")
        .bind("Fake Remote Actor")
        .bind(actor_uri)
        .bind(self.runtime.clock.now())
        .execute(&self.pool)
        .await
        .map_err(|err| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
        self.by_uri
            .lock()
            .unwrap()
            .insert(actor_uri.to_string(), id);
        Ok(id)
    }
}

const ALICE_ACTOR_URI: &str = "https://remote.example/users/alice";
const NOTE_URI: &str = "https://remote.example/notes/1";
const TEST_DOMAIN: &str = "kawasemi.example";

fn ok_response(body: Value) -> HttpResponse {
    HttpResponse {
        status: StatusCode::OK,
        headers: HeaderMap::new(),
        body: serde_json::to_vec(&body).expect("test fixture body must serialize"),
    }
}

fn jrd_document(actor_uri: &str) -> Value {
    json!({
        "subject": "acct:alice@remote.example",
        "links": [
            {"rel": "self", "type": "application/activity+json", "href": actor_uri}
        ]
    })
}

fn actor_document(actor_uri: &str) -> Value {
    json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": actor_uri,
        "type": "Person",
        "preferredUsername": "alice",
        "name": "Alice Example",
    })
}

fn note_document(uri: &str, attributed_to: &str) -> Value {
    json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": uri,
        "type": "Note",
        "attributedTo": attributed_to,
        "content": "hello from remote.example",
        "to": ["https://www.w3.org/ns/activitystreams#Public"],
    })
}

/// Builds a real [`RemoteResolver`] against `app`'s real pool/runtime and
/// `mock`'s queued federation responses -- mirrors `crate::search::
/// remote_resolver::tests::resolver_for`.
fn remote_resolver_for(
    app: &TestApp,
    mock: Arc<MockFederationHttpClient>,
) -> RemoteResolver<MockFederationHttpClient, FakeRemoteActors, ActorDirectory> {
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
        Arc::new(FakeRemoteActors::new(app.runtime.clone(), app.pool.clone())),
        TEST_DOMAIN,
        ActorDirectory::new(app.pool.clone()),
    ));
    RemoteResolver::new(mock, account_fetcher, status_ingest)
}

/// Builds a real [`SearchHydrator`] from `app`'s real accounts/media/
/// statuses collaborators -- mirrors
/// `tests/search_endpoint_it.rs::build_hydrator`.
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

type TestSearchService =
    SearchService<PgSearchBackend, MockFederationHttpClient, FakeRemoteActors, ActorDirectory>;
type TestSearchState = SearchEndpointsState<
    PgSearchBackend,
    MockFederationHttpClient,
    FakeRemoteActors,
    ActorDirectory,
>;

/// Builds the [`SearchEndpointsState`] these tests drive requests through --
/// the real default [`PgSearchBackend`] (task 3.1) plus the mocked
/// federation boundary (see this file's own doc comment).
fn build_state(app: &TestApp, mock: Arc<MockFederationHttpClient>) -> TestSearchState {
    let backend = PgSearchBackend::new(app.pool.clone(), app.runtime.clone());
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

/// Mounts [`search`] onto a test-only router -- this task's own boundary
/// explicitly forbids mounting a mocked-federation-client instance onto the
/// real production router (see this file's own doc comment).
fn build_router(state: TestSearchState) -> Router {
    Router::new()
        .route(
            SEARCH_PATH,
            get(search::<
                PgSearchBackend,
                MockFederationHttpClient,
                FakeRemoteActors,
                ActorDirectory,
            >),
        )
        .with_state(state)
}

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
            name: "Search Resolve IT Client".to_string(),
            redirect_uris: vec!["https://client.example/callback".to_string()],
            scopes: ModelScopeSet::new(["read", "write"]),
        },
    )
    .await
    .expect("register_app must succeed");
    registered.id
}

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
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method("GET").uri(path_and_query);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let request = builder
        .body(Body::empty())
        .expect("building the test request must succeed");
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("dispatching the test request must succeed");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("reading the response body must succeed");
    let value: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("response body must be valid JSON")
    };
    (status, value)
}

/// Issues a real `read:search`-scoped viewer token against a fresh actor,
/// returning it -- every test below needs an authenticated caller for its
/// actual resolve scenario.
async fn searcher_token(app: &TestApp, handle: &str) -> String {
    let owner_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    kawasemi::actor::owner::create_owner(&app.pool, owner_id, now)
        .await
        .expect("creating the owner fixture must succeed");
    let actor_id = app.runtime.ids.next_id();
    let mut tx = app
        .pool
        .begin()
        .await
        .expect("opening a transaction must succeed");
    kawasemi::actor::repository::insert_actor(
        &mut tx,
        &kawasemi::actor::model::LocalActor {
            id: actor_id,
            owner_id,
            handle: kawasemi::actor::Handle::new(handle).expect("test handle must be valid"),
            actor_type: kawasemi::actor::ActorType::Person,
            display_name: "Search Resolve IT Searcher".to_string(),
            summary: "an actor used by the search_resolve_it integration test".to_string(),
            state: kawasemi::actor::ActorState::Active,
            created_at: now,
            updated_at: now,
        },
    )
    .await
    .expect("insert_actor must succeed");
    tx.commit().await.expect("committing must succeed");

    let app_id = register_test_app(app).await;
    issue_test_token(app, app_id, actor_id, &["read:search"]).await
}

// ==========================================================================
// Requirement 6.1: `acct:user@domain` + `resolve=true` + authenticated ->
// WebFinger (mocked) -> `RemoteAccountFetcher::fetch_and_normalize` -> a
// genuinely persisted remote account, surfaced in `accounts`.
// ==========================================================================

#[tokio::test]
async fn resolve_true_authenticated_acct_resolves_to_a_persisted_remote_account() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    mock.queue_fetch_response(ok_response(jrd_document(ALICE_ACTOR_URI)));
    mock.queue_fetch_response(ok_response(actor_document(ALICE_ACTOR_URI)));
    let router = build_router(build_state(&app, mock));
    let token = searcher_token(&app, "resolve_acct_searcher").await;

    let (status, body) = dispatch(
        &router,
        &format!("{SEARCH_PATH}?q=acct:alice@remote.example&resolve=true"),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {body:?}");

    let accounts = body["accounts"]
        .as_array()
        .expect("accounts must be an array");
    assert_eq!(accounts.len(), 1, "the resolved remote account must appear");
    assert_eq!(body["statuses"], json!([]));
    assert_eq!(body["hashtags"], json!([]));

    let persisted = find_remote_by_uri(&app.pool, ALICE_ACTOR_URI)
        .await
        .expect("find_remote_by_uri must succeed")
        .expect("fetch_and_normalize must have upserted the remote account");
    assert_eq!(accounts[0]["id"], persisted.id.as_i64().to_string());

    app.cleanup().await;
}

// ==========================================================================
// Requirement 6.2: a URL resolving to a `Note` + `resolve=true` +
// authenticated -> federation fetch + JSON-LD safe expansion ->
// `StatusIngestService::ingest_document` -> a genuinely persisted status,
// surfaced in `statuses`.
// ==========================================================================

#[tokio::test]
async fn resolve_true_authenticated_url_resolves_to_a_persisted_status() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    mock.queue_fetch_response(ok_response(note_document(NOTE_URI, ALICE_ACTOR_URI)));
    let router = build_router(build_state(&app, mock));
    let token = searcher_token(&app, "resolve_url_note_searcher").await;

    let (status, body) = dispatch(
        &router,
        &format!("{SEARCH_PATH}?q={NOTE_URI}&resolve=true"),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {body:?}");

    let statuses = body["statuses"]
        .as_array()
        .expect("statuses must be an array");
    assert_eq!(statuses.len(), 1, "the resolved remote status must appear");
    assert_eq!(body["accounts"], json!([]));
    assert_eq!(body["hashtags"], json!([]));

    let persisted = find_by_uri(&app.pool, NOTE_URI)
        .await
        .expect("find_by_uri must succeed")
        .expect("ingest_document must have persisted the Note as a Status");
    assert_eq!(statuses[0]["id"], persisted.id.as_i64().to_string());

    app.cleanup().await;
}

// ==========================================================================
// Requirement 6.2: a URL resolving to an Actor document + `resolve=true` +
// authenticated -> normalizes via `RemoteAccountFetcher::
// fetch_and_normalize` (a deliberate double fetch, see `remote_resolver.rs`'s
// own doc comment) -> `accounts`.
// ==========================================================================

#[tokio::test]
async fn resolve_true_authenticated_actor_url_resolves_to_a_persisted_account() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    mock.queue_fetch_response(ok_response(actor_document(ALICE_ACTOR_URI)));
    mock.queue_fetch_response(ok_response(actor_document(ALICE_ACTOR_URI)));
    let router = build_router(build_state(&app, mock));
    let token = searcher_token(&app, "resolve_url_actor_searcher").await;

    let (status, body) = dispatch(
        &router,
        &format!("{SEARCH_PATH}?q={ALICE_ACTOR_URI}&resolve=true"),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {body:?}");

    let accounts = body["accounts"]
        .as_array()
        .expect("accounts must be an array");
    assert_eq!(accounts.len(), 1);

    let persisted = find_remote_by_uri(&app.pool, ALICE_ACTOR_URI)
        .await
        .expect("find_remote_by_uri must succeed")
        .expect("fetch_and_normalize must have upserted the remote account");
    assert_eq!(accounts[0]["id"], persisted.id.as_i64().to_string());

    app.cleanup().await;
}

// ==========================================================================
// Requirement 6.4: a WebFinger fetch failure excludes the target from
// results without failing the request (200, empty array, not null).
// ==========================================================================

#[tokio::test]
async fn resolve_true_webfinger_fetch_failure_excludes_the_target_and_still_returns_200() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    mock.queue_fetch_error(StatusCode::BAD_GATEWAY, "network unreachable");
    let router = build_router(build_state(&app, mock));
    let token = searcher_token(&app, "resolve_fail_searcher").await;

    let (status, body) = dispatch(
        &router,
        &format!("{SEARCH_PATH}?q=acct:alice@remote.example&resolve=true"),
        Some(&token),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a remote-resolution failure must not fail the request"
    );
    assert_eq!(body["accounts"], json!([]));
    assert_ne!(
        body["accounts"],
        Value::Null,
        "must be [] not null (Requirement 1.4)"
    );

    app.cleanup().await;
}

// ==========================================================================
// Requirement 6.4 + the origin/authority anchor (`check_fetched_host`,
// `remote_resolver.rs`'s own doc comment): a self-consistent-but-wrong-host
// `Note` must be excluded and never persisted -- the cross-host content-
// spoofing scenario named in this spec's own Implementation Notes (task
// 4.3's security-fix note).
// ==========================================================================

#[tokio::test]
async fn resolve_true_cross_host_note_is_excluded_and_never_persisted() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    const CROSS_HOST_NOTE_URI: &str = "https://victim.example/notes/1";
    const CROSS_HOST_ATTRIBUTED_TO: &str = "https://victim.example/users/alice";
    // Fetched from `remote.example` (`NOTE_URI`), but the document's own
    // `id`/`attributedTo` both claim `victim.example` -- internally
    // self-consistent, but not with the URL actually dereferenced.
    mock.queue_fetch_response(ok_response(note_document(
        CROSS_HOST_NOTE_URI,
        CROSS_HOST_ATTRIBUTED_TO,
    )));
    let router = build_router(build_state(&app, mock));
    let token = searcher_token(&app, "resolve_cross_host_searcher").await;

    let (status, body) = dispatch(
        &router,
        &format!("{SEARCH_PATH}?q={NOTE_URI}&resolve=true"),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {body:?}");
    assert_eq!(body["statuses"], json!([]));

    let persisted = find_by_uri(&app.pool, CROSS_HOST_NOTE_URI)
        .await
        .expect("find_by_uri must succeed");
    assert!(
        persisted.is_none(),
        "a cross-host document must never be ingested into the statuses table"
    );

    app.cleanup().await;
}

// ==========================================================================
// Requirements 6.3, 6.5: the *same* authenticated request, differing only in
// `resolve`, genuinely branches -- `resolve` absent (defaults to false)
// never triggers a federation fetch and stays local-only; `resolve=true`
// (queued responses) does. This is the "control comparison" this task's own
// dispatch brief asks for.
// ==========================================================================

#[tokio::test]
async fn resolve_false_or_absent_never_fetches_remotely_while_resolve_true_does() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    let router = build_router(build_state(&app, Arc::clone(&mock)));
    let token = searcher_token(&app, "resolve_control_searcher").await;

    // -- Control: `resolve` omitted entirely (defaults to false). --
    let (status, body) = dispatch(
        &router,
        &format!("{SEARCH_PATH}?q=acct:alice@remote.example"),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {body:?}");
    assert_eq!(body["accounts"], json!([]));
    assert!(
        mock.fetched_urls().is_empty(),
        "resolve omitted (Requirement 6.3) must never make a federation fetch"
    );

    // -- Control: `resolve=false` explicitly. --
    let (status, body) = dispatch(
        &router,
        &format!("{SEARCH_PATH}?q=acct:alice@remote.example&resolve=false"),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {body:?}");
    assert_eq!(body["accounts"], json!([]));
    assert!(
        mock.fetched_urls().is_empty(),
        "resolve=false (Requirement 6.3) must never make a federation fetch"
    );

    // -- The same query, `resolve=true`: genuinely branches. --
    mock.queue_fetch_response(ok_response(jrd_document(ALICE_ACTOR_URI)));
    mock.queue_fetch_response(ok_response(actor_document(ALICE_ACTOR_URI)));
    let (status, body) = dispatch(
        &router,
        &format!("{SEARCH_PATH}?q=acct:alice@remote.example&resolve=true"),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {body:?}");
    assert_eq!(
        body["accounts"]
            .as_array()
            .expect("accounts must be an array")
            .len(),
        1,
        "resolve=true must genuinely resolve and surface the remote account"
    );
    assert!(
        !mock.fetched_urls().is_empty(),
        "resolve=true must actually make a federation fetch"
    );

    app.cleanup().await;
}

// ==========================================================================
// Requirement 6.5: an unauthenticated request never reaches remote
// resolution at all -- the Bearer/`read:search` gate (Requirement 9.1)
// rejects the request with 401 before `SearchService`/`RemoteResolver` ever
// runs, so no federation fetch is made even though `resolve=true` is
// present.
// ==========================================================================

#[tokio::test]
async fn unauthenticated_resolve_true_request_is_401_and_never_triggers_a_fetch() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    let router = build_router(build_state(&app, Arc::clone(&mock)));

    let (status, _body) = dispatch(
        &router,
        &format!("{SEARCH_PATH}?q=acct:alice@remote.example&resolve=true"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(
        mock.fetched_urls().is_empty(),
        "an unauthenticated resolve=true request must never make a federation fetch \
         (Requirement 6.5)"
    );

    app.cleanup().await;
}
