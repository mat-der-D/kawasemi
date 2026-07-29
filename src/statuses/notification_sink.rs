//! `NotificationEventSink` / `NotificationEvent` / `NotificationType` — task
//! 9.2, `_Boundary: InteractionService, StatusService, PollService_`.
//!
//! ## Why this module exists inside statuses-core, not notifications
//! `.kiro/specs/notifications/design.md` ("Port / 委譲層" ->
//! `NotificationEventSink / NotificationDeliverySink`) is the **owning**
//! spec for this trait/type shape — task 9.2's own instruction is explicit
//! that "イベント型/シンク契約は notifications 所有で再定義しない". But
//! `.kiro/steering/roadmap.md` places notifications *downstream* of
//! statuses-core (it depends on the Status/Poll contracts this spec
//! establishes), so by construction notifications has zero implemented
//! tasks and no `src/notifications/` module exists yet when this task runs
//! — there is nothing to import from.
//!
//! This mirrors the identical ordering problem task 3.1 already solved for
//! `RelationshipQuery` (owned by social-graph, not yet implemented at the
//! time statuses-core needed it): the spec that needs the port *first*
//! defines the port's Rust trait/type/default-impl within its own module
//! boundary (`src/statuses/visibility.rs`'s `RelationshipQuery` +
//! `NoRelationshipQuery`), so it can proceed standalone; the real owning
//! spec later supplies/migrates the real implementation. This module is
//! that same precedent applied to `NotificationEventSink`. See
//! `tasks.md`'s 9.2 Implementation Note for the full write-up.
//!
//! **Field shapes are copied verbatim** from notifications/design.md's own
//! Service Interface excerpt (`NotificationType`, `NotificationEvent`,
//! `NotificationEventSink::emit`, `NoopSink`) — not invented here — so a
//! future notifications implementation can adopt/migrate this module's
//! types without a breaking rewrite.
//!
//! ## One deliberate shape deviation: boxed future, not a literal `async fn`
//! notifications/design.md's excerpt writes `async fn emit(&self, event:
//! NotificationEvent) -> Result<(), AppError>;`. Taken literally, that
//! trait is not `dyn`-compatible (native `async fn`-in-trait desugars to an
//! opaque per-impl associated type that cannot be named in a trait object)
//! — the same non-object-safety `crate::accounts::ports`'s own doc comment
//! ("Why boxed futures, not design.md's literal `async fn` sketch")
//! documents for the identical situation. [`NotificationSinkRegistry`]
//! (below) needs `Arc<dyn NotificationEventSink>` to be constructible (a
//! runtime-swappable slot a future notifications bootstrap replaces
//! *after* this registry is already live inside a service), so this module
//! follows `accounts::ports`'s exact escape hatch: [`NotificationEventSink::emit`]
//! returns `Pin<Box<dyn Future<Output = Result<(), AppError>> + Send + 'a>>`
//! instead. Method name/parameter/return-`Result` shape is otherwise
//! unchanged from design.md's excerpt.
//!
//! ## Registry: one replaceable slot, mirroring `AccountPortsRegistry`
//! [`NotificationSinkRegistry`] replicates `crate::accounts::ports::AccountPortsRegistry`'s
//! exact idiom (see that module's own doc comment, "Registry shape"): a
//! single `Arc<RwLock<Arc<dyn NotificationEventSink>>>` slot, defaulting to
//! [`NoopSink`], swapped wholesale via `set_sink` (`&self`, not
//! `&mut self` — `AppState`/`StatusesModule` are immutable-after-
//! construction, yet a future notifications spec's registration must be
//! able to happen after this registry is already live), and cheap to
//! `Clone` (clones one `Arc`). `StatusService`/`InteractionService` each
//! hold their own clone of the *same* registry instance (each composition-
//! root caller — `src/bootstrap.rs`/`src/test_harness.rs`/
//! `src/federation/test_harness.rs` — constructs it once, *before*
//! `federation::build_federation_module` runs, and passes a clone to both
//! `build_statuses_module` and `statuses::register_downstream_handlers`; see
//! `build_statuses_module`'s own doc comment, task 10.2) so
//! `inbound_handlers.rs`'s `CreateNoteHandler`/`AnnounceHandler`/
//! `LikeHandler` (remote-origin emit, task 10.2) hold a clone of the
//! identical instance too, and a single future `set_sink` call — exposed via
//! `StatusesModule::notification_sink_registry`, mirroring
//! `AccountsModule::ports()` — reaches every emit call site at once.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

use time::OffsetDateTime;

use crate::domain::{AccountRef, Id};
use crate::error::AppError;

/// notifications/design.md's exact v1 type set (its own "型定義（抜粋）"
/// section) — not redefined differently here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationType {
    Mention,
    Follow,
    FollowRequest,
    Favourite,
    Reblog,
    Poll,
    Status,
    Update,
}

/// A single upstream-emitted, immutable notification-generation event
/// (notifications/design.md: "`NotificationEvent` は通知生成イベント...
/// 上流が emit する不変ペイロード"). `recipient`/`origin` are
/// [`AccountRef`] (core-runtime's shared domain primitive, imported from
/// `crate::domain` — not redefined here); `target_status_id` is `None` for
/// status-less kinds (`Follow`/`FollowRequest` — not emitted by this
/// module, listed for shape completeness only).
#[derive(Debug, Clone, PartialEq)]
pub struct NotificationEvent {
    pub recipient: AccountRef,
    pub origin: AccountRef,
    pub kind: NotificationType,
    pub target_status_id: Option<Id>,
    pub occurred_at: OffsetDateTime,
}

/// Upstream event-receipt seam (notifications/design.md:
/// `NotificationEventSink` — "上流イベント受領シーム"). See this module's
/// doc comment ("One deliberate shape deviation") for why this returns a
/// boxed future instead of design.md's literal `async fn` sketch.
pub trait NotificationEventSink: Send + Sync {
    fn emit<'a>(
        &'a self,
        event: NotificationEvent,
    ) -> Pin<Box<dyn Future<Output = Result<(), AppError>> + Send + 'a>>;
}

/// notifications/design.md's exact default-implementation name. Touches no
/// DB/network — a zero-field unit struct, so "既定実装はネットワーク/DB に
/// 触れず何もしない" (design.md's own postcondition) holds structurally,
/// not just behaviorally, mirroring `accounts::ports::EmptyStatusesProvider`'s
/// identical proof shape.
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

/// The runtime-replaceable registry (see this module's doc comment,
/// "Registry: one replaceable slot"). Held by [`crate::statuses::StatusService`]/
/// [`crate::statuses::interaction_service::InteractionService`], defaulting
/// every fresh instance to [`NoopSink`] until a future notifications
/// bootstrap calls [`NotificationSinkRegistry::set_sink`].
#[derive(Clone)]
pub struct NotificationSinkRegistry {
    sink: Arc<RwLock<Arc<dyn NotificationEventSink>>>,
}

impl Default for NotificationSinkRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl NotificationSinkRegistry {
    /// Builds a registry defaulting to [`NoopSink`] — no notifications
    /// implementation is reachable from a freshly built registry until
    /// [`set_sink`](Self::set_sink) is called.
    pub fn new() -> Self {
        Self {
            sink: Arc::new(RwLock::new(Arc::new(NoopSink))),
        }
    }

    /// Replaces the registered [`NotificationEventSink`] (a future
    /// notifications bootstrap's own registration entry point, mirroring
    /// `AccountPortsRegistry::set_statuses_provider`). `&self`, not
    /// `&mut self` — see this module's doc comment.
    pub fn set_sink(&self, sink: Arc<dyn NotificationEventSink>) {
        *self
            .sink
            .write()
            .expect("NotificationSinkRegistry lock must not be poisoned") = sink;
    }

    /// Delegates `event` to the currently registered sink (the built-in
    /// [`NoopSink`] until a future notifications bootstrap replaces it).
    pub async fn emit(&self, event: NotificationEvent) -> Result<(), AppError> {
        let sink = self
            .sink
            .read()
            .expect("NotificationSinkRegistry lock must not be poisoned")
            .clone();
        sink.emit(event).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_event() -> NotificationEvent {
        NotificationEvent {
            recipient: AccountRef::Local(Id::from_i64(1)),
            origin: AccountRef::Local(Id::from_i64(2)),
            kind: NotificationType::Favourite,
            target_status_id: Some(Id::from_i64(3)),
            occurred_at: OffsetDateTime::now_utc(),
        }
    }

    #[tokio::test]
    async fn noop_sink_returns_ok_and_touches_nothing() {
        let sink = NoopSink;
        assert!(sink.emit(sample_event()).await.is_ok());
    }

    #[tokio::test]
    async fn registry_defaults_to_noop_when_nothing_registered() {
        let registry = NotificationSinkRegistry::new();
        assert!(registry.emit(sample_event()).await.is_ok());
    }

    struct RecordingSink {
        events: std::sync::Mutex<Vec<NotificationEvent>>,
    }

    impl RecordingSink {
        fn new() -> Self {
            Self {
                events: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    impl NotificationEventSink for RecordingSink {
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

    #[tokio::test]
    async fn registry_uses_a_registered_sink_instead_of_the_default() {
        let registry = NotificationSinkRegistry::new();
        let sink = Arc::new(RecordingSink::new());
        registry.set_sink(Arc::clone(&sink) as Arc<dyn NotificationEventSink>);

        registry.emit(sample_event()).await.unwrap();

        let events = sink.events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, NotificationType::Favourite);
    }

    #[tokio::test]
    async fn registry_clone_shares_the_same_slot() {
        let registry = NotificationSinkRegistry::new();
        let clone = registry.clone();
        let sink = Arc::new(RecordingSink::new());
        registry.set_sink(Arc::clone(&sink) as Arc<dyn NotificationEventSink>);

        // Registering on `registry` must be visible through `clone` too —
        // proves the two share one underlying slot, not independent copies.
        clone.emit(sample_event()).await.unwrap();
        assert_eq!(sink.events.lock().unwrap().len(), 1);
    }
}
