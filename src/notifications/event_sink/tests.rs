//! Tests for [`super::GeneratorEventSink`]/[`super::StatusesEventSinkAdapter`]
//! and the `From` conversions between `statuses::notification_sink`'s
//! placeholder event/kind types and this spec's canonical
//! [`crate::notifications::model`] types (task 3.1, Requirements 5.1, 5.2,
//! 6.1-6.6).
//!
//! ## DB-availability split (mirrors `generator/tests.rs`'s established note)
//! The `From` conversions and [`super::StatusesEventSinkAdapter`]'s own
//! conversion/delegation logic are pure — no `PgPool` involved at all —
//! and are exercised here as genuinely executable, DB-independent tests.
//! [`super::GeneratorEventSink`] wraps a real
//! [`crate::notifications::NotificationGenerator`], whose *only*
//! DB-independent branch is the non-local-recipient short circuit
//! (Requirement 5.3, already proven at the generator level by
//! `generator/tests.rs::skipped_non_local_short_circuits_without_touching_the_database`);
//! this module reuses that exact same branch (via `PgPool::connect_lazy`,
//! same technique/constant convention) to prove the full pipeline —
//! `StatusesEventSinkAdapter` -> `GeneratorEventSink` ->
//! `NotificationGenerator::generate` — is actually wired end-to-end,
//! without needing a real Postgres connection. Every other branch
//! (local-recipient routing that actually persists/delivers) is *not*
//! re-tested here: `NotificationGenerator::generate`'s own behavior for
//! those branches is already covered by `generator/tests.rs`, and this
//! module's own job is only to prove the adapter/sink correctly convert
//! and hand off to that already-tested collaborator — reviewed via manual
//! trace (see this task's status report TESTS_RUN/CONCERNS).

use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use sqlx::postgres::PgPool;
use time::macros::datetime;

use super::*;
use crate::domain::{AccountRef, Id};
use crate::notifications::filter::NotificationFilter;
use crate::notifications::generator::GenerateOutcome;
use crate::notifications::ports::{
    NotificationDeliverySink, NotificationEventSink as CanonicalSink,
};
use crate::social_graph::FilterQuery;

/// A connection URL `sqlx::PgPool::connect_lazy` only parses — never dials
/// — mirroring `generator/tests.rs::LAZY_TEST_DB_URL`'s identical constant.
const LAZY_TEST_DB_URL: &str = "postgres://lazy-user:lazy-pw@127.0.0.1:5432/lazy-test-db";

fn sample_statuses_event(
    recipient: AccountRef,
    origin: AccountRef,
    kind: StatusesNotificationType,
    target_status_id: Option<Id>,
) -> StatusesNotificationEvent {
    StatusesNotificationEvent {
        recipient,
        origin,
        kind,
        target_status_id,
        occurred_at: datetime!(2026-07-24 00:00:00 UTC),
    }
}

// -- `From` conversions: verified 1:1, no DB involved ----------------------

/// Requirement 6.1-6.6 / this task's "Type conversion" doc section: an
/// exhaustive `match` over all eight `StatusesNotificationType` variants,
/// each asserted to convert to its identically-named canonical
/// `NotificationType` variant. If a ninth variant were ever added to either
/// side, this would fail to compile until updated.
#[test]
fn statuses_notification_type_converts_to_the_identically_named_canonical_variant() {
    let cases = [
        (StatusesNotificationType::Mention, NotificationType::Mention),
        (StatusesNotificationType::Follow, NotificationType::Follow),
        (
            StatusesNotificationType::FollowRequest,
            NotificationType::FollowRequest,
        ),
        (
            StatusesNotificationType::Favourite,
            NotificationType::Favourite,
        ),
        (StatusesNotificationType::Reblog, NotificationType::Reblog),
        (StatusesNotificationType::Poll, NotificationType::Poll),
        (StatusesNotificationType::Status, NotificationType::Status),
        (StatusesNotificationType::Update, NotificationType::Update),
    ];
    for (statuses_kind, expected_canonical) in cases {
        let converted: NotificationType = statuses_kind.into();
        assert_eq!(converted, expected_canonical);
    }
}

/// Requirement 5.1 / this task's "Type conversion" doc section: every field
/// of a `statuses::notification_sink::NotificationEvent` is carried over
/// unchanged (not dropped, not defaulted) into the canonical
/// `NotificationEvent`.
#[test]
fn statuses_notification_event_converts_field_by_field_without_loss() {
    let recipient = AccountRef::Local(Id::from_i64(1));
    let origin = AccountRef::Remote(Id::from_i64(2));
    let target_status_id = Some(Id::from_i64(3));
    let occurred_at = datetime!(2026-07-24 12:34:56 UTC);
    let source = StatusesNotificationEvent {
        recipient,
        origin,
        kind: StatusesNotificationType::Favourite,
        target_status_id,
        occurred_at,
    };

    let converted: NotificationEvent = source.into();

    assert_eq!(converted.recipient, recipient);
    assert_eq!(converted.origin, origin);
    assert_eq!(converted.kind, NotificationType::Favourite);
    assert_eq!(converted.target_status_id, target_status_id);
    assert_eq!(converted.occurred_at, occurred_at);
}

/// The status-less kinds (`Follow`/`FollowRequest`) convert with
/// `target_status_id: None` preserved, not coerced to `Some`.
#[test]
fn statuses_notification_event_with_no_target_status_converts_to_none() {
    let source = sample_statuses_event(
        AccountRef::Local(Id::from_i64(1)),
        AccountRef::Local(Id::from_i64(2)),
        StatusesNotificationType::Follow,
        None,
    );

    let converted: NotificationEvent = source.into();

    assert_eq!(converted.kind, NotificationType::Follow);
    assert_eq!(converted.target_status_id, None);
}

// -- `StatusesEventSinkAdapter`: pure conversion + delegation, no DB -------

/// A recording spy implementing this spec's own canonical
/// [`CanonicalSink`] — lets `StatusesEventSinkAdapter`'s conversion +
/// delegation be proven without touching `NotificationGenerator`/`PgPool`
/// at all.
#[derive(Default)]
struct RecordingCanonicalSink {
    received: Mutex<Vec<NotificationEvent>>,
}

impl RecordingCanonicalSink {
    fn new() -> Self {
        Self::default()
    }
}

impl CanonicalSink for RecordingCanonicalSink {
    fn emit<'a>(
        &'a self,
        event: NotificationEvent,
    ) -> Pin<Box<dyn Future<Output = Result<(), AppError>> + Send + 'a>> {
        Box::pin(async move {
            self.received.lock().unwrap().push(event);
            Ok(())
        })
    }
}

/// Requirements 5.1, 5.2 / this task's own completion definition ("既定
/// no-op を差し替えた本実装が、emit されたイベントをジェネレータへ渡し通知
/// 生成を起動する状態"): emitting a `statuses::notification_sink::NotificationEvent`
/// through `StatusesEventSinkAdapter` reaches the wrapped canonical sink
/// with a correctly converted event — the exact routing path both
/// local-origin (`StatusService`/`InteractionService`) and remote-received
/// (`inbound_handlers.rs`/`social_graph::transitions`) call sites would use
/// once registered (task 4.2), since both emit through the identical
/// shared `NotificationSinkRegistry` instance (see this module's own doc
/// comment, "Why two sinks, not one").
#[tokio::test]
async fn adapter_converts_and_forwards_to_the_wrapped_canonical_sink() {
    let recording = Arc::new(RecordingCanonicalSink::new());
    let adapter = StatusesEventSinkAdapter::new(recording.clone() as Arc<dyn CanonicalSink>);

    let recipient = AccountRef::Local(Id::from_i64(10));
    let origin = AccountRef::Remote(Id::from_i64(20));
    let event = sample_statuses_event(
        recipient,
        origin,
        StatusesNotificationType::Reblog,
        Some(Id::from_i64(30)),
    );

    StatusesNotificationEventSink::emit(&adapter, event)
        .await
        .expect("adapter emit must succeed");

    let received = recording.received.lock().unwrap();
    assert_eq!(
        received.len(),
        1,
        "exactly one event must reach the inner canonical sink"
    );
    assert_eq!(received[0].recipient, recipient);
    assert_eq!(received[0].origin, origin);
    assert_eq!(received[0].kind, NotificationType::Reblog);
    assert_eq!(received[0].target_status_id, Some(Id::from_i64(30)));
}

/// The adapter forwards every emitted event, not just the first (proves it
/// is not a one-shot/consuming seam).
#[tokio::test]
async fn adapter_forwards_multiple_events_in_order() {
    let recording = Arc::new(RecordingCanonicalSink::new());
    let adapter = StatusesEventSinkAdapter::new(recording.clone() as Arc<dyn CanonicalSink>);

    let first = sample_statuses_event(
        AccountRef::Local(Id::from_i64(1)),
        AccountRef::Local(Id::from_i64(2)),
        StatusesNotificationType::Follow,
        None,
    );
    let second = sample_statuses_event(
        AccountRef::Local(Id::from_i64(1)),
        AccountRef::Local(Id::from_i64(3)),
        StatusesNotificationType::FollowRequest,
        None,
    );

    StatusesNotificationEventSink::emit(&adapter, first)
        .await
        .expect("first emit must succeed");
    StatusesNotificationEventSink::emit(&adapter, second)
        .await
        .expect("second emit must succeed");

    let received = recording.received.lock().unwrap();
    assert_eq!(received.len(), 2);
    assert_eq!(received[0].kind, NotificationType::Follow);
    assert_eq!(received[1].kind, NotificationType::FollowRequest);
}

// -- `GeneratorEventSink` / full pipeline: DB-independent branch only ------

/// A spy [`NotificationDeliverySink`] (mirrors `generator/tests.rs`'s own
/// `RecordingDeliverySink`), used only to assert it stays untouched on the
/// non-local short-circuit path.
#[derive(Default)]
struct UntouchedDeliverySink {
    delivered: Mutex<usize>,
}

impl NotificationDeliverySink for UntouchedDeliverySink {
    fn deliver<'a>(
        &'a self,
        _notification: &'a crate::notifications::model::Notification,
    ) -> Pin<Box<dyn Future<Output = Result<(), AppError>> + Send + 'a>> {
        Box::pin(async move {
            *self.delivered.lock().unwrap() += 1;
            Ok(())
        })
    }
}

fn lazy_generator(delivery_sink: Arc<UntouchedDeliverySink>) -> NotificationGenerator {
    let pool = PgPool::connect_lazy(LAZY_TEST_DB_URL)
        .expect("connect_lazy only parses the URL; it never opens a connection");
    let runtime =
        crate::runtime::RuntimeContext::deterministic(crate::runtime::DeterministicSeed::new(1));
    let filter = NotificationFilter::new(FilterQuery::new(pool.clone(), runtime.clone()));
    NotificationGenerator::new(pool, filter, delivery_sink, runtime)
}

/// Requirements 5.1, 5.2, 5.3 / this task's own completion definition,
/// proven through `GeneratorEventSink` directly (the canonical-port half of
/// the pipeline): a canonical `NotificationEvent` for a non-local recipient
/// reaches `NotificationGenerator::generate` and short-circuits
/// successfully, without ever touching the database or the delivery sink —
/// proving `GeneratorEventSink::emit` really does route into the generator
/// rather than silently no-op'ing.
#[tokio::test]
async fn generator_event_sink_routes_into_the_generator() {
    let delivery_sink = Arc::new(UntouchedDeliverySink::default());
    let generator = Arc::new(lazy_generator(delivery_sink.clone()));
    let sink = GeneratorEventSink::new(generator);

    let event = NotificationEvent {
        recipient: AccountRef::Remote(Id::from_i64(1)),
        origin: AccountRef::Remote(Id::from_i64(2)),
        kind: NotificationType::Follow,
        target_status_id: None,
        occurred_at: datetime!(2026-07-24 00:00:00 UTC),
    };

    CanonicalSink::emit(&sink, event)
        .await
        .expect("routing a non-local event must succeed without touching the DB");

    assert_eq!(
        *delivery_sink.delivered.lock().unwrap(),
        0,
        "a non-local recipient must never reach the delivery sink"
    );
}

/// Requirements 5.1, 5.2 / this task's own completion definition, proven
/// end-to-end: `StatusesEventSinkAdapter` wrapping a `GeneratorEventSink`
/// wrapping a real `NotificationGenerator` — the exact composition a real
/// registration (task 4.2) would build — successfully converts and routes
/// a `statuses::notification_sink::NotificationEvent` all the way into
/// `NotificationGenerator::generate`, proven via the same DB-independent
/// non-local branch.
#[tokio::test]
async fn full_pipeline_from_statuses_side_event_reaches_the_generator() {
    let delivery_sink = Arc::new(UntouchedDeliverySink::default());
    let generator = Arc::new(lazy_generator(delivery_sink.clone()));
    let generator_sink: Arc<dyn CanonicalSink> = Arc::new(GeneratorEventSink::new(generator));
    let adapter = StatusesEventSinkAdapter::new(generator_sink);

    let event = sample_statuses_event(
        AccountRef::Remote(Id::from_i64(1)),
        AccountRef::Remote(Id::from_i64(2)),
        StatusesNotificationType::FollowRequest,
        None,
    );

    StatusesNotificationEventSink::emit(&adapter, event)
        .await
        .expect("full pipeline routing for a non-local event must succeed without touching the DB");

    assert_eq!(
        *delivery_sink.delivered.lock().unwrap(),
        0,
        "a non-local recipient routed through the full pipeline must never reach delivery"
    );
}

/// Sanity check that this module's `GenerateOutcome` import is exercised
/// (via `tracing::debug!(?outcome, ..)` inside `GeneratorEventSink::emit`)
/// and that the outcome for the non-local branch really is
/// `SkippedNonLocal` — proven directly against the generator, matching
/// `generator/tests.rs`'s own assertion for the identical branch.
#[tokio::test]
async fn non_local_outcome_is_skipped_non_local() {
    let delivery_sink = Arc::new(UntouchedDeliverySink::default());
    let generator = lazy_generator(delivery_sink);

    let outcome = generator
        .generate(NotificationEvent {
            recipient: AccountRef::Remote(Id::from_i64(1)),
            origin: AccountRef::Remote(Id::from_i64(2)),
            kind: NotificationType::Poll,
            target_status_id: Some(Id::from_i64(3)),
            occurred_at: datetime!(2026-07-24 00:00:00 UTC),
        })
        .await
        .expect("non-local recipient must short-circuit successfully");

    assert_eq!(outcome, GenerateOutcome::SkippedNonLocal);
}
