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
//!
//! This file will eventually become the `NotificationModule` composition
//! point (design.md's File Structure Plan: "`src/notifications.rs` —
//! NotificationModule 組み立て・公開・ルータ装着点・EventSink/DeliverySink
//! 登録") once later tasks (2.x-4.x: `ports`, `filter`, `generator`,
//! `event_sink`, `serializer`, `service`, `endpoints`, and their wiring)
//! land. For this task (`Boundary: NotificationRepository`), it declares
//! the `model` and `repository` submodules and re-exports their public
//! types — no port, filter, generator, serializer, service, endpoint, or
//! wiring code exists yet.

pub mod model;
pub mod repository;

pub use model::{Notification, NotificationEvent, NotificationType};
pub use repository::{InsertOutcome, ListFilter};
