//! Delegation ports (design.md "Port / 委譲層" ->
//! `NotificationEventSink / NotificationDeliverySink`, Requirements 5.4,
//! 5.5; task 2.2, `Boundary: ports`).
//!
//! Scope: this module owns exactly the two delegation seams design.md's
//! Service Interface names — [`NotificationEventSink`] (upstream
//! statuses-core/social-graph event receipt, Requirement 5.4) and
//! [`NotificationDeliverySink`] (downstream streaming/web-push delivery of
//! an already-persisted [`Notification`], Requirement 5.5) — plus their
//! shared default no-op implementation ([`NoopSink`], design.md's exact
//! name) and the swap-in registry handle ([`NotificationPortsRegistry`])
//! a later task (4.2, module wiring) inserts into `AppState`. No real
//! (business-logic-backed) implementation of either trait lives here: the
//! `NotificationEventSink` *definition* is this task's job, its *real
//! implementation* (routing to [`crate::notifications::NotificationGenerator`],
//! not yet built) is task 3.1's; `NotificationDeliverySink`'s real
//! implementation is out of this spec's boundary entirely (owned by the
//! future streaming/web-push specs — design.md: "本 spec は定義と既定のみ
//! 所有、配信手段は実装しない"). This task also does not touch
//! `src/state.rs`/`src/bootstrap.rs` — registering this module's registry
//! into a live `AppState` is task 4.2's job.
//!
//! ## Why boxed futures, not design.md's literal `async fn` sketch
//! design.md's Service Interface excerpt writes both trait methods as a
//! plain `async fn` (`async fn emit(&self, event: NotificationEvent) ->
//! Result<(), AppError>;` / `async fn deliver(&self, notification: &Notification)
//! -> Result<(), AppError>;`). Taken literally, that makes each trait *not*
//! `dyn`-compatible (native `async fn`-in-trait desugars to an opaque
//! per-impl associated type that cannot be named in a trait object) — the
//! same non-object-safety `crate::accounts::ports` and
//! `crate::statuses::notification_sink` already document for the identical
//! situation. [`NotificationPortsRegistry`] needs `Arc<dyn
//! NotificationEventSink>` / `Arc<dyn NotificationDeliverySink>` to be
//! constructible (a runtime-swappable slot a later task's bootstrap
//! replaces *after* this registry is already live inside `AppState`), so
//! this module follows `accounts::ports`'s / `statuses::notification_sink`'s
//! exact escape hatch: each trait method returns
//! `Pin<Box<dyn Future<Output = Result<(), AppError>> + Send + 'a>>`
//! instead of a literal `async fn`. Method name/parameter/return-`Result`
//! shape is otherwise unchanged from design.md's excerpt. Flagged in this
//! task's status report CONCERNS for reviewer confirmation, mirroring the
//! precedent those two modules' own doc comments set for the identical
//! design.md-vs-object-safety deviation.
//!
//! ## One shared `NoopSink`, not two separate no-op types
//! design.md prints `pub struct NoopSink;` exactly once, positioned between
//! the two trait definitions, with the `NotificationDeliverySink::deliver`
//! comment simply noting "既定 no-op" rather than naming a second struct.
//! This module reads that as one shared no-op type serving as the default
//! for *both* seams (both are, structurally, "do nothing, touch no DB/
//! network" unit structs) rather than inventing an unnamed second type —
//! [`NoopSink`] implements both [`NotificationEventSink`] and
//! [`NotificationDeliverySink`].
//!
//! ## Registry shape: one handle, two independent replaceable slots
//! [`NotificationPortsRegistry`] mirrors `crate::accounts::ports::AccountPortsRegistry`'s
//! exact idiom (see that module's own doc comment, "Registry shape"): each
//! seam gets its own `Arc<RwLock<Arc<dyn Trait>>>` slot, defaulting to
//! [`NoopSink`], swapped wholesale via `set_event_sink`/`set_delivery_sink`
//! (`&self`, not `&mut self` — `AppState` is immutable-after-construction,
//! yet a later task's registration must be able to happen after this
//! registry is already live), and cheap to `Clone` (clones two `Arc`s, not
//! their contents). This is the single "AppState レジストリ用のハンドル"
//! this task's own instruction asks for — a later task (4.2) is the one
//! that actually stores an instance of it inside `AppState`.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

use crate::error::AppError;
use crate::notifications::model::{Notification, NotificationEvent};

/// Upstream event-receipt seam (design.md: `NotificationEventSink` —
/// "上流イベント受領シーム"). Real routing to
/// [`crate::notifications::NotificationGenerator`] is task 3.1's job; this
/// task defines only the trait and its default (Requirement 5.4). See this
/// module's doc comment ("Why boxed futures") for why this returns a boxed
/// future instead of design.md's literal `async fn` sketch.
pub trait NotificationEventSink: Send + Sync {
    fn emit<'a>(
        &'a self,
        event: NotificationEvent,
    ) -> Pin<Box<dyn Future<Output = Result<(), AppError>> + Send + 'a>>;
}

/// Downstream delivery seam (design.md: `NotificationDeliverySink` —
/// "後段配信シーム"). Hands an already-*persisted* [`Notification`] to
/// whichever downstream spec (streaming/web-push) wants to react — this
/// spec defines and defaults it, but never implements real delivery
/// (Requirement 5.5). See this module's doc comment ("Why boxed futures").
pub trait NotificationDeliverySink: Send + Sync {
    fn deliver<'a>(
        &'a self,
        notification: &'a Notification,
    ) -> Pin<Box<dyn Future<Output = Result<(), AppError>> + Send + 'a>>;
}

/// design.md's exact default-implementation name, shared by both seams
/// (see this module's doc comment, "One shared `NoopSink`"). A zero-field
/// unit struct, so "既定実装はネットワーク/DB に触れず何もしない"
/// (design.md's own postcondition, Requirements 5.4/5.5) holds
/// structurally, not just behaviorally — it cannot reach a `PgPool` or
/// network client even by accident, mirroring
/// `accounts::ports::EmptyStatusesProvider`'s identical proof shape.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopSink;

impl NotificationEventSink for NoopSink {
    fn emit<'a>(
        &'a self,
        _event: NotificationEvent,
    ) -> Pin<Box<dyn Future<Output = Result<(), AppError>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
}

impl NotificationDeliverySink for NoopSink {
    fn deliver<'a>(
        &'a self,
        _notification: &'a Notification,
    ) -> Pin<Box<dyn Future<Output = Result<(), AppError>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
}

/// The runtime-replaceable registry handle a later task (4.2) stores
/// inside `AppState` (see this module's doc comment, "Registry shape:
/// one handle, two independent replaceable slots"). Every freshly built
/// instance defaults both slots to [`NoopSink`] until a later task calls
/// [`NotificationPortsRegistry::set_event_sink`] /
/// [`NotificationPortsRegistry::set_delivery_sink`].
#[derive(Clone)]
pub struct NotificationPortsRegistry {
    event_sink: Arc<RwLock<Arc<dyn NotificationEventSink>>>,
    delivery_sink: Arc<RwLock<Arc<dyn NotificationDeliverySink>>>,
}

impl Default for NotificationPortsRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl NotificationPortsRegistry {
    /// Builds a registry with both slots defaulting to [`NoopSink`] — no
    /// upstream/downstream implementation is reachable from a freshly
    /// built registry until a later task's `set_*` call replaces one.
    pub fn new() -> Self {
        NotificationPortsRegistry {
            event_sink: Arc::new(RwLock::new(Arc::new(NoopSink))),
            delivery_sink: Arc::new(RwLock::new(Arc::new(NoopSink))),
        }
    }

    /// Replaces the registered [`NotificationEventSink`] (task 3.1's own
    /// registration entry point). `&self`, not `&mut self` — see this
    /// module's doc comment ("Registry shape").
    pub fn set_event_sink(&self, sink: Arc<dyn NotificationEventSink>) {
        *self
            .event_sink
            .write()
            .expect("NotificationPortsRegistry event_sink lock must not be poisoned") = sink;
    }

    /// Replaces the registered [`NotificationDeliverySink`] (a future
    /// streaming/web-push spec's own registration entry point).
    pub fn set_delivery_sink(&self, sink: Arc<dyn NotificationDeliverySink>) {
        *self
            .delivery_sink
            .write()
            .expect("NotificationPortsRegistry delivery_sink lock must not be poisoned") = sink;
    }

    /// Delegates `event` to the currently registered [`NotificationEventSink`]
    /// (the built-in [`NoopSink`] until a later task replaces it — upstream
    /// still succeeds, Requirement 5.4).
    pub async fn emit(&self, event: NotificationEvent) -> Result<(), AppError> {
        let sink = self
            .event_sink
            .read()
            .expect("NotificationPortsRegistry event_sink lock must not be poisoned")
            .clone();
        sink.emit(event).await
    }

    /// Delegates `notification` to the currently registered
    /// [`NotificationDeliverySink`] (the built-in [`NoopSink`] until a
    /// future streaming/web-push spec replaces it, Requirement 5.5).
    pub async fn deliver(&self, notification: &Notification) -> Result<(), AppError> {
        let sink = self
            .delivery_sink
            .read()
            .expect("NotificationPortsRegistry delivery_sink lock must not be poisoned")
            .clone();
        sink.deliver(notification).await
    }
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use super::*;
    use crate::domain::{AccountRef, Id};
    use crate::notifications::model::NotificationType;

    fn sample_event() -> NotificationEvent {
        NotificationEvent {
            recipient: AccountRef::Local(Id::from_i64(1)),
            origin: AccountRef::Local(Id::from_i64(2)),
            kind: NotificationType::Favourite,
            target_status_id: Some(Id::from_i64(3)),
            occurred_at: datetime!(2026-07-24 00:00:00 UTC),
        }
    }

    fn sample_notification() -> Notification {
        Notification {
            id: Id::from_i64(10),
            recipient_id: Id::from_i64(1),
            kind: NotificationType::Favourite,
            origin: AccountRef::Local(Id::from_i64(2)),
            status_id: Some(Id::from_i64(3)),
            dismissed: false,
            created_at: datetime!(2026-07-24 00:00:00 UTC),
        }
    }

    /// Requirement 5.4 / this task's own completion definition ("既定
    /// no-op が上流イベントを受けても何もせず... 上流が未登録でも成功する
    /// 状態"): calling [`NoopSink::emit`] directly succeeds and, being a
    /// zero-field unit struct, cannot have touched any DB/network handle.
    #[tokio::test]
    async fn noop_sink_emit_returns_ok_and_touches_nothing() {
        let sink = NoopSink;
        assert!(sink.emit(sample_event()).await.is_ok());
    }

    /// Requirement 5.5 / this task's own completion definition ("配信シー
    /// クが永続化済み通知を受け取る形でコンパイルが通る"): [`NoopSink::deliver`]
    /// accepts an already-persisted [`Notification`] by reference and
    /// succeeds.
    #[tokio::test]
    async fn noop_sink_deliver_returns_ok_and_touches_nothing() {
        let sink = NoopSink;
        let notification = sample_notification();
        assert!(sink.deliver(&notification).await.is_ok());
    }

    /// A freshly built registry defaults its event-sink slot to
    /// [`NoopSink`] — emitting through it succeeds even though nothing has
    /// registered a real upstream sink yet (Requirement 5.4).
    #[tokio::test]
    async fn registry_emit_defaults_to_noop_when_nothing_registered() {
        let registry = NotificationPortsRegistry::new();
        assert!(registry.emit(sample_event()).await.is_ok());
    }

    /// A freshly built registry defaults its delivery-sink slot to
    /// [`NoopSink`] — delivering through it succeeds even though nothing
    /// has registered a real downstream sink yet (Requirement 5.5).
    #[tokio::test]
    async fn registry_deliver_defaults_to_noop_when_nothing_registered() {
        let registry = NotificationPortsRegistry::new();
        let notification = sample_notification();
        assert!(registry.deliver(&notification).await.is_ok());
    }

    struct RecordingEventSink {
        events: std::sync::Mutex<Vec<NotificationEvent>>,
    }

    impl RecordingEventSink {
        fn new() -> Self {
            Self {
                events: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    impl NotificationEventSink for RecordingEventSink {
        fn emit<'a>(
            &'a self,
            event: NotificationEvent,
        ) -> Pin<Box<dyn Future<Output = Result<(), AppError>> + Send + 'a>> {
            Box::pin(async move {
                self.events.lock().unwrap().push(event);
                Ok(())
            })
        }
    }

    /// After `set_event_sink`, the registry routes to the newly registered
    /// sink instead of the default [`NoopSink`].
    #[tokio::test]
    async fn registry_uses_a_registered_event_sink_instead_of_the_default() {
        let registry = NotificationPortsRegistry::new();
        let sink = Arc::new(RecordingEventSink::new());
        registry.set_event_sink(Arc::clone(&sink) as Arc<dyn NotificationEventSink>);

        registry.emit(sample_event()).await.unwrap();

        let events = sink.events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, NotificationType::Favourite);
    }

    struct RecordingDeliverySink {
        notifications: std::sync::Mutex<Vec<Notification>>,
    }

    impl RecordingDeliverySink {
        fn new() -> Self {
            Self {
                notifications: std::sync::Mutex::new(Vec::new()),
            }
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

    /// After `set_delivery_sink`, the registry routes to the newly
    /// registered sink instead of the default [`NoopSink`].
    #[tokio::test]
    async fn registry_uses_a_registered_delivery_sink_instead_of_the_default() {
        let registry = NotificationPortsRegistry::new();
        let sink = Arc::new(RecordingDeliverySink::new());
        registry.set_delivery_sink(Arc::clone(&sink) as Arc<dyn NotificationDeliverySink>);
        let notification = sample_notification();

        registry.deliver(&notification).await.unwrap();

        let notifications = sink.notifications.lock().unwrap();
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].id, Id::from_i64(10));
    }

    /// Cloning the registry shares the same underlying slots (not
    /// independent copies) — registering on one clone is visible through
    /// another, mirroring `AccountPortsRegistry`'s/`NotificationSinkRegistry`'s
    /// identical clone-shares-state guarantee.
    #[tokio::test]
    async fn registry_clone_shares_the_same_slots() {
        let registry = NotificationPortsRegistry::new();
        let clone = registry.clone();
        let sink = Arc::new(RecordingEventSink::new());
        registry.set_event_sink(Arc::clone(&sink) as Arc<dyn NotificationEventSink>);

        clone.emit(sample_event()).await.unwrap();
        assert_eq!(sink.events.lock().unwrap().len(), 1);
    }
}
