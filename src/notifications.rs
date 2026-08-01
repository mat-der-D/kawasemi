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
//!
//! This file will eventually become the `NotificationModule` composition
//! point (design.md's File Structure Plan: "`src/notifications.rs` —
//! NotificationModule 組み立て・公開・ルータ装着点・EventSink/DeliverySink
//! 登録") once later tasks (2.x-4.x: `repository`, `ports`, `filter`,
//! `generator`, `event_sink`, `serializer`, `service`, `endpoints`, and
//! their wiring) land. For this task (`Boundary: model`), it declares only
//! the `model` submodule and re-exports its types — no repository, port,
//! filter, generator, serializer, service, endpoint, or wiring code exists
//! yet.

pub mod model;

pub use model::{Notification, NotificationEvent, NotificationType};
