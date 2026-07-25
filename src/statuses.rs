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
//! - Task 2.1 (`Boundary: StatusRepository, TagRepository`): [`status_repository`]
//!   (`Status`/`StatusEdit` persistence: insert, visible-scope fetch,
//!   ancestor/descendant traversal, delete with its two explicit
//!   self-referential cleanup steps, edit-apply + history, atomic counter
//!   updates) and [`tag_repository`] (hashtag persistence against `tags` /
//!   `status_tags`, and its tag<->status read boundary).
//!
//! - Task 2.2 (`Boundary: InteractionRepository`): [`interaction_repository`]
//!   (favourite/bookmark/pin record/revoke/exists against `favourites` /
//!   `bookmarks` / `pins`, the bookmark list's own creation-order cursor,
//!   and reblog's read-only duplicate-check against `statuses` — reblog
//!   record/revoke itself stays in [`status_repository`], see
//!   [`interaction_repository`]'s own doc comment).
//!
//! - Task 2.3 (`Boundary: PollRepository, IdempotencyStore`): [`poll_repository`]
//!   (`Poll`/`PollOption`/`PollVote` persistence: poll/option insertion, vote
//!   recording with deadline/range/single-vs-multiple/duplicate validation,
//!   and aggregate tally retrieval against `polls` / `poll_options` /
//!   `poll_votes`) and [`idempotency`] (the `Idempotency-Key` ledger:
//!   `(actor_id, key)` -> `status_id` lookup and resend resolution against
//!   `status_idempotency_keys`).
//!
//! - Task 3.1 (`Boundary: VisibilityPolicy, RelationshipQuery(port)`):
//!   [`visibility`] (the single visibility judgment
//!   [`visibility::is_visible`] that retrieval/context/interaction
//!   visibility checks are meant to funnel through, plus the
//!   [`visibility::RelationshipQuery`] delegation-port contract and its
//!   safe default [`visibility::NoRelationshipQuery`]). Does not yet wire
//!   `status_repository`'s queries to call [`visibility::is_visible`] —
//!   that remains `status_repository`'s own documented temporary stand-in
//!   until a later task replaces it.
//!
//! - Task 3.2 (`Boundary: Addressing`): [`addressing`] (the single
//!   `Visibility` -> `to`/`cc`/recipient derivation,
//!   [`addressing::derive_addressing`] / [`addressing::derive_recipients`],
//!   that local origination and remote delivery both funnel through — see
//!   [`addressing`]'s own doc comment for the `ActorRef` type it defines
//!   and the `to`/`cc` placement convention it follows).
//!
//!   No Activity generation (`StatusActivityBuilder`), no serializers
//!   (`StatusSerializer` / `PollSerializer`), no services (`StatusService` /
//!   `InteractionService` / `PollService`), no inbound handlers, and no HTTP
//!   surface exist yet — this module is not wired into
//!   `crate::state::AppState`/`crate::bootstrap`/`crate::server` yet. See
//!   design.md's "File Structure Plan" for the full planned module set.

pub mod addressing;
pub mod idempotency;
pub mod interaction_repository;
pub mod model;
pub mod poll_repository;
pub mod status_repository;
pub mod tag_repository;
pub mod visibility;

pub use model::{IdempotencyRecord, Poll, PollOption, PollVote, Status, StatusEdit, Tag};
