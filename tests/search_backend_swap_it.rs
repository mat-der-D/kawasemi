//! Full-HTTP-pipeline integration test proving `SearchBackend`'s engine-swap
//! boundary (search spec task 6.4, `.kiro/specs/search/tasks.md`, "6.4 (P)
//! 検索バックエンド差し替えの統合テスト", `_Depends: 5.3_`), Requirements 7.1,
//! 7.3, 7.4, 7.5, 8.4. design.md's File Structure Plan names this exact file
//! (`tests/search_backend_swap_it.rs`, "SearchBackend をスタブ実装へ差し替え
//! ても呼び出し側/結果組み立てが不変（統合）").
//!
//! ## Why this cannot drive the swap through `crate::server::build_router`
//! (unlike `tests/search_contract_it.rs`/`tests/search_type_scope_it.rs`)
//! `SearchBackend`'s `async fn` methods make it non-`dyn`-compatible (see
//! `src/search/ports.rs`'s own doc comment, "`async fn` in trait, not boxed
//! futures"), so `crate::search::build_search_module` (task 5.3, `src/
//! search.rs`) concretizes `SearchService`'s `B` type parameter to
//! `PgSearchBackend` exactly once, at compile time, via its `ProdSearchService`
//! type alias -- and `AppState`/`Router<AppState>` cannot themselves be
//! generic (axum's `State<S>`/`FromRef<AppState>` both need one concrete
//! `AppState`). There is therefore no live, request-time toggle inside the
//! already-booted production `AppState` this file could flip; the actual
//! "differing point" (design.md's task 5.3 own text, "差し替え点を1箇所に
//! 集約する") is the *construction* of that one `SearchService` instance
//! inside `build_search_module`'s body -- specifically the single line `let
//! backend = PgSearchBackend::new(pool, runtime);` immediately handed to
//! `SearchService::new(backend, ..)`. This file's own
//! [`build_service_with_backend`] below is a **byte-for-byte mirror of
//! `crate::search::build_search_module`'s own body** (confirmed by reading
//! that function in full: same collaborator construction calls, same
//! argument order, same concrete `H`/`R`/`M` triple --
//! `ReqwestFederationHttpClient`/`ProdRemoteActorResolver`/`ActorDirectory`,
//! the exact production choices, never a mock/fake) with exactly one
//! structural difference: it takes `backend: B` as a parameter instead of
//! constructing `PgSearchBackend` itself. A reviewer can diff the two
//! functions side by side and confirm the only line that differs is the
//! backend's own construction -- proving the swap point really is exactly
//! one line, not scattered wiring, without this file hand-rolling a parallel
//! ad-hoc harness that bypasses `SearchService`/`SearchHydrator`/
//! `RemoteResolver`/`SearchResultSerializer`'s real production construction.
//! [`stub_router`]/[`pg_router`] then mount the identical, unmodified
//! `crate::search::endpoint::search` handler (task 5.2, never touched by
//! this task) via the identical, unmodified `SearchEndpointsState<B, H, R,
//! M>` (task 5.2) onto a small per-file router -- the same technique `src/
//! search/endpoint/tests.rs`'s own `build_router` already established for
//! testing this handler with a substituted backend, just reused here at the
//! full-pipeline (real production `RemoteResolver`/`SearchHydrator`, real
//! `TestApp`-backed Postgres data) rather than mocked-collaborator level.
//!
//! ## What is proven here
//! 1. [`swapping_the_backend_reuses_service_hydrator_and_endpoint_unmodified_and_reproduces_identical_json`]:
//!    the *same* real fixture data (a real actor, a real `account_profiles`
//!    row, a real posted status with a real extracted/persisted hashtag) is
//!    searched twice for the identical term -- once through the real,
//!    production `PgSearchBackend` (`pg_router`, wired exactly as
//!    `build_search_module` wires it), once through a hand-registered
//!    `StubSearchBackend` naming the very same real ids
//!    (`stub_router`) -- and the two `SearchResults` JSON bodies are
//!    asserted **byte-for-byte identical** (`assert_eq!` on the whole
//!    `serde_json::Value`). Since `SearchHydrator`/`SearchResultSerializer`/
//!    the endpoint handler are the exact same, unmodified code in both
//!    calls, and the only thing that differs between the two requests is
//!    which `SearchBackend` impl supplied the matching identifiers, this
//!    identical output proves Requirement 7.4 ("呼び出し側・API 契約・結果
//!    組み立てを変更せずに...差し替え可能") directly rather than merely by
//!    assertion.
//! 2. [`stub_backend_results_are_governed_by_the_stub_not_by_real_postgres_content`]:
//!    the inverse control -- a term that genuinely matches a real account in
//!    Postgres returns that account through `pg_router` but returns *nothing*
//!    through `stub_router` (an empty `StubSearchBackend`), and a
//!    *different*, real account that Postgres would never match for that
//!    term is nonetheless returned by `stub_router` once explicitly
//!    registered on the stub. This proves the swapped-in engine, not some
//!    residual real-Postgres matching path, is what actually governs
//!    `stub_router`'s results (Requirement 7.5, "モック/スタブ実装で差し替え
//!    可能").
//! 3. [`stub_backed_endpoint_keeps_the_same_response_envelope_shape_and_status_codes_as_the_real_backend`]:
//!    the no-match case (`200`, every field `[]` not `null`), the
//!    unauthenticated case (`401`), the insufficient-scope case (`403`), and
//!    the empty-query case (`422`) all behave identically whether
//!    `pg_router` or `stub_router` handles the request -- proving the API
//!    contract (status codes, envelope shape) is unchanged by the swap
//!    (Requirements 7.1, 8.4).
//!
//! ## Fixture plumbing duplication
//! `insert_actor_fixture`/`insert_account_profile`/`register_test_app`/
//! `issue_test_token`/`req`/`send`/`create_status` are direct, intentional
//! per-file copies of `tests/search_type_scope_it.rs`'s/`tests/
//! search_contract_it.rs`'s own identically named helpers -- each
//! `tests/*.rs` file is its own compiled crate (cannot import another test
//! file's private items), and this crate's own established convention is
//! exactly this kind of small, documented, intentional duplication across
//! sibling test modules (see those files' own doc comments for the
//! identical rationale).

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::routing::get;
use serde_json::{Value, json};
use tower::ServiceExt;

use kawasemi::accounts::{DEFAULT_REMOTE_ACCOUNT_CACHE_TTL, RemoteAccountFetcher};
use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorDirectory, ActorType, Handle, NewActor, ResolvedActor};
use kawasemi::domain::{AccountRef, Id};
use kawasemi::federation::signatures::ReqwestFederationHttpClient;
use kawasemi::oauth::app_repository::{self, NewApp};
use kawasemi::oauth::middleware::AuthState;
use kawasemi::oauth::model::ScopeSet as ModelScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::search::endpoint::{SEARCH_PATH, SearchEndpointsState, search};
use kawasemi::search::{RemoteResolver, SearchHydrator, SearchResultSerializer, SearchService};
use kawasemi::search::{StubSearchBackend, ports::SearchBackend};
use kawasemi::server;
use kawasemi::statuses::ProdRemoteActorResolver;
use kawasemi::statuses::ingest_service::StatusIngestService;
use kawasemi::test_harness::{TestApp, spawn_test_app};

// ---- Fixture plumbing (per-file duplication, see this file's own doc
// comment) --------------------------------------------------------------

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
            display_name: format!("Search Backend Swap IT {handle_str}"),
            summary: "an actor used by the search_backend_swap_it integration test".to_string(),
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

/// Mirrors `tests/search_type_scope_it.rs::insert_account_profile`'s own
/// already-reviewed convention: `account_profiles` has no physical FK to
/// `local_actors`, and `PgSearchBackend::search_accounts` (task 3.1) only
/// ever matches against this table's own `display_name` column.
async fn insert_account_profile(app: &TestApp, actor_id: Id, display_name: &str) {
    sqlx::query(
        "INSERT INTO account_profiles (actor_id, display_name, updated_at) VALUES ($1, $2, $3)",
    )
    .bind(actor_id.as_i64())
    .bind(display_name)
    .bind(app.runtime.clock.now())
    .execute(&app.pool)
    .await
    .expect("inserting a fixture account_profiles row must succeed");
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
            name: "Search Backend Swap IT Client".to_string(),
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

fn req(method: &str, path: &str, token: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    builder.body(Body::empty()).expect("build request")
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
    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/statuses")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&body).expect("serialize body"),
        ))
        .expect("build request");
    let (status, resp) = send(router, request).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "status creation must succeed: {resp:?}"
    );
    resp
}

async fn dispatch_search(router: &Router, token: Option<&str>, query: &str) -> (StatusCode, Value) {
    send(router, req("GET", &format!("{SEARCH_PATH}?{query}"), token)).await
}

fn status_id_of(created: &Value) -> Id {
    let raw = created["id"].as_str().expect("status id must be a string");
    Id::from_i64(raw.parse::<i64>().expect("status id must be numeric"))
}

// ---- The real production router (uses the real, default `PgSearchBackend`
// wired by `crate::search::build_search_module`/`crate::bootstrap`, exactly
// the instance `spawn_test_app` boots) -------------------------------------

fn pg_router(app: &TestApp) -> Router {
    server::build_router(app.state.clone())
}

// ---- The stub-backed router: a byte-for-byte mirror of
// `crate::search::build_search_module`'s own body, see this file's own doc
// comment ("Why this cannot drive the swap through `crate::server::
// build_router`") ------------------------------------------------------

/// Builds a [`SearchService`] wired exactly the way `crate::search::
/// build_search_module` wires its own production instance -- same
/// collaborator construction calls in the same order, same concrete
/// `ReqwestFederationHttpClient`/`ProdRemoteActorResolver`/`ActorDirectory`
/// triple -- except `backend` is taken as a parameter rather than always
/// being a freshly-constructed `PgSearchBackend`. This is this file's own
/// proof surface for "the swap point is exactly one line": diff this
/// function against `build_search_module` and the only difference is the
/// backend argument.
fn build_service_with_backend<B>(
    app: &TestApp,
    backend: B,
) -> Arc<SearchService<B, ReqwestFederationHttpClient, ProdRemoteActorResolver, ActorDirectory>>
where
    B: SearchBackend + Send + Sync + 'static,
{
    let pool = app.pool.clone();
    let runtime = app.runtime.clone();
    let domain = app.state.config().server.domain.clone();
    let directory = Arc::clone(app.actor.directory());
    let accounts = app.state.accounts().service();
    let account_ports = app.state.accounts().ports();
    let media_store = app.state.media().store().clone();
    let relationship_query = app.state.statuses().relationship_query_registry();

    let http_client = Arc::new(ReqwestFederationHttpClient::new());
    let account_fetcher = Arc::new(RemoteAccountFetcher::new(
        pool.clone(),
        Arc::clone(&http_client),
        runtime.clone(),
        DEFAULT_REMOTE_ACCOUNT_CACHE_TTL,
    ));
    let remote_actor_resolver = Arc::new(ProdRemoteActorResolver::new(
        domain.clone(),
        directory,
        Arc::clone(&account_fetcher),
    ));
    let mentions = ActorDirectory::new(pool.clone());
    let status_ingest = Arc::new(StatusIngestService::new(
        pool.clone(),
        Arc::clone(&http_client),
        runtime.clone(),
        Arc::clone(&remote_actor_resolver),
        domain.clone(),
        mentions,
    ));
    let remote_resolver = RemoteResolver::new(http_client, account_fetcher, status_ingest);

    let hydrator = SearchHydrator::new(
        pool,
        accounts,
        account_ports,
        media_store,
        relationship_query,
        runtime,
        domain,
    );

    Arc::new(SearchService::new(
        backend,
        remote_resolver,
        hydrator,
        SearchResultSerializer::new(),
    ))
}

type StubSearchState = SearchEndpointsState<
    StubSearchBackend,
    ReqwestFederationHttpClient,
    ProdRemoteActorResolver,
    ActorDirectory,
>;

/// Mounts the unmodified `crate::search::endpoint::search` handler (task
/// 5.2) via the unmodified `SearchEndpointsState` (task 5.2) onto a
/// per-file router, backed by `backend` instead of the production
/// `PgSearchBackend` -- see this file's own doc comment.
fn stub_router(app: &TestApp, backend: StubSearchBackend) -> Router {
    let search_service = build_service_with_backend(app, backend);
    let auth = AuthState {
        pool: app.pool.clone(),
        token_hash_key: app.state.config().oauth.token_hash_key.clone(),
    };
    let state: StubSearchState = SearchEndpointsState {
        search_service,
        auth,
    };
    Router::new()
        .route(
            SEARCH_PATH,
            get(search::<
                StubSearchBackend,
                ReqwestFederationHttpClient,
                ProdRemoteActorResolver,
                ActorDirectory,
            >),
        )
        .with_state(state)
}

// ==========================================================================
// (1) Requirements 7.1, 7.4: swapping the backend needs zero changes to
// `SearchService`/`SearchHydrator`/`SearchResultSerializer`/the endpoint
// handler, and reproduces byte-for-byte identical output for the same real
// data.
// ==========================================================================

const SWAP_TERM: &str = "backendswapquill";

#[tokio::test]
async fn swapping_the_backend_reuses_service_hydrator_and_endpoint_unmodified_and_reproduces_identical_json()
 {
    let app = spawn_test_app().await;
    let pg = pg_router(&app);

    // Real fixture data: one actor, one matching `account_profiles` row,
    // one real posted status (real hashtag extraction/persistence via
    // `StatusService::create_status`) -- all created through the real,
    // production router.
    let target = insert_actor_fixture(&app, "swap_target").await;
    insert_account_profile(&app, target.id, "Backend Swap BACKENDSWAPQUILL Actor").await;

    let app_id = register_test_app(&app).await;
    let poster_token = issue_test_token(&app, app_id, target.id, &["write:statuses"]).await;
    let created = create_status(
        &pg,
        &poster_token,
        json!({"status": "posting about backendswapquill today #backendswapquill"}),
    )
    .await;
    let status_id = status_id_of(&created);

    let searcher = insert_actor_fixture(&app, "swap_searcher").await;
    let searcher_token = issue_test_token(&app, app_id, searcher.id, &["read:search"]).await;

    // Pass 1: the real, default `PgSearchBackend`, through the real
    // production router (`build_search_module`'s own wiring).
    let (pg_status, pg_body) =
        dispatch_search(&pg, Some(&searcher_token), &format!("q={SWAP_TERM}")).await;
    assert_eq!(pg_status, StatusCode::OK, "got: {pg_body:?}");
    // Sanity: this scenario must genuinely populate every field, or the
    // byte-for-byte comparison below would be a vacuous all-empty match.
    assert_eq!(pg_body["accounts"].as_array().unwrap().len(), 1);
    assert_eq!(pg_body["statuses"].as_array().unwrap().len(), 1);
    assert_eq!(pg_body["hashtags"].as_array().unwrap().len(), 1);

    // Pass 2: the identical real ids, but supplied by a hand-registered
    // `StubSearchBackend` instead of any real SQL matching.
    let backend = StubSearchBackend::new()
        .with_account(
            AccountRef::Local(target.id),
            "Backend Swap BACKENDSWAPQUILL Actor",
        )
        .with_status(status_id, target.id, "posting about backendswapquill today")
        .with_hashtag(SWAP_TERM);
    let stub = stub_router(&app, backend);
    let (stub_status, stub_body) =
        dispatch_search(&stub, Some(&searcher_token), &format!("q={SWAP_TERM}")).await;
    assert_eq!(stub_status, StatusCode::OK, "got: {stub_body:?}");

    // The two responses are byte-for-byte identical `serde_json::Value`s:
    // same status code, same envelope, same embedded upstream Account/
    // Status/Tag JSON -- proving `SearchHydrator`/`SearchResultSerializer`/
    // the endpoint handler ran completely unmodified in both passes; only
    // which `SearchBackend` impl supplied the matching identifiers differed.
    assert_eq!(pg_status, stub_status);
    assert_eq!(
        pg_body, stub_body,
        "swapping PgSearchBackend for StubSearchBackend must not change the \
         SearchResults JSON for identical underlying data"
    );

    app.cleanup().await;
}

// ==========================================================================
// (2) Requirement 7.5: the stub-backed router's results are genuinely
// governed by the stub, not by leftover real-Postgres matching.
// ==========================================================================

#[tokio::test]
async fn stub_backend_results_are_governed_by_the_stub_not_by_real_postgres_content() {
    let app = spawn_test_app().await;
    let pg = pg_router(&app);

    // A real account that *does* match `pg_term` in real Postgres.
    let pg_matching = insert_actor_fixture(&app, "swap_control_pg_match").await;
    insert_account_profile(&app, pg_matching.id, "SwapControlPgMatchOnly Actor").await;

    // A different real account whose real `display_name` does *not* match
    // `pg_term` at all -- Postgres could never surface it for that term.
    let stub_only = insert_actor_fixture(&app, "swap_control_stub_only").await;
    insert_account_profile(&app, stub_only.id, "Totally Unrelated Display Name").await;

    let app_id = register_test_app(&app).await;
    let searcher = insert_actor_fixture(&app, "swap_control_searcher").await;
    let searcher_token = issue_test_token(&app, app_id, searcher.id, &["read:search"]).await;

    const PG_TERM: &str = "swapcontrolpgmatchonly";

    // Real backend: finds the real match, never the unrelated account.
    let (pg_status, pg_body) = dispatch_search(
        &pg,
        Some(&searcher_token),
        &format!("q={PG_TERM}&type=accounts"),
    )
    .await;
    assert_eq!(pg_status, StatusCode::OK, "got: {pg_body:?}");
    let pg_accounts = pg_body["accounts"].as_array().unwrap();
    assert_eq!(pg_accounts.len(), 1);
    assert_eq!(pg_accounts[0]["id"], pg_matching.id.as_i64().to_string());

    // Stub backend, deliberately registered *without* the real Postgres
    // match and *with* the unrelated account instead: an empty
    // `StubSearchBackend` returns nothing for `pg_term` (proving the real
    // match is not leaking through), and once the unrelated account is
    // explicitly registered on the stub for `pg_term`, it -- and only it --
    // is returned, even though real Postgres would never have surfaced it.
    let empty_stub = stub_router(&app, StubSearchBackend::new());
    let (empty_status, empty_body) = dispatch_search(
        &empty_stub,
        Some(&searcher_token),
        &format!("q={PG_TERM}&type=accounts"),
    )
    .await;
    assert_eq!(empty_status, StatusCode::OK, "got: {empty_body:?}");
    assert_eq!(empty_body["accounts"], json!([]));

    let stub_governed = StubSearchBackend::new().with_account(
        AccountRef::Local(stub_only.id),
        "swapcontrolpgmatchonly stub-only haystack",
    );
    let governed_router = stub_router(&app, stub_governed);
    let (governed_status, governed_body) = dispatch_search(
        &governed_router,
        Some(&searcher_token),
        &format!("q={PG_TERM}&type=accounts"),
    )
    .await;
    assert_eq!(governed_status, StatusCode::OK, "got: {governed_body:?}");
    let governed_accounts = governed_body["accounts"].as_array().unwrap();
    assert_eq!(governed_accounts.len(), 1);
    assert_eq!(
        governed_accounts[0]["id"],
        stub_only.id.as_i64().to_string(),
        "the stub-registered account, not the real-Postgres-matching one, must be returned"
    );

    app.cleanup().await;
}

// ==========================================================================
// (3) Requirements 7.1, 8.4: the API contract (status codes, envelope
// shape) is unchanged by the backend swap -- no-match/auth/scope/empty-query
// behavior is identical whether the real or stub backend handles the
// request.
// ==========================================================================

#[tokio::test]
async fn stub_backed_endpoint_keeps_the_same_response_envelope_shape_and_status_codes_as_the_real_backend()
 {
    let app = spawn_test_app().await;
    let pg = pg_router(&app);
    let stub = stub_router(&app, StubSearchBackend::new());

    let app_id = register_test_app(&app).await;
    let searcher = insert_actor_fixture(&app, "swap_shape_searcher").await;
    let searcher_token = issue_test_token(&app, app_id, searcher.id, &["read:search"]).await;
    let insufficient_token = issue_test_token(&app, app_id, searcher.id, &["read:accounts"]).await;

    for (label, router) in [("pg", &pg), ("stub", &stub)] {
        // No match: 200, every field `[]`, never `null`.
        let (status, body) = dispatch_search(
            router,
            Some(&searcher_token),
            "q=nonexistentbackendswapscanterm",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "[{label}] got: {body:?}");
        assert_eq!(body["accounts"], json!([]), "[{label}]");
        assert_eq!(body["statuses"], json!([]), "[{label}]");
        assert_eq!(body["hashtags"], json!([]), "[{label}]");

        // No bearer token: 401.
        let (status, body) = dispatch_search(router, None, "q=anything").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "[{label}] got: {body:?}");

        // Insufficient scope: 403.
        let (status, body) = dispatch_search(router, Some(&insufficient_token), "q=anything").await;
        assert_eq!(status, StatusCode::FORBIDDEN, "[{label}] got: {body:?}");

        // Empty query: 422.
        let (status, body) = dispatch_search(router, Some(&searcher_token), "q=").await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "[{label}] got: {body:?}"
        );
    }

    app.cleanup().await;
}
