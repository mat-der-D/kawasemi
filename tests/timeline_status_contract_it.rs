//! Integration tests for timelines task 6.2 (`.kiro/specs/timelines/tasks.md`,
//! "6.2 (P) 単一生成点の一致と契約テストを整備する", `_Boundary:
//! TimelineMatcher, StatusHydrator_`): part (b) of that task's own dispatch
//! brief — "タイムライン応答が statuses-core Status ゴールデンと整合（viewer
//! 操作状態・reblog ネスト・null 規律を上流から継承）することを
//! api-foundation 契約ハーネスで検証、実クライアントキャプチャをフィクスチャ
//! 登録する" (Requirements 10.1-10.4). Part (a) (REST-vs-`TimelineMatcher`
//! membership equivalence, Requirements 8.1, 8.2) lives in the sibling file
//! `tests/timeline_matcher_rest_equivalence_it.rs`.
//!
//! ## "Reuses the statuses-core Status golden, does not redefine one"
//! design.md's own Test subsection is explicit: "契約は statuses-core の
//! Status ゴールデンを再利用し、タイムライン応答は実クライアントキャプチャを
//! フィクスチャ化して受け入れ基準にする" — i.e. this file must *not* invent
//! a second, parallel Status-JSON schema/golden of its own. `tests/
//! status_contract_it.rs` (statuses-core task 8.2, already reviewed) already
//! owns that golden via `GET /api/v1/statuses/{id}`
//! (`StatusesEndpointsState::render_status_json`). This file's own strategy
//! for "reusing, not redefining" is therefore the strongest form of reuse
//! available: every test below drives the *same* fixture status through
//! *both* `GET /api/v1/statuses/{id}` (the already-contract-tested detail
//! endpoint) *and* a real timeline endpoint (`GET /api/v1/timelines/home`),
//! for the identical viewer, and asserts the two JSON values are byte-for-
//! byte `assert_eq!`-identical. This is a meaningfully *stronger* proof than
//! re-checking the same field list against a fresh golden file would be:
//! `StatusHydrator::hydrate_one` (`src/timelines/hydrator.rs`, task 4.1) is
//! its own, independently-written `Status -> StatusRenderInput` assembly
//! (that module's own doc comment: "since `StatusesEndpointsState`'s version
//! is private to its own module and not reusable as a function call from
//! `timelines`... you must write this assembly glue yourself") — i.e. two
//! separately-implemented pipelines that both delegate the actual field
//! mapping to the one shared `statuses::serializer::status_to_json`
//! (Requirement 10.1). A byte-for-byte comparison between their two outputs
//! for the identical underlying row is exactly the check that would catch
//! either pipeline silently drifting from the other — precisely what
//! "整合" (conformance) means here, and a strictly stronger claim than
//! "each individually contains the field."
//!
//! ## Field-presence / null-discipline list
//! `normal_status_json_matches_the_registered_contract_golden_reflects_null_discipline_via_timeline`
//! below reuses the *exact same* field list `tests/status_contract_it.rs::
//! normal_status_json_matches_the_registered_contract_golden` already checks
//! (copied verbatim, not re-derived) — the golden's own field contract,
//! applied to a timeline-sourced element instead of a detail-endpoint one.
//!
//! ## The real-client-capture-fixture requirement
//! Mirrors `tests/status_contract_it.rs`'s own already-reviewed resolution
//! (its own doc comment, "The real-client-capture-fixture requirement"): this
//! sandboxed development environment has no way to capture genuine live
//! traffic from a real Mastodon client. `home_timeline_response_registers_a_real_client_capture_fixture_and_holds_a_second_instances_live_output`
//! below registers a real (not mocked/hand-typed) home-timeline response
//! array — produced by this crate's own real, deterministic-boundary
//! `TimelineService`/`StatusHydrator` pipeline via `register_fixture` — and
//! proves a second, independently-booted instance's live output for the
//! equivalent scenario is held to that fixture's `response_body`, exactly
//! the shape a real client capture would eventually be swapped into.
//!
//! ## Fixture plumbing
//! Duplicated per this crate's own established sibling-test-file convention
//! — mirrors `tests/status_contract_it.rs`'s/`tests/
//! timelines_endpoints_it.rs`'s own identical helpers.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor};
use kawasemi::contract::{CapturedExchange, load_fixture, register_fixture};
use kawasemi::domain::Id;
use kawasemi::oauth::app_repository::{self, NewApp};
use kawasemi::oauth::model::ScopeSet as ModelScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::server;
use kawasemi::test_harness::{TestApp, spawn_test_app};
use kawasemi::timelines::endpoints::HOME_TIMELINE_PATH;

// ---- Fixture plumbing ------------------------------------------------------

async fn actor_fixture(app: &TestApp, handle_str: &str) -> Id {
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
            display_name: format!("Timeline Status Contract IT {handle_str}"),
            summary: "an actor used by the timeline_status_contract_it integration test"
                .to_string(),
        })
        .await
        .expect("create_actor (with signing key provisioning) must succeed");

    actor.id
}

fn real_router(app: &TestApp) -> Router {
    server::build_router(app.state.clone())
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
            name: "Timeline Status Contract IT Client".to_string(),
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

fn req(method: &str, path: &str, token: Option<&str>, body: Option<Value>) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(path);
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

fn id_of(v: &Value) -> String {
    v["id"].as_str().expect("id must be a string").to_string()
}

/// Finds the timeline item with `id` inside a `GET .../timelines/*` response
/// array — panics with the whole array if not found, so a failing test shows
/// exactly what *was* returned.
fn find_item<'a>(timeline_body: &'a Value, id: &str) -> &'a Value {
    timeline_body
        .as_array()
        .expect("timeline response body must be a JSON array")
        .iter()
        .find(|item| item["id"].as_str() == Some(id))
        .unwrap_or_else(|| panic!("expected status {id} in timeline response: {timeline_body:?}"))
}

fn unique_fixture_name(label: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("timeline_status_contract_it_{label}_{nanos}_{seq}")
}

/// Best-effort cleanup of a fixture file this test registers, regardless of
/// whether the test body panicked — mirrors `tests/status_contract_it.rs`'s
/// identical `FixtureGuard` convention.
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
// (1) Field presence / null discipline, reusing statuses-core's own field
// list (Requirements 10.1, 10.2)
// ==========================================================================

/// A plain, non-reblog, no-poll, no-reply, no-edit post: every contract field
/// is present and every optional field observes null discipline, on the
/// *timeline*-sourced element — the exact field list `tests/
/// status_contract_it.rs::normal_status_json_matches_the_registered_contract_golden`
/// already checks on the detail-endpoint element, copied verbatim, not
/// re-derived (Requirement 10.1: "本 spec で再定義しない").
#[tokio::test]
async fn home_timeline_status_json_has_every_contract_field_and_observes_null_discipline() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = actor_fixture(&app, "alice_tl_contract_normal").await;
    let app_id = register_test_app(&app).await;
    let alice_token =
        issue_test_token(&app, app_id, alice, &["write:statuses", "read:statuses"]).await;

    let created = create_status(
        &router,
        &alice_token,
        json!({"status": "hello timeline contract world", "language": "en"}),
    )
    .await;
    let id = id_of(&created);

    let (status, timeline_body) = send(
        &router,
        req(
            "GET",
            &format!("{HOME_TIMELINE_PATH}?limit=40"),
            Some(&alice_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {timeline_body:?}");
    let item = find_item(&timeline_body, &id);

    for field in [
        "id",
        "uri",
        "url",
        "account",
        "content",
        "created_at",
        "visibility",
        "sensitive",
        "spoiler_text",
        "media_attachments",
        "mentions",
        "tags",
        "emojis",
        "reblogs_count",
        "favourites_count",
        "replies_count",
        "in_reply_to_id",
        "in_reply_to_account_id",
        "reblog",
        "poll",
        "language",
        "edited_at",
        "favourited",
        "reblogged",
        "bookmarked",
        "pinned",
        "muted",
    ] {
        assert!(
            item.get(field).is_some(),
            "expected field {field:?} in timeline Status JSON, got {item:?}"
        );
    }

    assert_eq!(item["poll"], Value::Null);
    assert_eq!(item["in_reply_to_id"], Value::Null);
    assert_eq!(item["in_reply_to_account_id"], Value::Null);
    assert_eq!(item["edited_at"], Value::Null);
    assert_eq!(item["reblog"], Value::Null);
    assert_eq!(item["language"], "en");

    let obj = item.as_object().expect("Status JSON is an object");
    for dialect_field in [
        "quote",
        "quote_id",
        "quoted_status_id",
        "emoji_reactions",
        "reactions",
    ] {
        assert!(
            !obj.contains_key(dialect_field),
            "timeline Status JSON must not contain dialect field {dialect_field:?}"
        );
    }

    app.cleanup().await;
}

// ==========================================================================
// (2) Byte-identical to the already-contract-tested detail endpoint —
// "reuses, does not redefine" the statuses-core Status golden, and proves
// StatusHydrator's independently-written assembly glue agrees with
// StatusesEndpointsState's own (Requirements 10.1, 10.2)
// ==========================================================================

/// The identical post, fetched through `GET /api/v1/statuses/{id}` (the
/// already contract-tested detail endpoint) and through
/// `GET /api/v1/timelines/home` (this task's own boundary), after the
/// viewer has favourited/bookmarked their own post — both real viewer
/// operation-state fields (Requirement 10.2) and every other field must be
/// byte-for-byte identical between the two independently-implemented
/// pipelines.
#[tokio::test]
async fn home_timeline_status_json_is_byte_identical_to_the_statuses_detail_endpoint_including_operation_state()
 {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = actor_fixture(&app, "alice_tl_contract_ops").await;
    let app_id = register_test_app(&app).await;
    let alice_token = issue_test_token(
        &app,
        app_id,
        alice,
        &[
            "write:statuses",
            "write:favourites",
            "write:bookmarks",
            "read:statuses",
        ],
    )
    .await;

    let created = create_status(
        &router,
        &alice_token,
        json!({"status": "alice's own post, favourited and bookmarked by herself"}),
    )
    .await;
    let id = id_of(&created);

    for action in ["favourite", "bookmark"] {
        let (status, resp) = send(
            &router,
            req(
                "POST",
                &format!("/api/v1/statuses/{id}/{action}"),
                Some(&alice_token),
                None,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{action} must succeed: {resp:?}");
    }

    let (status, detail) = send(
        &router,
        req(
            "GET",
            &format!("/api/v1/statuses/{id}"),
            Some(&alice_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {detail:?}");
    assert_eq!(detail["favourited"], true);
    assert_eq!(detail["bookmarked"], true);

    let (status, timeline_body) = send(
        &router,
        req(
            "GET",
            &format!("{HOME_TIMELINE_PATH}?limit=40"),
            Some(&alice_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {timeline_body:?}");
    let item = find_item(&timeline_body, &id);

    assert_eq!(
        item, &detail,
        "a timeline element for the same status, viewed by the same actor, must be \
         byte-for-byte identical to that status's own GET /api/v1/statuses/{{id}} response \
         (Requirement 10.1: the timeline never redefines the Status JSON contract)"
    );

    app.cleanup().await;
}

/// A boost, nested under `reblog` on the timeline: the outer boost item and
/// its nested `reblog` object must each be byte-for-byte identical to their
/// own respective `GET /api/v1/statuses/{id}` responses (Requirement 10.3),
/// proving `StatusHydrator`'s non-recursive nesting reuses the same
/// contract at both levels.
#[tokio::test]
async fn home_timeline_boost_nests_a_reblog_byte_identical_to_its_own_detail_endpoint_response() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = actor_fixture(&app, "alice_tl_contract_reblog").await;
    let bob = actor_fixture(&app, "bob_tl_contract_reblog").await;
    let app_id = register_test_app(&app).await;
    let alice_token =
        issue_test_token(&app, app_id, alice, &["write:statuses", "read:statuses"]).await;
    let bob_token = issue_test_token(&app, app_id, bob, &["write:statuses", "read:statuses"]).await;

    let original = create_status(
        &router,
        &alice_token,
        json!({"status": "boost this timeline contract fixture"}),
    )
    .await;
    let original_id = id_of(&original);

    let (status, boost) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/statuses/{original_id}/reblog"),
            Some(&bob_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {boost:?}");
    let boost_id = id_of(&boost);

    let (status, boost_detail) = send(
        &router,
        req(
            "GET",
            &format!("/api/v1/statuses/{boost_id}"),
            Some(&bob_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {boost_detail:?}");

    let (status, original_detail_for_bob) = send(
        &router,
        req(
            "GET",
            &format!("/api/v1/statuses/{original_id}"),
            Some(&bob_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {original_detail_for_bob:?}");

    let (status, timeline_body) = send(
        &router,
        req(
            "GET",
            &format!("{HOME_TIMELINE_PATH}?limit=40"),
            Some(&bob_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {timeline_body:?}");
    let item = find_item(&timeline_body, &boost_id);

    assert!(
        item["reblog"].is_object(),
        "the boost's timeline element must nest the boosted status under `reblog` \
         (Requirement 10.3): {item:?}"
    );
    assert_eq!(item["reblog"]["id"].as_str(), Some(original_id.as_str()));

    // The outer boost element's non-`reblog` fields (including its own
    // `reblogged`/`favourited`/etc. operation state, scoped to the boost
    // row's own id) must equal the boost's own detail-endpoint response.
    let mut item_without_reblog = item.clone();
    let mut boost_detail_without_reblog = boost_detail.clone();
    item_without_reblog["reblog"] = Value::Null;
    boost_detail_without_reblog["reblog"] = Value::Null;
    assert_eq!(
        item_without_reblog, boost_detail_without_reblog,
        "the boost's own outer fields must be byte-for-byte identical to GET \
         /api/v1/statuses/{{boost_id}} once `reblog` is normalized out of both sides"
    );

    // The nested `reblog` object itself must equal the original's own
    // detail-endpoint response as viewed by the same actor (bob) — proving
    // `StatusHydrator::leaf_render_input`'s independently-assembled nested
    // render reuses the identical contract the top-level pipeline does.
    assert_eq!(
        item["reblog"], original_detail_for_bob,
        "the nested `reblog` object must be byte-for-byte identical to the boosted \
         status's own GET /api/v1/statuses/{{id}} response for the same viewer \
         (Requirement 10.2's operation state — e.g. `reblogged: true` for bob on the \
         original — must be reflected identically in both places)"
    );
    assert_eq!(original_detail_for_bob["reblogged"], true);

    app.cleanup().await;
}

// ==========================================================================
// (3) Real-client-capture fixture registration proof
// ==========================================================================

/// Registers a real home-timeline response array — produced by the real
/// `TimelineService`/`StatusHydrator` pipeline through a first
/// `spawn_test_app` instance — as a fixture via `register_fixture`, then
/// proves a *second*, independently-booted instance's live output for the
/// equivalent scenario is held to that fixture's `response_body`.
#[tokio::test]
async fn home_timeline_response_registers_a_real_client_capture_fixture_and_holds_a_second_instances_live_output()
 {
    async fn build_home_timeline(app: &TestApp) -> Value {
        let router = real_router(app);
        let alice = actor_fixture(app, "contract_fixture_tl_author").await;
        let app_id = register_test_app(app).await;
        let alice_token =
            issue_test_token(app, app_id, alice, &["write:statuses", "read:statuses"]).await;

        create_status(
            &router,
            &alice_token,
            json!({"status": "a real fixture post for the timeline contract harness"}),
        )
        .await;

        let (status, timeline_body) = send(
            &router,
            req(
                "GET",
                &format!("{HOME_TIMELINE_PATH}?limit=40"),
                Some(&alice_token),
                None,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "got: {timeline_body:?}");
        timeline_body
    }

    let app_a = spawn_test_app().await;
    let timeline_a = build_home_timeline(&app_a).await;

    let fixture_name = unique_fixture_name("real_home_timeline");
    let _fixture_guard = FixtureGuard(fixture_name.clone());
    register_fixture(
        &fixture_name,
        CapturedExchange {
            method: "GET".to_string(),
            path: HOME_TIMELINE_PATH.to_string(),
            request_body: None,
            status: 200,
            response_body: timeline_a.clone(),
        },
    );

    let loaded = load_fixture(&fixture_name);
    assert_eq!(loaded.response_body, timeline_a);

    let app_b = spawn_test_app().await;
    let timeline_b = build_home_timeline(&app_b).await;
    assert_eq!(
        timeline_b, loaded.response_body,
        "a second independently-booted instance's live home-timeline JSON must match the \
         fixture-registered real-client-capture stand-in exactly"
    );

    app_a.cleanup().await;
    app_b.cleanup().await;
}
