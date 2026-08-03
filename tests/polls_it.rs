//! Integration tests for task 8.1 (`.kiro/specs/statuses-core/tasks.md`,
//! "8.1 統合テスト（CRUD・冪等・context・操作・投票）を整備する"): poll
//! voting's rejection cases and tally/aggregation correctness — driven as
//! real HTTP requests through the fully-wired application router booted by
//! `spawn_test_app` (Requirements 13.2, 13.3, 13.4, 13.5).
//!
//! ## Poll fixtures are inserted directly, not created through the endpoint
//! `StatusService::create_status` does not persist a caller-supplied poll
//! at all yet — every poll-bearing create request is rejected 422 (see
//! `status_service.rs`'s own "Poll handling" doc comment,
//! `poll_service.rs`'s own "Poll creation (Requirement 13.1) is out of this
//! task's scope" doc comment, and `tests/status_crud_it.rs`'s own coverage
//! of that documented, already-reviewed cross-task boundary decision). This
//! file therefore fixtures a poll-bearing [`Status`] directly via
//! `status_repository::insert_status` + `poll_repository::insert_poll` —
//! exactly the "insert fixtures directly, bypass the creating service"
//! pattern `poll_service.rs`'s own doc comment documents as the accepted
//! technique for exercising `PollService`/`StatusEndpoints` voting
//! end-to-end while 13.1 (poll *creation*) remains unwired. Voting itself
//! (13.2-13.5) and its reflection back through `GET /api/v1/statuses/:id`'s
//! nested `poll` field are both exercised as real HTTP requests, so this
//! file's own boundary (`PollService`, `StatusEndpoints`) is still driven
//! end-to-end through the router, not called as a Rust function directly.
//!
//! See `tests/status_crud_it.rs`'s own doc comment for this file's shared
//! conventions (in-process `tower::ServiceExt::oneshot` against
//! `crate::server::build_router(app.state.clone())`, fixture-plumbing
//! duplication across sibling test files).

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use time::Duration as TimeDuration;
use tower::ServiceExt;

use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor, ResolvedActor};
use kawasemi::domain::{Id, Visibility};
use kawasemi::oauth::app_repository::{self, NewApp};
use kawasemi::oauth::model::ScopeSet as ModelScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::server;
use kawasemi::statuses::{Poll, PollOption, Status, poll_repository, status_repository};
use kawasemi::test_harness::{TestApp, spawn_test_app};

// ---- Fixture plumbing (duplicated per sibling test-file convention — see
// `tests/status_crud_it.rs`'s own doc comment). ----

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
            display_name: format!("Polls IT {handle_str}"),
            summary: "an actor used by the polls_it integration test".to_string(),
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
            name: "Polls IT Client".to_string(),
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

/// Inserts a poll-bearing [`Status`] directly (bypassing the
/// not-yet-wired create-with-poll endpoint path — see this file's own doc
/// comment). `statuses.poll_id` carries no FK (`migrations/0007_statuses.sql`),
/// so the status row may be inserted first with `poll_id` already pointing
/// at a not-yet-existing poll id; `polls.status_id` *does* carry a mandatory
/// FK, so the poll is inserted second, once its owning status row exists.
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
        content: "a poll-bearing post inserted directly by the polls_it fixture".to_string(),
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

// ==== Vote recording and tally (Requirement 13.2) ====

#[tokio::test]
async fn voting_records_the_choice_and_updates_the_tally() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "alice_poll_vote").await;
    let bob = insert_actor_fixture(&app, "bob_poll_vote").await;
    let app_id = register_test_app(&app).await;
    let bob_token = issue_test_token(&app, app_id, bob.id, &["write:statuses"]).await;

    let (status_id, poll_id) = insert_poll_status_fixture(
        &app,
        alice.id,
        Visibility::Public,
        false,
        None,
        &["cats", "dogs", "birds"],
    )
    .await;

    let (status, before) = send(
        &router,
        req(
            "GET",
            &format!("/api/v1/polls/{}", poll_id.as_i64()),
            None,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {before:?}");
    assert_eq!(before["voted"], false);
    assert_eq!(before["votes_count"], 0);

    let (status, after) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/polls/{}/votes", poll_id.as_i64()),
            Some(&bob_token),
            Some(json!({"choices": [1]})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {after:?}");
    assert_eq!(after["voted"], true);
    assert_eq!(after["votes_count"], 1);
    assert_eq!(after["voters_count"], 1);
    assert_eq!(after["own_votes"], json!([1]));
    let options = after["options"].as_array().expect("options array");
    assert_eq!(options[0]["votes_count"], 0);
    assert_eq!(options[1]["votes_count"], 1);
    assert_eq!(options[2]["votes_count"], 0);

    // The poll is also reflected inside the owning status's own JSON
    // (ties `PollService` and `StatusEndpoints` together end to end).
    let (status, rendered_status) = send(
        &router,
        req(
            "GET",
            &format!("/api/v1/statuses/{}", status_id.as_i64()),
            Some(&bob_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {rendered_status:?}");
    assert_eq!(rendered_status["poll"]["voted"], true);
    assert_eq!(rendered_status["poll"]["votes_count"], 1);

    app.cleanup().await;
}

// ==== Rejection: deadline passed (Requirement 13.3) ====

#[tokio::test]
async fn voting_after_the_deadline_is_rejected() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "alice_poll_expired").await;
    let bob = insert_actor_fixture(&app, "bob_poll_expired").await;
    let app_id = register_test_app(&app).await;
    let bob_token = issue_test_token(&app, app_id, bob.id, &["write:statuses"]).await;
    let now = app.runtime.clock.now();

    let (_, poll_id) = insert_poll_status_fixture(
        &app,
        alice.id,
        Visibility::Public,
        false,
        Some(now - TimeDuration::seconds(1)),
        &["a", "b"],
    )
    .await;

    let (status, body) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/polls/{}/votes", poll_id.as_i64()),
            Some(&bob_token),
            Some(json!({"choices": [0]})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "got: {body:?}");

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
    assert_eq!(status, StatusCode::OK);
    assert_eq!(poll["expired"], true);

    app.cleanup().await;
}

// ==== Rejection: single-choice / out-of-range (Requirement 13.4) ====

#[tokio::test]
async fn a_single_choice_poll_rejects_multiple_selected_options() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "alice_poll_single").await;
    let bob = insert_actor_fixture(&app, "bob_poll_single").await;
    let app_id = register_test_app(&app).await;
    let bob_token = issue_test_token(&app, app_id, bob.id, &["write:statuses"]).await;

    let (_, poll_id) = insert_poll_status_fixture(
        &app,
        alice.id,
        Visibility::Public,
        false,
        None,
        &["a", "b", "c"],
    )
    .await;

    let (status, body) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/polls/{}/votes", poll_id.as_i64()),
            Some(&bob_token),
            Some(json!({"choices": [0, 1]})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "got: {body:?}");

    app.cleanup().await;
}

#[tokio::test]
async fn voting_rejects_an_out_of_range_option_index() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "alice_poll_range").await;
    let bob = insert_actor_fixture(&app, "bob_poll_range").await;
    let app_id = register_test_app(&app).await;
    let bob_token = issue_test_token(&app, app_id, bob.id, &["write:statuses"]).await;

    let (_, poll_id) =
        insert_poll_status_fixture(&app, alice.id, Visibility::Public, false, None, &["a", "b"])
            .await;

    let (status, body) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/polls/{}/votes", poll_id.as_i64()),
            Some(&bob_token),
            Some(json!({"choices": [99]})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "got: {body:?}");

    app.cleanup().await;
}

// ==== Rejection: duplicate vote (Requirement 13.5) ====

#[tokio::test]
async fn a_second_vote_by_the_same_actor_is_rejected_and_does_not_change_the_tally() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "alice_poll_dup").await;
    let bob = insert_actor_fixture(&app, "bob_poll_dup").await;
    let app_id = register_test_app(&app).await;
    let bob_token = issue_test_token(&app, app_id, bob.id, &["write:statuses"]).await;

    let (_, poll_id) =
        insert_poll_status_fixture(&app, alice.id, Visibility::Public, false, None, &["a", "b"])
            .await;

    let (status, _) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/polls/{}/votes", poll_id.as_i64()),
            Some(&bob_token),
            Some(json!({"choices": [0]})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/polls/{}/votes", poll_id.as_i64()),
            Some(&bob_token),
            Some(json!({"choices": [1]})),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a repeat vote by the same actor must be rejected: {body:?}"
    );

    let (status, poll) = send(
        &router,
        req(
            "GET",
            &format!("/api/v1/polls/{}", poll_id.as_i64()),
            Some(&bob_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        poll["votes_count"], 1,
        "the rejected repeat vote must not have changed the tally"
    );
    assert_eq!(poll["own_votes"], json!([0]));

    app.cleanup().await;
}

// ==== Multiple-choice aggregation (Requirement 13.2) ====

#[tokio::test]
async fn a_multiple_choice_poll_records_every_selected_option() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "alice_poll_multi").await;
    let bob = insert_actor_fixture(&app, "bob_poll_multi").await;
    let app_id = register_test_app(&app).await;
    let bob_token = issue_test_token(&app, app_id, bob.id, &["write:statuses"]).await;

    let (_, poll_id) = insert_poll_status_fixture(
        &app,
        alice.id,
        Visibility::Public,
        true,
        None,
        &["a", "b", "c"],
    )
    .await;

    let (status, after) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/polls/{}/votes", poll_id.as_i64()),
            Some(&bob_token),
            Some(json!({"choices": [0, 2]})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {after:?}");
    assert_eq!(after["votes_count"], 2);
    assert_eq!(after["voters_count"], 1);
    let mut own_votes: Vec<i64> = after["own_votes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_i64().unwrap())
        .collect();
    own_votes.sort_unstable();
    assert_eq!(own_votes, vec![0, 2]);
    let options = after["options"].as_array().unwrap();
    assert_eq!(options[0]["votes_count"], 1);
    assert_eq!(options[1]["votes_count"], 0);
    assert_eq!(options[2]["votes_count"], 1);

    app.cleanup().await;
}

// ==== Visibility gate shared with StatusService (Requirement 4.1) ====

#[tokio::test]
async fn voting_on_a_poll_invisible_to_the_actor_is_rejected_as_not_found() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "alice_poll_private").await;
    let bob = insert_actor_fixture(&app, "bob_poll_private").await;
    let app_id = register_test_app(&app).await;
    let alice_token = issue_test_token(&app, app_id, alice.id, &["write:statuses"]).await;
    let bob_token = issue_test_token(&app, app_id, bob.id, &["write:statuses"]).await;

    let (_, poll_id) = insert_poll_status_fixture(
        &app,
        alice.id,
        Visibility::Private,
        false,
        None,
        &["a", "b"],
    )
    .await;

    let (status, body) = send(
        &router,
        req(
            "GET",
            &format!("/api/v1/polls/{}", poll_id.as_i64()),
            Some(&bob_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "got: {body:?}");

    let (status, body) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/polls/{}/votes", poll_id.as_i64()),
            Some(&bob_token),
            Some(json!({"choices": [0]})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "got: {body:?}");

    // The author can still vote on their own private poll.
    let (status, body) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/polls/{}/votes", poll_id.as_i64()),
            Some(&alice_token),
            Some(json!({"choices": [0]})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {body:?}");

    app.cleanup().await;
}
