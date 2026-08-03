//! Integration tests for `PgSearchBackend::search_accounts` (search spec
//! task 3.1, `Boundary: PgSearchBackend`; Requirements 3.1, 3.4, 7.2),
//! design.md's `search_accounts_it.rs` ("アカウント検索（ローカル/既知リモート
//! 一致・following 絞り・一意化・limit/offset）（統合, SearchBackend 実体）").
//!
//! `following_of`-scoping is *not* exercised at the `PgSearchBackend` level
//! below: design.md's own `PgSearchBackend::search_accounts` Responsibilities
//! note is explicit that the default backend does not filter by `following`
//! at the matching stage ("following は照合段では候補抽出に留め、フォロー限定
//! は Hydrator/上流関係に委譲") — that behavior belongs to `SearchHydrator`,
//! strictly outside task 3.1's boundary. Task 6.2 (this task's own addition,
//! see "Task 6.2 additions" below) closes that gap at the full-pipeline
//! level instead.
//!
//! Fixtures for the `PgSearchBackend`-level tests below are inserted
//! directly via raw SQL against `account_profiles`/`remote_accounts` rather
//! than through the full `ActorService`/`AccountService` creation path: both
//! tables' primary keys have no physical FK to `local_actors`/`owners`
//! (`migrations/0006_accounts.sql`'s own doc comment — "1:1 論理参照... no
//! REFERENCES"), so a bare row is sufficient to exercise `PgSearchBackend`'s
//! own SQL in isolation, mirroring `tests/search_migrations_it.rs`'s own "raw
//! SQL against this spec's own table" convention for a repository test that
//! does not need the full upstream object graph.
//!
//! ## Task 6.2 additions: `following=true` through the real, full pipeline
//! (search spec task 6.2, `Boundary: search_accounts_it`; Requirements 3.3,
//! 3.5)
//!
//! Task 3.1's own tests above prove `PgSearchBackend::search_accounts`
//! itself never filters by `following` (by design) and task 4.2's own
//! `src/search/hydrator/tests.rs` already proves `SearchHydrator::
//! hydrate_accounts`'s `following_only` filter and dedup logic in isolation
//! — but always against a *stub* `RelationshipStateProvider`
//! (`SelectiveFollowingProvider`), never the real, `follows`-table-backed
//! `social_graph::providers::RelProviderImpl` this instance actually wires
//! by default (`social_graph::build_social_graph_module`, already registered
//! into `AppState` by `spawn_test_app`/bootstrap). The gap this task's own
//! dispatch brief calls out — "following-filter... likely lives at the
//! `SearchService`/`SearchHydrator` layer, not the raw `PgSearchBackend`" —
//! is closed here by driving a real `GET /api/v2/search?following=true`
//! request through the actual HTTP router (`crate::server::build_router`,
//! mirroring `tests/search_contract_it.rs`'s established full-pipeline
//! technique) against a genuine `follows` row inserted via
//! `social_graph::repository::upsert_follow` (the real production repository
//! function, not a mock), proving the default, real relationship wiring
//! narrows results end to end — not merely that the hydrator's own filter
//! logic is correct in isolation against a double.
//!
//! Requirement 3.5 (uniqueness/dedup) is deliberately *not* re-tested at this
//! full-pipeline level: `PgSearchBackend::search_accounts`'s own doc comment
//! proves a single call can never itself return the same `AccountRef` twice
//! (each account row contributes at most one output row per side of its
//! `UNION ALL`), so the only way a genuine duplicate can reach
//! `SearchHydrator::hydrate_accounts` in production is via a `resolve=true`
//! remote-resolution candidate that happens to coincide with an
//! already-matched backend result — a `resolve=true`/WebFinger-mocking
//! scenario that is task 6.3's own boundary (`search_resolve_it`), not this
//! task's. `src/search/hydrator/tests.rs::
//! hydrate_accounts_dedups_duplicate_refs_and_renders_every_distinct_account`
//! already exercises the dedup logic itself as a real, DB-backed test
//! (`spawn_test_app`, per this repo's own "unit tests in `<file>/tests.rs`"
//! convention) — reusing that existing, real coverage was judged preferable
//! to fabricating an artificial duplicate-`AccountRef` scenario here that
//! would not correspond to any code path `PgSearchBackend` can actually
//! produce (documented as a CONCERN in this task's own status report).

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::Value;
use tower::ServiceExt;

use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor, ResolvedActor};
use kawasemi::domain::{AccountRef, Id};
use kawasemi::oauth::app_repository::{self, NewApp};
use kawasemi::oauth::model::ScopeSet as ModelScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::search::pg_backend::PgSearchBackend;
use kawasemi::search::ports::{AccountQuery, SearchBackend};
use kawasemi::server;
use kawasemi::social_graph::model::Follow;
use kawasemi::social_graph::repository::upsert_follow;
use kawasemi::test_harness::{TestApp, spawn_test_app};

/// Inserts a minimal `account_profiles` row (a local account), returning the
/// `actor_id` used.
async fn insert_local_account(app: &TestApp, display_name: &str) -> Id {
    let actor_id = app.runtime.ids.next_id();
    sqlx::query(
        "INSERT INTO account_profiles (actor_id, display_name, updated_at) VALUES ($1, $2, $3)",
    )
    .bind(actor_id.as_i64())
    .bind(display_name)
    .bind(app.runtime.clock.now())
    .execute(&app.pool)
    .await
    .expect("inserting a fixture account_profiles row must succeed");
    actor_id
}

/// Inserts a minimal `remote_accounts` row (a known remote account),
/// returning the `id` used.
async fn insert_remote_account(
    app: &TestApp,
    username: &str,
    domain: &str,
    display_name: &str,
) -> Id {
    let id = app.runtime.ids.next_id();
    sqlx::query(
        "INSERT INTO remote_accounts (id, actor_uri, username, domain, display_name, url, \
         fetched_at) VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(id.as_i64())
    .bind(format!("https://{domain}/users/{username}"))
    .bind(username)
    .bind(domain)
    .bind(display_name)
    .bind(format!("https://{domain}/@{username}"))
    .bind(app.runtime.clock.now())
    .execute(&app.pool)
    .await
    .expect("inserting a fixture remote_accounts row must succeed");
    id
}

fn query(term: &str, limit: u32, offset: u32) -> AccountQuery {
    AccountQuery {
        term: term.to_string(),
        following_of: None,
        limit,
        offset,
    }
}

/// A local account is matched by a partial, case-insensitive `display_name`
/// substring (Requirement 3.1), returning a bare `AccountRef::Local`
/// (Requirement 7.2 — identifiers only).
#[tokio::test]
async fn search_accounts_matches_local_account_by_display_name_substring() {
    let app = spawn_test_app().await;
    let backend = PgSearchBackend::new(app.pool.clone(), app.runtime.clone());
    let alice = insert_local_account(&app, "Alice Wonderland").await;

    let matches = backend
        .search_accounts(&query("wonder", 50, 0))
        .await
        .expect("search_accounts must succeed");

    assert_eq!(matches, vec![AccountRef::Local(alice)]);

    app.cleanup().await;
}

/// A known remote account is matched by a partial, case-insensitive
/// `username` substring (Requirement 3.1), returning a bare
/// `AccountRef::Remote`.
#[tokio::test]
async fn search_accounts_matches_remote_account_by_username_substring() {
    let app = spawn_test_app().await;
    let backend = PgSearchBackend::new(app.pool.clone(), app.runtime.clone());
    let bob = insert_remote_account(&app, "bobby", "example.social", "Bob Marley").await;

    let matches = backend
        .search_accounts(&query("BOB", 50, 0))
        .await
        .expect("search_accounts must succeed");

    assert_eq!(matches, vec![AccountRef::Remote(bob)]);

    app.cleanup().await;
}

/// A known remote account is matched by its synthesized `username@domain`
/// acct form even when neither `username` nor `domain` alone contains the
/// full query term (Requirement 3.1's "ハンドル（acct）に対する一致").
#[tokio::test]
async fn search_accounts_matches_remote_account_by_synthesized_acct() {
    let app = spawn_test_app().await;
    let backend = PgSearchBackend::new(app.pool.clone(), app.runtime.clone());
    let carol = insert_remote_account(&app, "carol", "remote.example", "Carol Danvers").await;

    let matches = backend
        .search_accounts(&query("carol@remote", 50, 0))
        .await
        .expect("search_accounts must succeed");

    assert_eq!(matches, vec![AccountRef::Remote(carol)]);

    app.cleanup().await;
}

/// A term matching neither a local `display_name` nor any remote
/// username/domain/display_name/acct field matches nothing on that side —
/// only accounts that actually match appear in the combined result
/// (proving local and remote matching are independently applied, not
/// cross-contaminated).
#[tokio::test]
async fn search_accounts_only_returns_accounts_that_actually_match() {
    let app = spawn_test_app().await;
    let backend = PgSearchBackend::new(app.pool.clone(), app.runtime.clone());
    let alice = insert_local_account(&app, "Alice Wonderland").await;
    insert_remote_account(&app, "bobby", "example.social", "Bob Marley").await;

    let matches = backend
        .search_accounts(&query("wonderland", 50, 0))
        .await
        .expect("search_accounts must succeed");

    assert_eq!(matches, vec![AccountRef::Local(alice)]);

    app.cleanup().await;
}

/// `limit`/`offset` are applied to the combined local+remote result
/// (Requirement 3.4).
#[tokio::test]
async fn search_accounts_applies_limit_and_offset() {
    let app = spawn_test_app().await;
    let backend = PgSearchBackend::new(app.pool.clone(), app.runtime.clone());
    let first = insert_local_account(&app, "Tagalpha").await;
    let second = insert_local_account(&app, "Tagbeta").await;
    let third = insert_remote_account(&app, "taggamma", "example.social", "Tag Gamma").await;

    let page1 = backend
        .search_accounts(&query("tag", 1, 0))
        .await
        .expect("search_accounts page 1 must succeed");
    assert_eq!(page1, vec![AccountRef::Local(first)]);

    let page2 = backend
        .search_accounts(&query("tag", 1, 1))
        .await
        .expect("search_accounts page 2 must succeed");
    assert_eq!(page2, vec![AccountRef::Local(second)]);

    let page3 = backend
        .search_accounts(&query("tag", 1, 2))
        .await
        .expect("search_accounts page 3 must succeed");
    assert_eq!(page3, vec![AccountRef::Remote(third)]);

    let beyond = backend
        .search_accounts(&query("tag", 50, 100))
        .await
        .expect("search_accounts offset-beyond-end must succeed");
    assert!(beyond.is_empty());

    app.cleanup().await;
}

/// A term matching nothing at all returns an empty `Vec`, not an error.
#[tokio::test]
async fn search_accounts_returns_empty_for_no_match() {
    let app = spawn_test_app().await;
    let backend = PgSearchBackend::new(app.pool.clone(), app.runtime.clone());
    insert_local_account(&app, "Alice Wonderland").await;

    let matches = backend
        .search_accounts(&query("nonexistentterm", 50, 0))
        .await
        .expect("search_accounts must succeed even with no matches");
    assert!(matches.is_empty());

    app.cleanup().await;
}

// ==========================================================================
// Task 6.2 additions: `following=true` through the real, full pipeline
// (Requirement 3.3). See this file's own doc comment, "Task 6.2 additions",
// for why this drives the actual `GET /api/v2/search` HTTP endpoint against
// a genuine `social_graph::repository::upsert_follow` row rather than a
// stub `RelationshipStateProvider`.
//
// Fixture plumbing below mirrors `tests/search_contract_it.rs`'s own
// already-reviewed helpers of the same names (each `tests/*.rs` file is its
// own compiled crate, so this deliberately duplicates rather than imports).
// ==========================================================================

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
            display_name: format!("Search Accounts IT {handle_str}"),
            summary: "an actor used by the search_accounts_it integration test".to_string(),
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

async fn insert_account_profile_for(app: &TestApp, actor_id: Id, display_name: &str) {
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
            name: "Search Accounts IT Client".to_string(),
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

/// Records a genuine `follower -> followee` follow via the real production
/// repository function (`social_graph::repository::upsert_follow`), not a
/// mock/stub — the ground truth `RelProviderImpl::relationships` (this
/// instance's real, default `RelationshipStateProvider`) reads from.
async fn create_real_follow(app: &TestApp, follower: Id, followee: Id) {
    let follow_id = app.runtime.ids.next_id();
    upsert_follow(
        &app.pool,
        follow_id,
        &Follow {
            follower: AccountRef::Local(follower),
            followee: AccountRef::Local(followee),
            reblogs: true,
            notify: false,
            languages: Vec::new(),
            activity_id: format!("https://kawasemi.example/activities/{}", follow_id.as_i64()),
            created_at: app.runtime.clock.now(),
        },
    )
    .await
    .expect("upsert_follow must succeed for a fresh (follower, followee) pair");
}

const TEST_DOMAIN: &str = "test-harness.kawasemi.internal";

fn req(method: &str, path: &str, token: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("x-forwarded-proto", "https")
        .header("x-forwarded-host", TEST_DOMAIN);
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

async fn search(router: &Router, token: &str, query: &str) -> (StatusCode, Value) {
    send(
        router,
        req("GET", &format!("/api/v2/search?{query}"), Some(token)),
    )
    .await
}

fn account_ids(body: &Value) -> Vec<String> {
    body["accounts"]
        .as_array()
        .expect("accounts must be a JSON array")
        .iter()
        .map(|account| account["id"].as_str().unwrap().to_string())
        .collect()
}

/// Requirement 3.3: `following=true`, driven through the real
/// `GET /api/v2/search` HTTP endpoint against this instance's real, default
/// `follows`-table-backed relationship wiring (`social_graph::providers::
/// RelProviderImpl`, not a stub), narrows the `accounts` results to only the
/// accounts the authenticated searcher genuinely follows — proven against
/// two accounts that both match the search term, only one of which the
/// searcher actually follows via a real `upsert_follow` row.
#[tokio::test]
async fn search_following_true_narrows_to_genuinely_followed_accounts_through_full_pipeline() {
    let app = spawn_test_app().await;
    let router = server::build_router(app.state.clone());

    let searcher = insert_actor_fixture(&app, "search_acc_it_searcher").await;
    let followed = insert_actor_fixture(&app, "search_acc_it_followed").await;
    let not_followed = insert_actor_fixture(&app, "search_acc_it_notfollowed").await;
    insert_account_profile_for(&app, followed.id, "Followgate Followed Account").await;
    insert_account_profile_for(&app, not_followed.id, "Followgate NotFollowed Account").await;

    create_real_follow(&app, searcher.id, followed.id).await;

    let app_id = register_test_app(&app).await;
    let token = issue_test_token(&app, app_id, searcher.id, &["read:search"]).await;

    // Control: without `following=true`, both matching accounts are
    // returned -- proving the narrowing below is the `following` filter
    // actually taking effect, not an accidental empty/coincidental result.
    let (status, unscoped) = search(&router, &token, "q=Followgate&type=accounts").await;
    assert_eq!(status, StatusCode::OK, "got: {unscoped:?}");
    assert_eq!(
        account_ids(&unscoped).len(),
        2,
        "both accounts must match the unscoped search term"
    );

    let (status, scoped) =
        search(&router, &token, "q=Followgate&type=accounts&following=true").await;
    assert_eq!(status, StatusCode::OK, "got: {scoped:?}");
    assert_eq!(
        account_ids(&scoped),
        vec![followed.id.as_i64().to_string()],
        "following=true must narrow to exactly the genuinely-followed account"
    );

    app.cleanup().await;
}
