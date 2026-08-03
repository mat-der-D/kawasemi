//! Tests for [`super::SearchService`] (task 5.1 completion definition:
//! "type 絞り・空クエリ拒否・resolve 分岐・限定種別の空配列が一連で機能し、
//! 呼び出し側がエンジン非依存（`SearchBackend` 経由）で、失敗時に種別・箇所
//! を含む診断が出力される統合テストが通る"; Requirements 2.1, 2.2, 2.3, 2.5,
//! 5.4, 6.3, 6.5, 7.1, 9.5).
//!
//! Every test below is a real, executable DB-backed integration test
//! against `crate::test_harness::spawn_test_app` — mirroring
//! `search/hydrator/tests.rs`'/`search/remote_resolver/tests.rs`'
//! established convention (`create_test_actor` is an exact copy of those
//! modules' own helper of the same name; `resolver_for`/`FakeRemoteActors`
//! mirror `search/remote_resolver/tests.rs`'s own per-module-owned copies,
//! not a cross-module import — see that module's own doc comment for why).
//! `spawn_test_app` is required even for tests that never touch the
//! database through their own assertions (e.g. the empty-query-422 test)
//! because `SearchService::new` itself requires a fully-constructed
//! `RemoteResolver`/`SearchHydrator`, both of which need a real `PgPool` to
//! build (Requirement 9.5's own diagnostics aside, this module never mocks
//! the database itself -- only the federation HTTP boundary).
//!
//! [`crate::search::ports::StubSearchBackend`] stands in for
//! `SearchBackend` in most tests below specifically *because* it is a
//! swap-in, engine-agnostic double (Requirement 7.5) -- this module's own
//! code (`super::SearchService`) never once mentions `StubSearchBackend` by
//! name, so every test that exercises it is direct evidence for this task's
//! own "呼び出し側がエンジン非依存" completion condition (Requirement 7.1).
//! One test ([`search_end_to_end_with_the_default_pg_backend`]) additionally
//! exercises the real, default `PgSearchBackend` to prove the same generic
//! `SearchService<B, ..>` code also runs unmodified against the production
//! backend (Requirement 7.4).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::http::{HeaderMap, StatusCode};
use serde_json::json;

use super::*;
use crate::accounts::DEFAULT_REMOTE_ACCOUNT_CACHE_TTL;
use crate::accounts::remote_fetcher::RemoteAccountFetcher;
use crate::actor::ActorDirectory;
use crate::actor::owner::create_owner;
use crate::actor::repository::insert_actor;
use crate::actor::{ActorState, ActorType, Handle};
use crate::domain::Visibility;
use crate::error::AppError;
use crate::federation::signatures::{HttpResponse, MockFederationHttpClient};
use crate::search::hashtag_repository::upsert_tag_usage;
use crate::search::model::SearchType;
use crate::search::pg_backend::PgSearchBackend;
use crate::search::ports::StubSearchBackend;
use crate::statuses::inbound_handlers::RemoteActorResolver;
use crate::statuses::ingest_service::StatusIngestService;
use crate::statuses::model::Status;
use crate::statuses::status_repository::insert_status;
use crate::test_harness::{TestApp, spawn_test_app};

// ---- shared fixtures --------------------------------------------------

async fn create_test_actor(app: &TestApp, handle: &str) -> Id {
    let now = app.runtime.clock.now();
    let owner_id = app.runtime.ids.next_id();
    create_owner(&app.pool, owner_id, now)
        .await
        .expect("creating the owner must succeed");

    let actor_id = app.runtime.ids.next_id();
    let actor = crate::actor::model::LocalActor {
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

async fn create_test_status(app: &TestApp, actor_id: Id, content: &str) -> Id {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    let status = Status {
        id,
        actor_id,
        uri: format!("https://kawasemi.example/statuses/{}", id.as_i64()),
        url: None,
        content: content.to_string(),
        visibility: Visibility::Public,
        sensitive: false,
        spoiler_text: String::new(),
        in_reply_to_id: None,
        in_reply_to_account_id: None,
        reblog_of_id: None,
        poll_id: None,
        language: None,
        reblogs_count: 0,
        favourites_count: 0,
        replies_count: 0,
        local: true,
        created_at: now,
        edited_at: None,
    };
    insert_status(&app.pool, &status)
        .await
        .expect("insert_status must succeed");
    id
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
/// `search/remote_resolver/tests.rs::FakeRemoteActors`' identical
/// per-test-module-owned precedent.
struct FakeRemoteActors {
    runtime: RuntimeContextForFake,
    by_uri: Mutex<HashMap<String, Id>>,
}

// `RuntimeContext` itself is `Clone`; this thin alias keeps the struct
// definition above readable without importing `crate::runtime::
// RuntimeContext` under two different names.
type RuntimeContextForFake = crate::runtime::RuntimeContext;

impl FakeRemoteActors {
    fn new(runtime: RuntimeContextForFake) -> Self {
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

const ALICE_ACTOR_URI: &str = "https://remote.example/users/alice";
const TEST_DOMAIN: &str = "kawasemi.example";

fn ok_response(body: serde_json::Value) -> HttpResponse {
    HttpResponse {
        status: StatusCode::OK,
        headers: HeaderMap::new(),
        body: serde_json::to_vec(&body).expect("test fixture body must serialize"),
    }
}

fn jrd_document(actor_uri: &str) -> serde_json::Value {
    json!({
        "subject": "acct:alice@remote.example",
        "links": [
            {"rel": "self", "type": "application/activity+json", "href": actor_uri}
        ]
    })
}

fn actor_document(actor_uri: &str) -> serde_json::Value {
    json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": actor_uri,
        "type": "Person",
        "preferredUsername": "alice",
        "name": "Alice Example",
    })
}

/// Builds a [`RemoteResolver`] against `app`'s real pool and `mock`'s
/// queued federation responses -- mirrors `search/remote_resolver/
/// tests.rs::resolver_for`.
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
        Arc::new(FakeRemoteActors::new(app.runtime.clone())),
        TEST_DOMAIN,
        ActorDirectory::new(app.pool.clone()),
    ));
    RemoteResolver::new(mock, account_fetcher, status_ingest)
}

/// A `SearchService<StubSearchBackend, ..>` built from `app`'s real DB-backed
/// collaborators (`SearchHydrator`, `RemoteResolver`) plus a caller-supplied
/// `backend` -- the one seam every test below actually varies.
fn service_with_backend(
    app: &TestApp,
    mock: Arc<MockFederationHttpClient>,
    backend: StubSearchBackend,
) -> SearchService<StubSearchBackend, MockFederationHttpClient, FakeRemoteActors, ActorDirectory> {
    SearchService::new(
        backend,
        remote_resolver_for(app, mock),
        build_hydrator(app),
        SearchResultSerializer::new(),
    )
}

fn params(q: &str, viewer: Id) -> SearchParams {
    SearchParams {
        q: q.to_string(),
        kind: None,
        resolve: false,
        following: false,
        account_id: None,
        limit: 20,
        offset: 0,
        exclude_unreviewed: false,
        viewer,
    }
}

// ---- 2.3: empty query rejection ----------------------------------------

/// Requirement 2.3: an empty (or whitespace-only) `q` is rejected with a
/// `422 Unprocessable Entity` before any backend/hydrator/remote-resolver
/// call is made.
#[tokio::test]
async fn search_rejects_empty_query_with_422() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    let viewer = app.runtime.ids.next_id();
    let service = service_with_backend(&app, Arc::clone(&mock), StubSearchBackend::new());

    let err = service
        .search(params("   ", viewer))
        .await
        .expect_err("an empty/whitespace-only query must be rejected");

    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        mock.fetched_urls().is_empty(),
        "an empty query must never reach remote resolution"
    );
}

// ---- 2.1, 2.2: type dispatch --------------------------------------------

/// Requirement 2.2: `type=accounts` returns only `accounts`; `statuses`/
/// `hashtags` are `[]`, not omitted or `null`, even though matching data
/// exists for all three types.
#[tokio::test]
async fn search_type_accounts_returns_empty_arrays_for_other_types() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    let alice = create_test_actor(&app, "alice").await;
    let viewer = app.runtime.ids.next_id();
    let post = create_test_status(&app, alice, "hello rustlang world").await;

    let now = app.runtime.clock.now();
    let tag_id = app.runtime.ids.next_id();
    upsert_tag_usage(&app.pool, "rustlang", tag_id, post, now)
        .await
        .expect("upsert_tag_usage must succeed");

    let backend = StubSearchBackend::new()
        .with_account(AccountRef::Local(alice), "Test Actor alice rustlang")
        .with_status(post, alice, "hello rustlang world")
        .with_hashtag("rustlang");
    let service = service_with_backend(&app, mock, backend);

    let mut request = params("rustlang", viewer);
    request.kind = Some(SearchType::Accounts);
    let result = service
        .search(request)
        .await
        .expect("type-scoped search must succeed");

    assert_eq!(
        result["accounts"].as_array().unwrap().len(),
        1,
        "accounts must be populated"
    );
    assert_eq!(
        result["statuses"],
        json!([]),
        "statuses must be [] (Requirement 2.2, 1.4), not omitted or null"
    );
    assert_eq!(
        result["hashtags"],
        json!([]),
        "hashtags must be [] (Requirement 2.2, 1.4), not omitted or null"
    );
}

/// Requirement 2.1: an unscoped (`type` omitted) search returns all three
/// types populated.
#[tokio::test]
async fn search_unscoped_returns_all_three_types() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    let alice = create_test_actor(&app, "alice").await;
    let viewer = app.runtime.ids.next_id();
    let post = create_test_status(&app, alice, "commonterm mentioned here").await;

    let now = app.runtime.clock.now();
    let tag_id = app.runtime.ids.next_id();
    upsert_tag_usage(&app.pool, "commonterm", tag_id, post, now)
        .await
        .expect("upsert_tag_usage must succeed");

    let backend = StubSearchBackend::new()
        .with_account(AccountRef::Local(alice), "commonterm alice")
        .with_status(post, alice, "commonterm mentioned here")
        .with_hashtag("commonterm");
    let service = service_with_backend(&app, mock, backend);

    let result = service
        .search(params("commonterm", viewer))
        .await
        .expect("unscoped search must succeed");

    assert_eq!(result["accounts"].as_array().unwrap().len(), 1);
    assert_eq!(result["statuses"].as_array().unwrap().len(), 1);
    assert_eq!(result["hashtags"].as_array().unwrap().len(), 1);
}

// ---- 6.1, 6.3: resolve gating -------------------------------------------

/// Requirement 6.1: `resolve=true` against an `acct:` query adds the
/// remotely-resolved account to the `accounts` results, on top of whatever
/// `SearchBackend` itself matched (nothing, here).
#[tokio::test]
async fn search_resolve_true_adds_remote_account_to_results() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    mock.queue_fetch_response(ok_response(jrd_document(ALICE_ACTOR_URI)));
    mock.queue_fetch_response(ok_response(actor_document(ALICE_ACTOR_URI)));
    let viewer = app.runtime.ids.next_id();
    let service = service_with_backend(&app, mock, StubSearchBackend::new());

    let mut request = params("acct:alice@remote.example", viewer);
    request.resolve = true;
    let result = service
        .search(request)
        .await
        .expect("resolve=true search must succeed");

    let accounts = result["accounts"].as_array().unwrap();
    assert_eq!(
        accounts.len(),
        1,
        "the remotely-resolved account must appear in the results"
    );
}

/// Requirement 6.3: `resolve=false` (the default) against an `acct:` query
/// never triggers a federation fetch, and only locally-known matches (none,
/// here) are returned.
#[tokio::test]
async fn search_resolve_false_never_fetches_remotely() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    let viewer = app.runtime.ids.next_id();
    let service = service_with_backend(&app, Arc::clone(&mock), StubSearchBackend::new());

    let request = params("acct:alice@remote.example", viewer); // resolve: false (default)
    let result = service
        .search(request)
        .await
        .expect("resolve=false search must succeed");

    assert!(
        mock.fetched_urls().is_empty(),
        "resolve=false must never make a federation fetch (Requirement 6.3)"
    );
    assert_eq!(result["accounts"], json!([]));
}

/// Requirement 2.2 + this module's own documented "resolved candidates are
/// `type`-gated" behavior: `resolve=true` against an `acct:` query, but
/// `type=hashtags`, must not leak the resolved account into any result
/// array -- `accounts` stays `[]` because it was not requested.
#[tokio::test]
async fn search_resolved_account_is_not_leaked_when_type_excludes_accounts() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    mock.queue_fetch_response(ok_response(jrd_document(ALICE_ACTOR_URI)));
    mock.queue_fetch_response(ok_response(actor_document(ALICE_ACTOR_URI)));
    let viewer = app.runtime.ids.next_id();
    let service = service_with_backend(&app, mock, StubSearchBackend::new());

    let mut request = params("acct:alice@remote.example", viewer);
    request.resolve = true;
    request.kind = Some(SearchType::Hashtags);
    let result = service
        .search(request)
        .await
        .expect("type-scoped resolve search must succeed");

    assert_eq!(
        result["accounts"],
        json!([]),
        "a resolved account must never appear when type=hashtags was requested"
    );
    assert_eq!(result["hashtags"], json!([]));
}

// ---- 9.5: failure diagnostics / propagation -----------------------------

/// A [`SearchBackend`] implementation whose `search_accounts` always fails,
/// used only to prove Requirement 9.5's "collaborator failure propagates,
/// not silently swallowed" behavior.
struct FailingBackend;

impl SearchBackend for FailingBackend {
    async fn search_accounts(&self, _q: &AccountQuery) -> Result<Vec<AccountRef>, AppError> {
        Err(AppError::server(
            StatusCode::INTERNAL_SERVER_ERROR,
            "simulated backend failure",
        ))
    }

    async fn search_statuses(&self, _q: &StatusQuery) -> Result<Vec<Id>, AppError> {
        Ok(Vec::new())
    }

    async fn search_hashtags(
        &self,
        _q: &HashtagQuery,
    ) -> Result<Vec<crate::search::model::TagMatch>, AppError> {
        Ok(Vec::new())
    }
}

/// Requirement 9.5: a `SearchBackend` match failure is propagated as `Err`
/// (not downgraded to an empty result) -- this module's own doc comment,
/// "Structured failure diagnostics", documents that only remote-resolution
/// failure is required to degrade gracefully.
#[tokio::test]
async fn search_backend_failure_propagates_as_err() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    let viewer = app.runtime.ids.next_id();
    let service = SearchService::new(
        FailingBackend,
        remote_resolver_for(&app, mock),
        build_hydrator(&app),
        SearchResultSerializer::new(),
    );

    let err = service
        .search(params("hello", viewer))
        .await
        .expect_err("a SearchBackend failure must propagate, not be swallowed");
    assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
}

/// Requirement 6.4 (applied defensively at this layer, see this module's
/// doc comment "`resolve_remote`'s own `Err` arm"): a WebFinger fetch
/// failure during `resolve=true` resolution normalizes to no resolved
/// account (via `RemoteResolver` itself, Requirement 6.4) and the overall
/// search still succeeds with a 200-equivalent `Ok`.
#[tokio::test]
async fn search_remote_resolution_failure_does_not_fail_the_whole_search() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    mock.queue_fetch_error(StatusCode::BAD_GATEWAY, "network unreachable");
    let viewer = app.runtime.ids.next_id();
    let service = service_with_backend(&app, mock, StubSearchBackend::new());

    let mut request = params("acct:alice@remote.example", viewer);
    request.resolve = true;
    let result = service
        .search(request)
        .await
        .expect("a remote-resolution failure must not fail the whole search");

    assert_eq!(result["accounts"], json!([]));
}

// ---- 5.4: exclude_unreviewed accepted without altering behavior --------

/// Requirement 5.4: `exclude_unreviewed=true` is accepted (does not cause a
/// rejection) and does not change this minimal implementation's hashtag
/// matching.
#[tokio::test]
async fn search_exclude_unreviewed_is_accepted_without_changing_results() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    let alice = create_test_actor(&app, "alice").await;
    let viewer = app.runtime.ids.next_id();
    let post = create_test_status(&app, alice, "unreviewedterm body").await;
    let now = app.runtime.clock.now();
    let tag_id = app.runtime.ids.next_id();
    upsert_tag_usage(&app.pool, "unreviewedterm", tag_id, post, now)
        .await
        .expect("upsert_tag_usage must succeed");

    let backend = StubSearchBackend::new().with_hashtag("unreviewedterm");
    let service = service_with_backend(&app, mock, backend);

    let mut request = params("unreviewedterm", viewer);
    request.kind = Some(SearchType::Hashtags);
    request.exclude_unreviewed = true;
    let result = service
        .search(request)
        .await
        .expect("exclude_unreviewed=true must not be rejected");

    assert_eq!(result["hashtags"].as_array().unwrap().len(), 1);
}

// ---- 2.5, 3.4/4.6: limit/offset/account_id threaded through ------------

/// Requirement 4.3/2.5: `account_id` and `limit`/`offset` are threaded from
/// `SearchParams` through to the `SearchBackend` query (proven here via
/// `StubSearchBackend`'s own real filtering/pagination, not a mock
/// assertion).
#[tokio::test]
async fn search_threads_account_id_and_limit_offset_to_the_backend() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    let author_a = create_test_actor(&app, "authora").await;
    let author_b = create_test_actor(&app, "authorb").await;
    let viewer = app.runtime.ids.next_id();
    let post_a = create_test_status(&app, author_a, "scopeterm from a").await;
    let post_b = create_test_status(&app, author_b, "scopeterm from b").await;

    let backend = StubSearchBackend::new()
        .with_status(post_a, author_a, "scopeterm from a")
        .with_status(post_b, author_b, "scopeterm from b");
    let service = service_with_backend(&app, mock, backend);

    let mut request = params("scopeterm", viewer);
    request.kind = Some(SearchType::Statuses);
    request.account_id = Some(author_a);
    let result = service
        .search(request)
        .await
        .expect("account_id-scoped search must succeed");

    let statuses = result["statuses"].as_array().unwrap();
    assert_eq!(statuses.len(), 1, "only author_a's post must match");
    assert_eq!(
        statuses[0]["id"].as_str().unwrap(),
        post_a.as_i64().to_string()
    );
}

// ---- 7.1, 7.4: engine-agnostic caller against the default PgSearchBackend --

/// Requirement 7.4: the exact same generic `SearchService<B, ..>` code also
/// runs, unmodified, against the real default `PgSearchBackend` -- not just
/// the `StubSearchBackend` test double every other test in this file uses.
#[tokio::test]
async fn search_end_to_end_with_the_default_pg_backend() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    let alice = create_test_actor(&app, "alice").await;
    let viewer = app.runtime.ids.next_id();
    create_test_status(&app, alice, "pgbackendterm body").await;

    let backend = PgSearchBackend::new(app.pool.clone(), app.runtime.clone());
    let service = SearchService::new(
        backend,
        remote_resolver_for(&app, mock),
        build_hydrator(&app),
        SearchResultSerializer::new(),
    );

    let mut request = params("pgbackendterm", viewer);
    request.kind = Some(SearchType::Statuses);
    let result = service
        .search(request)
        .await
        .expect("PgSearchBackend-backed search must succeed");

    assert_eq!(result["statuses"].as_array().unwrap().len(), 1);
}
