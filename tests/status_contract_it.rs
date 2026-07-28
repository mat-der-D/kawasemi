//! Integration-level contract test for `StatusSerializer`/`PollSerializer`
//! (task 8.2, `.kiro/specs/statuses-core/tasks.md`, "8.2 (P) 契約テスト
//! （Status / Poll ゴールデン）を整備する", `_Boundary: StatusSerializer,
//! PollSerializer_`, `_Depends: 7.2_`), Requirements 1.1-1.6, 2.1-2.4.
//! design.md's File Structure Plan names this exact file
//! (`tests/status_contract_it.rs`, "Status / Poll ゴールデン（決定的・null
//! 規律・reblog ネスト・編集表現）（契約）").
//!
//! ## Relationship to task 3.3's own `src/statuses/serializer/tests.rs` goldens
//! Task 3.3 already registered six goldens
//! (`tests/golden/statuses/{status_normal,status_reblog,status_edited,
//! status_with_poll,poll_open,poll_expired}.json`) from its own
//! `#[cfg(test)] mod tests` unit tests, built from literal, hand-constructed
//! [`kawasemi::statuses::Status`]/[`kawasemi::statuses::Poll`] fixtures fed
//! directly into `serializer::status_to_json`/`poll_to_json` — see
//! `serializer.rs`'s own doc comment and its `tests.rs`'s own header comment
//! ("mirroring `accounts/serializer/tests.rs`'s identical precedent: a pure
//! serializer has nothing non-deterministic upstream to inject a
//! `RuntimeContext` boundary for"). Those six existing goldens are consumed
//! exclusively by that unit-level suite and are *not* reused here: a status
//! driven through the real `POST /api/v1/statuses` endpoint, a real
//! `InteractionService`/`PollService`, and `StatusesEndpointsState::
//! render_status_json`'s real `Status -> StatusRenderInput` assembly glue
//! (`src/statuses/endpoints.rs`, task 7.1) produces genuinely-resolved
//! `id`/`uri`/`url`/`created_at`/`account` values (from a real
//! `RuntimeContext.ids`/`.clock` and the real `accounts::serializer`) that
//! cannot be made to literally equal task 3.3's hand-picked stand-in
//! values. This file therefore registers its own, separate set of goldens
//! under `tests/golden/statuses/status_contract_it_*.json` /
//! `tests/golden/statuses/poll_contract_it_*.json`, mirroring
//! `tests/media_attachment_contract_it.rs`'s (media-pipeline task 6.3)
//! established "unit-level goldens vs. this file's own integration-level
//! goldens, driven end to end through `spawn_test_app` and the real service
//! layer" precedent exactly, per `crate::contract::assert_golden`'s own
//! documented convention that golden-file paths are caller-owned.
//!
//! ## What "real pipeline" means here
//! Every scenario below drives the *actual* production seam end to end via
//! `tower::ServiceExt::oneshot` against `crate::server::build_router`
//! (mirroring `tests/status_crud_it.rs`/`tests/interactions_it.rs`/
//! `tests/polls_it.rs`'s own established in-process-HTTP technique): real
//! Bearer/scope enforcement, real `StatusService::create_status`/
//! `edit_status`, real `InteractionService::reblog`/`favourite`/`bookmark`/
//! `pin`, real `PollService::record_vote`, and real
//! `StatusesEndpointsState::render_status_json` (which itself calls the
//! real `serializer::status_to_json`/`poll_to_json`, task 3.3, unchanged by
//! this task). Poll fixtures are inserted directly via
//! `status_repository::insert_status`/`poll_repository::insert_poll` (not
//! through the create-status endpoint), mirroring `tests/polls_it.rs`'s own
//! documented, already-reviewed convention: `StatusService::create_status`
//! does not persist a caller-supplied poll yet (see `status_service.rs`'s
//! own "Poll handling" doc comment) — poll *voting* and poll *rendering*
//! (this task's actual boundary) are still exercised as real HTTP requests
//! against a real `PollService`/`StatusEndpoints`, only poll *creation*
//! (Requirement 13.1, a different task's boundary) is bypassed. No step is
//! stubbed, mocked, or hand-built in place of what these services actually
//! produce.
//!
//! ## Determinism (steering `tech.md`'s "決定性の強制"; Requirement 1.4, 2.4)
//! Every non-deterministic seam this pipeline touches is drawn from
//! `spawn_test_app`'s fixed `RuntimeContext::deterministic` boundary (id
//! generator, clock) — this file never reads the OS clock or generates its
//! own ids, so a given scenario's sequence of fixture/HTTP calls always
//! produces the same `id`/`created_at`/`expires_at` values run over run.
//! `the_same_scenario_reproduces_byte_identical_json_across_two_independently_spawned_instances`
//! below proves this directly by running the identical scenario against two
//! independently-`spawn_test_app`-booted instances in the same test run
//! (mirroring `tests/media_attachment_contract_it.rs`'s own identical
//! two-instance proof), in addition to every golden test's own comparison
//! against its checked-in file being itself a rerun-reproduces-the-same-JSON
//! proof each time the suite runs.
//!
//! ## The `KAWASEMI_UPDATE_GOLDEN` baseline was recorded, not embedded
//! Following `tests/media_attachment_contract_it.rs`'s own established
//! convention: this file's committed goldens
//! (`tests/golden/statuses/status_contract_it_*.json`,
//! `tests/golden/statuses/poll_contract_it_*.json`) were produced by a
//! one-off manual run of this whole test binary with
//! `KAWASEMI_UPDATE_GOLDEN=1` set as a process environment variable (never
//! set from *within* a test — this file has more than one `#[tokio::test]`,
//! several of which can run concurrently in the same process, so setting it
//! mid-test the way `tests/contract_harness_fixture_it.rs`'s single-test
//! file safely does would race), then re-run without it to confirm a clean
//! comparison.
//!
//! ## The real-client-capture-fixture requirement ("実クライアントキャプ
//! チャをフィクスチャ登録する")
//! Per this task's dispatch brief (mirroring `tests/
//! contract_harness_fixture_it.rs`'s, api-foundation task 9.5, own already-
//! reviewed resolution to the identical requirement): this sandboxed
//! development environment has no way to capture genuine live traffic from
//! a real Mastodon client. `real_status_and_poll_json_are_registered_as_fixtures_...`
//! below registers a real (not mocked/hand-typed) Status and Poll JSON
//! exchange — produced by this crate's own real, deterministic-boundary
//! `StatusService`/`PollService` + `serializer.rs` pipeline via
//! `register_fixture` — and proves a second, independently-booted
//! instance's live output for the equivalent scenario is held to that
//! fixture's `response_body`, exactly the shape a real client capture would
//! eventually be swapped into. Unlike `contract_harness_fixture_it.rs`
//! (a single-test file), this proof uses a direct `assert_eq!` against the
//! loaded fixture's `response_body` rather than routing through
//! `assert_golden` with `KAWASEMI_UPDATE_GOLDEN` — for the same
//! "more than one test can run concurrently in this process" reason noted
//! above, not a weaker check (a JSON `assert_eq!` and `assert_golden`'s
//! comparison are the same equality test; `assert_golden` only adds
//! file-persistence and location-pinpointed mismatch reporting on top).

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use time::Duration as TimeDuration;
use tower::ServiceExt;

use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor, ResolvedActor};
use kawasemi::contract::{CapturedExchange, assert_golden, load_fixture, register_fixture};
use kawasemi::domain::{Id, Visibility};
use kawasemi::oauth::app_repository::{self, NewApp};
use kawasemi::oauth::model::ScopeSet as ModelScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::server;
use kawasemi::statuses::{Poll, PollOption, Status, poll_repository, status_repository};
use kawasemi::test_harness::{TestApp, spawn_test_app};

// ---- Fixture plumbing (each `tests/*.rs` file is its own compiled crate,
// so this deliberately duplicates `tests/status_crud_it.rs`'s/`tests/
// polls_it.rs`'s own established conventions rather than importing them —
// this crate's own documented convention). ----

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
            display_name: format!("Status Contract IT {handle_str}"),
            summary: "an actor used by the status_contract_it integration test".to_string(),
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

async fn register_test_app(app: &TestApp) -> Id {
    let now = app.runtime.clock.now();
    let registered = app_repository::register_app(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        app.state.oauth().token_hash_key(),
        now,
        NewApp {
            name: "Status Contract IT Client".to_string(),
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

/// Inserts a poll-bearing [`Status`] directly (bypassing the not-yet-wired
/// create-with-poll endpoint path — see this file's own doc comment and
/// `tests/polls_it.rs`'s identical, already-reviewed convention).
async fn insert_poll_status_fixture(
    app: &TestApp,
    author_id: Id,
    visibility: Visibility,
    multiple: bool,
    expires_at: Option<time::OffsetDateTime>,
    option_titles: &[&str],
) -> (Id, Id) {
    let status_id = app.runtime.ids.next_id();
    let poll_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    let uri = format!(
        "https://test-harness.kawasemi.internal/statuses/{}",
        status_id.as_i64()
    );

    let status = Status {
        id: status_id,
        actor_id: author_id,
        uri: uri.clone(),
        url: Some(uri),
        content: "a poll-bearing post inserted directly by the status_contract_it fixture"
            .to_string(),
        visibility,
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
    status_repository::insert_status(&app.pool, &status)
        .await
        .expect("insert status fixture for poll");

    let poll = Poll {
        id: poll_id,
        status_id,
        expires_at,
        multiple,
    };
    let options: Vec<PollOption> = option_titles
        .iter()
        .enumerate()
        .map(|(idx, title)| PollOption {
            poll_id,
            idx: idx as i32,
            title: title.to_string(),
            votes_count: 0,
        })
        .collect();
    poll_repository::insert_poll(&app.pool, &poll, &options)
        .await
        .expect("insert poll fixture");

    (status_id, poll_id)
}

fn unique_fixture_name(label: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("status_contract_it_{label}_{nanos}_{seq}")
}

/// Best-effort cleanup of a fixture file this test registers, regardless of
/// whether the test body panicked (mirrors `tests/
/// contract_harness_fixture_it.rs`'s identical `FixtureGuard` convention),
/// so `tests/fixtures/` does not accumulate orphaned files across runs.
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
// (1) Status goldens: normal / reblog / edited / with-poll
// (Requirements 1.1-1.6)
// ==========================================================================

/// Requirements 1.1 (field presence), 1.5 (null discipline for a post with
/// no reply/poll/edit/reblog), 1.6/15.1 (no dialect field) — a normal,
/// non-reblog, non-edited, no-poll Status's real serialized JSON.
#[tokio::test]
async fn normal_status_json_matches_the_registered_contract_golden() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "alice_contract_normal").await;
    let app_id = register_test_app(&app).await;
    let alice_token = issue_test_token(&app, app_id, alice.id, &["write:statuses"]).await;

    let created = create_status(
        &router,
        &alice_token,
        json!({"status": "hello contract world", "language": "en"}),
    )
    .await;
    let id = id_of(&created);

    let (status, rendered) = send(
        &router,
        req(
            "GET",
            &format!("/api/v1/statuses/{id}"),
            Some(&alice_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {rendered:?}");

    // Requirement 1.1: every contract field is present.
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
            rendered.get(field).is_some(),
            "expected field {field:?} in real Status JSON, got {rendered:?}"
        );
    }

    // Requirement 1.5: null discipline for every absent optional field.
    assert_eq!(rendered["poll"], Value::Null);
    assert_eq!(rendered["in_reply_to_id"], Value::Null);
    assert_eq!(rendered["in_reply_to_account_id"], Value::Null);
    assert_eq!(rendered["edited_at"], Value::Null);
    assert_eq!(rendered["reblog"], Value::Null);
    assert_eq!(rendered["language"], "en");

    // Requirement 1.2: authenticated-but-uninteracted viewer state is all
    // real (queried, not hardcoded) false.
    assert_eq!(rendered["favourited"], false);
    assert_eq!(rendered["reblogged"], false);
    assert_eq!(rendered["bookmarked"], false);
    assert_eq!(rendered["pinned"], false);
    assert_eq!(rendered["muted"], false);

    // Requirement 1.6/15.1: no custom-federation dialect field ever appears.
    let obj = rendered.as_object().expect("Status JSON is an object");
    for dialect_field in [
        "quote",
        "quote_id",
        "quoted_status_id",
        "emoji_reactions",
        "reactions",
    ] {
        assert!(
            !obj.contains_key(dialect_field),
            "Status JSON must not contain dialect field {dialect_field:?}"
        );
    }

    assert_golden(
        "tests/golden/statuses/status_contract_it_normal.json",
        &rendered,
    );

    app.cleanup().await;
}

/// Requirement 1.3: a reblog Status's real serialized JSON — correct
/// nesting of the reblogged status under `reblog`, and `reblogged: true`
/// for the actor who performed the boost.
#[tokio::test]
async fn reblog_status_json_matches_the_registered_contract_golden() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "alice_contract_reblog").await;
    let bob = insert_actor_fixture(&app, "bob_contract_reblog").await;
    let app_id = register_test_app(&app).await;
    let alice_token = issue_test_token(&app, app_id, alice.id, &["write:statuses"]).await;
    let bob_token = issue_test_token(&app, app_id, bob.id, &["write:statuses"]).await;

    let original = create_status(
        &router,
        &alice_token,
        json!({"status": "boost this contract fixture"}),
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

    assert!(
        boost["reblog"].is_object(),
        "reblog must nest the boosted status"
    );
    assert_eq!(boost["reblog"]["id"].as_str(), Some(original_id.as_str()));
    assert_eq!(
        boost["reblog"]["content"], "boost this contract fixture",
        "the nested reblog target's own content field must be Mastodon-compatible"
    );
    // `boost["reblogged"]` is scoped to the *boost row's own* id (bob has
    // not reblogged his own boost, only the original) — real
    // `InteractionRepository::find_reblog(bob, boost_id)` state, not
    // hardcoded. Requirement 1.2's "reblogged" state for *the original* is
    // exercised separately below via a fresh `GET` on the original status.
    assert_eq!(boost["reblogged"], false);

    let (status, refreshed_original) = send(
        &router,
        req(
            "GET",
            &format!("/api/v1/statuses/{original_id}"),
            Some(&bob_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {refreshed_original:?}");
    assert_eq!(
        refreshed_original["reblogged"], true,
        "the original status, viewed by the actor who boosted it, must reflect real \
         reblogged state"
    );
    assert_eq!(refreshed_original["reblogs_count"], 1);

    assert_golden(
        "tests/golden/statuses/status_contract_it_reblog.json",
        &boost,
    );

    app.cleanup().await;
}

/// Requirement 1.1 (`edited_at`): an edited Status's real serialized JSON —
/// `edited_at` populated once edited (distinct from the null-discipline
/// case in the normal-status golden above).
#[tokio::test]
async fn edited_status_json_matches_the_registered_contract_golden() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "alice_contract_edited").await;
    let app_id = register_test_app(&app).await;
    let alice_token = issue_test_token(&app, app_id, alice.id, &["write:statuses"]).await;

    let created = create_status(&router, &alice_token, json!({"status": "before the edit"})).await;
    let id = id_of(&created);
    assert_eq!(created["edited_at"], Value::Null);

    let (status, edited) = send(
        &router,
        req(
            "PUT",
            &format!("/api/v1/statuses/{id}"),
            Some(&alice_token),
            Some(json!({"status": "after the edit", "spoiler_text": "edited"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {edited:?}");

    assert_eq!(edited["content"], "after the edit");
    assert_eq!(edited["spoiler_text"], "edited");
    assert_ne!(
        edited["edited_at"],
        Value::Null,
        "edited_at must be populated once a status has been edited"
    );

    assert_golden(
        "tests/golden/statuses/status_contract_it_edited.json",
        &edited,
    );

    app.cleanup().await;
}

/// Requirement 1.1 (`poll` embedding), 2.1-2.3 (voter state, `expired`): a
/// Status with an attached, currently-open Poll, after a real vote has been
/// cast through the real `PollService` — correct embedding of the real
/// nested Poll JSON, and `voted`/`own_votes` reflecting the real voting
/// actor's own state, not hardcoded.
#[tokio::test]
async fn status_with_open_poll_json_matches_the_registered_contract_golden() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "alice_contract_poll").await;
    let bob = insert_actor_fixture(&app, "bob_contract_poll").await;
    let app_id = register_test_app(&app).await;
    let bob_token = issue_test_token(&app, app_id, bob.id, &["write:statuses"]).await;
    let now = app.runtime.clock.now();

    let (status_id, poll_id) = insert_poll_status_fixture(
        &app,
        alice.id,
        Visibility::Public,
        false,
        Some(now + TimeDuration::hours(1)),
        &["Cats", "Dogs"],
    )
    .await;

    let (status, vote_result) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/polls/{}/votes", poll_id.as_i64()),
            Some(&bob_token),
            Some(json!({"choices": [0]})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {vote_result:?}");
    assert_eq!(vote_result["expired"], false);

    let (status, rendered) = send(
        &router,
        req(
            "GET",
            &format!("/api/v1/statuses/{}", status_id.as_i64()),
            Some(&bob_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {rendered:?}");

    assert!(
        rendered["poll"].is_object(),
        "poll must be embedded, not null"
    );
    assert_eq!(
        rendered["poll"]["id"].as_str(),
        Some(poll_id.as_i64().to_string().as_str())
    );
    assert_eq!(rendered["poll"]["expired"], false);
    assert_eq!(rendered["poll"]["voted"], true);
    assert_eq!(rendered["poll"]["own_votes"], json!([0]));
    assert_eq!(rendered["poll"]["votes_count"], 1);
    let options = rendered["poll"]["options"]
        .as_array()
        .expect("options array");
    assert_eq!(options[0]["title"], "Cats");
    assert_eq!(options[0]["votes_count"], 1);
    assert_eq!(options[1]["title"], "Dogs");
    assert_eq!(options[1]["votes_count"], 0);

    assert_golden(
        "tests/golden/statuses/status_contract_it_with_poll.json",
        &rendered,
    );

    app.cleanup().await;
}

// ==========================================================================
// (2) Poll goldens: standalone open / expired (Requirements 2.1-2.4)
// ==========================================================================

/// Requirement 2.3: a Poll whose `expires_at` has passed — `expired: true`,
/// computed at read time (`SerializeContext::now` against `poll.expires_at`
/// — see `serializer.rs`'s own `to_poll_json` doc comment), against the
/// real, fixed `RuntimeContext.clock`, not stored state.
#[tokio::test]
async fn expired_poll_json_matches_the_registered_contract_golden() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "alice_contract_poll_expired").await;
    let now = app.runtime.clock.now();

    let (_status_id, poll_id) = insert_poll_status_fixture(
        &app,
        alice.id,
        Visibility::Public,
        false,
        Some(now - TimeDuration::seconds(1)),
        &["Yes", "No"],
    )
    .await;

    let (status, poll) = send(
        &router,
        req(
            "GET",
            &format!("/api/v1/polls/{}", poll_id.as_i64()),
            None,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {poll:?}");

    assert_eq!(poll["expired"], true);
    assert_eq!(poll["voted"], false);
    assert_eq!(poll["own_votes"], json!([]));

    assert_golden("tests/golden/statuses/poll_contract_it_expired.json", &poll);

    app.cleanup().await;
}

// ==========================================================================
// (3) Operation/interaction state reflects real InteractionService state,
// per-viewer (Requirement 1.2)
// ==========================================================================

/// The owning actor favourites/bookmarks/pins their own real post through
/// the real `InteractionService`; a second, uninteracted viewer sees every
/// operation-state field as `false` for the identical post — proving these
/// booleans are genuinely viewer-scoped database reads, not hardcoded.
#[tokio::test]
async fn operation_state_reflects_real_interaction_service_state_through_the_real_pipeline() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "alice_contract_ops").await;
    let bob = insert_actor_fixture(&app, "bob_contract_ops").await;
    let app_id = register_test_app(&app).await;
    let alice_token = issue_test_token(
        &app,
        app_id,
        alice.id,
        &["write:statuses", "write:favourites", "write:bookmarks"],
    )
    .await;
    let bob_token = issue_test_token(&app, app_id, bob.id, &["write:statuses"]).await;

    let created = create_status(
        &router,
        &alice_token,
        json!({"status": "alice's own post, interacted with by herself"}),
    )
    .await;
    let id = id_of(&created);

    for action in ["favourite", "bookmark", "pin"] {
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

    let (status, alice_view) = send(
        &router,
        req(
            "GET",
            &format!("/api/v1/statuses/{id}"),
            Some(&alice_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {alice_view:?}");
    assert_eq!(alice_view["favourited"], true);
    assert_eq!(alice_view["bookmarked"], true);
    assert_eq!(alice_view["pinned"], true);
    assert_eq!(alice_view["reblogged"], false);

    let (status, bob_view) = send(
        &router,
        req(
            "GET",
            &format!("/api/v1/statuses/{id}"),
            Some(&bob_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {bob_view:?}");
    assert_eq!(
        bob_view["favourited"], false,
        "a different, uninteracted viewer must see false, not alice's own true state"
    );
    assert_eq!(bob_view["bookmarked"], false);
    assert_eq!(bob_view["pinned"], false);
    assert_eq!(bob_view["reblogged"], false);

    assert_golden(
        "tests/golden/statuses/status_contract_it_own_post_interactions.json",
        &alice_view,
    );

    app.cleanup().await;
}

// ==========================================================================
// (4) Reproducibility across two independently-spawned instances
// ==========================================================================

/// Requirement 1.4/2.4 ("決定的な非決定性境界... の下で再現可能"), steering
/// `tech.md`'s "決定性の強制": the identical normal-status scenario, driven
/// against two independently-`spawn_test_app`-booted instances, produces
/// byte-for-byte identical Status JSON — no non-determinism (clock, id)
/// leaks through the real pipeline. Mirrors `tests/
/// media_attachment_contract_it.rs`'s own identical two-instance proof.
#[tokio::test]
async fn the_same_scenario_reproduces_byte_identical_json_across_two_independently_spawned_instances()
 {
    async fn build_normal_status(app: &TestApp) -> Value {
        let router = real_router(app);
        // Deliberately the same handle literal `normal_status_json_matches_
        // the_registered_contract_golden` uses: this scenario is meant to
        // *be* that same golden's scenario, replayed against two more
        // independent instances, not a merely-similar one -- both tests
        // target the same golden file below, so they must produce
        // byte-identical JSON or `assert_golden` would flag drift between
        // them (see this file's own doc comment, "The `KAWASEMI_UPDATE_
        // GOLDEN` baseline was recorded, not embedded").
        let alice = insert_actor_fixture(app, "alice_contract_normal").await;
        let app_id = register_test_app(app).await;
        let alice_token = issue_test_token(app, app_id, alice.id, &["write:statuses"]).await;

        let created = create_status(
            &router,
            &alice_token,
            json!({"status": "hello contract world", "language": "en"}),
        )
        .await;
        let id = id_of(&created);

        let (status, rendered) = send(
            &router,
            req(
                "GET",
                &format!("/api/v1/statuses/{id}"),
                Some(&alice_token),
                None,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "got: {rendered:?}");
        rendered
    }

    let app_a = spawn_test_app().await;
    let app_b = spawn_test_app().await;

    let json_a = build_normal_status(&app_a).await;
    let json_b = build_normal_status(&app_b).await;

    assert_eq!(
        json_a, json_b,
        "two independently spawn_test_app-booted instances driving the identical \
         fixture/HTTP sequence must produce byte-for-byte identical Status JSON"
    );

    assert_golden(
        "tests/golden/statuses/status_contract_it_normal.json",
        &json_a,
    );
    assert_golden(
        "tests/golden/statuses/status_contract_it_normal.json",
        &json_b,
    );

    app_a.cleanup().await;
    app_b.cleanup().await;
}

// ==========================================================================
// (5) Real-client-capture fixture registration proof (see this file's own
// doc comment, "The real-client-capture-fixture requirement")
// ==========================================================================

/// Registers a real Status JSON and a real Poll JSON — each produced by the
/// real `StatusService`/`PollService` + `serializer.rs` pipeline through a
/// first `spawn_test_app` instance — as fixtures via `register_fixture`,
/// then proves a *second*, independently-booted instance's live output for
/// the equivalent scenario is held to each fixture's `response_body`.
#[tokio::test]
async fn real_status_and_poll_json_are_registered_as_fixtures_and_hold_a_second_instances_live_output()
 {
    // Both instances run this with the identical handle literals: each
    // `spawn_test_app` call gets its own isolated database (see
    // `test_harness.rs`'s own per-instance-database guarantee), so reusing
    // the same handles across `app_a`/`app_b` cannot collide, and using the
    // same handles (rather than an `app_a`/`app_b`-varying label) is what
    // keeps the two instances' `account` JSON (username/acct/uri/url, all
    // handle-derived) byte-for-byte identical — the whole point of this
    // proof.
    async fn build_status_and_poll(app: &TestApp) -> (Value, Value) {
        let router = real_router(app);
        let author = insert_actor_fixture(app, "contract_fixture_author").await;
        let voter = insert_actor_fixture(app, "contract_fixture_voter").await;
        let app_id = register_test_app(app).await;
        let voter_token = issue_test_token(app, app_id, voter.id, &["write:statuses"]).await;
        let now = app.runtime.clock.now();

        let (status_id, poll_id) = insert_poll_status_fixture(
            app,
            author.id,
            Visibility::Public,
            false,
            Some(now + TimeDuration::hours(1)),
            &["Real", "Fixture"],
        )
        .await;

        let (status, vote) = send(
            &router,
            req(
                "POST",
                &format!("/api/v1/polls/{}/votes", poll_id.as_i64()),
                Some(&voter_token),
                Some(json!({"choices": [0]})),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "vote must succeed: {vote:?}");

        let (status, status_json) = send(
            &router,
            req(
                "GET",
                &format!("/api/v1/statuses/{}", status_id.as_i64()),
                None,
                None,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "got: {status_json:?}");

        let (status, poll_json) = send(
            &router,
            req(
                "GET",
                &format!("/api/v1/polls/{}", poll_id.as_i64()),
                None,
                None,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "got: {poll_json:?}");

        (status_json, poll_json)
    }

    let app_a = spawn_test_app().await;
    let (status_json_a, poll_json_a) = build_status_and_poll(&app_a).await;

    let status_fixture_name = unique_fixture_name("real_status");
    let _status_fixture_guard = FixtureGuard(status_fixture_name.clone());
    register_fixture(
        &status_fixture_name,
        CapturedExchange {
            method: "GET".to_string(),
            path: "/api/v1/statuses/{id}".to_string(),
            request_body: None,
            status: 200,
            response_body: status_json_a.clone(),
        },
    );

    let poll_fixture_name = unique_fixture_name("real_poll");
    let _poll_fixture_guard = FixtureGuard(poll_fixture_name.clone());
    register_fixture(
        &poll_fixture_name,
        CapturedExchange {
            method: "GET".to_string(),
            path: "/api/v1/polls/{id}".to_string(),
            request_body: None,
            status: 200,
            response_body: poll_json_a.clone(),
        },
    );

    // (9.5) Round-trips unchanged through the public extension point.
    let loaded_status = load_fixture(&status_fixture_name);
    let loaded_poll = load_fixture(&poll_fixture_name);
    assert_eq!(loaded_status.response_body, status_json_a);
    assert_eq!(loaded_poll.response_body, poll_json_a);

    // (9.5, 9.1, 9.3) A second, independently-spawned instance's live
    // output — generated purely from its own deterministic RuntimeContext,
    // driving the identical scenario — is held to the fixture-derived
    // acceptance criterion via direct JSON comparison (see this file's own
    // doc comment for why a direct comparison is used here instead of
    // `assert_golden` + `KAWASEMI_UPDATE_GOLDEN`).
    let app_b = spawn_test_app().await;
    let (status_json_b, poll_json_b) = build_status_and_poll(&app_b).await;
    assert_eq!(
        status_json_b, loaded_status.response_body,
        "a second independently-booted instance's live Status JSON must match the \
         fixture-registered real-client-capture stand-in exactly"
    );
    assert_eq!(
        poll_json_b, loaded_poll.response_body,
        "a second independently-booted instance's live Poll JSON must match the \
         fixture-registered real-client-capture stand-in exactly"
    );

    app_a.cleanup().await;
    app_b.cleanup().await;
}
