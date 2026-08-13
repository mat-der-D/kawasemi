//! `StatusService` (design.md "Service / サービス層" -> `#### StatusService`,
//! design.md lines ~538-561; Requirements 3.1, 3.2, 3.3, 3.4, 3.5, 3.6, 5.1,
//! 5.2, 5.3, 6.1, 6.2, 6.3, 6.4, 7.1, 7.2, 7.3, 7.4, 8.1, 8.2, 8.3, 8.4, 8.5;
//! task 5.1, `Boundary: StatusService`): the create/show/context/delete/
//! edit/history/source orchestration that ties together `StatusRepository`
//! (task 2.1), `IdempotencyStore` (task 2.3), `visibility::is_visible`
//! (task 3.1), `addressing::derive_addressing`/`derive_recipients` (task
//! 3.2), and `StatusActivityBuilder` (task 4.1).
//!
//! ## Scope
//! Owns exactly the seven `StatusService` methods design.md's Service
//! Interface names — [`StatusService::create_status`],
//! [`StatusService::show`], [`StatusService::context`],
//! [`StatusService::delete_status`], [`StatusService::edit_status`],
//! [`StatusService::history`], [`StatusService::source`] — plus the
//! `CreateStatus`/`EditStatus`/`StatusContext`/`StatusSource` input/output
//! types design.md's own excerpt does not spell out (see "Input/output
//! shapes" below). Does not implement `InteractionService` (task 5.2) or
//! `PollService` (task 5.3) — `create_status` validates the poll/media
//! exclusivity rule and a minimal poll shape (Requirement 13.1) and
//! persists the poll itself via `PollRepository::insert_poll` (see "Poll
//! handling" below for the full reasoning); `PollService` remains the sole
//! owner of poll *voting* (Requirements 13.2-13.6). Does
//! not implement the HTTP surface (`StatusEndpoints`, task 7.1) or call
//! `serializer::status_to_json` itself — per that module's own doc comment
//! ("mentions/tags/emojis... are all supplied as already-resolved caller
//! input"), rendering to Mastodon JSON is the endpoint layer's job, this
//! service only returns domain [`Status`]/[`StatusEdit`] values, mirroring
//! `crate::media::service::MediaService`'s identical "service returns a
//! domain struct; only the endpoint layer serializes" boundary.
//!
//! ## Replacing `status_repository`'s provisional visibility stand-in
//! (Requirements 6.1, 6.3, 6.4)
//! `status_repository.rs`'s own module doc comment ("Visible-scope filtering
//! without `VisibilityPolicy`") and `tasks.md`'s "2.1 追補"/"3.1" notes both
//! name *this* task as the one meant to route retrieval/context/history
//! visibility decisions through the real policy
//! ([`crate::statuses::visibility::is_visible`], task 3.1) instead of that
//! repository's own narrower, fail-closed `is_visible_to` stand-in. This
//! module does exactly that: [`StatusService::show`]/[`context`]/
//! [`history`] fetch the **unfiltered** row/chain
//! (`status_repository::find_by_id`/`ancestors_unfiltered`/
//! `descendants_unfiltered` — thin `pub` wrappers this task adds over
//! `status_repository.rs`'s existing private traversal helpers, an
//! additive, behavior-preserving refactor that leaves `find_visible`/
//! `ancestors`/`descendants`'s own existing signatures and tests
//! untouched) and apply [`visibility::is_visible`] themselves via
//! [`StatusService::visible_to`].
//!
//! `visibility::is_visible`'s own doc comment already folds "the post's own
//! author always sees their own post" into its first check
//! (`if viewer == Some(status.actor_id) { return true; }`), so no
//! additional "OR author" condition is needed on top of it for that half.
//! The *other* half `visibility.rs`'s own "direct visibility: author-only"
//! note calls out — "本人 OR メンション先" for a `direct` post's *mentioned*
//! (non-author) recipients — genuinely cannot be added at this layer: unlike
//! private/unlisted's follower relation (resolved live via
//! `RelationshipQuery`), a `direct` post's mention set is never persisted
//! anywhere (`Status` carries no mentions field — see `model.rs`'s own
//! exhaustive-destructure proof — and `migrations/0007_statuses.sql` has no
//! mentions table), so there is no data this service could consult to
//! answer "is `viewer` one of this post's mentioned recipients" once the
//! post is no longer the one being freshly created (mentions are only ever
//! transient, re-extracted from `content` for addressing at
//! create/delete/edit time, never stored). This is a genuine, structural gap
//! left by an earlier task's migration, not a choice this task makes — see
//! this task's own status-report CONCERNS.
//!
//! ## Input/output shapes (not fully spelled out by design.md's excerpt)
//! - [`CreateStatus`][]: `content`/`visibility`/`spoiler_text`/`sensitive`/
//!   `media_ids`/`in_reply_to_id`/`language`/`poll` — every field
//!   Requirements 3.1-3.6 and 13.1 name as part of a create request.
//! - [`EditStatus`][]: `content`/`spoiler_text`/`sensitive`/`media_ids` —
//!   deliberately **not** `language` (see "Edit's language limitation"
//!   below).
//! - [`StatusContext`][]: `ancestors`/`descendants`, matching
//!   `context()`'s own Requirement 6.2 text exactly.
//! - [`StatusSource`][]: `text`/`spoiler_text`, matching Requirement 8.3's
//!   "編集に適した素の本文（text）と spoiler_text" verbatim.
//!
//! ## Edit's language limitation (CONCERN — documented judgment call)
//! Requirement 8.1 lists 言語 (`language`) alongside content/CW/sensitive/
//! media as an editable field, but `status_repository::apply_edit`'s own
//! `UPDATE` (task 2.1, already reviewed) only ever sets
//! `content`/`spoiler_text`/`sensitive`/`edited_at` — never `language` — and
//! `status_edits` (the history table `apply_edit` archives into) has no
//! `language` column at all (`migrations/0007_statuses.sql`, already
//! applied). Changing either would mean altering an already-reviewed task
//! 2.1 function's SQL/signature (risking its own existing test suite) or a
//! migration (task 1.1's committed boundary, out of reach here) for a value
//! history could not retain either way. [`EditStatus`] therefore omits
//! `language` entirely rather than silently accepting and dropping it —
//! flagged here and in this task's status report, not guessed past.
//!
//! ## Media/history limitation (CONCERN — same schema-gap class)
//! Requirement 8.2 asks `history()` to return each version's "本文・CW・
//! sensitive・メディア・作成/編集時刻" but `status_edits` has no media
//! column/join table at all, so [`StatusEdit`] (task 1.2/2.1, unmodified
//! here) structurally cannot carry per-version media — [`history`] returns
//! exactly what `status_repository::list_edits` stores (content/CW/
//! sensitive/timestamp), the literal subset the schema supports. The
//! *live* post's media set can still be replaced on edit (Requirement 8.1),
//! via this task's own new [`crate::statuses::status_repository::replace_media`] —
//! only the *historical* per-version media snapshot is unavailable.
//!
//! ## Poll handling (Requirement 13.1 — wired by feature-level remediation)
//! [`CreateStatus::poll`] is validated (mutual exclusivity with `media_ids`;
//! at least 2 non-blank options — see "Poll creation validation" below) and
//! then genuinely persisted: [`StatusService::create_status`] mints a poll
//! id alongside the status id, sets it on the new [`Status::poll_id`],
//! inserts the status row, then builds a [`Poll`]/`Vec<PollOption>` and
//! calls [`crate::statuses::poll_repository::insert_poll`] — the same
//! status-then-poll ordering `tests/polls_it.rs::insert_poll_status_fixture`
//! already established (`statuses.poll_id` carries no FK, `polls.status_id`
//! does, so the status row can safely be inserted first). This was
//! originally deferred (task 5.1) to a not-yet-existing `PollService` task
//! 5.3; task 5.3 itself explicitly excluded 13.1 from its own scope
//! (`tasks.md`'s 5.3 Implementation Notes) and left `PollRepository::insert_poll`
//! unwired "for a future task" — this module is that future task, closing a
//! feature-level `/kiro-validate-impl` NO-GO finding. `PollService` remains
//! the sole owner of poll *voting* (Requirements 13.2-13.6); this method
//! only ever creates a poll with zero votes. The persisted `(Poll,
//! Vec<PollOption>)` is kept in hand and threaded through to
//! `deliver_create`'s `poll` parameter (Requirement 13.7, task 10.1) so the
//! outbound `Create` Activity embeds it — see "Outbound wire format" below.
//!
//! ### Poll creation validation
//! Requirement 13.1's own text ("選択肢・締切・単一/複数選択") does not spell
//! out a minimum option count, but Requirement 13.4's voting-side rejection
//! of an "範囲外の選択肢インデックス" (out-of-range option index) only makes
//! sense for a poll that already has at least 2 selectable options — a
//! 0- or 1-option poll has no meaningful index range to vote among. This
//! method therefore rejects (`422`) a poll with fewer than 2 options, or
//! any blank (whitespace-only) option title, at creation time rather than
//! silently persisting a poll no client could sensibly render or vote on.
//! No upper bound on option count is enforced — Requirement 13.1 names none,
//! and inventing one would be scope creep beyond what this requirement (or
//! any requirement adjacent to it) actually calls for.
//!
//! ### Outbound wire format: `Create` embeds poll data (Requirement 13.7,
//! task 10.1 — closed; previously a documented gap here)
//! [`deliver_create`]
//! ([`crate::statuses::activity_builder::StatusActivityBuilder::deliver_create`])
//! fires for a poll-bearing `Status` exactly as it does for any other new
//! post — no separate "poll creation" Activity type exists, matching
//! design.md's own "a poll-bearing Note is just a Note with poll data
//! embedded in its JSON-LD representation" framing. This method passes its
//! own just-persisted `poll`/`options` (see "Poll handling" above) straight
//! through to `deliver_create`'s `poll` parameter, so `deliver_create`'s own
//! object builder now emits `type: "Question"`/`oneOf`/`anyOf`/`endTime` for
//! a status whose `poll_id` is `Some(_)` — see
//! `StatusActivityBuilder::deliver_create`'s own doc comment ("Poll embedding
//! in `Create`") for the exact shape and for why `deliver_update` is left
//! unchanged.
//!
//! ## `delete_status` rejects reblog rows (Requirement 7.x — documented
//! boundary decision, Group 5 cross-task remediation)
//! [`delete_status`](StatusService::delete_status) is owner-scoped: it does
//! not otherwise distinguish an original post from a boost/reblog row (a
//! `Status` with `reblog_of_id` set — task 5.2's `InteractionService::reblog`
//! own persistence shape, see that module's doc comment, "Reblog is a
//! `StatusRepository` row, not an `InteractionRepository` one"). A caller
//! could therefore point `delete_status` directly at their own boost's id
//! instead of calling `InteractionService::unreblog`. This matters because
//! the two operations are *not* interchangeable:
//! `status_repository::delete_status`'s own two explicit self-referential
//! cleanup steps (Requirement 7.4) only ever (a) cascade-delete boosts *of*
//! the row being deleted and (b) decrement the *reply-parent's*
//! `replies_count` when the deleted row is itself a reply — it has no third
//! case for decrementing the *reblog target's* `reblogs_count` when the
//! deleted row is itself a boost, and this service's own `delete_status`
//! always dispatches a canonical `Delete` (Requirement 7.3), never the
//! `Undo(Announce)` a boost's removal actually means in ActivityPub terms
//! (`InteractionService::unreblog`'s own contract). Reimplementing
//! `unreblog`'s counter-decrement-plus-`Undo(Announce)` semantics inside
//! `delete_status` was considered and rejected: design.md's Requirements
//! Traceability table attributes Requirement 7.x's delete path to
//! `StatusService`/`StatusActivityBuilder`/`InteractionRepository` and
//! Requirement 9.x's reblog/unreblog path to a *disjoint* set —
//! `InteractionService`/`InteractionRepository`/`StatusActivityBuilder` —
//! never both to `StatusService`, so folding reblog-removal semantics into
//! this service would blur a boundary design.md itself keeps separate.
//! `delete_status` therefore rejects a reblog-row target outright (a `422`
//! directing the caller to `InteractionService::unreblog`) rather than
//! silently leaving the target's `reblogs_count` permanently stale or
//! guessing at `unreblog`'s own already-reviewed (task 5.2) counter/dispatch
//! sequencing from outside that task's file.
//!
//! ## Mention resolution: local only (CONCERN — documented boundary gap)
//! Extracted `@handle`/`@handle@domain` mentions are resolved to an
//! `Addressing`-ready [`crate::statuses::addressing::ActorRef`] only when
//! the mention names *this* instance's own domain (or no domain at all,
//! i.e. a bare local mention) and a local actor is actually registered
//! under that handle ([`MentionLookup::resolve_local_handle`]). A mention
//! naming a *different* domain is extracted (so it does not silently
//! corrupt hashtag/mention parsing) but never resolved into a delivery
//! recipient: resolving a remote handle to its actor/inbox requires
//! WebFinger-class remote discovery, which `activity_builder.rs`'s own doc
//! comment already names as out of this spec's boundary ("リモートアクター
//! の完全なプロファイル永続化...は accounts-and-instance"). A `direct`
//! message naming only remote mentions therefore addresses nobody — a real,
//! narrow functional gap this task cannot close without a remote-actor
//! resolution port no earlier task in this spec's dependency set supplies;
//! flagged here and in this task's status report rather than guessed past
//! with an invented resolution mechanism.
//!
//! ## Notification emit (task 9.2, Requirements 9.1, 9.2, 10.1, 13.2)
//! [`create_status`](StatusService::create_status) emits one `Mention`
//! [`crate::statuses::notification_sink::NotificationEvent`] per resolved
//! *local* mention (see "Mention resolution: local only" above — a remote-
//! domain mention, never resolved to an [`ActorRef`]/[`Id`] in the first
//! place, cannot be tagged and is silently excluded from this emit loop,
//! the identical structural gap that section already documents, not a new
//! one) to this service's own
//! [`crate::statuses::notification_sink::NotificationSinkRegistry`]
//! (default [`crate::statuses::notification_sink::NoopSink`] — see that
//! module's own doc comment for why the type/trait live in this spec's
//! boundary rather than notifications', which owns the contract but has no
//! implemented tasks yet). Runs exactly once per `create_status` call
//! (idempotent re-sends short-circuit earlier, at the
//! `IdempotencyLookup::Existing` branch, before this code is ever reached),
//! and skips a self-mention (documented judgment call, see
//! `InteractionService`'s identical `reblog`/`favourite` self-interaction
//! skip for the same reasoning).
//!
//! **Edit notification: not implemented (documented judgment call)**. The
//! task text's own "（任意で）edit" marks an edit-triggered notification
//! discretionary, unlike favourite/reblog/mention. Unlike those three —
//! each with one structurally obvious recipient (the target's author, or
//! the mentioned actor) — an edit's real-world Mastodon-equivalent
//! notification (`NotificationType::Update`) fans out to every distinct
//! account that previously favourited/reblogged/participated in the edited
//! post's thread, none of which this task's boundary
//! (`StatusService`/`InteractionService`/`PollService`) can enumerate
//! without a cross-table join this task was not asked to design. Rather
//! than invent a single-recipient interpretation not grounded in either
//! spec, [`edit_status`](StatusService::edit_status) emits nothing —
//! flagged here for a future task that wants to design that fan-out.
//!
//! **`PollService::vote`: intentionally not touched.** Requirement 13.2 is
//! cited by this task only as the "投票が記録された" trigger for the
//! already-implemented `deliver_vote` Activity dispatch — the task's own
//! emit list (favourite/reblog/mention/edit) does not name poll votes, and
//! `NotificationType::Poll` (per this spec's and notifications/design.md's
//! own semantics) corresponds to a poll *ending*, not each individual
//! vote — a trigger the task text itself defers ("poll-end は...本 spec・
//! 現行 federation-core に存在しない...MVP では emit しない"). No change to
//! `poll_service.rs`.
//!
//! **Formerly-known asymmetry, closed by task 10.2**: `inbound_handlers.rs`
//! (task 6.1) ingests a remote `Create(Note)` without persisting mentions as
//! a table (still true — see that module's own doc comment, "What this
//! handler does *not* persist"), but as of task 10.2 it reuses
//! [`extract_content_tokens`]/[`ExtractedTokens::mentions`] (widened from
//! private to `pub(crate)`, the same treatment `hashtags` already got from
//! task 6.1) to resolve a mentioning remote `Create(Note)`'s local mentions
//! and emit a `Mention` `NotificationEvent` per resolved recipient —
//! transient emit, not persistence, closing the *notification* half of this
//! gap while the *persistence* half (a `status_mentions`-style table)
//! remains open, tracked by task 10.3. See `InteractionService`'s doc
//! comment ("Notification emit") for the identical reblog/favourite-side gap
//! task 10.2 also closes.
//!
//! ## Emoji shortcode extraction: extracted, resolved by the endpoint layer
//! (closed by task 10.4)
//! [`extract_content_tokens`] extracts `:shortcode:` tokens per Requirement
//! 3.6's literal text. `Status` still carries no emoji field (by design,
//! `model.rs`'s dialect-isolation proof) and no schema table associates
//! custom emoji shortcodes with a post — extraction stays a pure, stateless
//! scan, not a persistence sink. As of task 10.4, `ExtractedTokens::
//! emoji_shortcodes` (widened to `pub(crate)`, the same treatment
//! `hashtags`/`mentions` already got) is consumed by the endpoint-layer
//! `StatusRenderInput`-assembly glue (`endpoints.rs`'s `resolve_common`/
//! `poll_json`), which resolves each extracted shortcode against
//! accounts-and-instance's existing `emoji_repository::resolve_emojis` and
//! hands the result to `StatusSerializer`/`PollSerializer` as their already-
//! resolved `emojis` input — closing Requirement 3.6's `emojis` half (the
//! `mentions` half remains the separate, larger structural gap task 10.3
//! documents; not this task's boundary).

#[cfg(test)]
mod tests;

use std::collections::HashSet;

use axum::http::StatusCode;
use sqlx::postgres::PgPool;
use time::OffsetDateTime;

use crate::actor::{ActorDirectory, Handle};
use crate::api::db::map_server_error;
use crate::domain::{AccountRef, Id, Visibility};
use crate::error::AppError;
use crate::federation::{ActorUrls, DeliverySink, LocalActorLookup, ObjectKind, Recipient};
use crate::media::media_repository;
use crate::runtime::RuntimeContext;
use crate::statuses::activity_builder::{ActorHandleLookup, StatusActivityBuilder};
use crate::statuses::addressing::{self, ActorRef, Addressing};
use crate::statuses::idempotency::{self, IdempotencyLookup};
use crate::statuses::model::{Poll, PollOption, Status, StatusEdit, Tag};
use crate::statuses::notification_sink::{
    NotificationEvent, NotificationSinkRegistry, NotificationType,
};
use crate::statuses::poll_repository;
use crate::statuses::status_repository::{self, CountKind};
use crate::statuses::tag_repository;
use crate::statuses::visibility::{self, RelationshipQuery};

/// The narrow, DB-backed `Handle -> local actor` port [`StatusService`]
/// depends on to resolve an extracted `@handle` mention to a known local
/// actor (the reverse direction of [`ActorHandleLookup::resolve_handle`]).
/// See this module's doc comment ("Mention resolution: local only") for the
/// boundary this port deliberately does not cross (no remote resolution).
#[allow(async_fn_in_trait)]
pub trait MentionLookup: Send + Sync {
    /// Resolves `handle` to a registered local actor's [`Id`], if any.
    /// `Ok(None)` (not an error) when no local actor is registered under
    /// `handle` — mirrors `ActorDirectory::resolve_actor_by_handle`'s own
    /// "no error for absence" contract.
    async fn resolve_local_handle(&self, handle: &Handle) -> Result<Option<Id>, AppError>;
}

impl MentionLookup for ActorDirectory {
    async fn resolve_local_handle(&self, handle: &Handle) -> Result<Option<Id>, AppError> {
        Ok(self
            .resolve_actor_by_handle(handle)
            .await?
            .map(|resolved| resolved.id))
    }
}

/// Input to [`StatusService::create_status`] (Requirements 3.1-3.6, 13.1).
/// See this module's doc comment ("Input/output shapes").
#[derive(Debug, Clone, PartialEq)]
pub struct CreateStatus {
    pub content: String,
    pub visibility: Visibility,
    pub spoiler_text: String,
    pub sensitive: bool,
    pub media_ids: Vec<Id>,
    pub in_reply_to_id: Option<Id>,
    pub language: Option<String>,
    /// A caller-supplied poll spec. See this module's doc comment ("Poll
    /// handling") for why this is validated (mutual exclusivity with
    /// `media_ids`) but never persisted by this service.
    pub poll: Option<CreateStatusPoll>,
}

/// A caller-supplied poll spec attached to [`CreateStatus`] — validated for
/// Requirement 13.1's mutual-exclusivity rule only; see this module's doc
/// comment ("Poll handling").
#[derive(Debug, Clone, PartialEq)]
pub struct CreateStatusPoll {
    pub options: Vec<String>,
    pub multiple: bool,
    pub expires_at: Option<OffsetDateTime>,
}

/// Input to [`StatusService::edit_status`] (Requirement 8.1). See this
/// module's doc comment ("Edit's language limitation") for why this
/// deliberately has no `language` field.
#[derive(Debug, Clone, PartialEq)]
pub struct EditStatus {
    pub content: String,
    pub spoiler_text: String,
    pub sensitive: bool,
    pub media_ids: Vec<Id>,
}

/// Output of [`StatusService::context`] (Requirement 6.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusContext {
    pub ancestors: Vec<Status>,
    pub descendants: Vec<Status>,
}

/// Output of [`StatusService::source`] (Requirement 8.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusSource {
    pub text: String,
    pub spoiler_text: String,
}

/// A mentioned handle extracted from a post's `content`, before resolution
/// (see [`extract_content_tokens`]). `domain` is `None` for a bare `@handle`
/// mention (assumed local, matching this crate's own established
/// mention-shorthand convention).
///
/// `pub(crate)` (widened by task 10.2, `Boundary: InboundHandlers`, mirroring
/// `hashtags`' own identical task-6.1 widening below): a remote-origin
/// `Create(Note)` ingestion also needs to resolve mentions to local
/// notification recipients (Requirement 9.1/9.2/10.1's local/remote-symmetric
/// emit invariant, applied to the mention side) — see
/// `inbound_handlers.rs`'s own doc comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Mention {
    pub(crate) local: String,
    pub(crate) domain: Option<String>,
}

/// The result of scanning a post's `content` for mentions/hashtags/emoji
/// shortcodes (Requirement 3.6). See [`extract_content_tokens`].
///
/// `hashtags` is `pub(crate)` (widened by task 6.1, `InboundHandlers`):
/// inbound `Create(Note)` ingestion reuses this exact same extraction
/// function for hashtag persistence (Requirement 14.5's "共通コードパス" —
/// the same extraction logic local-origin `create_status` already uses,
/// rather than a second, duplicated hashtag scanner) — see
/// `inbound_handlers.rs`'s own doc comment. `mentions` is `pub(crate)` too
/// (task 10.2, the identical reuse rationale applied to mention-notification
/// resolution — see [`Mention`]'s own doc comment). `emoji_shortcodes` is
/// `pub(crate)` too (task 10.4, `Boundary: StatusService, StatusSerializer`):
/// the endpoint-layer `StatusRenderInput`-assembly glue (`endpoints.rs`)
/// reuses this exact same extraction to resolve a post's/poll's `emojis`
/// field via `accounts::emoji_repository::resolve_emojis`, the same
/// "reuse this module's own already-reviewed scanner rather than
/// duplicating it" rationale `hashtags`/`mentions` already established.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct ExtractedTokens {
    pub(crate) mentions: Vec<Mention>,
    pub(crate) hashtags: Vec<String>,
    pub(crate) emoji_shortcodes: Vec<String>,
}

/// Whether the character at `chars[i]` starts a new token (Requirement
/// 3.6's minimal scanner): true at the start of `content`, or when the
/// preceding character is not alphanumeric — so `@`/`#`/`:` embedded
/// mid-word (e.g. an email address's `@`, a URL's `:`) are not mistaken for
/// a token start.
fn is_token_boundary(chars: &[char], i: usize) -> bool {
    i == 0 || !chars[i - 1].is_alphanumeric()
}

/// Scans `content` for `@handle`/`@handle@domain` mentions, `#hashtag`
/// hashtags, and `:shortcode:` custom-emoji shortcodes (Requirement 3.6),
/// in first-appearance order, each deduplicated by exact token text.
/// Deliberately minimal — not a general markup engine (this task's own
/// instruction): no nested/escaped syntax, no code-block/link-aware
/// suppression, ASCII alphanumeric-or-underscore token bodies only. See
/// this module's doc comment ("Mention resolution: local only", "Emoji
/// shortcode extraction") for what consumes each extracted kind.
pub(crate) fn extract_content_tokens(content: &str) -> ExtractedTokens {
    let chars: Vec<char> = content.chars().collect();
    let mut result = ExtractedTokens::default();
    let mut seen_mentions: HashSet<String> = HashSet::new();
    let mut seen_hashtags: HashSet<String> = HashSet::new();
    let mut seen_emoji: HashSet<String> = HashSet::new();

    let scan_word = |chars: &[char], from: usize| -> usize {
        let mut j = from;
        while j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '_') {
            j += 1;
        }
        j
    };

    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '@' && is_token_boundary(&chars, i) {
            let local_end = scan_word(&chars, i + 1);
            if local_end > i + 1 {
                let local: String = chars[i + 1..local_end].iter().collect();
                let mut domain = None;
                let mut end = local_end;
                if local_end < chars.len() && chars[local_end] == '@' {
                    let domain_end = {
                        let mut k = local_end + 1;
                        while k < chars.len()
                            && (chars[k].is_ascii_alphanumeric()
                                || chars[k] == '.'
                                || chars[k] == '-')
                        {
                            k += 1;
                        }
                        k
                    };
                    if domain_end > local_end + 1 {
                        domain = Some(chars[local_end + 1..domain_end].iter().collect::<String>());
                        end = domain_end;
                    }
                }
                let key = format!("@{local}@{}", domain.clone().unwrap_or_default());
                if seen_mentions.insert(key) {
                    result.mentions.push(Mention { local, domain });
                }
                i = end;
                continue;
            }
        } else if c == '#' && is_token_boundary(&chars, i) {
            let end = scan_word(&chars, i + 1);
            if end > i + 1 {
                let tag: String = chars[i + 1..end].iter().collect::<String>().to_lowercase();
                if seen_hashtags.insert(tag.clone()) {
                    result.hashtags.push(tag);
                }
                i = end;
                continue;
            }
        } else if c == ':' && is_token_boundary(&chars, i) {
            let end = scan_word(&chars, i + 1);
            if end > i + 1 && end < chars.len() && chars[end] == ':' {
                let code: String = chars[i + 1..end].iter().collect();
                if seen_emoji.insert(code.clone()) {
                    result.emoji_shortcodes.push(code);
                }
                i = end + 1;
                continue;
            }
        }
        i += 1;
    }

    result
}

fn not_found() -> AppError {
    AppError::client(StatusCode::NOT_FOUND, "status not found")
}

fn rejected(message: impl Into<String>) -> AppError {
    AppError::client(StatusCode::UNPROCESSABLE_ENTITY, message.into())
}

/// The post business-service layer (design.md's `StatusService`, task 5.1).
/// Generic over exactly the same delegation ports [`StatusActivityBuilder`]
/// (task 4.1) already established — `A`/`D`/`L`/`H` — plus this service's
/// own two: `R` ([`RelationshipQuery`], task 3.1) and `M` ([`MentionLookup`],
/// this task). Mirrors this crate's established "generic type parameter,
/// not `Arc<dyn Trait>`" convention for non-object-safe async ports (see
/// `crate::media::service::MediaService`'s identical doc comment).
pub struct StatusService<A, D, L, H, R, M>
where
    A: ActorHandleLookup,
    D: LocalActorLookup,
    L: DeliverySink,
    H: DeliverySink,
    R: RelationshipQuery,
    M: MentionLookup,
{
    pool: PgPool,
    runtime: RuntimeContext,
    domain: String,
    urls: ActorUrls,
    activity_builder: StatusActivityBuilder<A, D, L, H>,
    relationship: R,
    mentions: M,
    notifications: NotificationSinkRegistry,
}

impl<A, D, L, H, R, M> StatusService<A, D, L, H, R, M>
where
    A: ActorHandleLookup,
    D: LocalActorLookup,
    L: DeliverySink,
    H: DeliverySink,
    R: RelationshipQuery,
    M: MentionLookup,
{
    /// Builds a `StatusService` bound to `pool` (repository calls),
    /// `runtime` (id/clock injection, never ad hoc generation), `domain`
    /// (this instance's own server domain — used to recognize a bare/
    /// same-domain mention as local, Requirement 3.6/4.2), `urls` (local
    /// object URI construction, e.g. a freshly-created post's own `uri`),
    /// `activity_builder` (Activity generation + delivery, task 4.1),
    /// `relationship` ([`RelationshipQuery`], task 3.1), `mentions`
    /// ([`MentionLookup`], this task), and `notifications`
    /// ([`NotificationSinkRegistry`], task 9.2 — see this module's doc
    /// comment, "Notification emit (task 9.2)").
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pool: PgPool,
        runtime: RuntimeContext,
        domain: impl Into<String>,
        urls: ActorUrls,
        activity_builder: StatusActivityBuilder<A, D, L, H>,
        relationship: R,
        mentions: M,
        notifications: NotificationSinkRegistry,
    ) -> Self {
        Self {
            pool,
            runtime,
            domain: domain.into(),
            urls,
            activity_builder,
            relationship,
            mentions,
            notifications,
        }
    }

    /// Resolves whether `status` is visible to `viewer` through the real
    /// visibility policy ([`visibility::is_visible`], task 3.1) — see this
    /// module's doc comment ("Replacing `status_repository`'s provisional
    /// visibility stand-in") for why this, not
    /// `status_repository::find_visible`'s own bundled filter, is what this
    /// service routes retrieval/context/history through.
    async fn visible_to(&self, status: &Status, viewer: Option<Id>) -> Result<bool, AppError> {
        let rel = self
            .relationship
            .viewer_relation(status.actor_id, viewer)
            .await?;
        Ok(visibility::is_visible(status, viewer, &rel))
    }

    /// Resolves `mentions` (already-extracted from a post's `content`) into
    /// `Addressing`-ready [`ActorRef`]s, then derives that post's
    /// `Addressing`/recipient set (Requirements 4.1, 4.2) — the single
    /// derivation [`create_status`](Self::create_status),
    /// [`delete_status`](Self::delete_status), and
    /// [`edit_status`](Self::edit_status) all funnel through. See this
    /// module's doc comment ("Mention resolution: local only") for which
    /// mentions resolve to a real recipient.
    ///
    /// Also returns the [`Id`]s of every mention that resolved to a
    /// registered local actor, in the same order as `mention_refs` (task
    /// 9.2, "Notification emit"): [`create_status`](Self::create_status) is
    /// the only caller that consumes this third element (to emit a
    /// `Mention` [`NotificationEvent`] per resolved recipient) —
    /// [`delete_status`](Self::delete_status)/[`edit_status`](Self::edit_status)
    /// ignore it, since neither emits a mention notification.
    async fn build_addressing(
        &self,
        status: &Status,
        mentions: &[Mention],
    ) -> Result<(Addressing, Vec<Recipient>, Vec<Id>), AppError> {
        let mut mention_refs = Vec::with_capacity(mentions.len());
        let mut mention_ids = Vec::with_capacity(mentions.len());
        for mention in mentions {
            if let Some(domain) = &mention.domain
                && !domain.eq_ignore_ascii_case(&self.domain)
            {
                // Remote mention: extraction only, no resolution — see this
                // module's doc comment ("Mention resolution: local only").
                continue;
            }
            let Ok(handle) = Handle::new(mention.local.clone()) else {
                continue; // not a syntactically valid local handle
            };
            let Some(mentioned_id) = self.mentions.resolve_local_handle(&handle).await? else {
                continue; // no local actor registered under this handle
            };
            let uri = self.urls.actor_url(&handle);
            mention_refs.push(ActorRef {
                uri,
                recipient: Recipient::Local(handle),
            });
            mention_ids.push(mentioned_id);
        }

        let followers = self.relationship.followers_of(status.actor_id).await?;
        let author_url = self
            .activity_builder
            .resolve_actor_url(status.actor_id)
            .await?;
        let followers_uri = format!("{author_url}/followers");

        let addressing = addressing::derive_addressing(status, &mention_refs, &followers_uri);
        let recipients = addressing::derive_recipients(&addressing, &mention_refs, &followers);
        Ok((addressing, recipients, mention_ids))
    }

    /// Persists every hashtag [`extract_content_tokens`] found in
    /// `status.content`, associated to `status.id` (Requirement 3.6).
    ///
    /// Runs against a caller-supplied connection rather than `self.pool`
    /// (task 5.2, Requirement 6.1): [`create_status`](Self::create_status)
    /// drives it with the same open transaction as the status/media/poll
    /// writes, so a later failure rolls the tag rows back along with
    /// everything else. Two statements per hashtag means a borrowed
    /// connection, not a `sqlx::PgExecutor` (which a single `execute`
    /// consumes) — the same distinction task 5.1 drew between its
    /// single-statement and multi-statement repository writers.
    async fn persist_tags(
        &self,
        conn: &mut sqlx::PgConnection,
        status_id: Id,
        hashtags: &[String],
        now: OffsetDateTime,
    ) -> Result<(), AppError> {
        for name in hashtags {
            let tag = tag_repository::upsert_tag(
                &mut *conn,
                &Tag {
                    id: self.runtime.ids.next_id(),
                    name: name.clone(),
                    created_at: now,
                },
            )
            .await?;
            tag_repository::associate_tag(&mut *conn, status_id, tag.id).await?;
        }
        Ok(())
    }

    /// Creates a new post (Requirements 3.1-3.6, 5.1-5.3, 13.1). See this
    /// module's doc comment ("Poll handling") for how a caller-supplied
    /// poll is validated and persisted alongside the post.
    pub async fn create_status(
        &self,
        actor_id: Id,
        input: CreateStatus,
        idem: Option<&str>,
    ) -> Result<Status, AppError> {
        if let Some(key) = idem
            && let IdempotencyLookup::Existing(status_id) =
                idempotency::check_or_reserve(&self.pool, actor_id, key).await?
        {
            return status_repository::find_by_id(&self.pool, status_id)
                .await?
                .ok_or_else(|| {
                    AppError::server(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "idempotency key bound to a status that no longer exists",
                    )
                });
        }

        let has_content = !input.content.trim().is_empty();
        let has_media = !input.media_ids.is_empty();
        let has_poll = input.poll.is_some();
        if !has_content && !has_media && !has_poll {
            return Err(rejected("a status must include content, media, or a poll"));
        }
        if has_poll && has_media {
            return Err(rejected(
                "a poll and media attachments are mutually exclusive",
            ));
        }
        if let Some(poll_input) = &input.poll {
            // Requirement 13.1 names "選択肢・締切・単一/複数選択" as a
            // poll's shape but does not itself spell out a minimum option
            // count. Requirement 13.4's voting-side "単一選択の投票へ複数
            // 選択肢が指定された...範囲外の選択肢インデックス" rejection only
            // makes sense for a poll that already has at least 2 selectable
            // options (a 0- or 1-option poll has no "index range" to be out
            // of, and nothing to meaningfully vote *among*), so this
            // service rejects a poll with fewer than 2 options, and rejects
            // a blank option title, at creation time rather than silently
            // persisting a degenerate poll no client could sensibly render.
            if poll_input.options.len() < 2 {
                return Err(rejected("a poll must have at least 2 options"));
            }
            if poll_input
                .options
                .iter()
                .any(|option| option.trim().is_empty())
            {
                return Err(rejected("a poll option must not be empty"));
            }
        }

        for media_id in &input.media_ids {
            media_repository::find_owned(&self.pool, *media_id, actor_id)
                .await?
                .ok_or_else(|| {
                    rejected(format!(
                        "media {media_id:?} is not owned by the requesting actor, or does not exist"
                    ))
                })?;
        }

        let (in_reply_to_id, in_reply_to_account_id, in_reply_to_uri) = match input.in_reply_to_id {
            Some(parent_id) => {
                let parent = status_repository::find_by_id(&self.pool, parent_id).await?;
                let parent = match parent {
                    Some(parent) if self.visible_to(&parent, Some(actor_id)).await? => parent,
                    _ => {
                        return Err(AppError::client(
                            StatusCode::NOT_FOUND,
                            "in_reply_to status not found",
                        ));
                    }
                };
                (
                    Some(parent_id),
                    Some(parent.actor_id),
                    Some(parent.uri.clone()),
                )
            }
            None => (None, None, None),
        };

        let id = self.runtime.ids.next_id();
        let now = self.runtime.clock.now();
        let uri = self.urls.object_url(ObjectKind::new("statuses"), id);
        // Minted up front (never inside the `if has_poll` block below) so it
        // can be set on `status.poll_id` *before* `status` is inserted —
        // `statuses.poll_id` carries no FK (see `migrations/0007_statuses.sql`'s
        // own comment: the real FK direction is `polls.status_id`), so the
        // status row may safely reference a poll id that is only inserted
        // immediately afterward, mirroring `tests/polls_it.rs::insert_poll_status_fixture`'s
        // established status-then-poll insert ordering.
        let poll_id = has_poll.then(|| self.runtime.ids.next_id());

        let status = Status {
            id,
            actor_id,
            uri: uri.clone(),
            url: Some(uri),
            content: input.content,
            visibility: input.visibility,
            sensitive: input.sensitive,
            spoiler_text: input.spoiler_text,
            in_reply_to_id,
            in_reply_to_account_id,
            reblog_of_id: None,
            poll_id,
            language: input.language,
            reblogs_count: 0,
            favourites_count: 0,
            replies_count: 0,
            local: true,
            created_at: now,
            edited_at: None,
        };

        // Task 5.2 (Requirements 6.1, 6.3, 6.4; design.md "複合書き込みの
        // トランザクション境界（A-3 後）"): the status row, its media
        // attachments, its poll, its tags, and the parent's reply-count
        // increment are one transaction that commits only if every one of
        // them succeeds. An early `?` return below drops `tx` un-committed,
        // which rolls the whole set back and returns the error to the
        // caller — never a "looks like it succeeded" partial write.
        //
        // Everything with network I/O or an observable side effect outside
        // this database — Activity delivery and notification emission —
        // deliberately stays *after* `tx.commit()`: holding a pooled
        // connection across an outbound HTTP wait would starve the pool,
        // and a delivery failure would otherwise roll back local writes
        // that are perfectly valid.
        let mut tx = self.pool.begin().await.map_err(map_server_error)?;

        status_repository::insert_status(&mut *tx, &status).await?;

        if !input.media_ids.is_empty() {
            status_repository::attach_media_on_conn(&mut tx, status.id, &input.media_ids).await?;
        }

        // Requirement 13.1: create the poll and associate it with the post
        // (design.md's create-flow sequence diagram: "insert status and
        // poll record" is a single step, status row first, poll row
        // second). `has_poll && has_media` was already rejected above, so
        // `input.media_ids` is always empty here when `poll_id` is `Some`.
        // The persisted `(Poll, Vec<PollOption>)` is kept in hand (not
        // dropped at the end of this block) so it can be passed straight
        // through to `deliver_create` below (Requirement 13.7) without a
        // redundant `PollRepository` re-fetch.
        let poll_payload: Option<(Poll, Vec<PollOption>)> =
            if let (Some(poll_id), Some(poll_input)) = (poll_id, input.poll) {
                let poll = Poll {
                    id: poll_id,
                    status_id: status.id,
                    expires_at: poll_input.expires_at,
                    multiple: poll_input.multiple,
                };
                let options: Vec<PollOption> = poll_input
                    .options
                    .iter()
                    .enumerate()
                    .map(|(idx, title)| PollOption {
                        poll_id,
                        idx: idx as i32,
                        title: title.clone(),
                        votes_count: 0,
                    })
                    .collect();
                // Under the enclosing transaction `insert_poll`'s own
                // `begin`/`commit` becomes a nested SAVEPOINT (task 5.1's
                // Implementation Note) — two extra round trips, and
                // correctness-preserving: releasing that savepoint does not
                // commit anything on its own, so a later failure still
                // rolls the poll back with the rest.
                poll_repository::insert_poll(&mut *tx, &poll, &options).await?;
                Some((poll, options))
            } else {
                None
            };

        let extracted = extract_content_tokens(&status.content);
        self.persist_tags(&mut tx, status.id, &extracted.hashtags, now)
            .await?;

        if let Some(parent_id) = status.in_reply_to_id {
            status_repository::adjust_counts(&mut *tx, parent_id, CountKind::Replies, 1).await?;
        }

        tx.commit().await.map_err(map_server_error)?;

        // -- everything below this line runs after the commit ------------

        let (addressing, recipients, mentioned_ids) =
            self.build_addressing(&status, &extracted.mentions).await?;
        self.activity_builder
            .deliver_create(
                &status,
                &addressing,
                recipients,
                in_reply_to_uri.as_deref(),
                poll_payload
                    .as_ref()
                    .map(|(poll, options)| (poll, options.as_slice())),
            )
            .await?;

        // Task 9.2: emit one `Mention` NotificationEvent per resolved local
        // mention (Requirement 3.6's extraction, "Mention resolution: local
        // only" above). One event per distinct mentioned actor — no
        // duplication risk: `extract_content_tokens` already dedupes
        // mentions by exact token text, and this runs exactly once, on
        // creation, never on a later re-fetch/re-render of the same post.
        // Skips a self-mention (mentioning your own handle) — see
        // `InteractionService`'s identical self-interaction skip for
        // `reblog`/`favourite` (same documented judgment call, applied
        // consistently here).
        for mentioned_id in mentioned_ids {
            if mentioned_id == actor_id {
                continue;
            }
            self.notifications
                .emit(NotificationEvent {
                    recipient: AccountRef::Local(mentioned_id),
                    origin: AccountRef::Local(actor_id),
                    kind: NotificationType::Mention,
                    target_status_id: Some(status.id),
                    occurred_at: now,
                })
                .await?;
        }

        // Idempotency-key binding stays *outside* the transaction, in the
        // position it has always occupied (task 5.2's "べき等キーの束縛
        // 処理の実際の呼び出し位置を確認し、トランザクションに含めるべきかを
        // 判断して記録する" — this comment is that record). Three reasons:
        //
        // 1. Ledger invariant. `status_idempotency_keys.status_id` is
        //    `NOT NULL REFERENCES statuses(id)` (`migrations/0007_statuses.sql`),
        //    and `check_or_reserve`'s `Existing` branch above returns a 500
        //    when a bound key points at a status that no longer exists. A
        //    key must therefore only ever be bound to a *committed* status;
        //    binding inside the transaction would write ledger rows that a
        //    rollback has to take back with them.
        // 2. Rollback semantics are already the ones we want. If the
        //    composite write above fails, nothing is bound, so a client
        //    retry with the same key is correctly free to create the post
        //    rather than resolving to a status that was rolled away.
        // 3. Boundary. `idempotency::bind` takes `&PgPool` and belongs to
        //    the `IdempotencyStore` module, which task 5.1 deliberately left
        //    out of its executor-generic conversion; including it here would
        //    mean a signature change outside this task's `StatusService`
        //    boundary for no correctness gain.
        //
        // Residual (pre-existing, unchanged by this task): a failure between
        // the commit and this bind leaves a created post whose key never got
        // bound, so a retry creates a second post. That window is exactly as
        // wide as before — Requirement 6.1 covers the create's own writes.
        if let Some(key) = idem {
            idempotency::bind(&self.pool, actor_id, key, status.id, now).await?;
        }

        Ok(status)
    }

    /// Retrieves a single post visible to `viewer` (Requirements 6.1, 6.4);
    /// `Ok(None)` for both an unknown and an invisible id (a uniform
    /// 404-equivalent).
    pub async fn show(&self, viewer: Option<Id>, id: Id) -> Result<Option<Status>, AppError> {
        let Some(status) = status_repository::find_by_id(&self.pool, id).await? else {
            return Ok(None);
        };
        if self.visible_to(&status, viewer).await? {
            Ok(Some(status))
        } else {
            Ok(None)
        }
    }

    /// Retrieves `id`'s thread context — ancestors and descendants,
    /// filtered to what `viewer` may see (Requirements 6.2, 6.3, 6.4). A
    /// 404-equivalent `AppError` if `id` itself is unknown or invisible to
    /// `viewer`.
    pub async fn context(&self, viewer: Option<Id>, id: Id) -> Result<StatusContext, AppError> {
        let root = status_repository::find_by_id(&self.pool, id)
            .await?
            .ok_or_else(not_found)?;
        if !self.visible_to(&root, viewer).await? {
            return Err(not_found());
        }

        let mut ancestors = Vec::new();
        for status in status_repository::ancestors_unfiltered(&self.pool, id).await? {
            if self.visible_to(&status, viewer).await? {
                ancestors.push(status);
            }
        }

        let mut descendants = Vec::new();
        for status in status_repository::descendants_unfiltered(&self.pool, id).await? {
            if self.visible_to(&status, viewer).await? {
                descendants.push(status);
            }
        }

        Ok(StatusContext {
            ancestors,
            descendants,
        })
    }

    /// Deletes `id`, owned by `actor_id` (Requirements 7.1-7.4): a
    /// 404-equivalent `AppError` if `id` is unknown or not owned by
    /// `actor_id`; a `422` if `id` is itself a reblog/boost row (see this
    /// module's doc comment, "`delete_status` rejects reblog rows" —
    /// `InteractionService::unreblog` is the only correct way to retire one);
    /// otherwise removes the row (and its two self-referential cleanup
    /// steps, `status_repository::delete_status`'s own contract) and
    /// delivers a canonical `Delete` to `id`'s own addressing. Returns the
    /// pre-deletion [`Status`] (Requirement 7.1's "削除された投稿の表現を
    /// 返す").
    pub async fn delete_status(&self, actor_id: Id, id: Id) -> Result<Status, AppError> {
        let status = status_repository::find_by_id(&self.pool, id)
            .await?
            .ok_or_else(not_found)?;
        if status.actor_id != actor_id {
            return Err(not_found());
        }
        if status.reblog_of_id.is_some() {
            return Err(rejected(
                "cannot delete a reblog through delete_status; use \
                 InteractionService::unreblog instead",
            ));
        }

        status_repository::delete_status(&self.pool, id).await?;

        let extracted = extract_content_tokens(&status.content);
        // `_mentioned_ids` unused here: deletion emits no `NotificationEvent`
        // (out of task 9.2's own emit list — favourite/reblog/mention/edit
        // — none of which "delete" is).
        let (addressing, recipients, _mentioned_ids) =
            self.build_addressing(&status, &extracted.mentions).await?;
        self.activity_builder
            .deliver_delete(&status, &addressing, recipients)
            .await?;

        Ok(status)
    }

    /// Edits `id`, owned by `actor_id` (Requirement 8.1, 8.4, 8.5): a
    /// 404-equivalent `AppError` if `id` is unknown or not owned by
    /// `actor_id`; otherwise archives the pre-edit content into history
    /// (`status_repository::apply_edit`'s own contract), replaces the
    /// attached media set, and delivers a canonical `Update`. See this
    /// module's doc comment ("Edit's language limitation") for the one
    /// Requirement 8.1 field this does not cover.
    pub async fn edit_status(
        &self,
        actor_id: Id,
        id: Id,
        input: EditStatus,
    ) -> Result<Status, AppError> {
        let current = status_repository::find_by_id(&self.pool, id)
            .await?
            .ok_or_else(not_found)?;
        if current.actor_id != actor_id {
            return Err(not_found());
        }

        for media_id in &input.media_ids {
            media_repository::find_owned(&self.pool, *media_id, actor_id)
                .await?
                .ok_or_else(|| {
                    rejected(format!(
                        "media {media_id:?} is not owned by the requesting actor, or does not exist"
                    ))
                })?;
        }

        let now = self.runtime.clock.now();
        let edit = StatusEdit {
            id: self.runtime.ids.next_id(),
            status_id: id,
            content: input.content,
            spoiler_text: input.spoiler_text,
            sensitive: input.sensitive,
            created_at: now,
        };
        status_repository::apply_edit(&self.pool, id, &edit, now).await?;
        status_repository::replace_media(&self.pool, id, &input.media_ids).await?;

        let updated = status_repository::find_by_id(&self.pool, id)
            .await?
            .ok_or_else(|| {
                AppError::server(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "status vanished immediately after being edited",
                )
            })?;

        let in_reply_to_uri = match updated.in_reply_to_id {
            Some(parent_id) => status_repository::find_by_id(&self.pool, parent_id)
                .await?
                .map(|parent| parent.uri),
            None => None,
        };

        let extracted = extract_content_tokens(&updated.content);
        // `_mentioned_ids` unused here — see this module's doc comment
        // ("Notification emit (task 9.2)", "Edit notification: not
        // implemented") for why `edit_status` does not emit a
        // `NotificationEvent` despite the task text's "（任意で）edit".
        let (addressing, recipients, _mentioned_ids) =
            self.build_addressing(&updated, &extracted.mentions).await?;
        self.activity_builder
            .deliver_update(
                &updated,
                &addressing,
                recipients,
                in_reply_to_uri.as_deref(),
            )
            .await?;

        Ok(updated)
    }

    /// Returns `id`'s edit history, oldest first (Requirement 8.2), subject
    /// to the same visibility rule as [`show`](Self::show). See this
    /// module's doc comment ("Media/history limitation") for the one
    /// Requirement 8.2 field the schema cannot retain per version.
    pub async fn history(&self, viewer: Option<Id>, id: Id) -> Result<Vec<StatusEdit>, AppError> {
        let status = status_repository::find_by_id(&self.pool, id)
            .await?
            .ok_or_else(not_found)?;
        if !self.visible_to(&status, viewer).await? {
            return Err(not_found());
        }
        status_repository::list_edits(&self.pool, id).await
    }

    /// Returns `id`'s raw edit source (Requirement 8.3), owner-scoped like
    /// [`delete_status`](Self::delete_status)/[`edit_status`](Self::edit_status)
    /// (not a general visibility check — only the author may fetch their
    /// own post's editable source).
    pub async fn source(&self, actor_id: Id, id: Id) -> Result<StatusSource, AppError> {
        let status = status_repository::find_by_id(&self.pool, id)
            .await?
            .ok_or_else(not_found)?;
        if status.actor_id != actor_id {
            return Err(not_found());
        }
        Ok(StatusSource {
            text: status.content,
            spoiler_text: status.spoiler_text,
        })
    }
}
