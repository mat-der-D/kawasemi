//! `NotificationSerializer` (design.md "Notification Domain / ドメイン層"
//! -> "NotificationSerializer"; Requirements 1.1, 1.2, 1.3, 1.4, 1.5, 1.6;
//! task 2.1, `Boundary: NotificationSerializer`): maps an already-persisted
//! [`Notification`] onto the Mastodon-compatible Notification JSON outer
//! shell (`id`/`type`/`created_at`/`account`/`status`), delegating the
//! `account` and `status` embeds to accounts-and-instance's and
//! statuses-core's own serializers rather than redefining either contract,
//! and registers the outer shell as goldens against api-foundation's
//! [`crate::contract::assert_golden`] harness.
//!
//! Scope: this module owns exactly [`to_notification_json`]/
//! [`notification_to_json`], the outer-shell JSON shape ([`NotificationJson`]),
//! and the pre-resolved-input carrier types ([`NotificationRenderInput`],
//! [`SerializeContext`]). It does not implement `NotificationService`
//! (task 3.2, the eventual caller that resolves a [`Notification`] plus its
//! origin `AccountView`/related `Status` and hands them to this module), does
//! not resolve an origin account or a related status itself (no repository,
//! no `PgPool`, no HTTP call — a pure function, mirroring
//! `src/statuses/serializer.rs`'s and `src/accounts/serializer.rs`'s
//! identical "pure serializer, pre-resolved inputs only" discipline), and
//! adds no HTTP surface, generation logic, or `NotificationModule`
//! composition (later tasks, `Boundary: NotificationGenerator` /
//! `NotificationService` / `NotificationEndpoints` / `NotificationModule`).
//!
//! ## Typed struct + `Serialize`, not a hand-built `serde_json::json!` value
//! Follows `src/media/serializer.rs`'s/`src/accounts/serializer.rs`'s/
//! `src/statuses/serializer.rs`'s established precedent: [`NotificationJson`]
//! is a plain `#[derive(Serialize)]` struct mirroring Requirement 1.1's
//! field list field-by-field (plus the `status` embed point Requirement
//! 1.2 adds), not a `json!{...}` literal a field could silently go missing
//! from. [`notification_to_json`] is a thin `serde_json::to_value` wrapper
//! over [`to_notification_json`], matching every other serializer's
//! `to_json` convention in this crate.
//!
//! ## Deliberate deviations from design.md's literal Service Interface
//! design.md's Service Interface sketch is:
//! ```text
//! pub fn to_json(&self, n: &Notification, ctx: &SerializeContext) -> serde_json::Value; // account/status は上流シリアライザへ委譲
//! pub struct SerializeContext { pub viewer: Id, pub now: OffsetDateTime } // viewer = 通知受信者
//! ```
//! Every deviation below is a documented, narrow gap-fill — the same class
//! of deviation `src/accounts/serializer.rs`'s and
//! `src/statuses/serializer.rs`'s own doc comments already establish
//! precedent for on this exact `&self`-sketch-vs-free-function-reality
//! point (neither sibling serializer is implemented as a struct method
//! either, despite their own design.md sketches also reading `&self`) —
//! not a silent guess:
//!
//! - **`to_json` becomes [`to_notification_json`]/[`notification_to_json`]
//!   free functions, not `&self` methods.** Mirrors both sibling
//!   serializers' identical deviation; there is no serializer-instance
//!   state anywhere in this crate's three prior serializers, so a struct
//!   receiver would be pure ceremony.
//! - **`to_json` gains an `input: &NotificationRenderInput` parameter in
//!   place of `n: &Notification` alone.** [`Notification`] (`model.rs`,
//!   task 1.2) carries only `status_id: Option<Id>` and `origin:
//!   AccountRef` — a reference, not a rendered `account`/`status` JSON
//!   value. Actually resolving those into JSON requires the origin
//!   account's full profile (username/avatar/counts/...) and, for
//!   post-related kinds, the related post's full render input (its own
//!   account, media, mentions, interaction state, ...) — every one of
//!   which is a *different* component's resolved output (accounts-and-
//!   instance's `AccountView`/`AccountService`, statuses-core's
//!   `StatusRenderInput`/`StatusService`), not something this pure
//!   function can query for itself without a repository call. Exactly
//!   `src/statuses/serializer.rs::StatusRenderInput.account`'s own
//!   "already-rendered `Value` — delegated upstream" precedent:
//!   [`NotificationRenderInput::account`]/[`NotificationRenderInput::status`]
//!   are the pre-rendered `Value`s a caller (`NotificationService`, task
//!   3.2) is expected to have produced via
//!   `crate::accounts::serializer::account_to_json`/
//!   `crate::statuses::serializer::status_to_json` *before* calling this
//!   module — this module embeds them verbatim (for `account`) or
//!   verbatim-unless-nulled-by-kind (for `status`, see "Null discipline is
//!   enforced here, not just documented" below), never reconstructing
//!   either contract.
//! - **`SerializeContext.now` is carried but currently unused.** No field
//!   of [`Notification`]'s outer shell is clock-relative (`created_at`
//!   is stored, not computed) — mirrors
//!   `src/statuses/serializer.rs::SerializeContext.viewer`'s identical
//!   "carried for interface parity with design.md's sketch, not read by
//!   this module's own functions" precedent. `SerializeContext.viewer`
//!   (design.md: "viewer = 通知受信者") is likewise unused *by this
//!   module* — the receiver-viewpoint requirement (1.2) is satisfied by
//!   whichever `StatusRenderInput` the caller built *before* calling
//!   `status_to_json` to produce [`NotificationRenderInput::status`] (its
//!   `interactions: StatusInteractionState` must already be queried from
//!   the recipient's own viewpoint) — this module has no `Notification`-
//!   level use for a second, independent read of `viewer`.
//!
//! ## Null discipline is enforced here, not just documented (Requirement 1.4)
//! Rather than trusting `NotificationRenderInput::status` to already be
//! `None` for `Follow`/`FollowRequest` (a caller-side invariant this pure
//! function cannot itself verify without seeing the caller's own logic —
//! the same class of un-enforceable caller invariant
//! `StatusRenderInput`'s own doc comment names for `reblog`), this module
//! actively discards any `status` value passed in for those two kinds. A
//! `NotificationRenderInput` built with a `Follow` notification but
//! `status: Some(..)` (e.g. a caller bug) still renders `status: null`.
//! This makes the completion condition ("follow系でstatusがnull")
//! unconditional at this module's own boundary, not merely
//! caller-convention-dependent. `status` is emitted as an explicit JSON
//! `null` (never an omitted key) for that case, matching
//! `src/statuses/serializer.rs::StatusJson`'s own established convention
//! of never `skip_serializing_if`-omitting an `Option` field (e.g.
//! `poll`/`in_reply_to_id`/`reblog` all serialize as explicit `null`, not
//! an absent key) — task 2.1's own dispatch brief flags this exact
//! "omitted key vs explicit null" question and directs following sibling
//! convention.
//!
//! ## `type` is restricted to the v1 kind set by construction (Requirement 1.5)
//! [`kind_str`] exhaustively matches all eight [`NotificationType`]
//! variants with no wildcard arm (mirrors `model.rs`'s identical
//! exhaustive-match technique) — there is no code path that can emit a
//! `type` string outside Mastodon's eight v1 values, and
//! [`NotificationType`] itself has no variant beyond those eight (task
//! 1.2's own closed-enum guarantee), so this requirement is already
//! doubly enforced before this module is reached at all.

#[cfg(test)]
mod tests;

use serde::Serialize;
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::domain::Id;
use crate::notifications::model::{Notification, NotificationType};

/// Non-`Notification`-specific inputs (design.md's `SerializeContext`
/// sketch, field-for-field). See this module's doc comment ("Deliberate
/// deviations") for why neither field is currently read by this module's
/// own functions — both are carried for interface parity with design.md
/// and for the caller-side resolution they document (`now` for a future
/// clock-relative field, `viewer` documenting the receiver-viewpoint
/// contract the caller's own `status` embed must already honor).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SerializeContext {
    pub viewer: Id,
    pub now: OffsetDateTime,
}

/// Every pre-resolved input [`to_notification_json`]/[`notification_to_json`]
/// need beyond the bare [`Notification`] row itself. See this module's doc
/// comment ("Deliberate deviations") for why each field exists and where it
/// is expected to come from.
pub struct NotificationRenderInput<'a> {
    pub notification: &'a Notification,
    /// Already-rendered Account JSON for `notification.origin` — delegated
    /// upstream via `crate::accounts::serializer::account_to_json` (or
    /// `AccountSerializer`'s higher-level builders), see this module's doc
    /// comment ("Deliberate deviations"). Always embedded (Requirement
    /// 1.3): every notification kind has an origin account.
    pub account: Value,
    /// Already-rendered Status JSON for the related post, from the
    /// notification recipient's own viewpoint — delegated upstream via
    /// `crate::statuses::serializer::status_to_json` (Requirement 1.2), or
    /// `None` when the notification has no related post
    /// (`notification.status_id.is_none()`, always true for `Follow`/
    /// `FollowRequest`). This module discards any `Some(..)` value here for
    /// `Follow`/`FollowRequest` regardless — see this module's doc comment
    /// ("Null discipline is enforced here, not just documented").
    pub status: Option<Value>,
}

/// The Mastodon-compatible Notification outer-shell JSON contract
/// (Requirement 1.1's field list plus Requirement 1.2's `status` embed
/// point). Holds no v2 grouping field (`group_key`, out of this spec's
/// scope per requirements.md's Boundary Context) — this module's own kind
/// set is exhaustively the eight v1 variants (Requirement 1.5).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NotificationJson {
    pub id: Id,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub created_at: String,
    pub account: Value,
    pub status: Option<Value>,
}

/// Renders `when` as an RFC 3339 timestamp string, matching
/// `accounts/serializer.rs::format_time`'s/`statuses/serializer.rs::format_time`'s
/// identical convention (Mastodon's own ISO 8601 timestamp shape).
fn format_time(when: OffsetDateTime) -> String {
    when.format(&Rfc3339)
        .expect("a valid OffsetDateTime always formats as RFC 3339")
}

/// Maps a [`NotificationType`] to Mastodon's own wire string for it
/// (Requirement 1.5). Exhaustive match, no wildcard arm — see this
/// module's doc comment ("`type` is restricted to the v1 kind set by
/// construction").
fn kind_str(kind: NotificationType) -> &'static str {
    match kind {
        NotificationType::Mention => "mention",
        NotificationType::Follow => "follow",
        NotificationType::FollowRequest => "follow_request",
        NotificationType::Favourite => "favourite",
        NotificationType::Reblog => "reblog",
        NotificationType::Poll => "poll",
        NotificationType::Status => "status",
        NotificationType::Update => "update",
    }
}

/// `true` for the two kinds that never carry a related post (Requirement
/// 1.4): `Follow`/`FollowRequest`. Every other v1 kind is post-related
/// (Requirement 1.2).
fn is_status_less_kind(kind: NotificationType) -> bool {
    matches!(
        kind,
        NotificationType::Follow | NotificationType::FollowRequest
    )
}

/// Projects `input` into [`NotificationJson`] — a pure, total mapping with
/// no business logic left to do (the origin account and, for post-related
/// kinds, the related status already arrived pre-resolved on `input`).
/// Enforces Requirement 1.4's null discipline unconditionally for
/// `Follow`/`FollowRequest` regardless of what `input.status` carries — see
/// this module's doc comment.
pub fn to_notification_json(input: &NotificationRenderInput) -> NotificationJson {
    let notification = input.notification;
    let status = if is_status_less_kind(notification.kind) {
        None
    } else {
        input.status.clone()
    };

    NotificationJson {
        id: notification.id,
        kind: kind_str(notification.kind),
        created_at: format_time(notification.created_at),
        account: input.account.clone(),
        status,
    }
}

/// [`to_notification_json`], converted to a plain [`serde_json::Value`]
/// (matching every other serializer's `to_json` convention in this crate)
/// — the shape [`crate::contract::assert_golden`] compares against
/// (Requirement 1.6).
pub fn notification_to_json(input: &NotificationRenderInput) -> Value {
    serde_json::to_value(to_notification_json(input))
        .expect("NotificationJson always serializes to JSON")
}
