//! Tests for [`NotificationGenerator::generate`] (task 2.4, Requirements
//! 5.1, 5.2, 5.3, 5.5, 6.1-6.6, 8.1, 8.2), per this task's own completion
//! state: "非ローカル受信者・抑制・重複では永続化も配信引き渡しも起こら
//! ず、新規生成時のみ配信シークが呼ばれ、fav/reblog/mention/follow/
//! follow_request/poll の各種別で受信者宛通知が作られる状態".
//!
//! ## DB-availability split (mirrors `filter/tests.rs`'s established note)
//! `NotificationGenerator::generate`'s **first** step (Requirement 5.3's
//! recipient-local check) runs before `NotificationFilter`/
//! `insert_dedup`/the delivery sink are ever touched — this module exploits
//! that ordering to give the `SkippedNonLocal` branch a genuinely DB-free
//! test ([`skipped_non_local_short_circuits_without_touching_the_database`]):
//! it builds a generator over an `sqlx::PgPool::connect_lazy` pool (parses
//! the connection URL only, never dials — the same technique
//! `src/state/tests.rs`/`src/server/tests.rs` already establish) and proves
//! the call still succeeds. Every other test needs a real Postgres
//! connection (`NotificationFilter`/`insert_dedup` both issue real SQL),
//! which — per this run's own cross-task, already-confirmed sandbox
//! constraint (`filter/tests.rs`'s and `repository/tests.rs`'s own doc
//! comments; `social_graph::providers`'s pre-existing tests fail
//! identically) — is unavailable here. Those tests are written as real,
//! executable `#[tokio::test]`s against `spawn_test_app`, not skipped or
//! stubbed; see this task's own status report for which ones actually ran
//! versus which failed only on connectivity (`PoolTimedOut`).

use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use sqlx::postgres::PgPool;
use time::macros::datetime;

use super::*;
use crate::domain::Id;
use crate::notifications::model::NotificationType;
use crate::notifications::repository::{ListFilter, list};
use crate::social_graph::FilterQuery;
use crate::social_graph::model::Block;
use crate::social_graph::repository as sg_repository;
use crate::test_harness::{TestApp, spawn_test_app};

/// A connection URL `sqlx::PgPool::connect_lazy` only parses — never dials
/// — mirroring `src/state/tests.rs::LAZY_TEST_DB_URL`'s identical constant
/// and its own doc comment's justification.
const LAZY_TEST_DB_URL: &str = "postgres://lazy-user:lazy-pw@127.0.0.1:5432/lazy-test-db";

/// A recording spy [`NotificationDeliverySink`] (mirrors `ports.rs`'s own
/// private `RecordingDeliverySink` test double, reimplemented locally here
/// since that one is `mod tests`-private to `ports.rs` and not reusable
/// across module boundaries).
#[derive(Default)]
struct RecordingDeliverySink {
    delivered: Mutex<Vec<Notification>>,
}

impl RecordingDeliverySink {
    fn new() -> Self {
        Self::default()
    }

    fn delivered_count(&self) -> usize {
        self.delivered.lock().unwrap().len()
    }
}

impl NotificationDeliverySink for RecordingDeliverySink {
    fn deliver<'a>(
        &'a self,
        notification: &'a Notification,
    ) -> Pin<Box<dyn Future<Output = Result<(), AppError>> + Send + 'a>> {
        Box::pin(async move {
            self.delivered.lock().unwrap().push(notification.clone());
            Ok(())
        })
    }
}

fn sample_event(
    recipient: AccountRef,
    origin: AccountRef,
    kind: NotificationType,
    target_status_id: Option<Id>,
) -> NotificationEvent {
    NotificationEvent {
        recipient,
        origin,
        kind,
        target_status_id,
        occurred_at: datetime!(2026-07-24 00:00:00 UTC),
    }
}

fn build_generator(app: &TestApp, sink: Arc<RecordingDeliverySink>) -> NotificationGenerator {
    let filter = NotificationFilter::new(FilterQuery::new(app.pool.clone(), app.runtime.clone()));
    NotificationGenerator::new(app.pool.clone(), filter, sink, app.runtime.clone())
}

async fn upsert_block(app: &TestApp, blocker: AccountRef, blocked: AccountRef) {
    sg_repository::upsert_block(
        &app.pool,
        app.runtime.ids.next_id(),
        &Block {
            blocker,
            blocked,
            activity_id: "https://example.test/activities/generator-block-1".to_string(),
            created_at: app.runtime.clock.now(),
        },
    )
    .await
    .expect("upsert_block must succeed");
}

// -- Requirement 5.3: non-local recipient, no DB touched at all -----------

/// The recipient-local check happens before any collaborator that could
/// touch the database is invoked — proven here by using a `connect_lazy`
/// pool that would fail immediately on any real query, plus a spy delivery
/// sink asserted untouched.
#[tokio::test]
async fn skipped_non_local_short_circuits_without_touching_the_database() {
    let pool = PgPool::connect_lazy(LAZY_TEST_DB_URL)
        .expect("connect_lazy only parses the URL; it never opens a connection");
    let runtime =
        crate::runtime::RuntimeContext::deterministic(crate::runtime::DeterministicSeed::new(1));
    let filter = NotificationFilter::new(FilterQuery::new(pool.clone(), runtime.clone()));
    let sink = Arc::new(RecordingDeliverySink::new());
    let generator = NotificationGenerator::new(pool, filter, sink.clone(), runtime);

    let event = sample_event(
        AccountRef::Remote(Id::from_i64(1)),
        AccountRef::Remote(Id::from_i64(2)),
        NotificationType::Follow,
        None,
    );

    let outcome = generator
        .generate(event)
        .await
        .expect("a non-local recipient must short-circuit successfully, no DB touched");

    assert_eq!(outcome, GenerateOutcome::SkippedNonLocal);
    assert_eq!(
        sink.delivered_count(),
        0,
        "the delivery sink must not be invoked for a non-local recipient"
    );
}

// -- Requirement 7.x via NotificationFilter: suppressed -------------------

#[tokio::test]
async fn suppressed_when_recipient_has_blocked_origin() {
    let app = spawn_test_app().await;
    let recipient = AccountRef::Local(app.runtime.ids.next_id());
    let origin = AccountRef::Remote(app.runtime.ids.next_id());
    upsert_block(&app, recipient, origin).await;

    let sink = Arc::new(RecordingDeliverySink::new());
    let generator = build_generator(&app, sink.clone());

    let event = sample_event(recipient, origin, NotificationType::Favourite, None);
    let outcome = generator
        .generate(event)
        .await
        .expect("generate must succeed even when suppressed");

    assert_eq!(outcome, GenerateOutcome::Suppressed);
    assert_eq!(
        sink.delivered_count(),
        0,
        "a suppressed event must never reach the delivery sink"
    );

    let page = list(
        &app.pool,
        match recipient {
            AccountRef::Local(id) => id,
            AccountRef::Remote(_) => unreachable!(),
        },
        &crate::api::pagination::PageParams::default(),
        &ListFilter::default(),
    )
    .await
    .expect("list must succeed");
    assert!(
        page.items.is_empty(),
        "a suppressed event must not persist any notification"
    );

    app.cleanup().await;
}

// -- Requirements 6.1-6.6: Created for each required kind ------------------

#[tokio::test]
async fn created_for_each_of_the_six_required_kinds() {
    let app = spawn_test_app().await;
    let sink = Arc::new(RecordingDeliverySink::new());
    let generator = build_generator(&app, sink.clone());

    let kinds_with_status = [
        (NotificationType::Favourite, true),
        (NotificationType::Reblog, true),
        (NotificationType::Mention, true),
        (NotificationType::Follow, false),
        (NotificationType::FollowRequest, false),
        (NotificationType::Poll, true),
    ];

    for (index, (kind, has_status)) in kinds_with_status.into_iter().enumerate() {
        let recipient_id = app.runtime.ids.next_id();
        let recipient = AccountRef::Local(recipient_id);
        let origin = AccountRef::Remote(app.runtime.ids.next_id());
        let target_status_id = if has_status {
            Some(app.runtime.ids.next_id())
        } else {
            None
        };

        let event = sample_event(recipient, origin, kind, target_status_id);
        let outcome = generator.generate(event).await.unwrap_or_else(|err| {
            panic!("generate must succeed for kind #{index} ({kind:?}): {err:?}")
        });

        assert_eq!(
            outcome,
            GenerateOutcome::Created,
            "kind {kind:?} (index {index}) must be created for its local recipient"
        );

        let page = list(
            &app.pool,
            recipient_id,
            &crate::api::pagination::PageParams::default(),
            &ListFilter::default(),
        )
        .await
        .expect("list must succeed");
        assert_eq!(
            page.items.len(),
            1,
            "exactly one notification for kind {kind:?}"
        );
        let persisted = &page.items[0];
        assert_eq!(persisted.recipient_id, recipient_id);
        assert_eq!(persisted.kind, kind);
        assert_eq!(persisted.origin, origin);
        assert_eq!(persisted.status_id, target_status_id);
        assert!(!persisted.dismissed);
    }

    assert_eq!(
        sink.delivered_count(),
        kinds_with_status.len(),
        "every Created outcome above must have reached the delivery sink exactly once"
    );

    app.cleanup().await;
}

// -- Requirements 8.1, 8.2: Duplicate on a second identical event ---------

#[tokio::test]
async fn duplicate_on_a_second_identical_event_and_delivery_only_happens_once() {
    let app = spawn_test_app().await;
    let sink = Arc::new(RecordingDeliverySink::new());
    let generator = build_generator(&app, sink.clone());

    let recipient_id = app.runtime.ids.next_id();
    let recipient = AccountRef::Local(recipient_id);
    let origin = AccountRef::Remote(app.runtime.ids.next_id());
    let target_status_id = Some(app.runtime.ids.next_id());

    let first_event = sample_event(
        recipient,
        origin,
        NotificationType::Favourite,
        target_status_id,
    );
    let first_outcome = generator
        .generate(first_event)
        .await
        .expect("first generate must succeed");
    assert_eq!(first_outcome, GenerateOutcome::Created);

    let second_event = sample_event(
        recipient,
        origin,
        NotificationType::Favourite,
        target_status_id,
    );
    let second_outcome = generator
        .generate(second_event)
        .await
        .expect("second generate must succeed");
    assert_eq!(second_outcome, GenerateOutcome::Duplicate);

    assert_eq!(
        sink.delivered_count(),
        1,
        "the delivery sink must be invoked exactly once, only for the first (Created) event"
    );

    let page = list(
        &app.pool,
        recipient_id,
        &crate::api::pagination::PageParams::default(),
        &ListFilter::default(),
    )
    .await
    .expect("list must succeed");
    assert_eq!(
        page.items.len(),
        1,
        "the duplicate event must not have persisted a second row"
    );

    app.cleanup().await;
}

/// Requirement 8.1's "取り消し→再実行" semantics, exercised through the
/// generator (not just the repository): dismissing the created notification
/// frees its dedup key, so an identical follow-up event is `Created` again,
/// not `Duplicate`.
#[tokio::test]
async fn a_fresh_event_after_dismissal_is_created_again_not_duplicate() {
    let app = spawn_test_app().await;
    let sink = Arc::new(RecordingDeliverySink::new());
    let generator = build_generator(&app, sink.clone());

    let recipient_id = app.runtime.ids.next_id();
    let recipient = AccountRef::Local(recipient_id);
    let origin = AccountRef::Remote(app.runtime.ids.next_id());

    let first_event = sample_event(recipient, origin, NotificationType::Follow, None);
    let first_outcome = generator
        .generate(first_event)
        .await
        .expect("first generate must succeed");
    assert_eq!(first_outcome, GenerateOutcome::Created);

    let page = list(
        &app.pool,
        recipient_id,
        &crate::api::pagination::PageParams::default(),
        &ListFilter::default(),
    )
    .await
    .expect("list must succeed");
    let created_id = page.items[0].id;
    let dismissed = crate::notifications::repository::dismiss(&app.pool, created_id, recipient_id)
        .await
        .expect("dismiss must succeed");
    assert!(dismissed);

    let second_event = sample_event(recipient, origin, NotificationType::Follow, None);
    let second_outcome = generator
        .generate(second_event)
        .await
        .expect("second generate after dismissal must succeed");
    assert_eq!(
        second_outcome,
        GenerateOutcome::Created,
        "a fresh event with the same dedup key must be Created again once the prior \
         notification is dismissed (Requirement 8.1's undo -> redo semantics)"
    );

    assert_eq!(
        sink.delivered_count(),
        2,
        "both the original and the post-dismissal notification must be delivered"
    );

    app.cleanup().await;
}
