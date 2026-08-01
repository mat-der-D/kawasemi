//! DB-backed tests for `NotificationService` (Requirements 2.1, 2.2, 2.3,
//! 2.4, 3.1, 3.2, 4.1, 4.2, 4.3, 4.4), per task 3.2's own completion
//! condition: "一覧が受信者宛のみをフィルタ適用して返し、単一取得が他者宛で
//! 未検出、dismiss/clear 後に取得から除外される状態".
//!
//! ## Sandbox DB availability (see this task's own status report)
//! Every one of `NotificationService`'s four methods calls straight into
//! `NotificationRepository` (task 1.3), which issues real SQL — there is no
//! collaborator-order short-circuit analogous to `NotificationGenerator::
//! generate`'s recipient-local check that could make any of these tests
//! genuinely DB-independent (mirrors `repository/tests.rs`'s own identical
//! situation: that module's own test suite is 100% DB-backed too, zero
//! `connect_lazy`-only tests, for the same underlying reason). This
//! sandbox — confirmed by every prior notifications task's own status report
//! and independently re-confirmed for this task (`pg_isready`/a raw TCP
//! probe against `127.0.0.1:5432` both refuse the connection) — has no
//! reachable Postgres, so every `#[tokio::test]` below is written as a real,
//! executable integration test against `crate::test_harness::spawn_test_app`
//! but could not be run to completion here; see this task's own status
//! report for the manual trace of each one.
//!
//! Mirrors `social_graph/follow_request_service/tests.rs`'s established
//! fixture conventions (`create_test_actor` is an exact copy of that
//! module's own helper of the same name) and `notifications/generator/
//! tests.rs`'s established `NotificationEvent`-adjacent fixture style for
//! seeding `Notification` rows directly via `repository::insert_dedup`
//! (this service never generates notifications itself — that is
//! `NotificationGenerator`'s boundary, task 2.4 — so tests seed rows
//! directly rather than routing through a generator this module does not
//! depend on).

use super::*;
use crate::actor::owner::create_owner;
use crate::actor::repository::insert_actor;
use crate::actor::{ActorState, ActorType, Handle};
use crate::api::pagination::PageParams;
use crate::domain::Visibility;
use crate::notifications::model::{Notification, NotificationType};
use crate::notifications::repository::insert_dedup;
use crate::statuses::status_repository::insert_status;
use crate::test_harness::{TestApp, spawn_test_app};

/// Creates a real owner + local actor row, returning the actor's `Id` — an
/// exact copy of `follow_request_service/tests.rs::create_test_actor`.
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

fn sample_status(id: Id, actor_id: Id, created_at: time::OffsetDateTime) -> Status {
    Status {
        id,
        actor_id,
        uri: format!("https://kawasemi.example/statuses/{}", id.as_i64()),
        url: None,
        content: "hello from a notification-embedded status".to_string(),
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
        created_at,
        edited_at: None,
    }
}

async fn create_test_status(app: &TestApp, actor_id: Id) -> Id {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    insert_status(&app.pool, &sample_status(id, actor_id, now))
        .await
        .expect("insert_status must succeed");
    id
}

#[allow(clippy::too_many_arguments)]
fn sample_notification(
    id: Id,
    recipient_id: Id,
    kind: NotificationType,
    origin: AccountRef,
    status_id: Option<Id>,
    created_at: time::OffsetDateTime,
) -> Notification {
    Notification {
        id,
        recipient_id,
        kind,
        origin,
        status_id,
        dismissed: false,
        created_at,
    }
}

async fn seed_notification(
    app: &TestApp,
    recipient_id: Id,
    kind: NotificationType,
    origin: AccountRef,
    status_id: Option<Id>,
) -> Id {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    let notification = sample_notification(id, recipient_id, kind, origin, status_id, now);
    insert_dedup(&app.pool, &notification)
        .await
        .expect("insert_dedup must succeed");
    id
}

fn build_service(app: &TestApp) -> NotificationService {
    NotificationService::new(
        app.pool.clone(),
        app.runtime.clone(),
        app.state.config().server.domain.clone(),
        app.state.accounts().service(),
        app.state.media().store().clone(),
    )
}

fn ctx_for(actor_id: Id) -> RequestActorContext {
    RequestActorContext {
        actor_id,
        scopes: crate::oauth::model::ScopeSet::default(),
    }
}

// -- list -------------------------------------------------------------------

/// Requirement 2.1: a recipient's list only ever contains their own
/// notifications, even when another recipient also has some.
#[tokio::test]
async fn list_returns_only_the_requesting_recipients_notifications() {
    let app = spawn_test_app().await;
    let service = build_service(&app);

    let recipient = create_test_actor(&app, "list-recipient").await;
    let other_recipient = create_test_actor(&app, "list-other-recipient").await;
    let origin = create_test_actor(&app, "list-origin").await;

    seed_notification(
        &app,
        recipient,
        NotificationType::Follow,
        AccountRef::Local(origin),
        None,
    )
    .await;
    seed_notification(
        &app,
        other_recipient,
        NotificationType::Follow,
        AccountRef::Local(origin),
        None,
    )
    .await;

    let page = service
        .list(
            &ctx_for(recipient),
            PageParams::default(),
            ListFilter::default(),
        )
        .await
        .expect("list must succeed");

    assert_eq!(
        page.items.len(),
        1,
        "only the requesting recipient's own notification"
    );
    assert_eq!(
        page.items[0].get("type").and_then(|v| v.as_str()),
        Some("follow")
    );
}

/// Requirement 2.4: a dismissed notification never appears in `list`.
#[tokio::test]
async fn list_excludes_dismissed_notifications() {
    let app = spawn_test_app().await;
    let service = build_service(&app);

    let recipient = create_test_actor(&app, "list-dismiss-recipient").await;
    let origin = create_test_actor(&app, "list-dismiss-origin").await;

    let notification_id = seed_notification(
        &app,
        recipient,
        NotificationType::Follow,
        AccountRef::Local(origin),
        None,
    )
    .await;

    let dismissed = repository::dismiss(&app.pool, notification_id, recipient)
        .await
        .expect("dismiss must succeed");
    assert!(dismissed);

    let page = service
        .list(
            &ctx_for(recipient),
            PageParams::default(),
            ListFilter::default(),
        )
        .await
        .expect("list must succeed");

    assert!(
        page.items.is_empty(),
        "a dismissed notification must not appear in list"
    );
}

/// Requirement 2.2: `types`/`exclude_types` narrow the result set.
#[tokio::test]
async fn list_applies_types_filter() {
    let app = spawn_test_app().await;
    let service = build_service(&app);

    let recipient = create_test_actor(&app, "list-types-recipient").await;
    let origin = create_test_actor(&app, "list-types-origin").await;

    seed_notification(
        &app,
        recipient,
        NotificationType::Follow,
        AccountRef::Local(origin),
        None,
    )
    .await;
    seed_notification(
        &app,
        recipient,
        NotificationType::FollowRequest,
        AccountRef::Local(origin),
        None,
    )
    .await;

    let filter = ListFilter {
        types: Some(vec![NotificationType::Follow]),
        ..ListFilter::default()
    };
    let page = service
        .list(&ctx_for(recipient), PageParams::default(), filter)
        .await
        .expect("list must succeed");

    assert_eq!(page.items.len(), 1);
    assert_eq!(
        page.items[0].get("type").and_then(|v| v.as_str()),
        Some("follow")
    );
}

/// Requirement 2.3: `account_id` (already-resolved `AccountRef`) narrows to
/// notifications from that origin only.
#[tokio::test]
async fn list_applies_account_id_filter() {
    let app = spawn_test_app().await;
    let service = build_service(&app);

    let recipient = create_test_actor(&app, "list-account-recipient").await;
    let origin_a = create_test_actor(&app, "list-account-origin-a").await;
    let origin_b = create_test_actor(&app, "list-account-origin-b").await;

    seed_notification(
        &app,
        recipient,
        NotificationType::Follow,
        AccountRef::Local(origin_a),
        None,
    )
    .await;
    seed_notification(
        &app,
        recipient,
        NotificationType::Follow,
        AccountRef::Local(origin_b),
        None,
    )
    .await;

    let filter = ListFilter {
        account_id: Some(AccountRef::Local(origin_a)),
        ..ListFilter::default()
    };
    let page = service
        .list(&ctx_for(recipient), PageParams::default(), filter)
        .await
        .expect("list must succeed");

    assert_eq!(page.items.len(), 1);
    assert_eq!(
        page.items[0]
            .get("account")
            .and_then(|a| a.get("id"))
            .and_then(|v| v.as_str()),
        Some(origin_a.as_i64().to_string()).as_deref()
    );
}

/// Requirement 1.2: a post-related notification embeds the related status,
/// rendered from the recipient's own viewpoint.
#[tokio::test]
async fn list_embeds_related_status_for_post_related_kinds() {
    let app = spawn_test_app().await;
    let service = build_service(&app);

    let recipient = create_test_actor(&app, "list-status-recipient").await;
    let origin = create_test_actor(&app, "list-status-origin").await;
    let status_id = create_test_status(&app, recipient).await;

    seed_notification(
        &app,
        recipient,
        NotificationType::Favourite,
        AccountRef::Local(origin),
        Some(status_id),
    )
    .await;

    let page = service
        .list(
            &ctx_for(recipient),
            PageParams::default(),
            ListFilter::default(),
        )
        .await
        .expect("list must succeed");

    assert_eq!(page.items.len(), 1);
    let status = page.items[0]
        .get("status")
        .expect("status field must be present");
    assert!(!status.is_null(), "favourite notifications embed a status");
    assert_eq!(
        status.get("id").and_then(|v| v.as_str()),
        Some(status_id.as_i64().to_string()).as_deref()
    );
}

/// Requirement 1.4: `follow`/`follow_request` never embed a status.
#[tokio::test]
async fn list_null_status_for_follow_kinds() {
    let app = spawn_test_app().await;
    let service = build_service(&app);

    let recipient = create_test_actor(&app, "list-follow-null-recipient").await;
    let origin = create_test_actor(&app, "list-follow-null-origin").await;

    seed_notification(
        &app,
        recipient,
        NotificationType::Follow,
        AccountRef::Local(origin),
        None,
    )
    .await;

    let page = service
        .list(
            &ctx_for(recipient),
            PageParams::default(),
            ListFilter::default(),
        )
        .await
        .expect("list must succeed");

    assert_eq!(page.items.len(), 1);
    assert!(
        page.items[0]
            .get("status")
            .expect("status key present")
            .is_null()
    );
}

// -- show ---------------------------------------------------------------------

/// Requirement 3.1: a recipient can fetch their own notification by id.
#[tokio::test]
async fn show_returns_the_notification_for_its_own_recipient() {
    let app = spawn_test_app().await;
    let service = build_service(&app);

    let recipient = create_test_actor(&app, "show-recipient").await;
    let origin = create_test_actor(&app, "show-origin").await;
    let notification_id = seed_notification(
        &app,
        recipient,
        NotificationType::Follow,
        AccountRef::Local(origin),
        None,
    )
    .await;

    let json = service
        .show(&ctx_for(recipient), notification_id)
        .await
        .expect("show must succeed for the owning recipient");

    assert_eq!(
        json.get("id").and_then(|v| v.as_str()),
        Some(notification_id.as_i64().to_string()).as_deref()
    );
}

/// Requirement 3.2: another recipient's notification 404s.
#[tokio::test]
async fn show_404_for_another_recipients_notification() {
    let app = spawn_test_app().await;
    let service = build_service(&app);

    let recipient = create_test_actor(&app, "show-other-recipient").await;
    let owner = create_test_actor(&app, "show-owner").await;
    let origin = create_test_actor(&app, "show-other-origin").await;
    let notification_id = seed_notification(
        &app,
        owner,
        NotificationType::Follow,
        AccountRef::Local(origin),
        None,
    )
    .await;

    let err = service
        .show(&ctx_for(recipient), notification_id)
        .await
        .expect_err("another recipient's notification must 404");
    assert_eq!(err.status, StatusCode::NOT_FOUND);
}

/// Requirement 3.2: a nonexistent id 404s.
#[tokio::test]
async fn show_404_for_a_nonexistent_notification() {
    let app = spawn_test_app().await;
    let service = build_service(&app);
    let recipient = create_test_actor(&app, "show-nonexistent-recipient").await;

    let err = service
        .show(&ctx_for(recipient), app.runtime.ids.next_id())
        .await
        .expect_err("a nonexistent notification must 404");
    assert_eq!(err.status, StatusCode::NOT_FOUND);
}

// -- dismiss / clear ------------------------------------------------------------

/// Requirements 4.2, 4.4: dismissing a notification removes it from
/// subsequent retrieval.
#[tokio::test]
async fn dismiss_excludes_from_subsequent_show_and_list() {
    let app = spawn_test_app().await;
    let service = build_service(&app);

    let recipient = create_test_actor(&app, "dismiss-recipient").await;
    let origin = create_test_actor(&app, "dismiss-origin").await;
    let notification_id = seed_notification(
        &app,
        recipient,
        NotificationType::Follow,
        AccountRef::Local(origin),
        None,
    )
    .await;

    service
        .dismiss(&ctx_for(recipient), notification_id)
        .await
        .expect("dismiss must succeed for the owning recipient");

    let show_err = service
        .show(&ctx_for(recipient), notification_id)
        .await
        .expect_err("a dismissed notification must no longer be retrievable");
    assert_eq!(show_err.status, StatusCode::NOT_FOUND);

    let page = service
        .list(
            &ctx_for(recipient),
            PageParams::default(),
            ListFilter::default(),
        )
        .await
        .expect("list must succeed");
    assert!(page.items.is_empty());
}

/// Requirement 4.3: dismissing another recipient's notification 404s.
#[tokio::test]
async fn dismiss_404_for_another_recipients_notification() {
    let app = spawn_test_app().await;
    let service = build_service(&app);

    let recipient = create_test_actor(&app, "dismiss-other-recipient").await;
    let owner = create_test_actor(&app, "dismiss-owner").await;
    let origin = create_test_actor(&app, "dismiss-other-origin").await;
    let notification_id = seed_notification(
        &app,
        owner,
        NotificationType::Follow,
        AccountRef::Local(origin),
        None,
    )
    .await;

    let err = service
        .dismiss(&ctx_for(recipient), notification_id)
        .await
        .expect_err("another recipient's notification must 404 on dismiss");
    assert_eq!(err.status, StatusCode::NOT_FOUND);
}

/// Requirement 4.3: dismissing a nonexistent id 404s.
#[tokio::test]
async fn dismiss_404_for_a_nonexistent_notification() {
    let app = spawn_test_app().await;
    let service = build_service(&app);
    let recipient = create_test_actor(&app, "dismiss-nonexistent-recipient").await;

    let err = service
        .dismiss(&ctx_for(recipient), app.runtime.ids.next_id())
        .await
        .expect_err("a nonexistent notification must 404 on dismiss");
    assert_eq!(err.status, StatusCode::NOT_FOUND);
}

/// Requirements 4.1, 4.4: `clear` dismisses every one of the recipient's
/// notifications, excluding them from subsequent `list`.
#[tokio::test]
async fn clear_dismisses_every_notification_for_the_recipient() {
    let app = spawn_test_app().await;
    let service = build_service(&app);

    let recipient = create_test_actor(&app, "clear-recipient").await;
    let origin = create_test_actor(&app, "clear-origin").await;
    seed_notification(
        &app,
        recipient,
        NotificationType::Follow,
        AccountRef::Local(origin),
        None,
    )
    .await;
    seed_notification(
        &app,
        recipient,
        NotificationType::FollowRequest,
        AccountRef::Local(origin),
        None,
    )
    .await;

    service
        .clear(&ctx_for(recipient))
        .await
        .expect("clear must succeed");

    let page = service
        .list(
            &ctx_for(recipient),
            PageParams::default(),
            ListFilter::default(),
        )
        .await
        .expect("list must succeed");
    assert!(page.items.is_empty());
}

/// Requirement 4.1: `clear` is idempotent — succeeds even with nothing to
/// clear.
#[tokio::test]
async fn clear_succeeds_when_the_recipient_has_no_notifications() {
    let app = spawn_test_app().await;
    let service = build_service(&app);
    let recipient = create_test_actor(&app, "clear-empty-recipient").await;

    service
        .clear(&ctx_for(recipient))
        .await
        .expect("clear must succeed even with no notifications");
}
