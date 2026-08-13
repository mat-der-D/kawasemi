//! Integration tests for notifications task 5.3 (`.kiro/specs/notifications/
//! tasks.md`, "5.3 (P) 生成・フィルタの統合テスト", `_Depends: 4.2_`) — the
//! *generation* half. design.md's File Structure Plan names this exact file
//! (`tests/notification_generation_it.rs`, "種別ごと生成・受信者ローカル限
//! 定・重複排除・配信シーム引き渡し（統合）"). Requirements 5.1, 5.2, 5.3,
//! 5.5, 6.1-6.6, 8.1, 8.2. The sibling `tests/notification_filter_it.rs`
//! (this same task) covers Requirements 7.1-7.4 (block/mute suppression).
//!
//! ## Relationship to the already-committed sibling test files
//! `tests/notification_contract_it.rs` (task 5.1) already exercises every
//! v1 kind's exact JSON *shape* against a golden; `tests/
//! notification_list_it.rs` (task 5.2) already exercises list *retrieval*
//! semantics (ownership, pagination, filters, dismissed-exclusion). This
//! file's own concern — distinct from both — is
//! [`crate::notifications::generator::NotificationGenerator`]'s own
//! sequencing contract: exactly-once persistence per kind, the non-local
//! short-circuit, dedup idempotency, the dismiss->re-trigger asymmetry, and
//! the delivery-sink hand-off condition (new-only). It reuses the fixture/
//! HTTP plumbing both siblings already established (`insert_actor_fixture`,
//! `register_test_app`, `issue_test_token`, `real_router`, `req`/`send`,
//! the `follow`/`mention`/`favourite`/`follow_request`/`poll` trigger
//! helpers) rather than inventing new conventions — each `tests/*.rs` file
//! is its own compiled crate, so this deliberately duplicates rather than
//! imports them (this crate's own documented convention, restated by every
//! sibling file's own doc comment).
//!
//! ## Driving events: real upstream HTTP where an emitter exists, direct
//! `ports().emit()` where none does yet
//! `favourite`/`reblog`/`mention`/`follow` are driven through the real,
//! already-wired production HTTP surface (`POST /api/v1/statuses/{id}/
//! favourite`/`reblog`, `POST /api/v1/statuses` with a mention, `POST
//! /api/v1/accounts/{id}/follow`). `follow_request` is driven through
//! `crate::social_graph::Transitions::record_pending` directly — mirroring
//! `tests/notification_contract_it.rs`'s/`tests/notification_list_it.rs`'s
//! own identical, already-reviewed convention (see either file's own doc
//! comment, "follow_request", for why the real single generation point is
//! driven directly rather than the full signed-federation inbound
//! pipeline). `poll` (and, for the dedup/dismiss/delivery-sink scenarios
//! that need a kind-agnostic event, `follow`/`favourite` constructed by
//! hand) are driven through `app.state.notifications().ports().emit(..)` —
//! the real, already-wired single generation point (task 4.2), the same
//! substitute `tests/notification_contract_it.rs`'s own doc comment
//! ("poll/status/update") documents for the identical situation (no
//! production poll-close emitter exists in this codebase yet).
//!
//! ## Observing "no notification was created" and the dismiss/re-trigger
//! asymmetry: direct SQL, not the HTTP list endpoint
//! `GET /api/v1/notifications` (Requirement 2.4) *excludes* dismissed rows
//! by design — so it cannot by itself prove "the original dismissed row
//! still exists, now carrying a second, distinct id" (Requirement 8.1's
//! "取り消し→再実行"), and a non-local recipient has no token/actor
//! context to authenticate a list request as at all. This file therefore
//! reads the `notifications` table directly for exactly those two classes
//! of assertion (mirrors `tests/mute_block_it.rs`'s own established
//! `mutes_row_count`/`blocks_row_count` direct-row-count convention for the
//! analogous "assert on persisted state the HTTP surface doesn't expose"
//! situation), using the exact dedup-key column shape
//! `src/notifications/repository.rs`'s own `insert_dedup` documents
//! (`recipient_id`, `kind`, `origin_kind`, `origin_id`,
//! `COALESCE(status_id, 0)`).
//!
//! ## Delivery-sink hand-off: a real `NotificationDeliverySink`, installed
//! into the live registry
//! `crate::notifications::ports::NotificationPortsRegistry::set_delivery_sink`
//! (task 2.2/4.2) is the real, already-wired swap-in point a future
//! streaming/web-push spec's own bootstrap would use — this file installs a
//! recording test double there via `app.state.notifications().ports()`
//! (the exact same registry instance `NotificationGenerator`'s own
//! `DeliverySinkBridge` reads from, per `src/notifications.rs`'s own doc
//! comment on that bridge) to observe whether/how-many-times `deliver` was
//! actually called, mirroring `src/notifications/ports.rs`'s own
//! `RecordingDeliverySink` unit-test precedent for the identical technique.
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
//! generator.rs`, `src/notifications/repository.rs`, `src/notifications/
//! ports.rs`, `src/notifications/event_sink.rs`), mirroring `tests/
//! notification_contract_it.rs`'s/`tests/notification_list_it.rs`'s own
//! identical resolution to the identical constraint.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

use kawasemi::accounts::model::{ProfileField, RemoteAccount};
use kawasemi::accounts::remote_repository::upsert_remote;
use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor, ResolvedActor};
use kawasemi::domain::{AccountRef, Id};
use kawasemi::error::AppError;
use kawasemi::notifications::{
    Notification, NotificationDeliverySink, NotificationEvent, NotificationType,
};
use kawasemi::oauth::app_repository::{self, NewApp};
use kawasemi::oauth::model::ScopeSet as ModelScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::server;
use kawasemi::social_graph::{FollowRequest, FollowRequestDirection, Transitions};
use kawasemi::test_harness::{TestApp, spawn_test_app};

// ---- Fixture plumbing (duplicated per this crate's own established
// "each tests/*.rs file is its own compiled crate" convention — mirrors
// `tests/notification_contract_it.rs`'s/`tests/notification_list_it.rs`'s
// own identical helpers). ------------------------------------------------

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
            display_name: format!("Notification Generation IT {handle_str}"),
            summary: "an actor used by the notification_generation_it integration test".to_string(),
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

fn sample_remote_account(
    id: Id,
    actor_uri: &str,
    fetched_at: time::OffsetDateTime,
) -> RemoteAccount {
    RemoteAccount {
        id,
        actor_uri: actor_uri.to_string(),
        username: "notif_gen_remote".to_string(),
        domain: "remote.notif-gen.example".to_string(),
        display_name: "Notification Generation IT Remote Actor".to_string(),
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

/// Seeds a known remote account row directly — mirrors `tests/
/// notification_contract_it.rs::insert_remote_actor_fixture`'s identical
/// convention.
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
            name: "Notification Generation IT Client".to_string(),
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

/// `GET /api/v1/notifications<query>`, asserting `200`, returning the
/// parsed item array — mirrors `tests/notification_list_it.rs::list_ok`.
async fn list_ok(router: &Router, token: &str, query: &str) -> Vec<Value> {
    let path = if query.is_empty() {
        "/api/v1/notifications".to_string()
    } else {
        format!("/api/v1/notifications{query}")
    };
    let (status, body) = send(router, req("GET", &path, Some(token), None)).await;
    assert_eq!(status, StatusCode::OK, "got: {body:?}");
    body.as_array()
        .expect("notification list must be a JSON array")
        .clone()
}

/// Drives a real follow: `follower` -> `target` (real `POST /api/v1/
/// accounts/{id}/follow`, Requirement 6.4). Mirrors `tests/
/// notification_list_it.rs::trigger_follow`.
async fn trigger_follow(router: &Router, follower_token: &str, target_id: Id) {
    let (status, body) = send(
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

/// Drives a real mention: `author` posts a status mentioning `mention_handle`
/// (real `POST /api/v1/statuses`, Requirement 6.3). Mirrors `tests/
/// notification_list_it.rs::trigger_mention`.
async fn trigger_mention(router: &Router, author_token: &str, mention_handle: &str) {
    create_status(
        router,
        author_token,
        json!({"status": format!("hey @{mention_handle}, from notification_generation_it")}),
    )
    .await;
}

/// Drives a real favourite: `favouriter` favourites `status_id` (real `POST
/// /api/v1/statuses/{id}/favourite`, Requirement 6.1).
async fn trigger_favourite(router: &Router, favouriter_token: &str, status_id: &str) {
    let (status, body) = send(
        router,
        req(
            "POST",
            &format!("/api/v1/statuses/{status_id}/favourite"),
            Some(favouriter_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "favourite must succeed: {body:?}");
}

/// Drives a real reblog: `rebloger` reblogs `status_id` (real `POST /api/v1/
/// statuses/{id}/reblog`, Requirement 6.2).
async fn trigger_reblog(router: &Router, rebloger_token: &str, status_id: &str) {
    let (status, body) = send(
        router,
        req(
            "POST",
            &format!("/api/v1/statuses/{status_id}/reblog"),
            Some(rebloger_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "reblog must succeed: {body:?}");
}

/// Drives a real, single generation-point `follow_request` for a *local*
/// recipient without needing the full signed-federation inbound pipeline —
/// mirrors `tests/notification_contract_it.rs`'s/`tests/
/// notification_list_it.rs`'s own identical, already-reviewed convention
/// (see either file's own doc comment, "follow_request").
async fn trigger_follow_request(app: &TestApp, requester_remote_id: Id, target_id: Id) {
    let transitions = Transitions::new(
        app.pool.clone(),
        app.runtime.clone(),
        app.state.statuses().notification_sink_registry(),
    );
    let now = app.runtime.clock.now();
    transitions
        .record_pending(&FollowRequest {
            requester: AccountRef::Remote(requester_remote_id),
            target: AccountRef::Local(target_id),
            direction: FollowRequestDirection::Inbound,
            activity_id: format!(
                "https://remote.notif-gen.example/activities/follow-request-{}",
                app.runtime.ids.next_id().as_i64()
            ),
            created_at: now,
        })
        .await
        .expect("record_pending must succeed and emit a real FollowRequest NotificationEvent");
}

/// Emits a kind-agnostic `Follow` `NotificationEvent` directly through the
/// real, already-wired single generation point (task 4.2's own
/// `app.state.notifications().ports().emit(..)` — the same substitute
/// `tests/notification_contract_it.rs`'s own doc comment documents for
/// kinds with no production emitter). Used by the dedup/dismiss/delivery-
/// sink scenarios below, which need precise, hand-controlled `recipient`/
/// `origin` values rather than whatever a real upstream action would
/// produce.
async fn emit_follow_event(app: &TestApp, recipient: Id, origin: Id) {
    let now = app.runtime.clock.now();
    app.state
        .notifications()
        .ports()
        .emit(NotificationEvent {
            recipient: AccountRef::Local(recipient),
            origin: AccountRef::Local(origin),
            kind: NotificationType::Follow,
            target_status_id: None,
            occurred_at: now,
        })
        .await
        .expect("emit must succeed and reach the real NotificationGenerator");
}

/// Emits a `Favourite` `NotificationEvent` for `status_id` directly through
/// the same single generation point [`emit_follow_event`] uses.
///
/// Needed because a favourite *cannot* be driven through `POST /api/v1/
/// statuses/{id}/favourite` once the recipient has blocked the origin: a
/// local favourite is turned into an Activity and processed through the same
/// inbound pipeline as a remote one (the "意味論は対称・物理配送のみ最適化"
/// invariant), so `InboundService::process_verified`'s own block judgment
/// rejects the request with `403` before any `NotificationEvent` is emitted
/// at all. `tests/notification_filter_it.rs` drives its own block scenarios
/// through this same seam for the same reason.
async fn emit_favourite_event(app: &TestApp, recipient: Id, origin: Id, status_id: Id) {
    let now = app.runtime.clock.now();
    app.state
        .notifications()
        .ports()
        .emit(NotificationEvent {
            recipient: AccountRef::Local(recipient),
            origin: AccountRef::Local(origin),
            kind: NotificationType::Favourite,
            target_status_id: Some(status_id),
            occurred_at: now,
        })
        .await
        .expect("emit must succeed regardless of whether the event is ultimately suppressed");
}

/// Reads `notifications.id` directly for every row matching the exact
/// dedup-key column shape `src/notifications/repository.rs::insert_dedup`'s
/// own `ON CONFLICT` target uses (`recipient_id`, `kind`, `origin_kind`,
/// `origin_id`, `COALESCE(status_id, 0)`), ordered oldest-first — see this
/// file's own doc comment ("Observing 'no notification was created'...").
/// `kind`'s string literal must match `repository.rs::kind_to_str`'s own
/// mapping exactly (verified by direct reading of that function).
async fn dedup_key_row_ids(
    app: &TestApp,
    recipient: Id,
    kind: &str,
    origin: &AccountRef,
    status_id: Option<Id>,
) -> Vec<i64> {
    let (origin_kind, origin_id) = match origin {
        AccountRef::Local(id) => ("local", id.as_i64()),
        AccountRef::Remote(id) => ("remote", id.as_i64()),
    };
    sqlx::query_scalar::<_, i64>(
        "SELECT id FROM notifications \
         WHERE recipient_id = $1 AND kind = $2 AND origin_kind = $3 AND origin_id = $4 \
           AND COALESCE(status_id, 0) = COALESCE($5, 0) \
         ORDER BY id",
    )
    .bind(recipient.as_i64())
    .bind(kind)
    .bind(origin_kind)
    .bind(origin_id)
    .bind(status_id.map(|id| id.as_i64()))
    .fetch_all(&app.pool)
    .await
    .expect("querying notifications by dedup key must succeed")
}

async fn is_dismissed(app: &TestApp, id: i64) -> bool {
    sqlx::query_scalar::<_, bool>("SELECT dismissed FROM notifications WHERE id = $1")
        .bind(id)
        .fetch_one(&app.pool)
        .await
        .expect("fetching the dismissed flag must succeed")
}

/// Total row count across the whole (per-test, isolated-schema)
/// `notifications` table — used only by the non-local-recipient scenario,
/// where nothing else in that same test ever persists a notification, so
/// "zero total rows" is an unambiguous proof of "nothing was persisted"
/// (mirrors `tests/mute_block_it.rs`'s own direct-row-count convention).
async fn total_notifications_count(app: &TestApp) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM notifications")
        .fetch_one(&app.pool)
        .await
        .expect("counting notifications rows must succeed")
}

/// A test double for [`NotificationDeliverySink`] that records every
/// notification it was handed — mirrors `src/notifications/ports.rs`'s own
/// `RecordingDeliverySink` unit-test precedent exactly (this file's own
/// duplicate, since integration tests cannot reach that module-private
/// type).
struct RecordingDeliverySink {
    notifications: Mutex<Vec<Notification>>,
}

impl RecordingDeliverySink {
    fn new() -> Self {
        Self {
            notifications: Mutex::new(Vec::new()),
        }
    }

    fn count(&self) -> usize {
        self.notifications.lock().unwrap().len()
    }
}

impl NotificationDeliverySink for RecordingDeliverySink {
    fn deliver<'a>(
        &'a self,
        notification: &'a Notification,
    ) -> Pin<Box<dyn Future<Output = Result<(), AppError>> + Send + 'a>> {
        Box::pin(async move {
            self.notifications
                .lock()
                .unwrap()
                .push(notification.clone());
            Ok(())
        })
    }
}

// ==========================================================================
// (1) Per-kind generation: favourite + reblog (Requirements 6.1, 6.2).
// ==========================================================================

/// The post's author receives exactly one `favourite` notification (from
/// the favouriter) and exactly one `reblog` notification (from the
/// rebloger) — proving each kind's own event both produces the correct
/// notification kind and is attributed to the correct real actor.
#[tokio::test]
async fn favourite_and_reblog_events_notify_the_original_author_with_the_correct_kind() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let app_id = register_test_app(&app).await;

    let alice = insert_actor_fixture(&app, "notif_gen_favreblog_alice").await;
    let bob = insert_actor_fixture(&app, "notif_gen_favreblog_bob").await;
    let carol = insert_actor_fixture(&app, "notif_gen_favreblog_carol").await;
    let alice_token = issue_test_token(
        &app,
        app_id,
        alice.id,
        &["write:statuses", "read:notifications"],
    )
    .await;
    let bob_token = issue_test_token(&app, app_id, bob.id, &["write:favourites"]).await;
    let carol_token = issue_test_token(&app, app_id, carol.id, &["write:statuses"]).await;

    let post = create_status(
        &router,
        &alice_token,
        json!({"status": "fav+reblog generation check"}),
    )
    .await;
    let post_id = id_of(&post);

    trigger_favourite(&router, &bob_token, &post_id).await;
    trigger_reblog(&router, &carol_token, &post_id).await;

    let items = list_ok(&router, &alice_token, "").await;
    assert_eq!(
        items.len(),
        2,
        "alice must receive exactly one favourite and one reblog notification: {items:?}"
    );
    let favourite = items
        .iter()
        .find(|n| n["type"] == "favourite")
        .expect("a favourite notification must be present");
    assert_eq!(
        favourite["account"]["id"].as_str(),
        Some(bob.id.as_i64().to_string().as_str())
    );
    let reblog = items
        .iter()
        .find(|n| n["type"] == "reblog")
        .expect("a reblog notification must be present");
    assert_eq!(
        reblog["account"]["id"].as_str(),
        Some(carol.id.as_i64().to_string().as_str())
    );

    app.cleanup().await;
}

// ==========================================================================
// (2) Per-kind generation: mention + follow (Requirements 6.3, 6.4).
// ==========================================================================

/// The mentioned actor receives exactly one `mention` notification and the
/// followed actor receives exactly one `follow` notification, each
/// attributed to the correct real actor — two independent recipients, so
/// neither list can leak the other's notification.
#[tokio::test]
async fn mention_and_follow_events_notify_the_correct_recipient_with_the_correct_kind() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let app_id = register_test_app(&app).await;

    let alice = insert_actor_fixture(&app, "notif_gen_mentfollow_alice").await;
    let bob = insert_actor_fixture(&app, "notif_gen_mentfollow_bob").await;
    let carol = insert_actor_fixture(&app, "notif_gen_mentfollow_carol").await;
    let dave = insert_actor_fixture(&app, "notif_gen_mentfollow_dave").await;
    let alice_token = issue_test_token(&app, app_id, alice.id, &["write:statuses"]).await;
    let bob_token = issue_test_token(&app, app_id, bob.id, &["read:notifications"]).await;
    let carol_token = issue_test_token(&app, app_id, carol.id, &["follow"]).await;
    let dave_token = issue_test_token(&app, app_id, dave.id, &["read:notifications"]).await;

    trigger_mention(&router, &alice_token, "notif_gen_mentfollow_bob").await;
    trigger_follow(&router, &carol_token, dave.id).await;

    let bob_items = list_ok(&router, &bob_token, "").await;
    assert_eq!(
        bob_items.len(),
        1,
        "bob must receive exactly one mention notification: {bob_items:?}"
    );
    assert_eq!(bob_items[0]["type"], "mention");
    assert_eq!(
        bob_items[0]["account"]["id"].as_str(),
        Some(alice.id.as_i64().to_string().as_str())
    );

    let dave_items = list_ok(&router, &dave_token, "").await;
    assert_eq!(
        dave_items.len(),
        1,
        "dave must receive exactly one follow notification: {dave_items:?}"
    );
    assert_eq!(dave_items[0]["type"], "follow");
    assert_eq!(
        dave_items[0]["account"]["id"].as_str(),
        Some(carol.id.as_i64().to_string().as_str())
    );

    app.cleanup().await;
}

// ==========================================================================
// (3) Per-kind generation: follow_request + poll (Requirements 6.5, 6.6) —
// see this file's own doc comment for why each is driven the way it is
// (no full signed-federation inbound pipeline / no production poll-close
// emitter exists in this codebase yet).
// ==========================================================================

#[tokio::test]
async fn follow_request_and_poll_events_notify_the_correct_recipient_with_the_correct_kind() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let app_id = register_test_app(&app).await;

    let target = insert_actor_fixture(&app, "notif_gen_frpoll_target").await;
    let alice = insert_actor_fixture(&app, "notif_gen_frpoll_alice").await;
    let target_token = issue_test_token(&app, app_id, target.id, &["read:notifications"]).await;
    let alice_token = issue_test_token(
        &app,
        app_id,
        alice.id,
        &["write:statuses", "read:notifications"],
    )
    .await;

    let remote_id = insert_remote_actor_fixture(
        &app,
        "https://remote.notif-gen.example/actors/notif_gen_frpoll_requester",
    )
    .await;
    trigger_follow_request(&app, remote_id, target.id).await;

    let target_items = list_ok(&router, &target_token, "").await;
    assert_eq!(
        target_items.len(),
        1,
        "target must receive exactly one follow_request notification: {target_items:?}"
    );
    assert_eq!(target_items[0]["type"], "follow_request");
    assert_eq!(
        target_items[0]["account"]["id"].as_str(),
        Some(remote_id.as_i64().to_string().as_str())
    );

    let created = create_status(
        &router,
        &alice_token,
        json!({
            "status": "gen poll check",
            "poll": {"options": ["Cats", "Dogs"], "multiple": false}
        }),
    )
    .await;
    let status_id = id_of(&created);
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

    let alice_items = list_ok(&router, &alice_token, "").await;
    assert_eq!(
        alice_items.len(),
        1,
        "alice must receive exactly one poll notification: {alice_items:?}"
    );
    assert_eq!(alice_items[0]["type"], "poll");
    assert_eq!(
        alice_items[0]["account"]["id"].as_str(),
        Some(alice.id.as_i64().to_string().as_str())
    );

    app.cleanup().await;
}

// ==========================================================================
// (4) Non-local recipient: no persistence, no delivery (Requirement 5.3).
// ==========================================================================

/// An event whose recipient is not a local actor never reaches the filter,
/// the repository, or the delivery sink (design.md's sequence diagram: "alt
/// recipient not local -> skip no notification") — proven here by a
/// synthetic remote recipient id that was never even inserted into
/// `remote_accounts` (the non-local check runs before any DB lookup at
/// all, per `src/notifications/generator.rs`'s own doc comment, "Control
/// flow", step 1), and by a recording delivery sink that observes zero
/// calls.
#[tokio::test]
async fn non_local_recipient_event_produces_no_persisted_notification_and_is_never_delivered() {
    let app = spawn_test_app().await;

    let sink = Arc::new(RecordingDeliverySink::new());
    app.state
        .notifications()
        .ports()
        .set_delivery_sink(Arc::clone(&sink) as Arc<dyn NotificationDeliverySink>);

    let remote_recipient_id = app.runtime.ids.next_id();
    let local_origin_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    app.state
        .notifications()
        .ports()
        .emit(NotificationEvent {
            recipient: AccountRef::Remote(remote_recipient_id),
            origin: AccountRef::Local(local_origin_id),
            kind: NotificationType::Favourite,
            target_status_id: None,
            occurred_at: now,
        })
        .await
        .expect("emit itself must still succeed (Requirement 5.3 is a silent skip, not an error)");

    assert_eq!(
        total_notifications_count(&app).await,
        0,
        "a non-local recipient's event must never persist a notification (Requirement 5.3)"
    );
    assert_eq!(
        sink.count(),
        0,
        "a non-local recipient's event must never reach the delivery sink (Requirement 5.5)"
    );

    app.cleanup().await;
}

// ==========================================================================
// (5) Duplicate while undismissed is suppressed (Requirements 8.1, 8.2,
// 5.5) — contrasted directly against (6) immediately below, the identical
// dedup key's *opposite* outcome once dismissed.
// ==========================================================================

/// Resending the identical event (same recipient/kind/origin/status) while
/// the first notification remains undismissed must not create a second row
/// (Requirement 8.1, 8.2's own idempotency), and the delivery sink must be
/// handed the notification exactly once — never once per resend.
#[tokio::test]
async fn duplicate_event_while_undismissed_is_suppressed_and_not_delivered_twice() {
    let app = spawn_test_app().await;

    let sink = Arc::new(RecordingDeliverySink::new());
    app.state
        .notifications()
        .ports()
        .set_delivery_sink(Arc::clone(&sink) as Arc<dyn NotificationDeliverySink>);

    // No `register_test_app`/token issuance needed here: this scenario's
    // only assertions are the direct-SQL dedup-key row count and the
    // recording delivery sink's own call count, neither of which goes
    // through the HTTP surface.
    let bob = insert_actor_fixture(&app, "notif_gen_dup_bob").await;
    let origin_id = app.runtime.ids.next_id();

    emit_follow_event(&app, bob.id, origin_id).await;
    emit_follow_event(&app, bob.id, origin_id).await;

    let ids = dedup_key_row_ids(&app, bob.id, "follow", &AccountRef::Local(origin_id), None).await;
    assert_eq!(
        ids.len(),
        1,
        "an identical event resent while the first notification remains undismissed must not \
         create a second row: {ids:?}"
    );
    assert_eq!(
        sink.count(),
        1,
        "a duplicate (suppressed) resend must never be handed to the delivery sink a second time"
    );

    app.cleanup().await;
}

// ==========================================================================
// (6) Dismiss -> re-trigger produces a NEW notification (Requirement 8.1's
// "取り消し→再実行") — the identical dedup key as (5) above, but the
// *opposite* outcome once the first row is dismissed.
// ==========================================================================

/// Once the first notification for a given dedup key is dismissed, an
/// identical event resent afterward creates a genuinely new, distinct
/// notification — the original row remains (now dismissed), and the second
/// row starts undismissed. This is the direct contrast to (5) above: same
/// dedup key, opposite outcome, depending entirely on the first row's
/// `dismissed` state (design.md: the dedup partial index is
/// `WHERE NOT dismissed`).
#[tokio::test]
async fn dismiss_then_resend_the_identical_event_creates_a_new_notification() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let app_id = register_test_app(&app).await;

    let bob = insert_actor_fixture(&app, "notif_gen_redismiss_bob").await;
    let bob_token = issue_test_token(&app, app_id, bob.id, &["write:notifications"]).await;
    let origin_id = app.runtime.ids.next_id();

    emit_follow_event(&app, bob.id, origin_id).await;
    let ids_before =
        dedup_key_row_ids(&app, bob.id, "follow", &AccountRef::Local(origin_id), None).await;
    assert_eq!(
        ids_before.len(),
        1,
        "the first event must create exactly one notification: {ids_before:?}"
    );
    let first_id = ids_before[0];

    let (dismiss_status, dismiss_body) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/notifications/{first_id}/dismiss"),
            Some(&bob_token),
            None,
        ),
    )
    .await;
    assert_eq!(
        dismiss_status,
        StatusCode::OK,
        "dismiss must succeed: {dismiss_body:?}"
    );

    emit_follow_event(&app, bob.id, origin_id).await;
    let ids_after =
        dedup_key_row_ids(&app, bob.id, "follow", &AccountRef::Local(origin_id), None).await;
    assert_eq!(
        ids_after.len(),
        2,
        "dismissing the original notification and resending the identical event must create a \
         new, distinct notification (Requirement 8.1's 取り消し→再実行): {ids_after:?}"
    );
    assert_eq!(
        ids_after[0], first_id,
        "the original (now-dismissed) row must still be present, not replaced"
    );
    let second_id = ids_after[1];
    assert_ne!(
        second_id, first_id,
        "the new notification must have a distinct id from the original"
    );
    assert!(
        is_dismissed(&app, first_id).await,
        "the original row must remain dismissed"
    );
    assert!(
        !is_dismissed(&app, second_id).await,
        "the newly created notification must start undismissed"
    );

    app.cleanup().await;
}

// ==========================================================================
// (7) Delivery-sink hand-off: exactly once for a newly-created notification,
// never for a suppressed (blocked) event (Requirements 5.5, 7.1).
// ==========================================================================

/// A newly-created notification is handed to the configured delivery sink
/// exactly once (a real favourite through the real pipeline); a suppressed
/// event (the recipient has blocked the origin — `NotificationFilter`,
/// task 2.3) never reaches the delivery sink at all, and never persists a
/// notification either. `tests/notification_filter_it.rs` (this same task)
/// owns the full block/blocked-by/notification-mute/expiry combinatorial
/// coverage (Requirements 7.1-7.4); this test's own concern is narrower:
/// proving the delivery-sink hand-off condition itself (new-only) holds
/// even when the "not new" reason is suppression rather than duplication.
///
/// The two halves deliberately use different drivers: the created half runs
/// through the real `POST /api/v1/statuses/{id}/favourite`, while the
/// suppressed half emits its event through
/// [`emit_favourite_event`] — see that helper's own doc comment for why the
/// REST surface cannot express "blocked origin favourites" at all.
#[tokio::test]
async fn delivery_sink_is_called_exactly_once_per_created_notification_and_never_for_a_suppressed_event()
 {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let app_id = register_test_app(&app).await;

    let sink = Arc::new(RecordingDeliverySink::new());
    app.state
        .notifications()
        .ports()
        .set_delivery_sink(Arc::clone(&sink) as Arc<dyn NotificationDeliverySink>);

    let alice = insert_actor_fixture(&app, "notif_gen_delivery_alice").await;
    let bob = insert_actor_fixture(&app, "notif_gen_delivery_bob").await;
    let carol = insert_actor_fixture(&app, "notif_gen_delivery_carol").await;
    let alice_token = issue_test_token(
        &app,
        app_id,
        alice.id,
        &["write:statuses", "follow", "read:notifications"],
    )
    .await;
    let bob_token = issue_test_token(&app, app_id, bob.id, &["write:favourites"]).await;

    // Created: bob favourites alice's post -> handed to the sink exactly
    // once.
    let post1 = create_status(
        &router,
        &alice_token,
        json!({"status": "delivery sink check"}),
    )
    .await;
    let post1_id = id_of(&post1);
    trigger_favourite(&router, &bob_token, &post1_id).await;
    assert_eq!(
        sink.count(),
        1,
        "a newly created notification must be handed to the delivery sink exactly once"
    );

    // Suppressed: alice blocks carol, then carol favourites a second alice
    // post -> the event must be suppressed before ever reaching the
    // repository or the delivery sink.
    let post2 = create_status(
        &router,
        &alice_token,
        json!({"status": "delivery sink check 2"}),
    )
    .await;
    let post2_id = id_of(&post2);
    let (block_status, block_body) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/accounts/{}/block", carol.id.as_i64()),
            Some(&alice_token),
            None,
        ),
    )
    .await;
    assert_eq!(
        block_status,
        StatusCode::OK,
        "block must succeed: {block_body:?}"
    );
    emit_favourite_event(&app, alice.id, carol.id, id_domain_of(&post2_id)).await;

    assert_eq!(
        sink.count(),
        1,
        "a suppressed (blocked) event must never be handed to the delivery sink"
    );
    let suppressed_ids = dedup_key_row_ids(
        &app,
        alice.id,
        "favourite",
        &AccountRef::Local(carol.id),
        Some(id_domain_of(&post2_id)),
    )
    .await;
    assert!(
        suppressed_ids.is_empty(),
        "a blocked origin's favourite must never persist a notification: {suppressed_ids:?}"
    );

    app.cleanup().await;
}
