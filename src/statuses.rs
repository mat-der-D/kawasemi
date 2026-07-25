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
//! - Task 3.3 (`Boundary: StatusSerializer, PollSerializer`): [`serializer`]
//!   ([`serializer::status_to_json`]/[`serializer::poll_to_json`]: the
//!   Mastodon-compatible Status/Poll JSON contract, Account/media rendering
//!   delegated to `crate::accounts::serializer`/`crate::media::serializer`,
//!   contract-harness goldens under `tests/golden/statuses/` — see
//!   [`serializer`]'s own doc comment for the pre-resolved-input carrier
//!   types it defines and why).
//!
//! - Task 4.1 (`Boundary: StatusActivityBuilder`): [`activity_builder`]
//!   ([`activity_builder::StatusActivityBuilder`]: generates the six
//!   canonical post-related Activities — `Create(Note)` / `Announce` /
//!   `Like` / `Delete` / `Update` / `Undo(Announce|Like)` — plus the
//!   Mastodon-compatible vote wire form `Create{Note, name=...}`, and hands
//!   each one, with its `Addressing`-derived recipients, to
//!   `DeliveryService::deliver` unmodified — see [`activity_builder`]'s own
//!   doc comment for the `UndoKind`/`ActorHandleLookup` gap-fills and its
//!   deliberate deviations from design.md's literal Service Interface).
//!
//! - Task 5.1 (`Boundary: StatusService`): [`status_service`]
//!   ([`status_service::StatusService`]: create/show/context/delete/edit/
//!   history/source orchestration — idempotency check, empty-post
//!   rejection, media-ownership verification, poll/media exclusivity
//!   validation, mention/tag/emoji extraction, visibility-filtered
//!   retrieval/context routed through `visibility::is_visible` rather than
//!   `status_repository`'s own provisional stand-in, and Create/Delete/
//!   Update dispatch via `StatusActivityBuilder` — see [`status_service`]'s
//!   own doc comment for its documented boundary decisions on poll
//!   handling, remote-mention resolution, and the edit/history fields the
//!   schema cannot fully retain).
//!
//!   No `InteractionService` (task 5.2) or `PollService` (task 5.3), no
//!   inbound handlers, and no HTTP surface exist yet — this module is not
//!   wired into `crate::state::AppState`/`crate::bootstrap`/`crate::server`
//!   yet. See design.md's "File Structure Plan" for the full planned module
//!   set.

pub mod activity_builder;
pub mod addressing;
pub mod idempotency;
pub mod interaction_repository;
pub mod interaction_service;
pub mod model;
pub mod poll_repository;
pub mod serializer;
pub mod status_repository;
pub mod status_service;
pub mod tag_repository;
pub mod visibility;

pub use model::{IdempotencyRecord, Poll, PollOption, PollVote, Status, StatusEdit, Tag};
