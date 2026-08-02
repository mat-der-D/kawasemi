//! Integration tests for notifications task 5.2 (`.kiro/specs/notifications/
//! tasks.md`, "5.2 (P) 取得・消去の統合テスト", `_Depends: 4.2_`) — the
//! single-notification-retrieval and clear/dismiss half. design.md's File
//! Structure Plan names this exact file (`tests/
//! notification_show_dismiss_it.rs`, "単一取得・他者宛 404・dismiss/clear・
//! 消去後除外・スコープ（統合）"). Requirements 3.1-3.2, 4.1-4.4, 9.1-9.4.
//!
//! ## Relationship to `src/notifications/endpoints/tests.rs` (task 4.1) and
//! `tests/notification_list_it.rs`/`tests/notification_contract_it.rs`
//! (task 5.2's own sibling / task 5.1) — why this file, given all three
//! already exist
//! Mirrors `tests/notification_list_it.rs`'s own identical "why this file"
//! reasoning (see that file's own doc comment) for the `show`/`clear`/
//! `dismiss` handlers specifically: `src/notifications/endpoints/tests.rs`
//! (task 4.1) already covers these three handlers' auth/scope/response-code
//! wiring, but only against a hand-built, test-only `Router` (that task's
//! own explicit boundary, "Not wired into the module tree yet") — task 4.2
//! has since mounted them for real, so this file is the first to drive
//! `show_notification`/`clear_notifications`/`dismiss_notification` through
//! the *real, fully-wired* production router
//! (`kawasemi::server::build_router`), mirroring `tests/
//! timelines_endpoints_it.rs`'s established precedent. This file is split
//! from `tests/notification_list_it.rs` purely by design.md's own File
//! Structure Plan naming both files separately — there is no other boundary
//! difference; both dispatch through the identical real router/harness
//! conventions.
//!
//! ## Scope note: dismiss/clear's *effect on subsequent list results*
//! (Requirement 4.4) is asserted both here (as the direct completion
//! criterion of a dismiss/clear call) and, from the list side, in `tests/
//! notification_list_it.rs::list_excludes_dismissed_and_cleared_notifications`
//! — the two files deliberately overlap at that one seam rather than one
//! silently assuming the other proves it, since Requirement 4.4's own text
//! binds dismiss/clear (this file's boundary) *and* list/show exclusion
//! (both files') together in a single acceptance criterion.
//!
//! ## RED phase evidence
//! Before this file existed, `cargo test --test notification_show_dismiss_it`
//! failed with `error: no test target named
//! `notification_show_dismiss_it`` (no such file, no such Cargo-discovered
//! integration test binary) — mirrors `tests/notification_list_it.rs`'s own
//! identical situation and resolution (pure test-authoring against
//! already-implemented, already-wired behavior; every assertion verified by
//! direct reading of `src/notifications/endpoints.rs`/`service.rs`/
//! `repository.rs`).
//!
//! ## Sandbox DB availability
//! This sandbox has no reachable PostgreSQL (`pg_isready` confirms no
//! response, the same constraint every earlier task in this spec's own
//! `tasks.md` "## Implementation Notes" documents), so none of the
//! `#[tokio::test]`s below could be executed to completion here — see this
//! task's own status report, and `tests/notification_list_it.rs`'s/`tests/
//! notification_contract_it.rs`'s own identical documented resolution.

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
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

// ---- Fixture plumbing (each `tests/*.rs` file is its own compiled crate,
// so this deliberately duplicates `tests/notification_list_it.rs`'s/`tests/
// notification_contract_it.rs`'s own established conventions rather than
// importing them — this crate's own documented convention). -------------

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
            display_name: format!("Notification Show/Dismiss IT {handle_str}"),
            summary: "an actor used by the notification_show_dismiss_it integration test"
                .to_string(),
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
            name: "Notification Show/Dismiss IT Client".to_string(),
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

async fn send(router: &Router, request: Request<Body>) -> (StatusCode, HeaderMap, Value) {
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("router must not fail to produce a response");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read response body");
    let value: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("response body must be valid JSON")
    };
    (status, headers, value)
}

fn assert_error_shape(body: &Value) {
    assert!(
        body.get("error").and_then(Value::as_str).is_some(),
        "expected a Mastodon-compatible {{\"error\": ...}} body, got: {body}"
    );
}

async fn create_status(router: &Router, token: &str, body: Value) -> Value {
    let (status, _headers, resp) = send(
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

/// Drives a real follow: `follower` -> `target` (real `POST /api/v1/
/// accounts/{id}/follow`), the cheapest real trigger for a single
/// notification this file needs (mirrors `tests/
/// notification_list_it.rs::trigger_follow`).
async fn trigger_follow(router: &Router, follower_token: &str, target_id: Id) {
    let (status, _headers, body) = send(
        router,
        req(
            "POST",
            &format!("/api/v1/accounts/{}/follow", target_id.as_i64()),
            Some(follower_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "follow must succeed: {body:?}");
}

/// Drives a real mention (real `POST /api/v1/statuses`) — used where this
/// file wants a notification kind distinguishable from `follow`.
async fn trigger_mention(router: &Router, author_token: &str, mention_handle: &str) {
    create_status(
        router,
        author_token,
        json!({"status": format!("hey @{mention_handle}, from notification_show_dismiss_it")}),
    )
    .await;
}

/// `GET /api/v1/notifications?types[]=<kind>` — used to fetch a just-created
/// notification's own id without reaching into `notifications::repository`
/// directly, keeping this file's fixtures purely at the HTTP layer (mirrors
/// `tests/notification_contract_it.rs::list_notifications_by_type`).
async fn list_notifications_by_type(router: &Router, token: &str, kind: &str) -> Vec<Value> {
    let (status, _headers, body) = send(
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

async fn single_notification_id(router: &Router, token: &str, kind: &str) -> String {
    let items = list_notifications_by_type(router, token, kind).await;
    assert_eq!(
        items.len(),
        1,
        "expected exactly one {kind} notification: {items:?}"
    );
    items[0]["id"].as_str().unwrap().to_string()
}

async fn show_raw(
    router: &Router,
    token: Option<&str>,
    id: &str,
) -> (StatusCode, HeaderMap, Value) {
    send(
        router,
        req("GET", &format!("/api/v1/notifications/{id}"), token, None),
    )
    .await
}

async fn dismiss_raw(
    router: &Router,
    token: Option<&str>,
    id: &str,
) -> (StatusCode, HeaderMap, Value) {
    send(
        router,
        req(
            "POST",
            &format!("/api/v1/notifications/{id}/dismiss"),
            token,
            None,
        ),
    )
    .await
}

async fn clear_raw(router: &Router, token: Option<&str>) -> (StatusCode, HeaderMap, Value) {
    send(
        router,
        req("POST", "/api/v1/notifications/clear", token, None),
    )
    .await
}

async fn list_all(router: &Router, token: &str) -> Vec<Value> {
    let (status, _headers, body) = send(
        router,
        req("GET", "/api/v1/notifications", Some(token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {body:?}");
    body.as_array()
        .expect("notification list must be a JSON array")
        .clone()
}

// ==========================================================================
// (1) show_notification: own notification -> 200 with correct JSON
// (Requirement 3.1).
// ==========================================================================

#[tokio::test]
async fn show_returns_200_with_the_correct_notification_json_for_the_recipients_own_notification() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let app_id = register_test_app(&app).await;

    let alice = insert_actor_fixture(&app, "notif_show_alice").await;
    let bob = insert_actor_fixture(&app, "notif_show_bob").await;
    let alice_token = issue_test_token(&app, app_id, alice.id, &["write:statuses"]).await;
    let bob_token = issue_test_token(&app, app_id, bob.id, &["read:notifications"]).await;

    trigger_mention(&router, &alice_token, "notif_show_bob").await;
    let notification_id = single_notification_id(&router, &bob_token, "mention").await;

    let (status, _headers, body) = show_raw(&router, Some(&bob_token), &notification_id).await;
    assert_eq!(status, StatusCode::OK, "got: {body:?}");
    assert_eq!(body["id"].as_str(), Some(notification_id.as_str()));
    assert_eq!(body["type"], "mention");
    assert_eq!(
        body["account"]["id"].as_str(),
        Some(alice.id.as_i64().to_string().as_str()),
        "the embedded account must be the real mentioning actor"
    );
    assert!(
        body["status"].is_object(),
        "a mention notification must embed the real Status JSON, not null: {body:?}"
    );

    app.cleanup().await;
}

// ==========================================================================
// (2) show_notification: other-recipient / nonexistent -> 404
// (Requirement 3.2).
// ==========================================================================

#[tokio::test]
async fn show_for_another_actors_notification_is_404() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let app_id = register_test_app(&app).await;

    let owner = insert_actor_fixture(&app, "notif_show_404_owner").await;
    let intruder = insert_actor_fixture(&app, "notif_show_404_intruder").await;
    let follower = insert_actor_fixture(&app, "notif_show_404_follower").await;
    let owner_token = issue_test_token(&app, app_id, owner.id, &["read:notifications"]).await;
    let intruder_token = issue_test_token(&app, app_id, intruder.id, &["read:notifications"]).await;
    let follower_token = issue_test_token(&app, app_id, follower.id, &["follow"]).await;

    trigger_follow(&router, &follower_token, owner.id).await;
    let notification_id = single_notification_id(&router, &owner_token, "follow").await;

    let (status, _headers, body) = show_raw(&router, Some(&intruder_token), &notification_id).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "another actor's own notification must never be visible to a non-owner: {body:?}"
    );
    assert_error_shape(&body);

    app.cleanup().await;
}

#[tokio::test]
async fn show_for_a_nonexistent_id_is_404() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let app_id = register_test_app(&app).await;

    let viewer = insert_actor_fixture(&app, "notif_show_404_nonexistent").await;
    let token = issue_test_token(&app, app_id, viewer.id, &["read:notifications"]).await;

    let never_used_id = app.runtime.ids.next_id().as_i64();
    let (status, _headers, body) =
        show_raw(&router, Some(&token), &never_used_id.to_string()).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "got: {body:?}");
    assert_error_shape(&body);

    app.cleanup().await;
}

// ==========================================================================
// (3) show_notification: auth/scope discipline (Requirements 9.1, 9.2).
// ==========================================================================

#[tokio::test]
async fn show_requires_a_bearer_token_and_read_notifications_scope() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let app_id = register_test_app(&app).await;

    let (status_no_token, _headers, body_no_token) = show_raw(&router, None, "1").await;
    assert_eq!(
        status_no_token,
        StatusCode::UNAUTHORIZED,
        "got: {body_no_token:?}"
    );
    assert_error_shape(&body_no_token);

    let viewer = insert_actor_fixture(&app, "notif_show_scope_viewer").await;
    // `write:notifications` deliberately omits `read:notifications`.
    let token = issue_test_token(&app, app_id, viewer.id, &["write:notifications"]).await;
    let (status_bad_scope, _headers, body_bad_scope) = show_raw(&router, Some(&token), "1").await;
    assert_eq!(
        status_bad_scope,
        StatusCode::FORBIDDEN,
        "got: {body_bad_scope:?}"
    );
    assert_error_shape(&body_bad_scope);

    app.cleanup().await;
}

// ==========================================================================
// (4) dismiss_notification: own notification succeeds, excludes from
// subsequent list/show (Requirements 4.2, 4.4).
// ==========================================================================

#[tokio::test]
async fn dismiss_own_notification_succeeds_and_is_excluded_from_subsequent_list_and_show() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let app_id = register_test_app(&app).await;

    let bob = insert_actor_fixture(&app, "notif_dismiss_ok_bob").await;
    let alice = insert_actor_fixture(&app, "notif_dismiss_ok_alice").await;
    let bob_read_token = issue_test_token(&app, app_id, bob.id, &["read:notifications"]).await;
    let bob_write_token = issue_test_token(&app, app_id, bob.id, &["write:notifications"]).await;
    let alice_token = issue_test_token(&app, app_id, alice.id, &["follow"]).await;

    trigger_follow(&router, &alice_token, bob.id).await;
    let notification_id = single_notification_id(&router, &bob_read_token, "follow").await;

    let (status, _headers, body) =
        dismiss_raw(&router, Some(&bob_write_token), &notification_id).await;
    assert_eq!(status, StatusCode::OK, "got: {body:?}");
    assert_eq!(body, json!({}));

    let (show_status, _headers2, show_body) =
        show_raw(&router, Some(&bob_read_token), &notification_id).await;
    assert_eq!(
        show_status,
        StatusCode::NOT_FOUND,
        "a dismissed notification must 404 on subsequent show (Requirement 4.4): {show_body:?}"
    );

    let after = list_all(&router, &bob_read_token).await;
    assert!(
        after.is_empty(),
        "a dismissed notification must never appear in list results (Requirement 4.4): \
         {after:?}"
    );

    app.cleanup().await;
}

// ==========================================================================
// (5) dismiss_notification: other-recipient / nonexistent -> 404, no silent
// success, no dismissal side effect (Requirement 4.3).
// ==========================================================================

#[tokio::test]
async fn dismiss_of_another_actors_notification_is_404_and_leaves_it_undismissed() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let app_id = register_test_app(&app).await;

    let owner = insert_actor_fixture(&app, "notif_dismiss_404_owner").await;
    let intruder = insert_actor_fixture(&app, "notif_dismiss_404_intruder").await;
    let follower = insert_actor_fixture(&app, "notif_dismiss_404_follower").await;
    let owner_read_token = issue_test_token(&app, app_id, owner.id, &["read:notifications"]).await;
    let intruder_write_token =
        issue_test_token(&app, app_id, intruder.id, &["write:notifications"]).await;
    let follower_token = issue_test_token(&app, app_id, follower.id, &["follow"]).await;

    trigger_follow(&router, &follower_token, owner.id).await;
    let notification_id = single_notification_id(&router, &owner_read_token, "follow").await;

    let (status, _headers, body) =
        dismiss_raw(&router, Some(&intruder_write_token), &notification_id).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "dismissing another actor's notification must never silently succeed: {body:?}"
    );
    assert_error_shape(&body);

    // The real owner's own view must be untouched by the intruder's failed
    // attempt — proves the 404 did not leak into an actual dismissal.
    let (owner_show_status, _headers2, owner_show_body) =
        show_raw(&router, Some(&owner_read_token), &notification_id).await;
    assert_eq!(
        owner_show_status,
        StatusCode::OK,
        "the owner's own notification must remain visible after an intruder's failed dismiss \
         attempt: {owner_show_body:?}"
    );

    app.cleanup().await;
}

#[tokio::test]
async fn dismiss_of_a_nonexistent_id_is_404() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let app_id = register_test_app(&app).await;

    let viewer = insert_actor_fixture(&app, "notif_dismiss_404_nonexistent").await;
    let token = issue_test_token(&app, app_id, viewer.id, &["write:notifications"]).await;

    let never_used_id = app.runtime.ids.next_id().as_i64();
    let (status, _headers, body) =
        dismiss_raw(&router, Some(&token), &never_used_id.to_string()).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "got: {body:?}");
    assert_error_shape(&body);

    app.cleanup().await;
}

// ==========================================================================
// (6) dismiss_notification: auth/scope discipline (Requirements 9.1, 9.2).
// ==========================================================================

#[tokio::test]
async fn dismiss_requires_a_bearer_token_and_write_notifications_scope() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let app_id = register_test_app(&app).await;

    let (status_no_token, _headers, body_no_token) = dismiss_raw(&router, None, "1").await;
    assert_eq!(
        status_no_token,
        StatusCode::UNAUTHORIZED,
        "got: {body_no_token:?}"
    );
    assert_error_shape(&body_no_token);

    let viewer = insert_actor_fixture(&app, "notif_dismiss_scope_viewer").await;
    // `read:notifications` deliberately omits `write:notifications`.
    let token = issue_test_token(&app, app_id, viewer.id, &["read:notifications"]).await;
    let (status_bad_scope, _headers, body_bad_scope) =
        dismiss_raw(&router, Some(&token), "1").await;
    assert_eq!(
        status_bad_scope,
        StatusCode::FORBIDDEN,
        "got: {body_bad_scope:?}"
    );
    assert_error_shape(&body_bad_scope);

    app.cleanup().await;
}

// ==========================================================================
// (7) clear_notifications: dismisses all of the recipient's notifications,
// leaves other actors' notifications untouched (Requirement 4.1, scope
// isolation).
// ==========================================================================

#[tokio::test]
async fn clear_dismisses_all_of_the_recipients_notifications_and_leaves_other_actors_untouched() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let app_id = register_test_app(&app).await;

    let bob = insert_actor_fixture(&app, "notif_clear_bob").await;
    let alice = insert_actor_fixture(&app, "notif_clear_alice").await;
    let carol = insert_actor_fixture(&app, "notif_clear_carol").await;
    let bob_read_token = issue_test_token(&app, app_id, bob.id, &["read:notifications"]).await;
    let bob_write_token = issue_test_token(&app, app_id, bob.id, &["write:notifications"]).await;
    let alice_read_token = issue_test_token(&app, app_id, alice.id, &["read:notifications"]).await;
    let alice_write_token =
        issue_test_token(&app, app_id, alice.id, &["write:statuses", "follow"]).await;
    let carol_token = issue_test_token(&app, app_id, carol.id, &["follow"]).await;

    // bob gets two notifications (alice + carol both follow him).
    trigger_follow(&router, &alice_write_token, bob.id).await;
    trigger_follow(&router, &carol_token, bob.id).await;
    let bob_before = list_all(&router, &bob_read_token).await;
    assert_eq!(bob_before.len(), 2);

    // alice separately gets exactly one notification of her own (bob
    // mentions her) — proves clear is recipient-scoped, not global.
    let bob_mention_token = issue_test_token(&app, app_id, bob.id, &["write:statuses"]).await;
    trigger_mention(&router, &bob_mention_token, "notif_clear_alice").await;
    let alice_before = list_all(&router, &alice_read_token).await;
    assert_eq!(alice_before.len(), 1);

    let (status, _headers, body) = clear_raw(&router, Some(&bob_write_token)).await;
    assert_eq!(status, StatusCode::OK, "got: {body:?}");
    assert_eq!(body, json!({}));

    let bob_after = list_all(&router, &bob_read_token).await;
    assert!(
        bob_after.is_empty(),
        "clear must dismiss every one of bob's own notifications: {bob_after:?}"
    );

    let alice_after = list_all(&router, &alice_read_token).await;
    assert_eq!(
        alice_after.len(),
        1,
        "clearing bob's notifications must never affect alice's own (scope isolation, \
         Requirement 4.1): {alice_after:?}"
    );
    assert_eq!(alice_after, alice_before);

    app.cleanup().await;
}

// ==========================================================================
// (8) clear_notifications: auth/scope discipline (Requirements 9.1, 9.2).
// ==========================================================================

#[tokio::test]
async fn clear_requires_a_bearer_token_and_write_notifications_scope() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let app_id = register_test_app(&app).await;

    let (status_no_token, _headers, body_no_token) = clear_raw(&router, None).await;
    assert_eq!(
        status_no_token,
        StatusCode::UNAUTHORIZED,
        "got: {body_no_token:?}"
    );
    assert_error_shape(&body_no_token);

    let viewer = insert_actor_fixture(&app, "notif_clear_scope_viewer").await;
    // `read:notifications` deliberately omits `write:notifications`.
    let token = issue_test_token(&app, app_id, viewer.id, &["read:notifications"]).await;
    let (status_bad_scope, _headers, body_bad_scope) = clear_raw(&router, Some(&token)).await;
    assert_eq!(
        status_bad_scope,
        StatusCode::FORBIDDEN,
        "got: {body_bad_scope:?}"
    );
    assert_error_shape(&body_bad_scope);

    app.cleanup().await;
}
