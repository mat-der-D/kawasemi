//! Integration tests for notifications task 5.3 (`.kiro/specs/notifications/
//! tasks.md`, "5.3 (P) 生成・フィルタの統合テスト", `_Depends: 4.2_`) — the
//! *filter* half. design.md's File Structure Plan names this exact file
//! (`tests/notification_filter_it.rs`, "ブロック/被ブロック/通知ミュート
//! （期限考慮）での生成抑制（統合）"). Requirements 7.1, 7.2, 7.3, 7.4. The
//! sibling `tests/notification_generation_it.rs` (this same task) covers
//! Requirements 5.1-5.3, 5.5, 6.1-6.6, 8.1, 8.2.
//!
//! ## What this file proves, and how
//! [`crate::notifications::filter::NotificationFilter::should_suppress`]
//! (task 2.3, already reviewed) is a thin consumer of social-graph's
//! `FilterQuery::blocked_set` — this file's own concern is proving that
//! consumption is genuinely reachable end to end through the real
//! generation point (task 2.4/3.1/4.2), against *real* block/mute rows
//! created through the real, already-mounted `POST /api/v1/accounts/{id}/
//! {block,mute}` endpoints (`src/social_graph/endpoints.rs`, social-graph
//! task 5.1/5.2) — not by re-deriving `NotificationFilter`'s own already-
//! unit-tested suppression logic in isolation. Every scenario below: (1)
//! establishes a real block/mute relationship through the real HTTP
//! surface, (2) emits a `Follow` `NotificationEvent` directly through the
//! real single generation point (`app.state.notifications().ports().emit`,
//! kind chosen for its own `status_id: None` simplicity — `NotificationFilter`
//! does not vary its decision by kind), and (3) asserts, via a direct read
//! of the `notifications` table (mirrors `tests/
//! notification_generation_it.rs`'s own identical `dedup_key_row_ids`
//! convention, itself mirroring `tests/mute_block_it.rs`'s own direct-row-
//! count convention), whether a notification was persisted.
//!
//! ## Deterministic expiry, without waiting on real time
//! `spawn_test_app` always injects a fixed, non-advancing clock
//! (`RuntimeContext::deterministic`'s `FixedClock`) — mirrors `tests/
//! mute_block_it.rs`'s own doc comment ("Testing '期限後の解除扱い' with a
//! *deterministic* clock") exactly: `MuteService::mute` resolves a
//! *relative* `duration` (seconds) into an *absolute* `expires_at = now +
//! duration` at mute time, so a **negative** `duration` deterministically
//! produces an `expires_at` already in the past relative to this same
//! fixed `now` the instant the mute is recorded. `social_graph::FilterQuery::
//! blocked_set`'s own `muted_notifications` set already excludes any such
//! expired row (Requirement 7.3, social-graph's own already-reviewed
//! expiry-aware query) — this file's own job is proving that exclusion is
//! genuinely reachable through `NotificationFilter::should_suppress` and
//! therefore through the real notification generation pipeline, not
//! re-deriving the underlying expiry-filter proof itself (already covered
//! by `tests/mute_block_it.rs`'s own `mute_with_an_already_elapsed_
//! duration_is_recorded_but_excluded_from_muting`).
//!
//! ## Plain `muted` vs `muted_notifications` (Requirement 7.2's explicit
//! scope)
//! Requirement 7.2's own wording restricts suppression to the notification-
//! mute flag specifically (`muting_notifications`), not plain mute alone —
//! `src/notifications/filter.rs`'s own doc comment states this explicitly
//! ("`sets.muted`... is deliberately never consulted"). This file proves
//! both directions of that claim are actually observable end to end: a
//! `{"notifications": true}` mute suppresses (this file's scenario 3), while
//! a `{"notifications": false}` mute — `muting=true` but
//! `muting_notifications=false`, per `POST .../mute`'s own real Mastodon-
//! compatible default-vs-explicit-`false` behavior (`tests/
//! mute_block_it.rs::mute_records_muting_true_and_reflects_notifications_flag`) —
//! does not (this file's scenario 5), proving the filter does not
//! over-suppress on plain mute alone.
//!
//! ## Sandbox DB availability
//! This sandbox has no reachable PostgreSQL (`pg_isready` confirms no
//! response — the same constraint every earlier task in this spec's own
//! `tasks.md` "## Implementation Notes" documents), so none of the
//! `#[tokio::test]`s below could be executed to completion here. Every test
//! is written as a real, executable integration test against
//! `crate::test_harness::spawn_test_app` and the real production router/
//! generation point; this file compiles cleanly (`cargo check --tests` /
//! `cargo test --no-run`) and every assertion was verified by direct
//! reading of the real collaborators it calls (`src/notifications/
//! filter.rs`, `src/notifications/generator.rs`, `src/social_graph/
//! providers.rs`, `src/social_graph/endpoints.rs`, `src/social_graph/
//! mute_service.rs`), mirroring `tests/notification_generation_it.rs`'s own
//! identical resolution to the identical constraint.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor, ResolvedActor};
use kawasemi::domain::{AccountRef, Id};
use kawasemi::notifications::{NotificationEvent, NotificationType};
use kawasemi::oauth::app_repository::{self, NewApp};
use kawasemi::oauth::model::ScopeSet as ModelScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::server;
use kawasemi::test_harness::{TestApp, spawn_test_app};

// ---- Fixture plumbing (duplicated per this crate's own established
// convention — mirrors `tests/notification_generation_it.rs`'s/`tests/
// mute_block_it.rs`'s own identical helpers). --------------------------

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
            display_name: format!("Notification Filter IT {handle_str}"),
            summary: "an actor used by the notification_filter_it integration test".to_string(),
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
            name: "Notification Filter IT Client".to_string(),
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

/// `POST /api/v1/accounts/{id}/block` (real, already-mounted social-graph
/// endpoint) on `actor_token`'s own behalf, targeting `target_id`.
async fn block(router: &Router, actor_token: &str, target_id: Id) {
    let (status, body) = send(
        router,
        req(
            "POST",
            &format!("/api/v1/accounts/{}/block", target_id.as_i64()),
            Some(actor_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "block must succeed: {body:?}");
}

/// `POST /api/v1/accounts/{id}/mute` (real, already-mounted social-graph
/// endpoint) on `actor_token`'s own behalf, targeting `target_id`, with an
/// explicit `{"notifications": ..., "duration": ...}` body — mirrors
/// `tests/mute_block_it.rs`'s own established request shape.
async fn mute(
    router: &Router,
    actor_token: &str,
    target_id: Id,
    notifications: bool,
    duration_seconds: Option<i64>,
) -> Value {
    let mut body = json!({"notifications": notifications});
    if let Some(duration) = duration_seconds {
        body["duration"] = json!(duration);
    }
    let (status, resp) = send(
        router,
        req(
            "POST",
            &format!("/api/v1/accounts/{}/mute", target_id.as_i64()),
            Some(actor_token),
            Some(body),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "mute must succeed: {resp:?}");
    resp
}

/// Emits a kind-agnostic `Follow` `NotificationEvent` directly through the
/// real, already-wired single generation point
/// (`app.state.notifications().ports().emit(..)`, task 4.2) —
/// `NotificationFilter::should_suppress` does not vary its decision by
/// kind (`src/notifications/filter.rs`'s own doc comment), so `Follow`
/// (no `status_id` needed) keeps every scenario below minimal. `emit`
/// itself always returns `Ok(())` regardless of whether the event was
/// suppressed (a silent skip, not an error — `src/notifications/
/// generator.rs`'s own doc comment).
async fn emit_follow_event(app: &TestApp, recipient: Id, origin: AccountRef) {
    let now = app.runtime.clock.now();
    app.state
        .notifications()
        .ports()
        .emit(NotificationEvent {
            recipient: AccountRef::Local(recipient),
            origin,
            kind: NotificationType::Follow,
            target_status_id: None,
            occurred_at: now,
        })
        .await
        .expect("emit must succeed regardless of whether the event is ultimately suppressed");
}

/// `true` iff a `follow`-kind notification exists for `recipient` from
/// `origin` — a direct read of the `notifications` table (mirrors `tests/
/// notification_generation_it.rs::dedup_key_row_ids`/`tests/
/// mute_block_it.rs`'s own direct-row-count convention), since this file's
/// scenarios never call `GET /api/v1/notifications` (no `read:notifications`
/// token is needed for any assertion here).
async fn follow_notification_exists(app: &TestApp, recipient: Id, origin: AccountRef) -> bool {
    let (origin_kind, origin_id) = match origin {
        AccountRef::Local(id) => ("local", id.as_i64()),
        AccountRef::Remote(id) => ("remote", id.as_i64()),
    };
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM notifications \
         WHERE recipient_id = $1 AND kind = 'follow' AND origin_kind = $2 AND origin_id = $3",
    )
    .bind(recipient.as_i64())
    .bind(origin_kind)
    .bind(origin_id)
    .fetch_one(&app.pool)
    .await
    .expect("counting notifications rows must succeed");
    count > 0
}

// ==========================================================================
// (1) Recipient blocks origin -> suppressed (Requirement 7.1).
// ==========================================================================

#[tokio::test]
async fn recipient_blocking_origin_suppresses_generation() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let app_id = register_test_app(&app).await;

    let recipient = insert_actor_fixture(&app, "notif_filter_block_recipient").await;
    let origin = insert_actor_fixture(&app, "notif_filter_block_origin").await;
    let recipient_token = issue_test_token(&app, app_id, recipient.id, &["follow"]).await;

    block(&router, &recipient_token, origin.id).await;
    emit_follow_event(&app, recipient.id, AccountRef::Local(origin.id)).await;

    assert!(
        !follow_notification_exists(&app, recipient.id, AccountRef::Local(origin.id)).await,
        "an event from an origin the recipient has blocked must not produce a notification \
         (Requirement 7.1)"
    );

    app.cleanup().await;
}

// ==========================================================================
// (2) Origin blocks recipient -> suppressed via `blocked_by` (Requirement
// 7.1's mirror direction).
// ==========================================================================

#[tokio::test]
async fn origin_blocking_recipient_suppresses_generation_via_blocked_by() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let app_id = register_test_app(&app).await;

    let recipient = insert_actor_fixture(&app, "notif_filter_blockedby_recipient").await;
    let origin = insert_actor_fixture(&app, "notif_filter_blockedby_origin").await;
    let origin_token = issue_test_token(&app, app_id, origin.id, &["follow"]).await;

    // The *origin* blocks the recipient — the mirror direction of scenario
    // (1) above.
    block(&router, &origin_token, recipient.id).await;
    emit_follow_event(&app, recipient.id, AccountRef::Local(origin.id)).await;

    assert!(
        !follow_notification_exists(&app, recipient.id, AccountRef::Local(origin.id)).await,
        "an event from an origin that has blocked the recipient must not produce a notification \
         (Requirement 7.1's blocked_by direction)"
    );

    app.cleanup().await;
}

// ==========================================================================
// (3) Notification-muted origin -> suppressed (Requirement 7.2).
// ==========================================================================

#[tokio::test]
async fn notification_muted_origin_suppresses_generation() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let app_id = register_test_app(&app).await;

    let recipient = insert_actor_fixture(&app, "notif_filter_mutenotif_recipient").await;
    let origin = insert_actor_fixture(&app, "notif_filter_mutenotif_origin").await;
    let recipient_token = issue_test_token(&app, app_id, recipient.id, &["follow"]).await;

    let relationship = mute(&router, &recipient_token, origin.id, true, None).await;
    assert_eq!(
        relationship["muting_notifications"].as_bool(),
        Some(true),
        "the mute fixture itself must carry muting_notifications=true: {relationship:?}"
    );

    emit_follow_event(&app, recipient.id, AccountRef::Local(origin.id)).await;

    assert!(
        !follow_notification_exists(&app, recipient.id, AccountRef::Local(origin.id)).await,
        "an event from a notification-muted origin must not produce a notification \
         (Requirement 7.2)"
    );

    app.cleanup().await;
}

// ==========================================================================
// (4) An EXPIRED notification-mute does NOT suppress (Requirement 7.3).
// ==========================================================================

#[tokio::test]
async fn an_expired_notification_mute_does_not_suppress_generation() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let app_id = register_test_app(&app).await;

    let recipient = insert_actor_fixture(&app, "notif_filter_expired_recipient").await;
    let origin = insert_actor_fixture(&app, "notif_filter_expired_origin").await;
    let recipient_token = issue_test_token(&app, app_id, recipient.id, &["follow"]).await;

    // A negative duration deterministically produces an `expires_at`
    // already in the past relative to this instance's fixed clock the
    // instant the mute is recorded — see this file's own doc comment,
    // "Deterministic expiry, without waiting on real time".
    let relationship = mute(&router, &recipient_token, origin.id, true, Some(-3600)).await;
    assert_eq!(
        relationship["muting_notifications"].as_bool(),
        Some(false),
        "the mute row's own *derived* Relationship flag must already read as expired at mute \
         time: {relationship:?}"
    );

    emit_follow_event(&app, recipient.id, AccountRef::Local(origin.id)).await;

    assert!(
        follow_notification_exists(&app, recipient.id, AccountRef::Local(origin.id)).await,
        "an expired notification-mute must not be treated as active for suppression purposes \
         (Requirement 7.3) — the notification must still be generated"
    );

    app.cleanup().await;
}

// ==========================================================================
// (5) A plain (non-notification) mute alone does NOT suppress (Requirement
// 7.2's explicit `muted_notifications`-only scope — see this file's own doc
// comment).
// ==========================================================================

#[tokio::test]
async fn a_plain_non_notification_mute_alone_does_not_suppress_generation() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let app_id = register_test_app(&app).await;

    let recipient = insert_actor_fixture(&app, "notif_filter_plainmute_recipient").await;
    let origin = insert_actor_fixture(&app, "notif_filter_plainmute_origin").await;
    let recipient_token = issue_test_token(&app, app_id, recipient.id, &["follow"]).await;

    let relationship = mute(&router, &recipient_token, origin.id, false, None).await;
    assert_eq!(
        relationship["muting"].as_bool(),
        Some(true),
        "the mute fixture itself must be a genuine (plain) mute: {relationship:?}"
    );
    assert_eq!(
        relationship["muting_notifications"].as_bool(),
        Some(false),
        "the mute fixture must explicitly NOT be a notification-mute: {relationship:?}"
    );

    emit_follow_event(&app, recipient.id, AccountRef::Local(origin.id)).await;

    assert!(
        follow_notification_exists(&app, recipient.id, AccountRef::Local(origin.id)).await,
        "a plain mute (without muting_notifications) must never suppress notification \
         generation — only muted_notifications does (Requirement 7.2); the filter must not \
         over-suppress"
    );

    app.cleanup().await;
}
