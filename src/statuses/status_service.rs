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
//! `PollService` (task 5.3) — `create_status` only *validates* the
//! poll/media exclusivity rule (Requirement 13.1) and otherwise leaves real
//! poll persistence to `PollService`, per design.md's own Requirements
//! Traceability table, which lists 13.1's owning components as
//! "PollService, PollRepository, StatusActivityBuilder", **not**
//! `StatusService` (see "Poll handling" below for the full reasoning). Does
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
//! ## Poll handling (Requirement 13.1 — documented boundary decision)
//! design.md's Requirements Traceability table lists 13.1's owning
//! components as "PollService, PollRepository, StatusActivityBuilder" —
//! `StatusService` is not among them. [`CreateStatus::poll`] is therefore
//! validated (mutual exclusivity with `media_ids`, Requirement 13.1) but
//! never persisted here: a caller-supplied poll causes
//! [`StatusService::create_status`] to reject the request with a clear,
//! non-silent `422` (never a silent drop) rather than guess at
//! `PollRepository::insert_poll`'s call-site sequencing (poll id minting,
//! option persistence, and how a not-yet-invented `PollService` (task 5.3)
//! is meant to layer atop the status row this method creates) — a decision
//! this task's own boundary instructions call out as the correct,
//! structurally-sound interpretation when full poll wiring is out of a
//! task's dependency set.
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
//! ## Emoji shortcode extraction: extracted, not yet consumed (CONCERN)
//! [`extract_content_tokens`] extracts `:shortcode:` tokens per Requirement
//! 3.6's literal text, but nothing in this task's own boundary consumes
//! them: `Status` carries no emoji field (by design, `model.rs`'s dialect-
//! isolation proof), no schema table associates custom emoji shortcodes
//! with a post, and rendering them is `StatusSerializer`/the endpoint
//! layer's job (out of this task's boundary, and that layer's own doc
//! comment already documents taking emoji info as a pre-resolved caller
//! input it does not yet have a real source for). Extraction is
//! implemented and unit-tested per the requirement's letter; there is
//! deliberately no persistence sink for it yet.

#[cfg(test)]
mod tests;

use std::collections::HashSet;

use axum::http::StatusCode;
use sqlx::postgres::PgPool;
use time::OffsetDateTime;

use crate::actor::{ActorDirectory, Handle};
use crate::domain::{Id, Visibility};
use crate::error::AppError;
use crate::federation::{ActorUrls, DeliverySink, LocalActorLookup, ObjectKind, Recipient};
use crate::media::media_repository;
use crate::runtime::RuntimeContext;
use crate::statuses::activity_builder::{ActorHandleLookup, StatusActivityBuilder};
use crate::statuses::addressing::{self, ActorRef, Addressing};
use crate::statuses::idempotency::{self, IdempotencyLookup};
use crate::statuses::model::{Status, StatusEdit, Tag};
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
#[derive(Debug, Clone, PartialEq, Eq)]
struct Mention {
    local: String,
    domain: Option<String>,
}

/// The result of scanning a post's `content` for mentions/hashtags/emoji
/// shortcodes (Requirement 3.6). See [`extract_content_tokens`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct ExtractedTokens {
    mentions: Vec<Mention>,
    hashtags: Vec<String>,
    emoji_shortcodes: Vec<String>,
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
fn extract_content_tokens(content: &str) -> ExtractedTokens {
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
    /// `relationship` ([`RelationshipQuery`], task 3.1), and `mentions`
    /// ([`MentionLookup`], this task).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pool: PgPool,
        runtime: RuntimeContext,
        domain: impl Into<String>,
        urls: ActorUrls,
        activity_builder: StatusActivityBuilder<A, D, L, H>,
        relationship: R,
        mentions: M,
    ) -> Self {
        Self {
            pool,
            runtime,
            domain: domain.into(),
            urls,
            activity_builder,
            relationship,
            mentions,
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
    async fn build_addressing(
        &self,
        status: &Status,
        mentions: &[Mention],
    ) -> Result<(Addressing, Vec<Recipient>), AppError> {
        let mut mention_refs = Vec::with_capacity(mentions.len());
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
            if self.mentions.resolve_local_handle(&handle).await?.is_none() {
                continue; // no local actor registered under this handle
            }
            let uri = self.urls.actor_url(&handle);
            mention_refs.push(ActorRef {
                uri,
                recipient: Recipient::Local(handle),
            });
        }

        let followers = self.relationship.followers_of(status.actor_id).await?;
        let author_url = self
            .activity_builder
            .resolve_actor_url(status.actor_id)
            .await?;
        let followers_uri = format!("{author_url}/followers");

        let addressing = addressing::derive_addressing(status, &mention_refs, &followers_uri);
        let recipients = addressing::derive_recipients(&addressing, &mention_refs, &followers);
        Ok((addressing, recipients))
    }

    /// Persists every hashtag [`extract_content_tokens`] found in
    /// `status.content`, associated to `status.id` (Requirement 3.6).
    async fn persist_tags(
        &self,
        status_id: Id,
        hashtags: &[String],
        now: OffsetDateTime,
    ) -> Result<(), AppError> {
        for name in hashtags {
            let tag = tag_repository::upsert_tag(
                &self.pool,
                &Tag {
                    id: self.runtime.ids.next_id(),
                    name: name.clone(),
                    created_at: now,
                },
            )
            .await?;
            tag_repository::associate_tag(&self.pool, status_id, tag.id).await?;
        }
        Ok(())
    }

    /// Creates a new post (Requirements 3.1-3.6, 5.1-5.3, 13.1). See this
    /// module's doc comment ("Poll handling") for why a caller-supplied
    /// poll is validated but rejected rather than persisted.
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
        if has_poll {
            // See this module's doc comment ("Poll handling"): real poll
            // persistence is `PollService`'s (task 5.3) boundary, not
            // this service's.
            return Err(rejected(
                "poll creation is not implemented by StatusService; it is owned by PollService",
            ));
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
            poll_id: None,
            language: input.language,
            reblogs_count: 0,
            favourites_count: 0,
            replies_count: 0,
            local: true,
            created_at: now,
            edited_at: None,
        };

        status_repository::insert_status(&self.pool, &status).await?;

        if !input.media_ids.is_empty() {
            status_repository::attach_media(&self.pool, status.id, &input.media_ids).await?;
        }

        let extracted = extract_content_tokens(&status.content);
        self.persist_tags(status.id, &extracted.hashtags, now)
            .await?;

        if let Some(parent_id) = status.in_reply_to_id {
            status_repository::adjust_counts(&self.pool, parent_id, CountKind::Replies, 1).await?;
        }

        let (addressing, recipients) = self.build_addressing(&status, &extracted.mentions).await?;
        self.activity_builder
            .deliver_create(&status, &addressing, recipients, in_reply_to_uri.as_deref())
            .await?;

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
        let (addressing, recipients) = self.build_addressing(&status, &extracted.mentions).await?;
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
        let (addressing, recipients) = self.build_addressing(&updated, &extracted.mentions).await?;
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
