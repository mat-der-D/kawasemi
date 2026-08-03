//! `NotificationGenerator` (design.md "Generation / 生成層" ->
//! `NotificationGenerator`, Requirements 5.1, 5.2, 5.3, 5.5, 6.1, 6.2, 6.3,
//! 6.4, 6.5, 6.6, 8.1, 8.2; task 2.4, `Boundary: NotificationGenerator`).
//!
//! Scope: this module owns exactly [`NotificationGenerator::generate`] —
//! the **single generation point** design.md's Architecture Integration
//! names as this spec's most important structural property: recipient-
//! local-check -> filter (delegates to [`crate::notifications::filter::NotificationFilter`],
//! task 2.3) -> kind-agnostic event-to-notification mapping -> dedup-
//! idempotent persistence (delegates to
//! [`crate::notifications::repository::insert_dedup`], task 1.3) ->
//! new-only delivery hand-off (delegates to
//! [`crate::notifications::ports::NotificationDeliverySink`], task 2.2).
//! This module implements no filtering logic, no dedup logic, and no
//! delivery logic of its own — it only sequences already-built collaborators
//! in design.md's own sequence-diagram order (System Flows, "通知生成（単
//! 一生成点...）"). No `NotificationEventSink` real implementation (task
//! 3.1, the upstream-facing caller of this generator), no `NotificationModule`
//! wiring/`AppState` registration (task 4.2/3.1), and no `model.rs`/
//! `repository.rs`/`filter.rs`/`ports.rs` changes live here.
//!
//! ## Control flow (design.md's sequence diagram, followed verbatim)
//! 1. `event.recipient` is checked for `AccountRef::Local` **before any
//!    other collaborator is touched** — a `Remote` recipient short-circuits
//!    to [`GenerateOutcome::SkippedNonLocal`] without calling
//!    `NotificationFilter`, `insert_dedup`, or the delivery sink at all
//!    (Requirement 5.3, design.md: "alt recipient not local -> skip no
//!    notification"). This is what makes the non-local branch provably
//!    DB-independent (see this module's own test module's doc comment).
//! 2. `NotificationFilter::should_suppress` decides block/notification-mute
//!    suppression (Requirement 7.x, task 2.3's own boundary — this module
//!    performs no relationship-state judgment of its own). A suppressed
//!    event returns [`GenerateOutcome::Suppressed`] without ever calling
//!    `insert_dedup` or the delivery sink.
//! 3. The event is mapped to a [`Notification`] — see "Event-to-notification
//!    mapping" below.
//! 4. [`crate::notifications::repository::insert_dedup`] persists it
//!    idempotently (Requirement 8.1, 8.2). This module adds **no**
//!    additional existence pre-check of its own before calling it — the
//!    task instruction is explicit that doing so would be redundant and
//!    racy against `insert_dedup`'s own `ON CONFLICT ... DO NOTHING`, the
//!    single source of truth for "is this a duplicate".
//! 5. Only on [`crate::notifications::repository::InsertOutcome::Created`]
//!    is the persisted notification handed to the configured
//!    [`NotificationDeliverySink`] (Requirement 5.5, "新規時のみ配信シーク
//!    引き渡し") — see "Delivery failure isolation" below for why a
//!    `deliver` error never becomes this method's own `Err`.
//!
//! ## Event-to-notification mapping (Requirements 6.1-6.6)
//! [`NotificationEvent`] and [`Notification`] were deliberately designed
//! with parallel shapes in task 1.2 (`model.rs`'s own doc comment: "this
//! matches ... verbatim by construction"), so this module needs exactly one
//! generic, kind-agnostic mapping rather than a six-armed `match` per kind:
//! `event.recipient` (checked `Local`) -> `notification.recipient_id`,
//! `event.origin` -> `notification.origin` (kept as the full
//! [`AccountRef`] — the local/remote split into separate
//! `origin_kind`/`origin_id` columns is `repository.rs`'s own SQL-layer
//! concern, not reproduced here), `event.kind` -> `notification.kind`,
//! `event.target_status_id` -> `notification.status_id`, plus
//! `dismissed: false` (a freshly generated notification is never
//! pre-dismissed) and `id`/`created_at` freshly minted via
//! [`RuntimeContext`] (see "Determinism" below). Because this mapping does
//! not special-case any one kind, it covers all eight [`NotificationType`]
//! variants identically — including `Status`/`Update`, whose upstream
//! emitters are wired in a later task (3.1) — while this task's own test
//! module still exercises the six kinds this task's completion state
//! enumerates explicitly (`Favourite`/`Reblog`/`Mention`/`Follow`/
//! `FollowRequest`/`Poll`).
//!
//! ## Determinism: `created_at`/`id` come from `RuntimeContext`, never
//! `event.occurred_at`
//! [`NotificationEvent::occurred_at`] records when the *upstream* action
//! happened (design.md's model doc); [`Notification::created_at`] records
//! when *this notification* was generated — this generator's own act, at
//! this generator's own moment, per this crate's crate-wide determinism
//! rule ("`RuntimeContext`（`Clock`/`IdGenerator`）must be used for
//! `Notification.id`/`created_at` — no direct wall-clock/random calls").
//! `self.runtime.clock.now()`/`self.runtime.ids.next_id()` are therefore
//! called at generation time rather than copying `event.occurred_at` (which
//! would conflate "when the underlying action happened" with "when the
//! notification row was minted" — two different instants in general, e.g.
//! if event delivery to this generator were ever queued/retried).
//!
//! ## Delivery failure isolation (Requirement 5.5's Error Handling note)
//! design.md's Error Handling section states delivery-seam failure is a
//! downstream concern that must not desynchronize from generation's own
//! success/failure ("配信シームの失敗は後段の関心であり、生成の成否に同期
//! しない"). This module honors that by treating
//! [`NotificationDeliverySink::deliver`]'s `Result` as fire-and-forget on
//! its `Err` arm: a delivery failure is logged via `tracing::warn!` (this
//! crate's established diagnostic convention, `src/error.rs`'s own doc
//! comment: "a 5xx log emitted... nests inside whatever request span is
//! active") and then discarded — `generate` still returns
//! `Ok(GenerateOutcome::Created)`, since the notification itself was
//! already durably persisted by the time `deliver` is even called.
//!
//! ## Why `Arc<dyn NotificationDeliverySink>`, not `NotificationPortsRegistry`
//! design.md's own Components table lists `NotificationGenerator`'s
//! dependencies as "NotificationFilter, NotificationRepository,
//! DeliverySink, RuntimeContext" — `DeliverySink` singular, not the ports
//! module's combined event-sink-and-delivery-sink registry. This generator
//! never emits (only receives, from a future `NotificationEventSink`
//! implementation task 3.1 builds), so it has no use for the registry's
//! `event_sink` slot or its `emit`/`set_event_sink` surface; taking the
//! narrower `Arc<dyn NotificationDeliverySink>` directly (constructible
//! from `NotificationPortsRegistry`'s own `Arc<dyn NotificationDeliverySink>`
//! that a future wiring task, 4.2, will pass at construction) keeps this
//! module's own dependency surface exactly as wide as design.md's
//! Components table states and no wider, and keeps this type composable in
//! a unit test without needing a full registry (see this module's test
//! module).
//!
//! ## `GenerateOutcome`: a plain fieldless enum, matching design.md's
//! Service Interface comment verbatim
//! design.md's own Service Interface excerpt writes only
//! `// Created | Suppressed | Duplicate | SkippedNonLocal` as a bare
//! variant list, with no payload named for any arm. This module implements
//! that literally — [`GenerateOutcome`] carries no data. A caller that
//! needs the persisted notification on the `Created` path (e.g. a future
//! `NotificationEventSink` real implementation, or this module's own
//! tests) re-fetches it via [`crate::notifications::repository::find_for_recipient`]
//! /[`crate::notifications::repository::list`] using the same dedup key it
//! already knows from the event it sent in, exactly like
//! `InsertOutcome::Duplicate`'s own already-established "no payload on the
//! non-primary arm" precedent in `repository.rs`.

#[cfg(test)]
mod tests;

use std::sync::Arc;

use sqlx::postgres::PgPool;

use crate::domain::AccountRef;
use crate::error::AppError;
use crate::notifications::filter::NotificationFilter;
use crate::notifications::model::{Notification, NotificationEvent};
use crate::notifications::ports::NotificationDeliverySink;
use crate::notifications::repository::{self, InsertOutcome};
use crate::runtime::RuntimeContext;

/// Outcome of [`NotificationGenerator::generate`] (design.md's own Service
/// Interface comment, reproduced verbatim as this enum's variant list — see
/// this module's doc comment, "`GenerateOutcome`: a plain fieldless enum").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerateOutcome {
    /// A new, not-previously-existing (per [`InsertOutcome::Created`])
    /// notification was persisted and handed to the configured
    /// [`NotificationDeliverySink`] (Requirement 5.5).
    Created,
    /// [`NotificationFilter::should_suppress`] reported this event should
    /// be suppressed (block/notification-mute) — nothing was persisted or
    /// delivered (Requirement 7.x).
    Suppressed,
    /// A not-yet-dismissed notification with the identical dedup key
    /// already existed (per [`InsertOutcome::Duplicate`]) — nothing new was
    /// persisted or delivered (Requirement 8.1, 8.2).
    Duplicate,
    /// `event.recipient` was not [`AccountRef::Local`] — no filter/insert/
    /// deliver call was made at all (Requirement 5.3).
    SkippedNonLocal,
}

/// The single notification generation point (design.md "Generation / 生成
/// 層" -> `NotificationGenerator`; see this module's doc comment for the
/// exact control flow [`NotificationGenerator::generate`] follows).
pub struct NotificationGenerator {
    pool: PgPool,
    filter: NotificationFilter,
    delivery_sink: Arc<dyn NotificationDeliverySink>,
    runtime: RuntimeContext,
}

impl NotificationGenerator {
    /// Builds a generator bound to `pool` (passed straight through to
    /// [`crate::notifications::repository::insert_dedup`]), `filter`
    /// (task 2.3's suppression decision), `delivery_sink` (see this
    /// module's doc comment, "Why `Arc<dyn NotificationDeliverySink>`"),
    /// and `runtime` (id/time minting, see "Determinism").
    pub fn new(
        pool: PgPool,
        filter: NotificationFilter,
        delivery_sink: Arc<dyn NotificationDeliverySink>,
        runtime: RuntimeContext,
    ) -> Self {
        Self {
            pool,
            filter,
            delivery_sink,
            runtime,
        }
    }

    /// Runs `event` through the single generation point (design.md's
    /// sequence diagram; see this module's doc comment, "Control flow", for
    /// the exact step-by-step ordering and short-circuit conditions).
    pub async fn generate(&self, event: NotificationEvent) -> Result<GenerateOutcome, AppError> {
        // Step 1 (Requirement 5.3): recipient-local check, before touching
        // the filter, the repository, or the delivery sink at all.
        let recipient_id = match event.recipient {
            AccountRef::Local(id) => id,
            AccountRef::Remote(_) => return Ok(GenerateOutcome::SkippedNonLocal),
        };

        // Step 2 (Requirements 7.1-7.4, delegated entirely to
        // `NotificationFilter`): suppression short-circuits before any
        // persistence or delivery call.
        if self
            .filter
            .should_suppress(&event.recipient, &event.origin)
            .await?
        {
            return Ok(GenerateOutcome::Suppressed);
        }

        // Step 3 (Requirements 6.1-6.6): kind-agnostic event-to-notification
        // mapping — see this module's doc comment, "Event-to-notification
        // mapping".
        let notification = Notification {
            id: self.runtime.ids.next_id(),
            recipient_id,
            kind: event.kind,
            origin: event.origin,
            status_id: event.target_status_id,
            dismissed: false,
            created_at: self.runtime.clock.now(),
        };

        // Step 4 (Requirements 8.1, 8.2): the single source of truth for
        // "is this a duplicate" is `insert_dedup`'s own `ON CONFLICT`
        // outcome — no redundant pre-check is added here (see this
        // module's doc comment, "Control flow", step 4).
        match repository::insert_dedup(&self.pool, &notification).await? {
            InsertOutcome::Duplicate => Ok(GenerateOutcome::Duplicate),
            InsertOutcome::Created(created) => {
                // Step 5 (Requirement 5.5): new-only delivery hand-off. A
                // delivery failure is logged and swallowed — see this
                // module's doc comment, "Delivery failure isolation".
                if let Err(err) = self.delivery_sink.deliver(&created).await {
                    tracing::warn!(
                        status = %err.status,
                        message = %err.public_message,
                        "notification delivery sink failed; generation itself already \
                         succeeded and is not affected (Requirement 5.5)"
                    );
                }
                Ok(GenerateOutcome::Created)
            }
        }
    }
}
