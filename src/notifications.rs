//! Notifications domain module (notifications spec, made up of
//! `src/notifications.rs` and `src/notifications/`, mirroring the
//! module-with-submodule convention established by `src/media.rs`/
//! `src/media/`, `src/accounts.rs`/`src/accounts/`,
//! `src/statuses.rs`/`src/statuses/`, and
//! `src/social_graph.rs`/`src/social_graph/`).
//!
//! Scope so far:
//! - Task 1.1 (`Boundary: NotificationRepository`, no Rust code):
//!   `migrations/0009_notifications.sql` — the `notifications` table, its
//!   dedup partial unique index (未消去限定), and its recipient cursor
//!   index.
//! - Task 1.2 (`Boundary: model`): the domain value types design.md's
//!   "model" component names — [`model::NotificationType`],
//!   [`model::Notification`], and [`model::NotificationEvent`]. `Id`/
//!   `AccountRef` are not redefined here: both are imported from
//!   `crate::domain` (core-runtime's canonical shared primitives module,
//!   mirroring `src/statuses/model.rs`'s and `src/social_graph/model.rs`'s
//!   identical precedent) — see [`model`]'s own doc comment.
//! - Task 1.3 (`Boundary: NotificationRepository`): the notification's own
//!   persistence — [`repository::insert_dedup`] (dedup-idempotent insert),
//!   [`repository::list`] (recipient-scoped/dismissed-excluded/cursor-
//!   paginated/type-and-account-filtered), [`repository::find_for_recipient`]
//!   (single fetch), [`repository::dismiss`], [`repository::clear`], plus
//!   [`repository::InsertOutcome`]/[`repository::ListFilter`] — against
//!   `notifications` (`migrations/0009_notifications.sql`, task 1.1). See
//!   [`repository`]'s own doc comment.
//! - Task 2.1 (`Boundary: NotificationSerializer`): the Notification JSON
//!   outer shell — [`serializer::to_notification_json`]/
//!   [`serializer::notification_to_json`], delegating the `account`/
//!   `status` embeds to accounts-and-instance's/statuses-core's own
//!   serializers rather than redefining either contract. See
//!   [`serializer`]'s own doc comment.
//! - Task 2.2 (`Boundary: ports`): the delegation seams —
//!   [`ports::NotificationEventSink`] (upstream event receipt, default
//!   [`ports::NoopSink`]) and [`ports::NotificationDeliverySink`]
//!   (downstream delivery of a persisted notification, same default), plus
//!   the swap-in registry handle [`ports::NotificationPortsRegistry`] a
//!   later task (4.2) stores inside `AppState`. See [`ports`]'s own doc
//!   comment.
//! - Task 2.3 (`Boundary: NotificationFilter`): the generation-stage
//!   block/notification-mute suppression decision —
//!   [`filter::NotificationFilter::should_suppress`], a thin consumer of
//!   social-graph's `FilterQuery::blocked_set` that reimplements no
//!   relationship state or expiry logic of its own. See [`filter`]'s own
//!   doc comment.
//!
//! This file will eventually become the `NotificationModule` composition
//! point (design.md's File Structure Plan: "`src/notifications.rs` —
//! NotificationModule 組み立て・公開・ルータ装着点・EventSink/DeliverySink
//! 登録") once later tasks (2.x-4.x: `generator`, `event_sink`, `service`,
//! `endpoints`, and their wiring) land. Declaring `pub mod ports;`/
//! `pub mod filter;` here (tasks 2.2/2.3, boundaries `ports`/
//! `NotificationFilter`) is the same minimal, precedented "add this task's
//! new submodule + its public re-exports, nothing else" touch tasks
//! 1.2/1.3/2.1 already made — no `AppState`/bootstrap/router composition or
//! any other task's boundary is touched here. See this task's own status
//! report for why this narrow addition was necessary despite this file
//! being named as a boundary exclusion.

pub mod filter;
pub mod model;
pub mod ports;
pub mod repository;
pub mod serializer;

pub use filter::NotificationFilter;
pub use model::{Notification, NotificationEvent, NotificationType};
pub use ports::{
    NoopSink, NotificationDeliverySink, NotificationEventSink, NotificationPortsRegistry,
};
pub use repository::{InsertOutcome, ListFilter};
pub use serializer::{
    NotificationJson, NotificationRenderInput, SerializeContext, notification_to_json,
    to_notification_json,
};
