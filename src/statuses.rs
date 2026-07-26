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
//! - Task 5.2 (`Boundary: InteractionService`): [`interaction_service`]
//!   ([`interaction_service::InteractionService`]: reblog/favourite/
//!   bookmark/pin orchestration — visibility gate, duplicate prevention,
//!   counter update, and Announce/Like/Undo dispatch for reblog/favourite,
//!   local-only state for bookmark/pin — see [`interaction_service`]'s own
//!   doc comment for its documented boundary decisions).
//!
//! - Task 5.3 (`Boundary: PollService`): [`poll_service`]
//!   ([`poll_service::PollService`]: poll get/vote orchestration — a poll's
//!   owning status visibility gate, delegating deadline/range/single-vs-
//!   multiple/duplicate validation to `poll_repository::record_vote`,
//!   tally reflection, and vote-Activity dispatch via
//!   `StatusActivityBuilder::deliver_vote` (Requirements 13.2-13.6) — see
//!   [`poll_service`]'s own doc comment for why poll *creation*
//!   (Requirement 13.1) stays out of this task's scope and
//!   `status_service.rs` is left untouched).
//!
//! - Task 6.1 (`Boundary: InboundHandlers`): [`inbound_handlers`]
//!   ([`inbound_handlers::CreateNoteHandler`]/[`inbound_handlers::AnnounceHandler`]/
//!   [`inbound_handlers::LikeHandler`]/[`inbound_handlers::DeleteHandler`]/
//!   [`inbound_handlers::UpdateHandler`]/[`inbound_handlers::UndoHandler`] —
//!   federation-core's `InboundActivityHandler` implemented for the six
//!   post-related inbound Activity kinds, each calling the exact same
//!   repository functions the corresponding local-origin service already
//!   calls (Requirement 14.5) — plus
//!   [`inbound_handlers::register_status_handlers`], which registers all six
//!   against an `InboundActivityDispatcher`. Adds two small additive
//!   widenings this task's own boundary permits: `status_repository::find_by_uri`
//!   (a thin `pub` uri-keyed lookup, mirroring `find_by_id`'s existing
//!   precedent) and `status_service::extract_content_tokens`/`ExtractedTokens::hashtags`
//!   widened to `pub(crate)` (so inbound `Create(Note)` hashtag persistence
//!   reuses the exact same extraction function local-origin `create_status`
//!   already uses) — see [`inbound_handlers`]'s own doc comment for the full
//!   rationale, including its documented cross-spec dependency on
//!   accounts-and-instance's already-implemented `RemoteAccountFetcher` for
//!   resolving a remote actor's `actor_uri` to a stable `Id`.
//!
//!   Still no HTTP surface, and `register_status_handlers` is not yet wired
//!   into `crate::state::AppState`/`crate::bootstrap`/`crate::server` (task
//!   7.2's boundary) — this module remains a standalone, independently
//!   unit-testable set of handlers with no live caller yet. See design.md's
//!   "File Structure Plan" for the full planned module set.
//!
//! - Task 6.2 (`Boundary: StatusIngestService`, `_Depends: 6.1_`):
//!   [`ingest_service`] ([`ingest_service::StatusIngestService`]: a remote
//!   Note "URL/document → Status" ingestion entry point callable from
//!   outside federation-core's inbound Activity dispatch, e.g. a future
//!   search spec's `RemoteResolver`). Reuses
//!   [`inbound_handlers::ingest_note_object`] (a `pub(crate)` function
//!   extracted from `CreateNoteHandler`'s own inbound `Create(Note)` path by
//!   this task) verbatim, so this entry point produces identical results to
//!   the inbound-dispatch path for the same input (Requirement 14.5). Not
//!   wired into `AppState`/bootstrap/any live HTTP path or the `search` spec
//!   itself (which does not exist yet in this codebase) — see
//!   [`ingest_service`]'s own doc comment for the full contract.

pub mod activity_builder;
pub mod addressing;
pub mod idempotency;
pub mod inbound_handlers;
pub mod ingest_service;
pub mod interaction_repository;
pub mod interaction_service;
pub mod model;
pub mod poll_repository;
pub mod poll_service;
pub mod serializer;
pub mod status_repository;
pub mod status_service;
pub mod tag_repository;
pub mod visibility;

pub use model::{IdempotencyRecord, Poll, PollOption, PollVote, Status, StatusEdit, Tag};
