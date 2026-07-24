//! Statuses domain module (statuses-core spec, `src/statuses.rs` +
//! `src/statuses/`, mirroring the module-with-submodule convention
//! established by `src/media.rs`/`src/media/` and `src/accounts.rs`/
//! `src/accounts/`).
//!
//! Scope so far:
//! - Task 1.1 (`Boundary: migration`, no Rust code): `migrations/
//!   0007_statuses.sql` — `statuses` / `status_edits` / `status_media` /
//!   `favourites` / `bookmarks` / `pins` / `polls` / `poll_options` /
//!   `poll_votes` / `status_idempotency_keys` / `tags` / `status_tags`.
//! - Task 1.2 (`Boundary: model`): the domain value types this task's own
//!   instruction enumerates — [`model::Status`], [`model::StatusEdit`],
//!   [`model::Poll`], [`model::PollOption`], [`model::PollVote`],
//!   [`model::IdempotencyRecord`], and [`model::Tag`]. `Visibility` is
//!   imported from `crate::domain` (core-runtime's canonical shared
//!   primitives module) rather than redefined — see [`model`]'s own doc
//!   comment for why `AccountRef` is not additionally imported here.
//!
//!   No delegation ports (`RelationshipQuery`, task 1.3), no repositories
//!   (`StatusRepository` / `InteractionRepository` / `PollRepository` /
//!   `IdempotencyStore`, task 2.x), no visibility/addressing logic
//!   (`VisibilityPolicy` / `Addressing`), no Activity generation
//!   (`StatusActivityBuilder`), no serializers (`StatusSerializer` /
//!   `PollSerializer`), no services (`StatusService` / `InteractionService`
//!   / `PollService`), no inbound handlers, and no HTTP surface exist yet —
//!   this module is not wired into `crate::state::AppState`/
//!   `crate::bootstrap`/`crate::server` yet. See design.md's "File
//!   Structure Plan" for the full planned module set.

pub mod model;

pub use model::{IdempotencyRecord, Poll, PollOption, PollVote, Status, StatusEdit, Tag};
