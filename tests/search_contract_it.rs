//! Full-pipeline integration-level contract test for `SearchResults`/`Tag`
//! (search spec task 6.1, `.kiro/specs/search/tasks.md`, "6.1 (P)
//! SearchResults / Tag 契約のゴールデンテスト", `_Depends: 5.3_`), Requirements
//! 1.1-1.5. design.md's File Structure Plan names this exact file
//! (`tests/search_contract_it.rs`, "SearchResults / Tag ゴールデン（決定的・
//! 空配列規律・Account/Status 埋め込み形）（契約）").
//!
//! ## Relationship to task 4.1's own unit-level goldens
//! Task 4.1 already registered four goldens
//! (`tests/golden/search/{search_results_empty,search_results_populated,
//! tag_with_history,tag_without_history}.json`) from its own
//! `#[cfg(test)] mod tests` unit tests
//! (`src/search/tag_serializer/tests.rs`/`src/search/result_serializer/
//! tests.rs`), built by feeding hand-constructed `TagView`/stand-in Account-
//! and Status-shaped JSON directly into `TagSerializer::build_tag`/
//! `SearchResultSerializer::build_search_results` — see this spec's own
//! `tasks.md` "## Implementation Notes" (the note addressed to this task)
//! for why that unit-level registration deliberately deferred the
//! full-pipeline contract test to this task. Those four goldens are
//! consumed exclusively by that unit-level suite and are *not* reused here:
//! a search response driven through the real `GET /api/v2/search` endpoint,
//! a real `SearchService`/`PgSearchBackend`/`SearchHydrator`, and real
//! upstream `accounts::serializer`/`statuses::serializer` output produces
//! genuinely-resolved `id`/`created_at`/`account`/`status` values that
//! cannot be made to literally equal task 4.1's hand-picked stand-in
//! values. This file therefore registers its own, separate set of goldens
//! under `tests/golden/search/search_contract_it_*.json`, mirroring
//! `tests/notification_contract_it.rs`'s (notifications task 5.1 vs 2.1)/
//! `tests/status_contract_it.rs`'s (statuses-core task 8.2 vs 3.3) own
//! identical "unit-level goldens vs. this file's own integration-level
//! goldens, driven end to end through `spawn_test_app` and the real service
//! layer" precedent exactly, per `crate::contract::assert_golden`'s own
//! documented convention that golden-file paths are caller-owned.
//!
//! ## What "real pipeline" means here
//! Every scenario below drives the *actual* production seam end to end via
//! `tower::ServiceExt::oneshot` against `crate::server::build_router`
//! (mirroring `tests/notification_contract_it.rs`'s/`tests/
//! status_contract_it.rs`'s own established in-process-HTTP technique):
//! real Bearer/`read:search` scope enforcement (`crate::search::endpoint::
//! search`), a real `POST /api/v1/statuses` (`StatusService::create_status`,
//! including its real hashtag extraction/persistence, `persist_tags`), a
//! real `account_profiles` row (this file inserts one directly — mirroring
//! `tests/search_accounts_it.rs`'s own already-reviewed convention that
//! `account_profiles`/`local_actors` have no physical FK, so a bare fixture
//! row is sufficient to exercise the real `PgSearchBackend::search_accounts`
//! SQL, task 3.1's own boundary, strictly upstream of and outside this
//! task's boundary), and the real, already-wired (task 5.3) chain from
//! `GET /api/v2/search` through `SearchService::search` ->
//! `PgSearchBackend` (including its on-demand `HashtagIndexer::
//! catch_up_from_watermark`, task 3.2) -> `SearchHydrator` (task 4.2,
//! embedding real upstream `AccountService::show_account`/`Status` JSON
//! verbatim) -> `SearchResultSerializer`/`TagSerializer` (task 4.1, this
//! task's own contract).
//!
//! ## Forcing one consistent origin across every request (so "verbatim
//! upstream embedding" can be proven by *byte-for-byte* JSON equality, not
//! just field spot-checks)
//! `SearchHydrator`/`TagSerializer` resolve their own absolute URLs from a
//! **fixed** `ForwardedOrigin::resolve("https", <configured domain>, None,
//! None)` — never from the live request's own `X-Forwarded-*` headers (see
//! `src/search/hydrator.rs`'s/`src/search/tag_serializer.rs`'s own doc
//! comments, "No per-request `ForwardedOrigin` available", mirroring
//! `NotificationService::origin`'s identical precedent). `spawn_test_app`
//! configures that domain as the literal `"test-harness.kawasemi.internal"`
//! (`src/test_harness.rs`, already directly relied on by `tests/
//! status_contract_it.rs`/`tests/polls_it.rs`). Every *other* endpoint this
//! file calls (`POST /api/v1/statuses`, `GET /api/v1/accounts/:id`) resolves
//! its own per-request `ResolvedOrigin` from `X-Forwarded-Proto`/
//! `X-Forwarded-Host` when present (`crate::media::ResolvedOrigin`,
//! `crate::api::pagination::ForwardedOrigin::resolve`). [`req`] below always
//! sends `X-Forwarded-Proto: https` and `X-Forwarded-Host:
//! test-harness.kawasemi.internal` on every request in this file, so every
//! endpoint's own resolved origin — the create-status response's own,
//! the ground-truth account fetch's own, and the search response's
//! internally-fixed one — all agree, making genuine byte-for-byte
//! `assert_eq!` between a separately-fetched upstream JSON value and its
//! embedded copy inside a `SearchResults` response meaningful rather than
//! coincidental.
//!
//! ## Determinism (steering `tech.md`'s "決定性の強制"; Requirement 1.5)
//! Every non-deterministic seam this pipeline touches is drawn from
//! `spawn_test_app`'s fixed `RuntimeContext::deterministic` boundary (id
//! generator, clock) — this file never reads the OS clock or generates its
//! own ids, so a given scenario's sequence of fixture/HTTP calls always
//! produces the same `id`/`created_at` values run over run.
//! [`the_same_full_scenario_reproduces_byte_identical_json_across_two_independently_spawned_instances`]
//! below proves this directly by running the identical scenario against two
//! independently-`spawn_test_app`-booted instances in the same test run
//! (mirroring `tests/notification_contract_it.rs`'s/`tests/
//! status_contract_it.rs`'s own identical two-instance proof), in addition
//! to every golden test's own comparison against its checked-in file being
//! itself a rerun-reproduces-the-same-JSON proof each time the suite runs.
//!
//! ## The real-client-capture-fixture requirement ("実クライアントキャプ
//! チャをフィクスチャ登録する")
//! Per this task's own dispatch brief and mirroring `tests/
//! notification_contract_it.rs`'s/`tests/status_contract_it.rs`'s own
//! already-reviewed resolution to the identical requirement (this sandboxed
//! development environment has no way to capture genuine live traffic from
//! a real Mastodon client):
//! [`real_full_search_results_json_is_registered_as_a_fixture_and_holds_a_second_instances_live_output`]
//! below registers a real (not mocked/hand-typed) SearchResults JSON
//! exchange — produced by this crate's own real, deterministic-boundary
//! search pipeline via `register_fixture` — and proves a second,
//! independently-booted instance's live output for the equivalent scenario
//! is held to that fixture's `response_body`, exactly the shape a real
//! client capture would eventually be swapped into.
//!
//! Unlike `tests/notification_contract_it.rs`, this sandbox *does* have a
//! reachable PostgreSQL (`pg_lsclusters`'s `kawasemi_test` role/database),
//! so every golden below was actually recorded by running this file once
//! with `KAWASEMI_UPDATE_GOLDEN=1`, then re-run without it to confirm a
//! clean comparison — not fabricated by hand.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor, ResolvedActor};
use kawasemi::contract::{CapturedExchange, assert_golden, load_fixture, register_fixture};
use kawasemi::domain::Id;
use kawasemi::oauth::app_repository::{self, NewApp};
use kawasemi::oauth::model::ScopeSet as ModelScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::search::model::TagView;
use kawasemi::search::tag_serializer::TagSerializer;
use kawasemi::server;
use kawasemi::test_harness::{TestApp, spawn_test_app};

/// `spawn_test_app`'s own fixed `config.server.domain` literal (see this
/// file's own doc comment, "Forcing one consistent origin").
const TEST_DOMAIN: &str = "test-harness.kawasemi.internal";

// ---- Fixture plumbing (each `tests/*.rs` file is its own compiled crate,
// so this deliberately duplicates `tests/notification_contract_it.rs`'s own
// established conventions rather than importing them). ----

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
            display_name: format!("Search Contract IT {handle_str}"),
            summary: "an actor used by the search_contract_it integration test".to_string(),
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

/// Inserts a minimal `account_profiles` row directly (mirrors `tests/
/// search_accounts_it.rs::insert_local_account`'s own already-reviewed
/// convention: `account_profiles` has no physical FK to `local_actors`, and
/// `PgSearchBackend::search_accounts` (task 3.1, strictly upstream of this
/// task's own boundary) only ever matches against this table's own
/// `display_name` column, so this is the minimal real fixture needed for a
/// real actor to become findable by the real `search_accounts` SQL).
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
            name: "Search Contract IT Client".to_string(),
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

/// Builds a request, always forcing `X-Forwarded-Proto`/`X-Forwarded-Host`
/// to [`TEST_DOMAIN`] (see this file's own doc comment, "Forcing one
/// consistent origin") so every endpoint's own `ResolvedOrigin` agrees with
/// `SearchHydrator`/`TagSerializer`'s internally-fixed origin.
fn req(method: &str, path: &str, token: Option<&str>, body: Option<Value>) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("x-forwarded-proto", "https")
        .header("x-forwarded-host", TEST_DOMAIN);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    match body {
        Some(value) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::to_vec(&value).expect("serialize body"),
            ))
            .expect("build request"),
        None => builder.body(Body::empty()).expect("build request"),
    }
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
    let (status, resp) = send(
        router,
        req("POST", "/api/v1/statuses", Some(token), Some(body)),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "status creation must succeed: {resp:?}"
    );
    resp
}

/// Real, unauthenticated `GET /api/v1/accounts/:id` (`show_account` accepts
/// an `OptionalActor`, no scope required) — this file's own ground-truth
/// source for "what the real upstream Account JSON contract renders as",
/// fetched with the same forced origin every other request in this file
/// uses.
async fn get_account(router: &Router, actor_id: Id) -> Value {
    let (status, body) = send(
        router,
        req(
            "GET",
            &format!("/api/v1/accounts/{}", actor_id.as_i64()),
            None,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {body:?}");
    body
}

async fn search(router: &Router, token: &str, query: &str) -> (StatusCode, Value) {
    send(
        router,
        req("GET", &format!("/api/v2/search?{query}"), Some(token), None),
    )
    .await
}

fn unique_fixture_name(label: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("search_contract_it_{label}_{nanos}_{seq}")
}

/// Best-effort cleanup of a fixture file this test registers, regardless of
/// whether the test body panicked (mirrors `tests/
/// notification_contract_it.rs`'s/`tests/contract_harness_fixture_it.rs`'s
/// identical `FixtureGuard` convention), so `tests/fixtures/` does not
/// accumulate orphaned files across runs.
struct FixtureGuard(String);
impl Drop for FixtureGuard {
    fn drop(&mut self) {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(format!("{}.json", self.0));
        let _ = std::fs::remove_file(path);
    }
}

// ==========================================================================
// Shared scenario: one search term ("kwsmfull") that simultaneously matches
// a real account's `display_name`, a real status's `content`, and a real
// hashtag extracted from that same status's content — proving all three
// SearchResults arrays populate from one real pipeline run, each holding
// the real upstream JSON verbatim.
// ==========================================================================

const SEARCH_TERM: &str = "kwsmfull";
const TARGET_HANDLE: &str = "search_contract_full_target";
const SEARCHER_HANDLE: &str = "search_contract_full_searcher";

struct FullScenario {
    router: Router,
    searcher_token: String,
    ground_truth_account: Value,
    ground_truth_status: Value,
}

/// Builds the shared scenario end to end through real HTTP: a target actor
/// with a real `account_profiles.display_name` containing [`SEARCH_TERM`],
/// a real status (posted by that actor) whose content both contains
/// [`SEARCH_TERM`] as plain text and carries `#kwsmfull` as a real,
/// extracted-and-persisted hashtag, and a separate searcher actor holding
/// the `read:search`-scoped token every search call below uses.
async fn build_full_scenario(app: &TestApp) -> FullScenario {
    let router = real_router(app);
    let target = insert_actor_fixture(app, TARGET_HANDLE).await;
    let searcher = insert_actor_fixture(app, SEARCHER_HANDLE).await;
    insert_account_profile(app, target.id, "Search Contract KWSMFULL Actor").await;

    let app_id = register_test_app(app).await;
    let target_token = issue_test_token(app, app_id, target.id, &["write:statuses"]).await;
    let searcher_token = issue_test_token(app, app_id, searcher.id, &["read:search"]).await;

    let created = create_status(
        &router,
        &target_token,
        json!({"status": "posting about kwsmfull today #kwsmfull"}),
    )
    .await;

    let ground_truth_account = get_account(&router, target.id).await;
    let ground_truth_status = created;

    FullScenario {
        router,
        searcher_token,
        ground_truth_account,
        ground_truth_status,
    }
}

fn expected_tag_json() -> Value {
    TagSerializer::new(TEST_DOMAIN).build_tag(&TagView {
        name: SEARCH_TERM.to_string(),
        url: format!("/tags/{SEARCH_TERM}"),
        history: Vec::new(),
    })
}

// ==========================================================================
// (1) The main golden test: all three types populated in one unscoped
// search, each holding real upstream JSON verbatim (Requirements 1.1, 1.2,
// 1.3).
// ==========================================================================

/// Requirements 1.1 (three-field envelope), 1.2 (`accounts`/`statuses`
/// embed upstream Account/Status JSON verbatim, never re-derived), 1.3
/// (`hashtags` carries `name`/`url`/`history` Tag JSON).
#[tokio::test]
async fn full_search_results_json_matches_the_registered_contract_golden() {
    let app = spawn_test_app().await;
    let scenario = build_full_scenario(&app).await;

    let (status, body) = search(
        &scenario.router,
        &scenario.searcher_token,
        &format!("q={SEARCH_TERM}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {body:?}");

    let accounts = body["accounts"]
        .as_array()
        .expect("accounts must be a JSON array");
    let statuses = body["statuses"]
        .as_array()
        .expect("statuses must be a JSON array");
    let hashtags = body["hashtags"]
        .as_array()
        .expect("hashtags must be a JSON array");
    assert_eq!(accounts.len(), 1, "expected exactly one account match");
    assert_eq!(statuses.len(), 1, "expected exactly one status match");
    assert_eq!(hashtags.len(), 1, "expected exactly one hashtag match");

    // Requirement 1.2: byte-for-byte identical to the real, separately
    // fetched upstream Account/Status JSON -- never re-derived/reshaped by
    // SearchResultSerializer/SearchHydrator.
    assert_eq!(
        accounts[0], scenario.ground_truth_account,
        "the embedded account must be the real upstream Account JSON verbatim"
    );
    assert_eq!(
        statuses[0], scenario.ground_truth_status,
        "the embedded status must be the real upstream Status JSON verbatim"
    );

    // Requirement 1.3: Tag JSON carries name/url/history exactly as
    // TagSerializer builds it.
    assert_eq!(hashtags[0], expected_tag_json());
    assert_eq!(hashtags[0]["name"], SEARCH_TERM);
    assert_eq!(
        hashtags[0]["url"],
        format!("https://{TEST_DOMAIN}/tags/{SEARCH_TERM}")
    );
    assert_eq!(hashtags[0]["history"], json!([]));

    assert_golden("tests/golden/search/search_contract_it_full.json", &body);

    app.cleanup().await;
}

// ==========================================================================
// (2) `type`-scoped searches: only the requested type populates, the other
// two are always `[]`, never `null` (Requirements 1.1, 1.4, 2.2).
// ==========================================================================

/// Requirements 1.1, 1.4 (empty type -> `[]`, never `null`).
#[tokio::test]
async fn type_scoped_search_leaves_the_other_two_types_as_empty_arrays_not_null() {
    let app = spawn_test_app().await;
    let scenario = build_full_scenario(&app).await;

    for (kind, populated_field) in [
        ("accounts", "accounts"),
        ("statuses", "statuses"),
        ("hashtags", "hashtags"),
    ] {
        let (status, body) = search(
            &scenario.router,
            &scenario.searcher_token,
            &format!("q={SEARCH_TERM}&type={kind}"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "type={kind} got: {body:?}");

        for field in ["accounts", "statuses", "hashtags"] {
            assert!(
                body[field].is_array(),
                "type={kind}: field {field:?} must always be a JSON array, got {:?}",
                body[field]
            );
            assert_ne!(
                body[field],
                Value::Null,
                "type={kind}: field {field:?} must never be null"
            );
            if field == populated_field {
                assert_eq!(
                    body[field].as_array().unwrap().len(),
                    1,
                    "type={kind}: the requested type must be populated: {body:?}"
                );
            } else {
                assert_eq!(
                    body[field],
                    json!([]),
                    "type={kind}: unrequested field {field:?} must be an empty array: {body:?}"
                );
            }
        }
    }

    app.cleanup().await;
}

// ==========================================================================
// (3) A term matching nothing at all: every field is `[]`, never `null`
// (Requirement 1.4).
// ==========================================================================

/// Requirement 1.4, the "no results at all" case.
#[tokio::test]
async fn search_with_no_matches_returns_all_three_types_as_empty_arrays_not_null() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let searcher = insert_actor_fixture(&app, "search_contract_empty_searcher").await;
    let app_id = register_test_app(&app).await;
    let searcher_token = issue_test_token(&app, app_id, searcher.id, &["read:search"]).await;

    let (status, body) = search(&router, &searcher_token, "q=nonexistentsearchcontractterm").await;
    assert_eq!(status, StatusCode::OK, "got: {body:?}");

    assert_eq!(body["accounts"], json!([]));
    assert_eq!(body["statuses"], json!([]));
    assert_eq!(body["hashtags"], json!([]));
    assert_ne!(body["accounts"], Value::Null);
    assert_ne!(body["statuses"], Value::Null);
    assert_ne!(body["hashtags"], Value::Null);

    assert_golden("tests/golden/search/search_contract_it_empty.json", &body);

    app.cleanup().await;
}

// ==========================================================================
// (4) Reproducibility across two independently-spawned instances
// (Requirement 1.5, steering `tech.md`'s "決定性の強制").
// ==========================================================================

/// The identical full scenario, driven against two independently-
/// `spawn_test_app`-booted instances, produces byte-for-byte identical
/// SearchResults JSON -- no non-determinism (clock, id) leaks through the
/// real pipeline. Mirrors `tests/notification_contract_it.rs`'s/`tests/
/// status_contract_it.rs`'s own identical two-instance proof.
#[tokio::test]
async fn the_same_full_scenario_reproduces_byte_identical_json_across_two_independently_spawned_instances()
 {
    let app_a = spawn_test_app().await;
    let app_b = spawn_test_app().await;

    let scenario_a = build_full_scenario(&app_a).await;
    let scenario_b = build_full_scenario(&app_b).await;

    let (_status_a, json_a) = search(
        &scenario_a.router,
        &scenario_a.searcher_token,
        &format!("q={SEARCH_TERM}"),
    )
    .await;
    let (_status_b, json_b) = search(
        &scenario_b.router,
        &scenario_b.searcher_token,
        &format!("q={SEARCH_TERM}"),
    )
    .await;

    assert_eq!(
        json_a, json_b,
        "two independently spawn_test_app-booted instances driving the identical \
         fixture/HTTP sequence must produce byte-for-byte identical SearchResults JSON"
    );

    assert_golden("tests/golden/search/search_contract_it_full.json", &json_a);
    assert_golden("tests/golden/search/search_contract_it_full.json", &json_b);

    app_a.cleanup().await;
    app_b.cleanup().await;
}

// ==========================================================================
// (5) Real-client-capture fixture registration proof (see this file's own
// doc comment, "The real-client-capture-fixture requirement").
// ==========================================================================

/// Registers a real SearchResults JSON -- produced by the real search
/// pipeline through a first `spawn_test_app` instance -- as a fixture via
/// `register_fixture`, then proves a *second*, independently-booted
/// instance's live output for the equivalent scenario is held to that
/// fixture's `response_body`.
#[tokio::test]
async fn real_full_search_results_json_is_registered_as_a_fixture_and_holds_a_second_instances_live_output()
 {
    let app_a = spawn_test_app().await;
    let scenario_a = build_full_scenario(&app_a).await;
    let (_status_a, json_a) = search(
        &scenario_a.router,
        &scenario_a.searcher_token,
        &format!("q={SEARCH_TERM}"),
    )
    .await;

    let fixture_name = unique_fixture_name("real_full");
    let _fixture_guard = FixtureGuard(fixture_name.clone());
    register_fixture(
        &fixture_name,
        CapturedExchange {
            method: "GET".to_string(),
            path: "/api/v2/search?q={q}".to_string(),
            request_body: None,
            status: 200,
            response_body: json_a.clone(),
        },
    );

    // (Requirement 1.5) Round-trips unchanged through the public extension
    // point.
    let loaded = load_fixture(&fixture_name);
    assert_eq!(loaded.response_body, json_a);

    // A second, independently-spawned instance's live output -- generated
    // purely from its own deterministic RuntimeContext, driving the
    // identical scenario -- is held to the fixture-derived acceptance
    // criterion via direct JSON comparison (mirrors `tests/
    // notification_contract_it.rs`'s/`tests/status_contract_it.rs`'s own
    // identical "why a direct comparison, not `assert_golden`" reasoning:
    // more than one `#[tokio::test]` in this file can run concurrently, so
    // `KAWASEMI_UPDATE_GOLDEN` cannot safely be set mid-test).
    let app_b = spawn_test_app().await;
    let scenario_b = build_full_scenario(&app_b).await;
    let (_status_b, json_b) = search(
        &scenario_b.router,
        &scenario_b.searcher_token,
        &format!("q={SEARCH_TERM}"),
    )
    .await;
    assert_eq!(
        json_b, loaded.response_body,
        "a second independently-booted instance's live SearchResults JSON must match the \
         fixture-registered real-client-capture stand-in exactly"
    );

    app_a.cleanup().await;
    app_b.cleanup().await;
}
