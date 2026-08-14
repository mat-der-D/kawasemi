//! Integration-level contract test for `NotificationSerializer` (task 5.1,
//! `.kiro/specs/notifications/tasks.md`, "5.1 (P) Notification 契約ゴール
//! デンテスト", `_Depends: 4.2_`), Requirements 1.1-1.6.
//! design.md's File Structure Plan names this exact file
//! (`tests/notification_contract_it.rs`, "Notification 外殻ゴールデン（type
//! 種別・account/status 埋め込み・null 規律・決定性）（契約）").
//!
//! ## Relationship to task 2.1's own `src/notifications/serializer/tests.rs`
//! goldens
//! Task 2.1 already registered eight goldens
//! (`tests/golden/notifications/notification_{favourite,follow,
//! follow_request,mention,poll,reblog,status,update}.json`) from its own
//! `#[cfg(test)] mod tests` unit tests, built by feeding hand-constructed
//! [`kawasemi::notifications::model::Notification`]/`NotificationRenderInput`
//! fixtures directly into `serializer::to_notification_json`/
//! `notification_to_json` — see that module's own doc comment ("pure
//! function... no repository, no `PgPool`, no HTTP call"). Those eight
//! existing goldens are consumed exclusively by that unit-level suite and
//! are *not* reused here: a notification driven through the real
//! `crate::notifications::generator::NotificationGenerator`/
//! `NotificationService`/`NotificationEndpoints` pipeline produces
//! genuinely-resolved `id`/`created_at`/`account`/`status` values (from a
//! real `RuntimeContext.ids`/`.clock` and the real upstream
//! `accounts::serializer`/`statuses::serializer`) that cannot be made to
//! literally equal task 2.1's hand-picked stand-in values. This file
//! therefore registers its own, separate set of goldens under
//! `tests/golden/notifications/notification_contract_it_*.json` —
//! mirroring `tests/status_contract_it.rs`'s (statuses-core task 8.2) own
//! identical "unit-level goldens vs. this file's own integration-level
//! goldens, driven end to end through `spawn_test_app` and the real service
//! layer" precedent exactly, per `crate::contract::assert_golden`'s own
//! documented convention that golden-file paths are caller-owned.
//!
//! ## What "real pipeline" means here
//! Every scenario below drives the *actual* production seam end to end via
//! `tower::ServiceExt::oneshot` against `crate::server::build_router`
//! (mirroring `tests/status_contract_it.rs`'s/`tests/interactions_it.rs`'s/
//! `tests/status_crud_it.rs`'s own established in-process-HTTP technique):
//! real Bearer/scope enforcement, real `StatusService::create_status`/
//! `edit_status`, real `InteractionService::reblog`/`favourite`, real
//! `FollowService::follow` (`src/social_graph/endpoints.rs`), and the real,
//! already-wired (task 4.2) chain from each of those services' own local
//! `NotificationEvent` emit call straight through
//! `statuses::notification_sink::NotificationSinkRegistry` ->
//! `crate::notifications::event_sink::StatusesEventSinkAdapter` ->
//! `crate::notifications::event_sink::GeneratorEventSink` ->
//! `crate::notifications::generator::NotificationGenerator::generate` ->
//! `crate::notifications::repository::insert_dedup` ->
//! `crate::notifications::service::NotificationService` -> the mounted
//! `GET /api/v1/notifications`/`GET /api/v1/notifications/{id}` handlers
//! (`crate::notifications::endpoints`) — the identical chain
//! `tests/notifications_module_it.rs`'s own task-4.2 wiring test already proves
//! connects end to end for `favourite`, exercised here for every v1 kind
//! and asserted against this file's own registered goldens.
//!
//! ## Two kinds (`follow_request`, and `poll`/`status`/`update`) have no
//! real *upstream trigger* in this codebase yet — the real *generation
//! point* is still exercised directly instead of fabricating JSON
//! - **`follow_request`**: a genuinely *pending* inbound follow request for
//!   a *local* recipient can only ever be produced through this
//!   application's real signed-federation Follow-Activity-to-a-locked-actor
//!   inbound pipeline — `tests/social_graph_inbound_it.rs`'s own documented
//!   constraint (its scenario 2's doc comment, mirrored by
//!   `tests/follow_request_it.rs`'s own header comment): two *local*
//!   actors always establish a follow immediately
//!   (`FollowApprovalPolicy::requires_approval`'s "同一サーバー承認スキッ
//!   プ"), never pending. That whole HTTP-signature-verification pipeline
//!   is social-graph's own already-exhaustively-tested boundary (task 6.1
//!   there), not this task's — duplicating it here would test a different
//!   spec's boundary, not this one's (the Notification JSON envelope).
//!   [`follow_request_notification_json_matches_the_registered_contract_golden`]
//!   below instead drives the real
//!   `crate::social_graph::Transitions::record_pending` directly — the
//!   *single* place `notifications/design.md`'s own sequence diagram and
//!   `transitions.rs`'s own doc comment name as the "FollowRequest" emit
//!   convergence point, shared verbatim by both the inbound-Activity path
//!   and (for the `Outbound` direction, which never emits) the outbound-API
//!   path — with `direction: Inbound` (the only direction it ever emits
//!   for), against the exact same `NotificationSinkRegistry` instance the
//!   real `NotificationModule` wiring (task 4.2) is already registered on
//!   (`app.state.statuses().notification_sink_registry()`). Everything from
//!   that call onward — dedup, generation, persistence, serialization,
//!   HTTP retrieval — is the real production pipeline; only the upstream
//!   HTTP-signature-verification layer above `Transitions` is bypassed.
//! - **`poll`/`status`/`update`**: this codebase has *no* production call
//!   site that ever constructs a `NotificationEvent` of these three kinds
//!   at all yet — `src/statuses/status_service.rs`'s own doc comment
//!   documents both gaps as deliberate, already-reviewed MVP scope cuts:
//!   "poll-end は...本 spec・現行 federation-core に存在しない...MVP では
//!   emit しない" (`poll`), and "an edit's real-world Mastodon-equivalent
//!   notification (`NotificationType::Update`) fans out to every distinct
//!   account that previously favourited/reblogged/participated..." with no
//!   grounded single-recipient interpretation implemented (`update`); no
//!   "new post from a followed account" trigger (`status`) exists anywhere
//!   in `status_service.rs` either. `src/notifications/generator.rs`'s own
//!   test module already establishes the precedent for this exact
//!   situation ("this task's own test module still exercises the six kinds
//!   this task's completion state enumerates explicitly... including
//!   `Status`/`Update`, whose upstream emitters are wired in a later task"
//!   — by constructing the `NotificationEvent` by hand). The three
//!   scenarios below do the same, but drive it through the real, already-
//!   wired single generation point
//!   (`app.state.notifications().ports().emit(..)`, task 4.2's own real
//!   `NotificationPortsRegistry` -> `GeneratorEventSink` ->
//!   `NotificationGenerator`) against a real, HTTP-created (and, for
//!   `update`, genuinely HTTP-edited) `Status` row, rather than fabricating
//!   Notification JSON directly — only the upstream trigger (a poll-close
//!   scheduler / a followed-account fan-out / an edit fan-out, none of
//!   which exist in this codebase yet) is synthesized.
//!
//! ## Determinism (steering `tech.md`'s "決定性の強制"; Requirement 1.6)
//! Every non-deterministic seam this pipeline touches is drawn from
//! `spawn_test_app`'s fixed `RuntimeContext::deterministic` boundary (id
//! generator, clock) — this file never reads the OS clock or generates its
//! own ids, so a given scenario's sequence of fixture/HTTP calls always
//! produces the same `id`/`created_at` values run over run.
//! `the_same_mention_scenario_reproduces_byte_identical_json_across_two_independently_spawned_instances`
//! below proves this directly by running the identical mention scenario
//! against two independently-`spawn_test_app`-booted instances in the same
//! test run (mirroring `tests/status_contract_it.rs`'s own identical
//! two-instance proof), in addition to every golden test's own comparison
//! against its checked-in file being itself a rerun-reproduces-the-same-JSON
//! proof each time the suite runs.
//!
//! ## The `KAWASEMI_UPDATE_GOLDEN` baseline was recorded, not embedded
//! Following `tests/status_contract_it.rs`'s own established convention:
//! this file's committed goldens
//! (`tests/golden/notifications/notification_contract_it_*.json`) are meant
//! to be produced by a one-off manual run of this whole test binary with
//! `KAWASEMI_UPDATE_GOLDEN=1` set as a process environment variable (never
//! set from *within* a test — this file has more than one `#[tokio::test]`,
//! several of which can run concurrently in the same process, so setting it
//! mid-test would race), then re-run without it to confirm a clean
//! comparison. **This sandbox has no reachable PostgreSQL** (every earlier
//! task in this spec hit the identical constraint — see this spec's own
//! `tasks.md` "## Implementation Notes"), so that recording step could not
//! be performed here; no golden files are checked in alongside this file
//! for that reason (fabricating one by hand would defeat the entire point
//! of a golden — it must be the real pipeline's own output). This file
//! compiles cleanly (`cargo check --tests` / `cargo test --no-run`) and
//! every assertion was verified by direct reading of the real collaborators
//! it calls (see this task's own status report for exactly which).
//!
//! ## The real-client-capture-fixture requirement ("実クライアントキャプ
//! チャをフィクスチャ登録する")
//! Per `design.md`'s own Testing Strategy > Contract Tests section for this
//! task ("実クライアントキャプチャをフィクスチャ登録") and mirroring
//! `tests/status_contract_it.rs`'s/`tests/contract_harness_fixture_it.rs`'s
//! own already-reviewed resolution to the identical requirement (this
//! sandboxed development environment has no way to capture genuine live
//! traffic from a real Mastodon client):
//! [`real_mention_notification_json_is_registered_as_a_fixture_and_holds_a_second_instances_live_output`]
//! below registers a real (not mocked/hand-typed) Notification JSON
//! exchange — produced by this crate's own real, deterministic-boundary
//! notification pipeline via `register_fixture` — and proves a second,
//! independently-booted instance's live output for the equivalent scenario
//! is held to that fixture's `response_body`, exactly the shape a real
//! client capture would eventually be swapped into.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use time::OffsetDateTime;
use tower::ServiceExt;

use kawasemi::accounts::model::{ProfileField, RemoteAccount};
use kawasemi::accounts::remote_repository::upsert_remote;
use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor, ResolvedActor};
use kawasemi::contract::{CapturedExchange, assert_golden, load_fixture, register_fixture};
use kawasemi::domain::{AccountRef, Id};
use kawasemi::notifications::{NotificationEvent, NotificationType};
use kawasemi::oauth::app_repository::{self, NewApp};
use kawasemi::oauth::model::ScopeSet as ModelScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::server;
use kawasemi::social_graph::{FollowRequest, FollowRequestDirection, Transitions};
use kawasemi::test_harness::{TestApp, spawn_test_app};

// ---- Fixture plumbing (each `tests/*.rs` file is its own compiled crate,
// so this deliberately duplicates `tests/status_contract_it.rs`'s/`tests/
// interactions_it.rs`'s own established conventions rather than importing
// them — this crate's own documented convention). ----

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
            display_name: format!("Notification Contract IT {handle_str}"),
            summary: "an actor used by the notification_contract_it integration test".to_string(),
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

fn sample_remote_account(id: Id, actor_uri: &str, fetched_at: OffsetDateTime) -> RemoteAccount {
    RemoteAccount {
        id,
        actor_uri: actor_uri.to_string(),
        username: "notif_contract_remote".to_string(),
        domain: "remote.notif-contract.example".to_string(),
        display_name: "Notification Contract IT Remote Actor".to_string(),
        note: String::new(),
        url: actor_uri.to_string(),
        avatar_url: None,
        header_url: None,
        fields: Vec::<ProfileField>::new(),
        bot: false,
        locked: false,
        fetched_at,
    }
}

/// Seeds a known remote account row directly (mirrors `tests/
/// follow_unfollow_it.rs::create_test_remote`'s identical convention) — the
/// origin `crate::social_graph::Transitions::record_pending`'s
/// `follow_request` scenario needs to resolve via the real
/// `AccountService::show_account` embed path.
async fn insert_remote_actor_fixture(app: &TestApp, actor_uri: &str) -> Id {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    upsert_remote(&app.pool, &sample_remote_account(id, actor_uri, now))
        .await
        .expect("upsert_remote must succeed");
    id
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
            name: "Notification Contract IT Client".to_string(),
            redirect_uris: vec!["https://client.example/callback".to_string()],
            scopes: ModelScopeSet::new(["read", "write", "follow"]),
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

fn id_domain_of(raw_id: &str) -> Id {
    Id::from_i64(
        raw_id
            .parse::<i64>()
            .expect("a real HTTP-created status id must be numeric"),
    )
}

/// Lists `token`'s own notifications narrowed to exactly `kind` (Requirement
/// 2.2's `types[]` filter, real `list_notifications` handler).
async fn list_notifications_by_type(router: &Router, token: &str, kind: &str) -> Vec<Value> {
    let (status, body) = send(
        router,
        req(
            "GET",
            &format!("/api/v1/notifications?types[]={kind}"),
            Some(token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {body:?}");
    body.as_array()
        .expect("notification list must be a JSON array")
        .clone()
}

/// Fetches a single notification by id (real `show_notification` handler).
async fn get_notification(router: &Router, token: &str, id: &str) -> Value {
    let (status, body) = send(
        router,
        req(
            "GET",
            &format!("/api/v1/notifications/{id}"),
            Some(token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {body:?}");
    body
}

/// Requirement 1.1: every real Notification JSON carries all five outer-shell
/// fields (`status` always present, `null` or an object — Requirement 1.4).
fn assert_envelope_fields_present(notification: &Value) {
    for field in ["id", "type", "created_at", "account", "status"] {
        assert!(
            notification.get(field).is_some(),
            "expected field {field:?} in real Notification JSON, got {notification:?}"
        );
    }
}

fn unique_fixture_name(label: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("notification_contract_it_{label}_{nanos}_{seq}")
}

/// Best-effort cleanup of a fixture file this test registers, regardless of
/// whether the test body panicked (mirrors `tests/
/// contract_harness_fixture_it.rs`'s/`tests/status_contract_it.rs`'s
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
// (1) The mention scenario, shared by the main golden test, the
// determinism proof, and the fixture-registration proof below (all three
// need the identical fixture/HTTP sequence to produce comparable JSON).
// ==========================================================================

const MENTION_ALICE_HANDLE: &str = "notif_contract_mention_alice";
const MENTION_BOB_HANDLE: &str = "notif_contract_mention_bob";

/// Drives a real mention end to end: alice posts a status mentioning bob
/// (real `POST /api/v1/statuses`, real `StatusService::create_status`'s own
/// mention-resolution + `Mention` `NotificationEvent` emit loop), then
/// fetches bob's single resulting notification through the real endpoints.
/// Returns `(status_id, rendered_notification_json)`.
async fn build_mention_notification(app: &TestApp) -> (String, Value) {
    let router = real_router(app);
    let alice = insert_actor_fixture(app, MENTION_ALICE_HANDLE).await;
    let bob = insert_actor_fixture(app, MENTION_BOB_HANDLE).await;
    let app_id = register_test_app(app).await;
    let alice_token = issue_test_token(app, app_id, alice.id, &["write:statuses"]).await;
    let bob_token = issue_test_token(app, app_id, bob.id, &["read:notifications"]).await;

    let created = create_status(
        &router,
        &alice_token,
        json!({"status": format!("hey @{MENTION_BOB_HANDLE}, contract check")}),
    )
    .await;
    let status_id = id_of(&created);

    let items = list_notifications_by_type(&router, &bob_token, "mention").await;
    assert_eq!(
        items.len(),
        1,
        "expected exactly one mention notification: {items:?}"
    );
    let id = items[0]["id"]
        .as_str()
        .expect("id must be a string")
        .to_string();
    let rendered = get_notification(&router, &bob_token, &id).await;
    (status_id, rendered)
}

/// Requirements 1.1 (field presence), 1.2 (post-related kind embeds
/// `status`), 1.3 (`account` embed), 1.5 (`type` = `"mention"`).
#[tokio::test]
async fn mention_notification_json_matches_the_registered_contract_golden() {
    let app = spawn_test_app().await;
    let (status_id, rendered) = build_mention_notification(&app).await;

    assert_envelope_fields_present(&rendered);
    assert_eq!(rendered["type"], "mention");
    assert!(
        rendered["account"].is_object(),
        "mention notification must embed the mentioning actor's real Account JSON"
    );
    assert!(
        rendered["status"].is_object(),
        "mention notification must embed the mentioning Status JSON, not null"
    );
    assert_eq!(rendered["status"]["id"].as_str(), Some(status_id.as_str()));
    assert!(
        rendered["status"]["content"]
            .as_str()
            .is_some_and(|content| content.contains("contract check")),
        "the embedded status must be the real mentioning post: {rendered:?}"
    );

    assert_golden(
        "tests/golden/notifications/notification_contract_it_mention.json",
        &rendered,
    );

    app.cleanup().await;
}

// ==========================================================================
// (2) favourite: Requirements 1.1-1.3, 1.5, 6.1 — status embedded, account
// is the favouriter.
// ==========================================================================

/// Requirements 1.1-1.3, 1.5 (`type` = `"favourite"`), 6.1.
#[tokio::test]
async fn favourite_notification_json_matches_the_registered_contract_golden() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "notif_contract_favourite_alice").await;
    let bob = insert_actor_fixture(&app, "notif_contract_favourite_bob").await;
    let app_id = register_test_app(&app).await;
    let alice_token = issue_test_token(
        &app,
        app_id,
        alice.id,
        &["write:statuses", "read:notifications"],
    )
    .await;
    let bob_token = issue_test_token(&app, app_id, bob.id, &["write:favourites"]).await;

    let created = create_status(
        &router,
        &alice_token,
        json!({"status": "notify me on favourite"}),
    )
    .await;
    let status_id = id_of(&created);

    let (status, favourited) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/statuses/{status_id}/favourite"),
            Some(&bob_token),
            None,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "favourite must succeed: {favourited:?}"
    );

    let items = list_notifications_by_type(&router, &alice_token, "favourite").await;
    assert_eq!(
        items.len(),
        1,
        "expected exactly one favourite notification: {items:?}"
    );
    let id = items[0]["id"]
        .as_str()
        .expect("id must be a string")
        .to_string();
    let rendered = get_notification(&router, &alice_token, &id).await;

    assert_envelope_fields_present(&rendered);
    assert_eq!(rendered["type"], "favourite");
    assert_eq!(
        rendered["account"]["id"].as_str(),
        Some(bob.id.as_i64().to_string().as_str()),
        "the favourite notification's account must be the real favouriter, not the author"
    );
    assert!(
        rendered["status"].is_object(),
        "favourite notification must embed the favourited Status JSON, not null"
    );
    assert_eq!(rendered["status"]["id"].as_str(), Some(status_id.as_str()));
    assert_eq!(rendered["status"]["favourites_count"], 1);

    assert_golden(
        "tests/golden/notifications/notification_contract_it_favourite.json",
        &rendered,
    );

    app.cleanup().await;
}

// ==========================================================================
// (3) reblog: Requirements 1.1-1.3, 1.5, 6.2 — `status_id` is the *original*
// target, not the boost row's own id.
// ==========================================================================

/// Requirements 1.1-1.3, 1.5 (`type` = `"reblog"`), 6.2.
#[tokio::test]
async fn reblog_notification_json_matches_the_registered_contract_golden() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "notif_contract_reblog_alice").await;
    let bob = insert_actor_fixture(&app, "notif_contract_reblog_bob").await;
    let app_id = register_test_app(&app).await;
    let alice_token = issue_test_token(
        &app,
        app_id,
        alice.id,
        &["write:statuses", "read:notifications"],
    )
    .await;
    let bob_token = issue_test_token(&app, app_id, bob.id, &["write:statuses"]).await;

    let created = create_status(
        &router,
        &alice_token,
        json!({"status": "notify me on reblog"}),
    )
    .await;
    let status_id = id_of(&created);

    let (status, boosted) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/statuses/{status_id}/reblog"),
            Some(&bob_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "reblog must succeed: {boosted:?}");

    let items = list_notifications_by_type(&router, &alice_token, "reblog").await;
    assert_eq!(
        items.len(),
        1,
        "expected exactly one reblog notification: {items:?}"
    );
    let id = items[0]["id"]
        .as_str()
        .expect("id must be a string")
        .to_string();
    let rendered = get_notification(&router, &alice_token, &id).await;

    assert_envelope_fields_present(&rendered);
    assert_eq!(rendered["type"], "reblog");
    assert_eq!(
        rendered["account"]["id"].as_str(),
        Some(bob.id.as_i64().to_string().as_str()),
        "the reblog notification's account must be the real rebloger, not the author"
    );
    assert!(
        rendered["status"].is_object(),
        "reblog notification must embed the *original* Status JSON, not null"
    );
    assert_eq!(
        rendered["status"]["id"].as_str(),
        Some(status_id.as_str()),
        "the embedded status must be the original target, not the boost row's own id"
    );

    assert_golden(
        "tests/golden/notifications/notification_contract_it_reblog.json",
        &rendered,
    );

    app.cleanup().await;
}

// ==========================================================================
// (4) follow: Requirements 1.1, 1.3, 1.4 (`status` null), 1.5, 6.4.
// ==========================================================================

/// Requirements 1.1, 1.3, 1.4 (`status` must be `null`, never omitted), 1.5
/// (`type` = `"follow"`), 6.4.
#[tokio::test]
async fn follow_notification_json_matches_the_registered_contract_golden() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "notif_contract_follow_alice").await;
    let bob = insert_actor_fixture(&app, "notif_contract_follow_bob").await;
    let app_id = register_test_app(&app).await;
    let alice_token = issue_test_token(&app, app_id, alice.id, &["follow"]).await;
    let bob_token = issue_test_token(&app, app_id, bob.id, &["read:notifications"]).await;

    let (status, followed) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/accounts/{}/follow", bob.id.as_i64()),
            Some(&alice_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "follow must succeed: {followed:?}");

    let items = list_notifications_by_type(&router, &bob_token, "follow").await;
    assert_eq!(
        items.len(),
        1,
        "expected exactly one follow notification: {items:?}"
    );
    let id = items[0]["id"]
        .as_str()
        .expect("id must be a string")
        .to_string();
    let rendered = get_notification(&router, &bob_token, &id).await;

    assert_envelope_fields_present(&rendered);
    assert_eq!(rendered["type"], "follow");
    assert_eq!(
        rendered["account"]["id"].as_str(),
        Some(alice.id.as_i64().to_string().as_str())
    );
    assert_eq!(
        rendered["status"],
        Value::Null,
        "Requirement 1.4: a follow notification must never carry a related post"
    );

    assert_golden(
        "tests/golden/notifications/notification_contract_it_follow.json",
        &rendered,
    );

    app.cleanup().await;
}

// ==========================================================================
// (5) follow_request: Requirements 1.1, 1.3, 1.4 (`status` null), 1.5, 6.5 —
// see this file's own doc comment for why the real single generation point
// (`Transitions::record_pending`) is driven directly rather than through the
// full signed-federation inbound pipeline.
// ==========================================================================

/// Requirements 1.1, 1.3, 1.4 (`status` must be `null`), 1.5 (`type` =
/// `"follow_request"`), 6.5.
#[tokio::test]
async fn follow_request_notification_json_matches_the_registered_contract_golden() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let target = insert_actor_fixture(&app, "notif_contract_follow_request_target").await;
    let app_id = register_test_app(&app).await;
    let target_token = issue_test_token(&app, app_id, target.id, &["read:notifications"]).await;

    let remote_id = insert_remote_actor_fixture(
        &app,
        "https://remote.notif-contract.example/actors/notif_contract_requester",
    )
    .await;

    // See this file's own doc comment ("follow_request") for why this drives
    // the real `Transitions::record_pending` directly, against the exact
    // `NotificationSinkRegistry` instance the real task-4.2 wiring already
    // registered the real event sink onto.
    let transitions = Transitions::new(
        app.pool.clone(),
        app.runtime.clone(),
        app.state.statuses().notification_sink_registry(),
    );
    let now = app.runtime.clock.now();
    transitions
        .record_pending(&FollowRequest {
            requester: AccountRef::Remote(remote_id),
            target: AccountRef::Local(target.id),
            direction: FollowRequestDirection::Inbound,
            activity_id: "https://remote.notif-contract.example/activities/follow-request-1"
                .to_string(),
            created_at: now,
        })
        .await
        .expect("record_pending must succeed and emit a real FollowRequest NotificationEvent");

    let items = list_notifications_by_type(&router, &target_token, "follow_request").await;
    assert_eq!(
        items.len(),
        1,
        "expected exactly one follow_request notification: {items:?}"
    );
    let id = items[0]["id"]
        .as_str()
        .expect("id must be a string")
        .to_string();
    let rendered = get_notification(&router, &target_token, &id).await;

    assert_envelope_fields_present(&rendered);
    assert_eq!(rendered["type"], "follow_request");
    assert_eq!(
        rendered["account"]["id"].as_str(),
        Some(remote_id.as_i64().to_string().as_str())
    );
    assert_eq!(
        rendered["status"],
        Value::Null,
        "Requirement 1.4: a follow_request notification must never carry a related post"
    );

    assert_golden(
        "tests/golden/notifications/notification_contract_it_follow_request.json",
        &rendered,
    );

    app.cleanup().await;
}

// ==========================================================================
// (6) poll: Requirements 1.1-1.3, 1.5, 6.6 — see this file's own doc
// comment for why the real single generation point is driven directly
// (no production poll-close emitter exists yet).
// ==========================================================================

/// Requirements 1.1-1.3, 1.5 (`type` = `"poll"`), 6.6.
#[tokio::test]
async fn poll_notification_json_matches_the_registered_contract_golden() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "notif_contract_poll_alice").await;
    let app_id = register_test_app(&app).await;
    let alice_token = issue_test_token(
        &app,
        app_id,
        alice.id,
        &["write:statuses", "read:notifications"],
    )
    .await;

    let created = create_status(
        &router,
        &alice_token,
        json!({
            "status": "notif contract poll",
            "poll": {"options": ["Cats", "Dogs"], "multiple": false}
        }),
    )
    .await;
    let status_id = id_of(&created);
    assert!(
        created["poll"].is_object(),
        "the create response must embed a real poll: {created:?}"
    );

    // See this file's own doc comment ("poll/status/update") for why this
    // drives the real single generation point directly.
    let now = app.runtime.clock.now();
    app.state
        .notifications()
        .ports()
        .emit(NotificationEvent {
            recipient: AccountRef::Local(alice.id),
            origin: AccountRef::Local(alice.id),
            kind: NotificationType::Poll,
            target_status_id: Some(id_domain_of(&status_id)),
            occurred_at: now,
        })
        .await
        .expect("emit must succeed and reach the real NotificationGenerator");

    let items = list_notifications_by_type(&router, &alice_token, "poll").await;
    assert_eq!(
        items.len(),
        1,
        "expected exactly one poll notification: {items:?}"
    );
    let id = items[0]["id"]
        .as_str()
        .expect("id must be a string")
        .to_string();
    let rendered = get_notification(&router, &alice_token, &id).await;

    assert_envelope_fields_present(&rendered);
    assert_eq!(rendered["type"], "poll");
    assert_eq!(
        rendered["account"]["id"].as_str(),
        Some(alice.id.as_i64().to_string().as_str())
    );
    assert!(
        rendered["status"].is_object(),
        "poll notification must embed the poll-bearing Status JSON, not null"
    );
    assert_eq!(rendered["status"]["id"].as_str(), Some(status_id.as_str()));
    assert!(
        rendered["status"]["poll"].is_object(),
        "the embedded status must itself carry the real, delegated Poll JSON: {rendered:?}"
    );

    assert_golden(
        "tests/golden/notifications/notification_contract_it_poll.json",
        &rendered,
    );

    app.cleanup().await;
}

// ==========================================================================
// (7) status: Requirements 1.1-1.3, 1.5 — see this file's own doc comment
// ("poll/status/update") for why the real single generation point is
// driven directly (no production "new post from a followed account"
// emitter exists yet).
// ==========================================================================

/// Requirements 1.1-1.3, 1.5 (`type` = `"status"`).
#[tokio::test]
async fn status_notification_json_matches_the_registered_contract_golden() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "notif_contract_status_alice").await;
    let bob = insert_actor_fixture(&app, "notif_contract_status_bob").await;
    let app_id = register_test_app(&app).await;
    let alice_token = issue_test_token(&app, app_id, alice.id, &["write:statuses"]).await;
    let bob_token = issue_test_token(&app, app_id, bob.id, &["read:notifications"]).await;

    let created = create_status(
        &router,
        &alice_token,
        json!({"status": "notif contract status kind"}),
    )
    .await;
    let status_id = id_of(&created);

    let now = app.runtime.clock.now();
    app.state
        .notifications()
        .ports()
        .emit(NotificationEvent {
            recipient: AccountRef::Local(bob.id),
            origin: AccountRef::Local(alice.id),
            kind: NotificationType::Status,
            target_status_id: Some(id_domain_of(&status_id)),
            occurred_at: now,
        })
        .await
        .expect("emit must succeed and reach the real NotificationGenerator");

    let items = list_notifications_by_type(&router, &bob_token, "status").await;
    assert_eq!(
        items.len(),
        1,
        "expected exactly one status notification: {items:?}"
    );
    let id = items[0]["id"]
        .as_str()
        .expect("id must be a string")
        .to_string();
    let rendered = get_notification(&router, &bob_token, &id).await;

    assert_envelope_fields_present(&rendered);
    assert_eq!(rendered["type"], "status");
    assert_eq!(
        rendered["account"]["id"].as_str(),
        Some(alice.id.as_i64().to_string().as_str())
    );
    assert!(
        rendered["status"].is_object(),
        "status notification must embed the real Status JSON, not null"
    );
    assert_eq!(rendered["status"]["id"].as_str(), Some(status_id.as_str()));

    assert_golden(
        "tests/golden/notifications/notification_contract_it_status.json",
        &rendered,
    );

    app.cleanup().await;
}

// ==========================================================================
// (8) update: Requirements 1.1-1.3, 1.5 — see this file's own doc comment
// ("poll/status/update"). Distinct from "status" above: the embedded post
// is a genuinely *edited* one (`edited_at` populated), exercising the
// `NotificationType::Update` real-status-embedding path against a status
// whose shape actually differs from a freshly-created one.
// ==========================================================================

/// Requirements 1.1-1.3, 1.5 (`type` = `"update"`).
#[tokio::test]
async fn update_notification_json_matches_the_registered_contract_golden() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "notif_contract_update_alice").await;
    let bob = insert_actor_fixture(&app, "notif_contract_update_bob").await;
    let app_id = register_test_app(&app).await;
    let alice_token = issue_test_token(&app, app_id, alice.id, &["write:statuses"]).await;
    let bob_token = issue_test_token(&app, app_id, bob.id, &["read:notifications"]).await;

    let created = create_status(
        &router,
        &alice_token,
        json!({"status": "before the contract edit"}),
    )
    .await;
    let status_id = id_of(&created);
    assert_eq!(created["edited_at"], Value::Null);

    let (status, edited) = send(
        &router,
        req(
            "PUT",
            &format!("/api/v1/statuses/{status_id}"),
            Some(&alice_token),
            Some(json!({"status": "after the contract edit"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "edit must succeed: {edited:?}");
    assert_ne!(edited["edited_at"], Value::Null);

    let now = app.runtime.clock.now();
    app.state
        .notifications()
        .ports()
        .emit(NotificationEvent {
            recipient: AccountRef::Local(bob.id),
            origin: AccountRef::Local(alice.id),
            kind: NotificationType::Update,
            target_status_id: Some(id_domain_of(&status_id)),
            occurred_at: now,
        })
        .await
        .expect("emit must succeed and reach the real NotificationGenerator");

    let items = list_notifications_by_type(&router, &bob_token, "update").await;
    assert_eq!(
        items.len(),
        1,
        "expected exactly one update notification: {items:?}"
    );
    let id = items[0]["id"]
        .as_str()
        .expect("id must be a string")
        .to_string();
    let rendered = get_notification(&router, &bob_token, &id).await;

    assert_envelope_fields_present(&rendered);
    assert_eq!(rendered["type"], "update");
    assert_eq!(
        rendered["account"]["id"].as_str(),
        Some(alice.id.as_i64().to_string().as_str())
    );
    assert!(
        rendered["status"].is_object(),
        "update notification must embed the real, genuinely-edited Status JSON, not null"
    );
    assert_eq!(rendered["status"]["id"].as_str(), Some(status_id.as_str()));
    assert_ne!(
        rendered["status"]["edited_at"],
        Value::Null,
        "the embedded status must reflect its real edited_at, delegated verbatim to \
         statuses-core's own serializer"
    );

    assert_golden(
        "tests/golden/notifications/notification_contract_it_update.json",
        &rendered,
    );

    app.cleanup().await;
}

// ==========================================================================
// (9) Reproducibility across two independently-spawned instances
// (Requirement 1.6, steering `tech.md`'s "決定性の強制").
// ==========================================================================

/// The identical mention scenario, driven against two independently-
/// `spawn_test_app`-booted instances, produces byte-for-byte identical
/// Notification JSON — no non-determinism (clock, id) leaks through the
/// real pipeline. Mirrors `tests/status_contract_it.rs`'s own identical
/// two-instance proof, and (since it reuses the identical handle literals
/// and message text) both instances' output is held to the same golden the
/// main mention test above registers.
#[tokio::test]
async fn the_same_mention_scenario_reproduces_byte_identical_json_across_two_independently_spawned_instances()
 {
    let app_a = spawn_test_app().await;
    let app_b = spawn_test_app().await;

    let (_status_id_a, json_a) = build_mention_notification(&app_a).await;
    let (_status_id_b, json_b) = build_mention_notification(&app_b).await;

    assert_eq!(
        json_a, json_b,
        "two independently spawn_test_app-booted instances driving the identical \
         fixture/HTTP sequence must produce byte-for-byte identical Notification JSON"
    );

    assert_golden(
        "tests/golden/notifications/notification_contract_it_mention.json",
        &json_a,
    );
    assert_golden(
        "tests/golden/notifications/notification_contract_it_mention.json",
        &json_b,
    );

    app_a.cleanup().await;
    app_b.cleanup().await;
}

// ==========================================================================
// (10) Real-client-capture fixture registration proof (see this file's own
// doc comment, "The real-client-capture-fixture requirement").
// ==========================================================================

/// Registers a real mention Notification JSON — produced by the real
/// notification pipeline through a first `spawn_test_app` instance — as a
/// fixture via `register_fixture`, then proves a *second*, independently-
/// booted instance's live output for the equivalent scenario is held to
/// that fixture's `response_body`.
#[tokio::test]
async fn real_mention_notification_json_is_registered_as_a_fixture_and_holds_a_second_instances_live_output()
 {
    let app_a = spawn_test_app().await;
    let (_status_id_a, json_a) = build_mention_notification(&app_a).await;

    let fixture_name = unique_fixture_name("real_mention");
    let _fixture_guard = FixtureGuard(fixture_name.clone());
    register_fixture(
        &fixture_name,
        CapturedExchange {
            method: "GET".to_string(),
            path: "/api/v1/notifications/{id}".to_string(),
            request_body: None,
            status: 200,
            response_body: json_a.clone(),
        },
    );

    // (9.5) Round-trips unchanged through the public extension point.
    let loaded = load_fixture(&fixture_name);
    assert_eq!(loaded.response_body, json_a);

    // A second, independently-spawned instance's live output — generated
    // purely from its own deterministic RuntimeContext, driving the
    // identical scenario — is held to the fixture-derived acceptance
    // criterion via direct JSON comparison (mirrors `tests/
    // status_contract_it.rs`'s own identical "why a direct comparison, not
    // `assert_golden`" reasoning: more than one `#[tokio::test]` in this
    // file can run concurrently, so `KAWASEMI_UPDATE_GOLDEN` cannot safely
    // be set mid-test).
    let app_b = spawn_test_app().await;
    let (_status_id_b, json_b) = build_mention_notification(&app_b).await;
    assert_eq!(
        json_b, loaded.response_body,
        "a second independently-booted instance's live Notification JSON must match the \
         fixture-registered real-client-capture stand-in exactly"
    );

    app_a.cleanup().await;
    app_b.cleanup().await;
}
