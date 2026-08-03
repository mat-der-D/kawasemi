//! `StatusSerializer` / `PollSerializer` (design.md "API / Serialize 層" ->
//! "StatusSerializer / PollSerializer"; Requirements 1.1, 1.2, 1.3, 1.4,
//! 1.5, 1.6, 2.1, 2.2, 2.3, 2.4, 15.1; task 3.3, `Boundary: StatusSerializer,
//! PollSerializer`): maps an already-resolved [`Status`]/[`Poll`] (plus
//! every field this task's boundary explicitly delegates upstream or takes
//! as a pre-resolved input — see "Deliberate deviations" below) onto the
//! Mastodon-compatible Status/Poll JSON contract, and registers both as
//! goldens against `api-foundation`'s [`crate::contract::assert_golden`]
//! harness.
//!
//! Scope: this module owns exactly [`status_to_json`]/[`to_status_json`]
//! and [`poll_to_json`]/[`to_poll_json`], their typed JSON shapes
//! ([`StatusJson`], [`PollJson`], [`PollOptionJson`], [`MentionJson`],
//! [`TagJson`]), and the pre-resolved-input carrier types
//! ([`StatusRenderInput`], [`StatusInteractionState`],
//! [`SerializeContext`]). It does not implement `StatusService` /
//! `InteractionService` / `PollService` (later tasks, the eventual callers
//! that resolve a `Status`/`Poll`/`PollTally`/viewer-scoped operation state
//! and hand them to this module), does not reimplement Account or Media
//! JSON shape (both delegated to `accounts::serializer`/`media::serializer`
//! — see "Account/Media delegation" below), and adds no HTTP surface. It
//! does not touch `src/statuses/visibility.rs` (task 3.1) or
//! `src/statuses/addressing.rs` (task 3.2) — no visibility filtering or
//! addressing derivation happens inside a serializer, that is a caller's
//! job — though [`crate::domain::Visibility`] (the same type
//! `visibility.rs`/`addressing.rs` consume) is reused directly on
//! [`StatusJson::visibility`] rather than redefined.
//!
//! ## Typed structs + `Serialize`, not a hand-built `serde_json::json!` value
//! Follows `src/media/serializer.rs`'s established precedent (the first
//! entity serializer in this crate) and `src/accounts/serializer.rs`'s
//! identical convention (the most recent, most directly analogous prior
//! art): [`StatusJson`]/[`PollJson`]/[`PollOptionJson`]/[`MentionJson`]/
//! [`TagJson`] are plain `#[derive(Serialize)]` structs mirroring
//! Requirement 1.1's/2.1's field lists field-by-field, not a `json!{...}`
//! literal a field could silently go missing from. [`status_to_json`]/
//! [`poll_to_json`] are thin `serde_json::to_value` wrappers over
//! [`to_status_json`]/[`to_poll_json`], matching both prior serializers'
//! `to_json` convention.
//!
//! ## Account/Media delegation (this task's own boundary text: "Account/
//! メディアは上流シリアライズへ委譲")
//! [`StatusRenderInput::account`] and [`StatusRenderInput::media_attachments`]
//! are already-rendered [`serde_json::Value`]s — the caller is expected to
//! have produced them via `crate::accounts::serializer::account_to_json`
//! and `crate::media::serializer::to_json` respectively (one call per
//! attached medium) *before* calling [`status_to_json`]. This module never
//! constructs an Account or MediaAttachment shape itself, and never takes a
//! `ResolvedActor`/`AccountProfile`/`Media`/`MediaStore` as input — doing so
//! would duplicate those two modules' already-reviewed field lists and null
//! discipline instead of reusing them.
//!
//! ## Deliberate deviations from design.md's literal Service Interface
//! design.md's Service Interface sketch is:
//! ```text
//! pub fn status_to_json(status: &Status, ctx: &SerializeContext) -> serde_json::Value;
//! pub fn poll_to_json(poll: &Poll, tally: &PollTally, ctx: &SerializeContext) -> serde_json::Value;
//! pub struct SerializeContext { pub viewer: Option<Id>, pub now: OffsetDateTime, pub req_uri: RequestUriContext }
//! ```
//! Every deviation below is a documented, narrow gap-fill (the same
//! "sketch vs. actually-implemented" class of deviation `accounts/
//! serializer.rs`'s own doc comment already establishes precedent for),
//! not a silent guess:
//!
//! - **`status_to_json` takes `&StatusRenderInput`, not bare `&Status`.**
//!   [`Status`] (`model.rs`, task 1.2) stores none of Requirement 1.1's
//!   `account`/`media_attachments`/`mentions`/`tags`/`emojis`/`poll`
//!   fields, and none of Requirement 1.2's per-viewer operation state
//!   (`favourited`/`reblogged`/`bookmarked`/`pinned`/`muted`) — confirmed by
//!   reading `model.rs`'s own [`Status`] field list and its
//!   `status_holds_no_dialect_fields_beyond_the_core_field_set` exhaustive-
//!   destructure test, which fixes that field set at the type level. That
//!   is not an oversight this task can fix by editing `model.rs` (task
//!   1.2's boundary, not this task's) — a post's embedded account, its
//!   media, its extracted mentions/tags/emojis (Requirement 3.6's
//!   extraction is `StatusService::create_status`'s job, a later task), its
//!   attached poll, and its per-viewer operation state (`InteractionRepository`/
//!   `PollRepository::tally`'s job, both already-implemented but not
//!   *called from* this module — a pure serializer makes no DB/HTTP calls,
//!   mirroring `media/serializer.rs`'s and `accounts/serializer.rs`'s
//!   identical "no repository, no `PgPool`" discipline) are every one of
//!   them a *different* component's resolved output. [`StatusRenderInput`]
//!   is the single, explicit "pre-resolved input" carrier this function
//!   takes instead of a bare `&Status` — the same "pure function,
//!   pre-resolved inputs" contract `accounts/serializer.rs`'s
//!   `created_at: OffsetDateTime` parameter already establishes for the one
//!   field `ResolvedActor`/`AccountProfile` cannot supply.
//! - **`status_to_json` drops the `ctx: &SerializeContext` parameter
//!   entirely.** Every field [`SerializeContext`] could plausibly resolve
//!   for a Status (`viewer`-scoped operation state) already arrives
//!   pre-resolved via [`StatusRenderInput::interactions`] instead —
//!   `SerializeContext.viewer` alone (a bare `Id`) cannot itself produce
//!   `favourited`/`reblogged`/`bookmarked`/`pinned`/`muted` without
//!   querying `InteractionRepository`, which this pure function must not
//!   do. `SerializeContext.now` has no Status-level use (no field of
//!   [`Status`] is clock-relative — `expired` is a *Poll*-only concept,
//!   Requirement 2.3). Threading an unused `ctx` parameter through
//!   `status_to_json` just to match the sketch's shape would be pure noise;
//!   [`poll_to_json`] below still takes `ctx` because it genuinely needs
//!   `ctx.now` (see next point).
//! - **`poll_to_json` reuses the real `PollTally` type
//!   (`crate::statuses::poll_repository::PollTally`), not an invented
//!   `PollTally`.** design.md names a `tally: &PollTally` parameter but
//!   never defines that type anywhere in its own text; `poll_repository.rs`
//!   (task 2.3, already implemented and reviewed) already defines and
//!   returns exactly this shape from its own `tally()` function — `{
//!   poll_id, options: Vec<PollOption>, voters_count, own_votes: Vec<i32> }`
//!   — including `own_votes`, which already *is* Requirement 2.2's
//!   viewer-scoped state pre-resolved (task 2.3's own doc comment: "one
//!   layer down from the eventual `PollSerializer`"). Reusing it directly
//!   (rather than redefining a parallel, structurally-identical type here)
//!   is exactly the gap this task's own brief calls out investigating —
//!   `record_vote`'s only exported aggregate-read type already *is* the
//!   answer, so `poll_to_json`'s signature matches design.md's sketch
//!   character-for-character on this parameter.
//! - **`poll_to_json` gains an `emojis: &[CustomEmojiView]` parameter**,
//!   absent from design.md's sketch. Requirement 2.1 lists `emojis` as a
//!   required Poll JSON field (poll option titles may reference custom
//!   emoji shortcodes, the same way account `display_name`/`note` do — see
//!   `accounts/serializer.rs`'s `match_referenced_emojis`), but neither
//!   [`Poll`] nor [`PollOption`] (`model.rs`) carries any emoji-shortcode
//!   reference or resolved emoji field — confirmed by reading `model.rs`'s
//!   [`Poll`]/[`PollOption`] definitions, which hold no such field.
//!   Mirroring [`StatusRenderInput::mentions`]/`tags` below, this arrives
//!   as an already-resolved caller input rather than being matched/scanned
//!   inside this module (unlike `accounts/serializer.rs`'s
//!   `match_referenced_emojis`, this module does not itself scan poll
//!   option titles for shortcodes — that scanning behavior is
//!   `accounts/serializer.rs`'s own, not duplicated here for a different
//!   entity without a task boundary asking for it; a future task may wire a
//!   shared scanner if this exact duplication becomes a real problem).
//! - **`SerializeContext` drops `req_uri: RequestUriContext`.** Neither
//!   `status_to_json` nor `poll_to_json` construct any URL: a [`Status`]'s
//!   `uri`/`url` are already fully-resolved `String`/`Option<String>`
//!   fields on the domain model itself (`model.rs`), unlike `Account`
//!   (`AccountView`), which has no stored URL and must build one via
//!   `ActorUrls`/`MediaStore::public_url`. Even where a URL-shaped field
//!   does need to exist here ([`MentionJson::url`], [`TagJson::url`]), it
//!   arrives pre-resolved as part of [`StatusRenderInput::mentions`]/`tags`
//!   (see next point), so no URL-building capability is ever needed inside
//!   this module. `RequestUriContext` would also not have helped even if
//!   kept: `src/media/store.rs`'s own doc comment already establishes that
//!   `RequestUriContext` (`src/api/pagination.rs`) is Link-header-cursor-
//!   specific — "field non-public, only a private `url_with` for
//!   pagination `Link` headers" — and is *not* a general absolute-URL
//!   builder; that module's own resolution was to use `ForwardedOrigin`
//!   instead, and this module needs neither.
//! - **[`StatusRenderInput`] gains `mentions: Vec<MentionJson>` and
//!   `tags: Vec<TagJson>`**, both already-resolved caller inputs, for the
//!   same reason as `emojis` above: [`Status`] (`model.rs`) has no mentions
//!   field at all (`addressing.rs`'s own doc comment: mention extraction —
//!   Requirement 3.6 — is `StatusService::create_status`'s job, out of this
//!   task's boundary, and `addressing.rs`'s `ActorRef { uri, recipient }`
//!   is shaped for ActivityPub `to`/`cc` addressing, not for Mastodon's
//!   `Mention { id, username, url, acct }` JSON shape — reusing it directly
//!   would be structurally wrong, not merely inconvenient). `tag_repository.rs`'s
//!   [`crate::statuses::model::Tag`] (`{ id, name, created_at }`) has no
//!   `url` field either (Mastodon's `Tag { name, url }` needs one, and this
//!   module has no per-instance base-domain configuration to build one from
//!   without reintroducing the same "does this module need a domain/URL
//!   builder" question `req_uri`'s removal above already answered "no"
//!   to) — so both arrive as caller-supplied, already-shaped
//!   [`MentionJson`]/[`TagJson`] values rather than raw `ActorRef`/`Tag`
//!   domain values this module would have to (wrongly) reshape itself.

#[cfg(test)]
mod tests;

use serde::Serialize;
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::accounts::model::CustomEmojiView;
use crate::domain::{Id, Visibility};
use crate::statuses::model::{Poll, Status};
use crate::statuses::poll_repository::PollTally;

/// JSON shape of one `mentions` entry (Requirement 1.1). See this module's
/// doc comment ("Deliberate deviations") for why this arrives as an
/// already-resolved [`StatusRenderInput`] field rather than being built
/// from a domain type inside this module.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MentionJson {
    pub id: Id,
    pub username: String,
    pub url: String,
    pub acct: String,
}

/// JSON shape of one `tags` entry (Requirement 1.1). See this module's doc
/// comment ("Deliberate deviations") for why this arrives pre-resolved.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TagJson {
    pub name: String,
    pub url: String,
}

/// JSON shape of one `emojis` entry, shared by both [`StatusJson::emojis`]
/// and [`PollJson::emojis`] (Requirement 1.1, 2.1). Field-for-field
/// identical to `accounts::serializer::CustomEmojiJson` (the same Mastodon
/// `CustomEmoji` entity appears verbatim on Account/Status/Poll) — defined
/// locally rather than imported because that sibling module's own
/// `emoji_to_json` mapping helper is private to its module, and importing
/// just the struct while re-deriving the one-line mapping here is simpler
/// than exposing a new cross-module public helper for a five-field struct.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StatusEmojiJson {
    pub shortcode: String,
    pub url: String,
    pub static_url: String,
    pub visible_in_picker: bool,
    pub category: Option<String>,
}

fn emoji_to_json(emoji: &CustomEmojiView) -> StatusEmojiJson {
    StatusEmojiJson {
        shortcode: emoji.shortcode.clone(),
        url: emoji.url.clone(),
        static_url: emoji.static_url.clone(),
        visible_in_picker: emoji.visible_in_picker,
        category: emoji.category.clone(),
    }
}

/// Per-viewer operation state for one [`Status`] (Requirement 1.2:
/// `favourited`/`reblogged`/`bookmarked`/`pinned`/`muted`). Neither
/// `model.rs` nor this module resolves these — they come from
/// `InteractionRepository`'s `exists_favourite`/`find_reblog`/
/// `exists_bookmark`/`exists_pin` (task 2.2, already implemented) plus a
/// mute-state source (out of this spec's boundary — `social-graph`'s
/// relationship model, per requirements.md's Boundary Context: "フォロー/
/// ブロック/ミュート等の関係操作（social-graph）") — a future
/// `InteractionService`/`StatusService` caller resolves all five and
/// supplies this bundle. When `viewer` is `None` (unauthenticated), every
/// field is expected to be `false` (Requirement 1.2 only applies "認証済み
/// アクター文脈で" — the caller's responsibility, not enforced here).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StatusInteractionState {
    pub favourited: bool,
    pub reblogged: bool,
    pub bookmarked: bool,
    pub pinned: bool,
    pub muted: bool,
}

/// Every pre-resolved input [`status_to_json`] needs beyond the bare
/// [`Status`] row itself. See this module's doc comment ("Deliberate
/// deviations") for why each field exists and where it is expected to come
/// from. `reblog` is `Some(..)` exactly when `status.reblog_of_id` is
/// `Some(..)` (Requirement 1.3) — this module does not itself check that
/// the two agree (a caller-side invariant, not something a pure serializer
/// can validate without a repository call).
pub struct StatusRenderInput<'a> {
    pub status: &'a Status,
    /// Already-rendered Account JSON — delegated upstream, see this
    /// module's doc comment ("Account/Media delegation").
    pub account: Value,
    /// Already-rendered MediaAttachment JSON, one entry per attached
    /// medium — delegated upstream, see this module's doc comment
    /// ("Account/Media delegation").
    pub media_attachments: Vec<Value>,
    pub mentions: Vec<MentionJson>,
    pub tags: Vec<TagJson>,
    pub emojis: Vec<CustomEmojiView>,
    /// Already-rendered Poll JSON (built via [`poll_to_json`] before
    /// calling [`status_to_json`]), or `None` when `status.poll_id` is
    /// `None` (Requirement 1.5's null discipline).
    pub poll: Option<Value>,
    pub interactions: StatusInteractionState,
    /// The full render input for the boosted [`Status`] when this status is
    /// a reblog (Requirement 1.3) — recursively rendered by
    /// [`status_to_json`]. `None` when `status.reblog_of_id` is `None`.
    pub reblog: Option<Box<StatusRenderInput<'a>>>,
}

/// Non-Status-specific inputs [`poll_to_json`] needs (design.md's
/// `SerializeContext` sketch, minus `req_uri` — see this module's doc
/// comment, "Deliberate deviations"). `now` is the caller-injected current
/// time (never wall-clock `OffsetDateTime::now_utc()` — Requirement 2.3's
/// determinism requirement), consulted against a poll's `expires_at` for
/// the `expired` computation. `viewer` is carried for shape-parity with
/// design.md's sketch; every viewer-scoped boolean this module actually
/// emits (`voted`/`own_votes`, `favourited`/`reblogged`/.../`muted`)
/// already arrives independently pre-resolved (via [`PollTally::own_votes`]
/// / [`StatusInteractionState`]), so this module's own functions never read
/// `viewer` directly — see "Deliberate deviations" for why `status_to_json`
/// drops this parameter entirely rather than accept it unused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SerializeContext {
    pub viewer: Option<Id>,
    pub now: OffsetDateTime,
}

/// The Mastodon-compatible Status JSON contract (Requirement 1.1's field
/// list, in that requirement's own listing order, followed by Requirement
/// 1.2's operation-state fields). Holds no quote-post/emoji-reaction
/// (custom-federation dialect) field — verified by construction, see this
/// task's self-review (Requirement 1.6, 15.1).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StatusJson {
    pub id: Id,
    pub uri: String,
    pub url: Option<String>,
    pub account: Value,
    pub content: String,
    pub created_at: String,
    pub visibility: Visibility,
    pub sensitive: bool,
    pub spoiler_text: String,
    pub media_attachments: Vec<Value>,
    pub mentions: Vec<MentionJson>,
    pub tags: Vec<TagJson>,
    pub emojis: Vec<StatusEmojiJson>,
    pub reblogs_count: i64,
    pub favourites_count: i64,
    pub replies_count: i64,
    pub in_reply_to_id: Option<Id>,
    pub in_reply_to_account_id: Option<Id>,
    pub reblog: Option<Value>,
    pub poll: Option<Value>,
    pub language: Option<String>,
    pub edited_at: Option<String>,
    pub favourited: bool,
    pub reblogged: bool,
    pub bookmarked: bool,
    pub pinned: bool,
    pub muted: bool,
}

/// JSON shape of one [`PollJson::options`] entry (Requirement 2.1: "`title`
/// と `votes_count`").
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PollOptionJson {
    pub title: String,
    pub votes_count: i64,
}

/// The Mastodon-compatible Poll JSON contract (Requirement 2.1's field
/// list, followed by Requirement 2.2's voter-state fields).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PollJson {
    pub id: Id,
    pub expires_at: Option<String>,
    pub expired: bool,
    pub multiple: bool,
    pub votes_count: i64,
    pub voters_count: i64,
    pub options: Vec<PollOptionJson>,
    pub emojis: Vec<StatusEmojiJson>,
    pub voted: bool,
    pub own_votes: Vec<i32>,
}

/// Renders `when` as an RFC 3339 timestamp string, matching
/// `accounts/serializer.rs::format_time`'s identical convention (Mastodon's
/// own ISO 8601 timestamp shape).
fn format_time(when: OffsetDateTime) -> String {
    when.format(&Rfc3339)
        .expect("a valid OffsetDateTime always formats as RFC 3339")
}

/// Projects `input` into [`StatusJson`] — a pure, total mapping with no
/// business logic left to do (viewer-scoped operation state, reblog
/// nesting, and every delegated field already arrived pre-resolved on
/// `input`). Recurses into `input.reblog` for Requirement 1.3's nested
/// reblog rendering.
pub fn to_status_json(input: &StatusRenderInput) -> StatusJson {
    let status = input.status;
    let reblog = input
        .reblog
        .as_ref()
        .map(|nested| status_to_json(nested.as_ref()));

    StatusJson {
        id: status.id,
        uri: status.uri.clone(),
        url: status.url.clone(),
        account: input.account.clone(),
        content: status.content.clone(),
        created_at: format_time(status.created_at),
        visibility: status.visibility,
        sensitive: status.sensitive,
        spoiler_text: status.spoiler_text.clone(),
        media_attachments: input.media_attachments.clone(),
        mentions: input.mentions.clone(),
        tags: input.tags.clone(),
        emojis: input.emojis.iter().map(emoji_to_json).collect(),
        reblogs_count: status.reblogs_count,
        favourites_count: status.favourites_count,
        replies_count: status.replies_count,
        in_reply_to_id: status.in_reply_to_id,
        in_reply_to_account_id: status.in_reply_to_account_id,
        reblog,
        poll: input.poll.clone(),
        language: status.language.clone(),
        edited_at: status.edited_at.map(format_time),
        favourited: input.interactions.favourited,
        reblogged: input.interactions.reblogged,
        bookmarked: input.interactions.bookmarked,
        pinned: input.interactions.pinned,
        muted: input.interactions.muted,
    }
}

/// [`to_status_json`], converted to a plain [`serde_json::Value`] (matching
/// `media/serializer.rs::to_json`'s / `accounts/serializer.rs::account_to_json`'s
/// convention) — the shape [`crate::contract::assert_golden`] compares
/// against (Requirement 1.4).
pub fn status_to_json(input: &StatusRenderInput) -> Value {
    serde_json::to_value(to_status_json(input)).expect("StatusJson always serializes to JSON")
}

/// Projects `poll`/`tally` into [`PollJson`] (Requirements 2.1, 2.2, 2.3).
/// `expired` is computed from `poll.expires_at` against `ctx.now` — never
/// wall-clock time (Requirement 2.3's determinism requirement). A poll with
/// no `expires_at` (never closes) is always `expired: false`, mirroring
/// `poll_repository.rs::record_vote`'s own deadline check
/// (`if let Some(expires_at) = expires_at && now >= expires_at`).
pub fn to_poll_json(
    poll: &Poll,
    tally: &PollTally,
    emojis: &[CustomEmojiView],
    ctx: &SerializeContext,
) -> PollJson {
    let expired = poll.expires_at.is_some_and(|deadline| ctx.now >= deadline);
    let votes_count = tally.options.iter().map(|option| option.votes_count).sum();
    let mut own_votes = tally.own_votes.clone();
    own_votes.sort_unstable();

    PollJson {
        id: poll.id,
        expires_at: poll.expires_at.map(format_time),
        expired,
        multiple: poll.multiple,
        votes_count,
        voters_count: tally.voters_count,
        options: tally
            .options
            .iter()
            .map(|option| PollOptionJson {
                title: option.title.clone(),
                votes_count: option.votes_count,
            })
            .collect(),
        emojis: emojis.iter().map(emoji_to_json).collect(),
        voted: !tally.own_votes.is_empty(),
        own_votes,
    }
}

/// [`to_poll_json`], converted to a plain [`serde_json::Value`] — the shape
/// [`crate::contract::assert_golden`] compares against (Requirement 2.4).
pub fn poll_to_json(
    poll: &Poll,
    tally: &PollTally,
    emojis: &[CustomEmojiView],
    ctx: &SerializeContext,
) -> Value {
    serde_json::to_value(to_poll_json(poll, tally, emojis, ctx))
        .expect("PollJson always serializes to JSON")
}
