//! Unit tests for `NotificationFilter::should_suppress` (task 2.3,
//! Requirements 7.1, 7.2, 7.3, 7.4), per this task's own observable
//! completion condition: "ブロック/被ブロック/通知ミュートで抑制が真、期
//! 限切れミュートでは抑制が偽になることを単体で確認できる状態".
//!
//! `NotificationFilter` has no seam to substitute a fake `FilterQuery`
//! behind (it wraps a real `PgPool`-backed `FilterQuery`, per design.md),
//! so — mirroring `src/social_graph/providers/tests.rs`'s own
//! `filter_query_blocked_set_*` tests for the exact same dependency — these
//! tests spin up a real, isolated Postgres-backed app via
//! `crate::test_harness::spawn_test_app` and seed real `blocks`/`mutes`
//! rows through `social_graph::repository`'s already-implemented
//! upsert functions, rather than reimplementing or mocking any block/mute/
//! expiry logic here (Requirement 7.4).

use time::Duration;

use super::*;
use crate::domain::Id;
use crate::social_graph::model::{Block, Mute};
use crate::social_graph::repository as sg_repository;
use crate::test_harness::{TestApp, spawn_test_app};

/// Mirrors `src/social_graph/providers/tests.rs::upsert_block`'s identical
/// helper — this module's own tests need the same real-row seeding, and
/// `social_graph::providers::tests` is a private sibling module this crate
/// cannot import from.
async fn upsert_block(app: &TestApp, blocker: AccountRef, blocked: AccountRef) {
    sg_repository::upsert_block(
        &app.pool,
        app.runtime.ids.next_id(),
        &Block {
            blocker,
            blocked,
            activity_id: "https://example.test/activities/block-1".to_string(),
            created_at: app.runtime.clock.now(),
        },
    )
    .await
    .expect("upsert_block must succeed");
}

/// Mirrors `src/social_graph/providers/tests.rs::upsert_mute`'s identical
/// helper.
async fn upsert_mute(
    app: &TestApp,
    muter: AccountRef,
    muted: AccountRef,
    notifications: bool,
    expires_at: Option<time::OffsetDateTime>,
) {
    sg_repository::upsert_mute(
        &app.pool,
        app.runtime.ids.next_id(),
        &Mute {
            muter,
            muted,
            notifications,
            expires_at,
            created_at: app.runtime.clock.now(),
        },
    )
    .await
    .expect("upsert_mute must succeed");
}

fn fresh_id(app: &TestApp) -> Id {
    app.runtime.ids.next_id()
}

// -- Requirement 7.1: origin blocked by recipient -------------------------

#[tokio::test]
async fn suppresses_when_recipient_has_blocked_origin() {
    let app = spawn_test_app().await;
    let recipient = AccountRef::Local(fresh_id(&app));
    let origin = AccountRef::Remote(fresh_id(&app));

    upsert_block(&app, recipient, origin).await;

    let filter = NotificationFilter::new(FilterQuery::new(app.pool.clone(), app.runtime.clone()));
    let suppress = filter
        .should_suppress(&recipient, &origin)
        .await
        .expect("should_suppress must succeed");

    assert!(
        suppress,
        "recipient having blocked origin must suppress the notification"
    );

    app.cleanup().await;
}

// -- Requirement 7.1: recipient blocked by origin --------------------------

#[tokio::test]
async fn suppresses_when_recipient_is_blocked_by_origin() {
    let app = spawn_test_app().await;
    let recipient = AccountRef::Local(fresh_id(&app));
    let origin = AccountRef::Remote(fresh_id(&app));

    // origin blocks recipient -- the mirror direction of Requirement 7.1.
    upsert_block(&app, origin, recipient).await;

    let filter = NotificationFilter::new(FilterQuery::new(app.pool.clone(), app.runtime.clone()));
    let suppress = filter
        .should_suppress(&recipient, &origin)
        .await
        .expect("should_suppress must succeed");

    assert!(
        suppress,
        "recipient being blocked by origin must suppress the notification"
    );

    app.cleanup().await;
}

// -- Requirement 7.2: notification mute -------------------------------------

#[tokio::test]
async fn suppresses_when_origin_is_notification_muted_by_recipient() {
    let app = spawn_test_app().await;
    let recipient = AccountRef::Local(fresh_id(&app));
    let origin = AccountRef::Remote(fresh_id(&app));

    upsert_mute(&app, recipient, origin, true, None).await;

    let filter = NotificationFilter::new(FilterQuery::new(app.pool.clone(), app.runtime.clone()));
    let suppress = filter
        .should_suppress(&recipient, &origin)
        .await
        .expect("should_suppress must succeed");

    assert!(
        suppress,
        "a notification mute (muting_notifications) must suppress the notification"
    );

    app.cleanup().await;
}

/// Requirement 7.2's own wording restricts suppression to
/// `muting_notifications` specifically -- a plain (non-notification) mute
/// must NOT, by itself, suppress notification generation.
#[tokio::test]
async fn does_not_suppress_on_plain_mute_without_notification_mute() {
    let app = spawn_test_app().await;
    let recipient = AccountRef::Local(fresh_id(&app));
    let origin = AccountRef::Remote(fresh_id(&app));

    upsert_mute(&app, recipient, origin, false, None).await;

    let filter = NotificationFilter::new(FilterQuery::new(app.pool.clone(), app.runtime.clone()));
    let suppress = filter
        .should_suppress(&recipient, &origin)
        .await
        .expect("should_suppress must succeed");

    assert!(
        !suppress,
        "a plain mute without muting_notifications must not suppress the notification"
    );

    app.cleanup().await;
}

// -- Requirement 7.3: expired notification mute -----------------------------

#[tokio::test]
async fn does_not_suppress_on_expired_notification_mute() {
    let app = spawn_test_app().await;
    let recipient = AccountRef::Local(fresh_id(&app));
    let origin = AccountRef::Remote(fresh_id(&app));
    let now = app.runtime.clock.now();

    upsert_mute(
        &app,
        recipient,
        origin,
        true,
        Some(now - Duration::seconds(1)),
    )
    .await;

    let filter = NotificationFilter::new(FilterQuery::new(app.pool.clone(), app.runtime.clone()));
    let suppress = filter
        .should_suppress(&recipient, &origin)
        .await
        .expect("should_suppress must succeed");

    assert!(
        !suppress,
        "an expired notification mute must not suppress the notification (Requirement 7.3)"
    );

    app.cleanup().await;
}

// -- Baseline: no relationship at all ---------------------------------------

#[tokio::test]
async fn does_not_suppress_with_no_relationship() {
    let app = spawn_test_app().await;
    let recipient = AccountRef::Local(fresh_id(&app));
    let origin = AccountRef::Remote(fresh_id(&app));

    let filter = NotificationFilter::new(FilterQuery::new(app.pool.clone(), app.runtime.clone()));
    let suppress = filter
        .should_suppress(&recipient, &origin)
        .await
        .expect("should_suppress must succeed");

    assert!(
        !suppress,
        "with no block/mute relationship, the notification must not be suppressed"
    );

    app.cleanup().await;
}
