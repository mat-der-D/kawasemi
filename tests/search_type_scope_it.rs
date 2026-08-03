//! Full-HTTP-pipeline integration tests for `type` scoping, empty-query
//! rejection, and `read:search` authentication/scope enforcement (search
//! spec task 6.3, `.kiro/specs/search/tasks.md`, "6.3 (P) リモート解決・
//! type/スコープの統合テスト", `_Depends: 5.3_`), Requirements 2.2, 2.4, 9.1,
//! 9.2. design.md's File Structure Plan names this exact file
//! (`tests/search_type_scope_it.rs`, "type 絞り込み・空クエリ拒否・
//! read:search 認証/スコープ・空配列規律（統合）").
//!
//! ## What "real pipeline" means here
//! Unlike `tests/search_resolve_it.rs` (this task's sibling file), none of
//! the scenarios below need a mocked `FederationHttpClient` -- `type`
//! scoping, empty-query rejection, and auth/scope enforcement never touch
//! `RemoteResolver`. Every request below therefore drives the *actual*
//! production seam end to end via `tower::ServiceExt::oneshot` against
//! `crate::server::build_router` (mirroring `tests/search_contract_it.rs`'s/
//! `tests/search_accounts_it.rs`'s own established in-process-HTTP
//! technique): real Bearer/`read:search` scope enforcement
//! (`crate::search::endpoint::search`), a real `POST /api/v1/statuses`
//! (`StatusService::create_status`, including its real hashtag extraction/
//! persistence), a real `account_profiles` row (this file inserts one
//! directly -- mirroring `tests/search_accounts_it.rs`'s/`tests/
//! search_contract_it.rs`'s own already-reviewed convention that
//! `account_profiles`/`local_actors` have no physical FK, so a bare fixture
//! row is sufficient), and the real, already-wired (task 5.3) chain from
//! `GET /api/v2/search` through `SearchService::search` -> the real default
//! `PgSearchBackend` (including its on-demand `HashtagIndexer::
//! catch_up_from_watermark`) -> `SearchHydrator` -> `SearchResultSerializer`.
//!
//! ## Relationship to already-existing coverage at other layers
//! `src/search/endpoint/tests.rs` (task 5.2) already unit-tests 401/403/422/
//! `type=hashtags` exclusion/malformed values against a *test-only* router
//! mounting `crate::search::endpoint::search` in isolation with a
//! `StubSearchBackend`; `tests/search_contract_it.rs` (task 6.1) already
//! proves `type` scoping's exact byte-for-byte JSON shape as a side effect
//! of its own contract goldens. Neither drives the request through the real
//! production `crate::server::build_router` specifically to prove this
//! task's own completion condition ("type 絞り・空クエリ拒否・スコープ制御
//! が成立する統合テストが通る") as its own, independently-owned assertion
//! surface -- that is this file's job (design.md's own File Structure Plan
//! entry, task 6.3's own `Boundary: search_resolve_it, search_type_scope_it`).
//! This file does not re-derive `PgSearchBackend`'s own SQL-matching
//! semantics (`tests/search_accounts_it.rs`/`tests/search_statuses_it.rs`/
//! `tests/search_hashtags_it.rs`, task 6.2's own boundary) or re-register a
//! contract golden (task 6.1's own boundary) -- it proves the `type`/auth/
//! empty-query *control flow* through the real, mounted endpoint.
//!
//! ## Fixture plumbing duplication
//! `insert_actor_fixture`/`insert_account_profile`/`create_status`/
//! `register_test_app`/`issue_test_token`/`req`/`send` are direct,
//! intentional per-file copies of `tests/search_contract_it.rs`'s own
//! identically named helpers -- each `tests/*.rs` file is its own compiled
//! crate (cannot import another test file's private items), and this
//! crate's own established convention is exactly this kind of small,
//! documented, intentional duplication across sibling test modules (see
//! that file's own doc comment for the identical rationale). Unlike that
//! file, this one does not force `X-Forwarded-Proto`/`X-Forwarded-Host`
//! headers -- no scenario below compares a response against a separately
//! fetched ground-truth JSON value byte-for-byte, so origin consistency is
//! not load-bearing here.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor, ResolvedActor};
use kawasemi::domain::Id;
use kawasemi::oauth::app_repository::{self, NewApp};
use kawasemi::oauth::model::ScopeSet as ModelScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::server;
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
            display_name: format!("Search Type Scope IT {handle_str}"),
            summary: "an actor used by the search_type_scope_it integration test".to_string(),
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

/// Mirrors `tests/search_accounts_it.rs::insert_local_account`'s/`tests/
/// search_contract_it.rs::insert_account_profile`'s own already-reviewed
/// convention: `account_profiles` has no physical FK to `local_actors`, and
/// `PgSearchBackend::search_accounts` (task 3.1) only ever matches against
/// this table's own `display_name` column.
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
            name: "Search Type Scope IT Client".to_string(),
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

async fn search(router: &Router, token: Option<&str>, query: &str) -> (StatusCode, Value) {
    send(
        router,
        req("GET", &format!("/api/v2/search?{query}"), token),
    )
    .await
}

// ==========================================================================
// Shared scenario: one unique search term matching a real account's
// `display_name`, a real status's content, and a real hashtag extracted
// from that same status -- so `type` scoping has a genuine populated
// candidate to *exclude* for every non-requested type (Requirement 2.2),
// not merely an already-empty field.
// ==========================================================================

const SEARCH_TERM: &str = "typescopequill";
const TARGET_HANDLE: &str = "search_type_scope_target";

struct Scenario {
    router: Router,
    token: String,
}

async fn build_scenario(app: &TestApp) -> Scenario {
    let router = real_router(app);
    let target = insert_actor_fixture(app, TARGET_HANDLE).await;
    insert_account_profile(app, target.id, "Search Type Scope TYPESCOPEQUILL Actor").await;

    let app_id = register_test_app(app).await;
    let poster_token = issue_test_token(app, app_id, target.id, &["write:statuses"]).await;
    let searcher = insert_actor_fixture(app, "search_type_scope_searcher").await;
    let searcher_token = issue_test_token(app, app_id, searcher.id, &["read:search"]).await;

    create_status(
        &router,
        &poster_token,
        json!({"status": "posting about typescopequill today #typescopequill"}),
    )
    .await;

    Scenario {
        router,
        token: searcher_token,
    }
}

fn assert_all_arrays_non_null(body: &Value) {
    for field in ["accounts", "statuses", "hashtags"] {
        assert!(
            body[field].is_array(),
            "field {field:?} must always be a JSON array, got {:?}",
            body[field]
        );
        assert_ne!(
            body[field],
            Value::Null,
            "field {field:?} must never be null"
        );
    }
}

// ==========================================================================
// (1) Control: an unscoped search (no `type`) populates all three types --
// the baseline every type-scoped assertion below is contrasted against.
// ==========================================================================

/// Requirement 2.1 (baseline), setting up the contrast for (2) below.
#[tokio::test]
async fn unscoped_search_populates_all_three_types() {
    let app = spawn_test_app().await;
    let scenario = build_scenario(&app).await;

    let (status, body) = search(
        &scenario.router,
        Some(&scenario.token),
        &format!("q={SEARCH_TERM}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {body:?}");
    assert_all_arrays_non_null(&body);
    assert_eq!(body["accounts"].as_array().unwrap().len(), 1);
    assert_eq!(body["statuses"].as_array().unwrap().len(), 1);
    assert_eq!(body["hashtags"].as_array().unwrap().len(), 1);

    app.cleanup().await;
}

// ==========================================================================
// (2) Requirement 2.2: `type` genuinely narrows matching AND forces the
// other two fields to `[]` (not omitted, not null) -- every one of the
// three types individually, each against the identical, otherwise-
// triple-matching term from (1).
// ==========================================================================

#[tokio::test]
async fn type_accounts_narrows_to_accounts_only() {
    let app = spawn_test_app().await;
    let scenario = build_scenario(&app).await;

    let (status, body) = search(
        &scenario.router,
        Some(&scenario.token),
        &format!("q={SEARCH_TERM}&type=accounts"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {body:?}");
    assert_all_arrays_non_null(&body);
    assert_eq!(body["accounts"].as_array().unwrap().len(), 1);
    assert_eq!(body["statuses"], json!([]));
    assert_eq!(body["hashtags"], json!([]));

    app.cleanup().await;
}

#[tokio::test]
async fn type_statuses_narrows_to_statuses_only() {
    let app = spawn_test_app().await;
    let scenario = build_scenario(&app).await;

    let (status, body) = search(
        &scenario.router,
        Some(&scenario.token),
        &format!("q={SEARCH_TERM}&type=statuses"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {body:?}");
    assert_all_arrays_non_null(&body);
    assert_eq!(body["statuses"].as_array().unwrap().len(), 1);
    assert_eq!(body["accounts"], json!([]));
    assert_eq!(body["hashtags"], json!([]));

    app.cleanup().await;
}

#[tokio::test]
async fn type_hashtags_narrows_to_hashtags_only() {
    let app = spawn_test_app().await;
    let scenario = build_scenario(&app).await;

    let (status, body) = search(
        &scenario.router,
        Some(&scenario.token),
        &format!("q={SEARCH_TERM}&type=hashtags"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {body:?}");
    assert_all_arrays_non_null(&body);
    assert_eq!(body["hashtags"].as_array().unwrap().len(), 1);
    assert_eq!(body["accounts"], json!([]));
    assert_eq!(body["statuses"], json!([]));

    app.cleanup().await;
}

/// An unrecognized `type` value is `422`, never silently ignored/defaulted
/// to "search everything" (mirrors `notifications::endpoints::
/// parse_notification_type`'s identical precedent, `crate::search::
/// endpoint`'s own doc comment).
#[tokio::test]
async fn unrecognized_type_value_is_422() {
    let app = spawn_test_app().await;
    let scenario = build_scenario(&app).await;

    let (status, body) = search(
        &scenario.router,
        Some(&scenario.token),
        &format!("q={SEARCH_TERM}&type=bogus"),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "got: {body:?}");

    app.cleanup().await;
}

// ==========================================================================
// (3) Requirement 2.3 (cross-referenced by this task's own Requirements
// list via 2.2/2.4/9.1/9.2, and design.md's own text for this file):
// empty/whitespace-only `q` is rejected with 422, through the real mounted
// endpoint.
// ==========================================================================

#[tokio::test]
async fn empty_query_is_422() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let searcher = insert_actor_fixture(&app, "empty_query_422_searcher").await;
    let app_id = register_test_app(&app).await;
    let token = issue_test_token(&app, app_id, searcher.id, &["read:search"]).await;

    // `q=` (present but empty).
    let (status, body) = search(&router, Some(&token), "q=").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "got: {body:?}");

    // `q` omitted entirely (defaults to an empty string, same rejection).
    let (status, body) = send(&router, req("GET", "/api/v2/search", Some(&token))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "got: {body:?}");

    // Whitespace-only `q`.
    let (status, body) = search(&router, Some(&token), "q=%20%20%20").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "got: {body:?}");

    app.cleanup().await;
}

// ==========================================================================
// (4) Requirements 2.4, 9.1, 9.2: `read:search` authentication/scope
// enforcement, through the real mounted endpoint -- a genuinely valid,
// correctly-scoped request (the control) is contrasted against a missing
// token (401) and an insufficiently-scoped token (403).
// ==========================================================================

/// No bearer token at all -> 401, before any query parsing/backend call.
#[tokio::test]
async fn missing_bearer_token_is_401() {
    let app = spawn_test_app().await;
    let router = real_router(&app);

    let (status, body) = search(&router, None, &format!("q={SEARCH_TERM}")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "got: {body:?}");

    app.cleanup().await;
}

/// An authenticated token that lacks `read:search` -> 403. Contrasted, in
/// the same test, against the identical actor/query succeeding once issued
/// a `read:search`-scoped token -- the genuine "same request, only the
/// scope differs" control comparison.
#[tokio::test]
async fn token_without_read_search_scope_is_403_but_succeeds_once_scoped() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let searcher = insert_actor_fixture(&app, "scope_403_control_searcher").await;
    let app_id = register_test_app(&app).await;

    // `read:accounts` deliberately omits `read:search`.
    let insufficient_token = issue_test_token(&app, app_id, searcher.id, &["read:accounts"]).await;
    let (status, body) = search(
        &router,
        Some(&insufficient_token),
        &format!("q={SEARCH_TERM}"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "got: {body:?}");

    // The identical actor, a fresh token that *does* carry `read:search`,
    // the identical query -- succeeds.
    let sufficient_token = issue_test_token(&app, app_id, searcher.id, &["read:search"]).await;
    let (status, body) = search(
        &router,
        Some(&sufficient_token),
        &format!("q={SEARCH_TERM}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {body:?}");
    assert_all_arrays_non_null(&body);

    app.cleanup().await;
}

// ==========================================================================
// (5) Requirement 1.4 (cross-referenced): a query matching nothing at all
// still returns every field as `[]`, never `null` -- the "no results"
// counterpart to (1)'s "all populated" baseline.
// ==========================================================================

#[tokio::test]
async fn search_with_no_matches_returns_every_type_as_empty_arrays_not_null() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let searcher = insert_actor_fixture(&app, "no_match_searcher").await;
    let app_id = register_test_app(&app).await;
    let token = issue_test_token(&app, app_id, searcher.id, &["read:search"]).await;

    let (status, body) = search(&router, Some(&token), "q=nonexistenttypescopescanterm").await;
    assert_eq!(status, StatusCode::OK, "got: {body:?}");
    assert_all_arrays_non_null(&body);
    assert_eq!(body["accounts"], json!([]));
    assert_eq!(body["statuses"], json!([]));
    assert_eq!(body["hashtags"], json!([]));

    app.cleanup().await;
}
