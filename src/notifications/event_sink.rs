//! `NotificationEventSink` real implementation (design.md File Structure
//! Plan: "`event_sink.rs` — NotificationEventSink 本実装（NotificationEvent
//! を受け Generator へ流す）"; Requirements 5.1, 5.2, 6.1, 6.2, 6.3, 6.4,
//! 6.5, 6.6; task 3.1, `Boundary: NotificationEventSink`).
//!
//! Scope: this module owns the real (non-`NoopSink`) implementation of
//! [`crate::notifications::ports::NotificationEventSink`] — [`GeneratorEventSink`]
//! — which routes an already-canonical [`NotificationEvent`] straight into
//! the single generation point, [`crate::notifications::NotificationGenerator::generate`]
//! (task 2.4). It also owns the bridge that makes that routing actually
//! reachable from the two upstream emitters that exist today,
//! [`StatusesEventSinkAdapter`], plus the field-by-field conversion between
//! `statuses::notification_sink`'s placeholder event/kind types and this
//! spec's canonical [`model`] types. No `NotificationModule` composition,
//! no `AppState`/`bootstrap.rs` registration, and no `NotificationService`
//! (task 3.2) live here — see this module's own doc comment section
//! "Registration is task 4.2's job, not this module's" below.
//!
//! ## Why two sinks, not one
//! statuses-core (task 9.2) and social-graph both emit through
//! `crate::statuses::notification_sink::NotificationSinkRegistry`, whose
//! swap-in slot is typed `Arc<dyn crate::statuses::notification_sink::NotificationEventSink>`
//! — a *different* trait, over a *different* (though field-identical)
//! event/kind pair, than this spec's own canonical
//! [`crate::notifications::ports::NotificationEventSink`] /
//! [`NotificationEvent`] (see `src/statuses/notification_sink.rs`'s own doc
//! comment and `src/notifications/model.rs`'s "Relationship to
//! `statuses::notification_sink`" section — both call out this exact
//! forward-declaration precedent and that migrating those call sites onto
//! this spec's types is deliberately deferred).
//!
//! Rather than rewriting `statuses::notification_sink`'s and
//! `social_graph::transitions`'s many call sites onto this spec's own
//! types (explicitly out of this task's boundary — `NotificationEventSink`
//! only, not `StatusService`/`InteractionService`/`social_graph`), this
//! module supplies an *adapter* that speaks the trait upstream already
//! calls through and converts+forwards onward:
//!
//! - [`GeneratorEventSink`] implements this spec's own canonical
//!   [`crate::notifications::ports::NotificationEventSink`] — the literal
//!   "NotificationEvent を受け Generator へ流す" this task's own name
//!   describes — by forwarding straight to
//!   [`crate::notifications::NotificationGenerator::generate`].
//! - [`StatusesEventSinkAdapter`] implements
//!   `crate::statuses::notification_sink::NotificationEventSink` — the
//!   trait `NotificationSinkRegistry`'s slot is actually typed for, and
//!   therefore the trait a real registration (task 4.2) must supply — by
//!   converting the incoming `statuses::notification_sink::NotificationEvent`
//!   into this spec's canonical [`NotificationEvent`] (via `From`, see
//!   below) and delegating to whichever canonical
//!   [`crate::notifications::ports::NotificationEventSink`] it wraps
//!   (ordinarily a [`GeneratorEventSink`], but kept generic over
//!   `Arc<dyn NotificationEventSink>` so the conversion/delegation logic is
//!   unit-testable against a fake canonical sink without needing a real
//!   `NotificationGenerator`/`PgPool` at all — see this module's test
//!   module).
//!
//! Composing `StatusesEventSinkAdapter::new(Arc::new(GeneratorEventSink::new(generator)))`
//! is exactly the "既定 no-op を差し替えた本実装" this task's own
//! completion definition names: local-origin emits (`StatusService`/
//! `InteractionService`, holding `NotificationSinkRegistry` directly) and
//! remote-received emits (`inbound_handlers.rs`, `social_graph::transitions`,
//! holding a clone of that exact same registry instance per
//! `src/bootstrap.rs`'s single shared construction) both flow through the
//! identical `Arc<dyn statuses::notification_sink::NotificationEventSink>`
//! slot and therefore both reach the identical adapter/generator pair once
//! registered — this is what gives Requirements 5.1/5.2's "ローカル発生・
//! リモート受信のいずれの経路から emit したイベントも単一のジェネレータへ
//! 流す" and 6.1-6.6's per-kind coverage, without this module adding any
//! new emit call site of its own (the six kinds are already emitted
//! upstream by earlier tasks' work).
//!
//! ## Registration is task 4.2's job, not this module's
//! design.md's File Structure Plan assigns "NotificationEventSink 本実装
//! （NotificationEvent を受け Generator へ流す）" to this file
//! (`event_sink.rs`) but assigns the act of *registering* that real
//! implementation into a live registry inside `AppState`/`bootstrap.rs` to
//! its Modified Files section, and `tasks.md`'s own task 4.2 text names
//! that registration step explicitly ("イベントシンク本実装をレジストリへ
//! 登録（上流既定 no-op を差し替え）") as part of "モジュール配線" —
//! distinct from and depending on this task (`_Depends: 3.1_`). This
//! module therefore supplies real, constructible, real-behavior types
//! (`GeneratorEventSink`, `StatusesEventSinkAdapter`) and proves via its
//! own tests that composing and calling them actually reaches the
//! generator — but it does not itself touch `src/state.rs`/
//! `src/bootstrap.rs` to call `NotificationSinkRegistry::set_sink`/
//! `NotificationPortsRegistry::set_event_sink` against a live `AppState`.
//! This is the deliberately narrower/conservative reading of this task's
//! own ambiguity note; see this task's status report CONCERNS.
//!
//! ## Type conversion: verified field-identical, not just assumed
//! `statuses::notification_sink::NotificationType`/`NotificationEvent` were
//! deliberately copied verbatim from this spec's own design.md Service
//! Interface excerpt (that module's own doc comment: "Field shapes are
//! copied verbatim... so a future notifications implementation can
//! adopt/migrate this module's types without a breaking rewrite"). Compared
//! field-by-field against this spec's [`model::NotificationType`]/
//! [`NotificationEvent`] as of this task: both enums list the identical
//! eight variants in the identical order (`Mention`, `Follow`,
//! `FollowRequest`, `Favourite`, `Reblog`, `Poll`, `Status`, `Update`), and
//! both event structs have the identical five fields with identical types
//! (`recipient: AccountRef`, `origin: AccountRef`, `kind: NotificationType`,
//! `target_status_id: Option<Id>`, `occurred_at: OffsetDateTime`) — so the
//! `From` impls below are a plain 1:1 field copy, not a lossy or
//! best-effort mapping. If a future change drifts the two shapes apart,
//! these `From` impls (an exhaustive `match` on the kind, no wildcard arm)
//! will fail to compile until updated, the same closed-set proof technique
//! `model.rs`'s own tests already use.

#[cfg(test)]
mod tests;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::error::AppError;
use crate::notifications::generator::NotificationGenerator;
use crate::notifications::model::{NotificationEvent, NotificationType};
use crate::notifications::ports::NotificationEventSink;
use crate::statuses::notification_sink::{
    NotificationEvent as StatusesNotificationEvent,
    NotificationEventSink as StatusesNotificationEventSink,
    NotificationType as StatusesNotificationType,
};

/// Field-by-field conversion from `statuses::notification_sink`'s
/// placeholder kind enum into this spec's canonical [`NotificationType`].
/// See this module's doc comment, "Type conversion", for why this is a
/// verified 1:1 mapping, not a best-effort one.
impl From<StatusesNotificationType> for NotificationType {
    fn from(kind: StatusesNotificationType) -> Self {
        match kind {
            StatusesNotificationType::Mention => NotificationType::Mention,
            StatusesNotificationType::Follow => NotificationType::Follow,
            StatusesNotificationType::FollowRequest => NotificationType::FollowRequest,
            StatusesNotificationType::Favourite => NotificationType::Favourite,
            StatusesNotificationType::Reblog => NotificationType::Reblog,
            StatusesNotificationType::Poll => NotificationType::Poll,
            StatusesNotificationType::Status => NotificationType::Status,
            StatusesNotificationType::Update => NotificationType::Update,
        }
    }
}

/// Field-by-field conversion from `statuses::notification_sink`'s
/// placeholder event type into this spec's canonical [`NotificationEvent`]
/// (see this module's doc comment, "Type conversion").
impl From<StatusesNotificationEvent> for NotificationEvent {
    fn from(event: StatusesNotificationEvent) -> Self {
        NotificationEvent {
            recipient: event.recipient,
            origin: event.origin,
            kind: event.kind.into(),
            target_status_id: event.target_status_id,
            occurred_at: event.occurred_at,
        }
    }
}

/// The real (non-`NoopSink`) implementation of this spec's own canonical
/// [`NotificationEventSink`] — routes an already-canonical
/// [`NotificationEvent`] straight into the single generation point,
/// [`NotificationGenerator::generate`] (task 2.4). This is the literal
/// artifact design.md's File Structure Plan names for `event_sink.rs`
/// ("NotificationEvent を受け Generator へ流す").
pub struct GeneratorEventSink {
    generator: Arc<NotificationGenerator>,
}

impl GeneratorEventSink {
    /// Builds a sink bound to `generator` — the single generation point
    /// every routed event is handed to.
    pub fn new(generator: Arc<NotificationGenerator>) -> Self {
        Self { generator }
    }
}

impl NotificationEventSink for GeneratorEventSink {
    fn emit<'a>(
        &'a self,
        event: NotificationEvent,
    ) -> Pin<Box<dyn Future<Output = Result<(), AppError>> + Send + 'a>> {
        Box::pin(async move {
            let outcome = self.generator.generate(event).await?;
            tracing::debug!(
                ?outcome,
                "notification event routed to the single generation point"
            );
            Ok(())
        })
    }
}

/// Bridges the two upstream emitters' actual call-through trait
/// (`crate::statuses::notification_sink::NotificationEventSink`, what
/// `NotificationSinkRegistry`'s swap-in slot is typed for) onto this
/// spec's own canonical [`NotificationEventSink`] port, by converting the
/// incoming placeholder event (`From<StatusesNotificationEvent> for
/// NotificationEvent`, above) and delegating to whichever canonical sink
/// it wraps. See this module's doc comment, "Why two sinks, not one", for
/// why this adapter — rather than migrating upstream call sites onto this
/// spec's own types directly — is this task's chosen shape.
pub struct StatusesEventSinkAdapter {
    inner: Arc<dyn NotificationEventSink>,
}

impl StatusesEventSinkAdapter {
    /// Builds an adapter that converts-and-forwards onto `inner` (ordinarily
    /// a [`GeneratorEventSink`], but kept generic over the canonical trait
    /// object so this adapter's own conversion/delegation logic is
    /// unit-testable against a fake canonical sink — see this module's test
    /// module).
    pub fn new(inner: Arc<dyn NotificationEventSink>) -> Self {
        Self { inner }
    }
}

impl StatusesNotificationEventSink for StatusesEventSinkAdapter {
    fn emit<'a>(
        &'a self,
        event: StatusesNotificationEvent,
    ) -> Pin<Box<dyn Future<Output = Result<(), AppError>> + Send + 'a>> {
        Box::pin(async move { self.inner.emit(event.into()).await })
    }
}
