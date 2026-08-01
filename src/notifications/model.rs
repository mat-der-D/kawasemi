//! Notification domain types (`model` component, design.md "Notification
//! Domain / ドメイン層" -> `model`, Requirements 1.1, 1.5, 5.1, 6.1, 8.1;
//! task 1.2, `Boundary: model`).
//!
//! Scope: this module owns exactly the three types task 1.2's own
//! instruction enumerates — the closed v1 kind enum [`NotificationType`],
//! the persisted notification [`Notification`], and the upstream-emitted
//! notification-generation event [`NotificationEvent`] — built on
//! core-runtime's [`Id`]/[`AccountRef`] (`crate::domain`, not redefined
//! here) and `time::OffsetDateTime`, mirroring `src/statuses/model.rs`'s
//! and `src/social_graph/model.rs`'s identical `use crate::domain::{..}`
//! precedent for consuming rather than redefining the shared primitives.
//!
//! No persistence (`NotificationRepository`, task 1.3), no delegation
//! ports (`NotificationEventSink`/`NotificationDeliverySink`, task 2.2), no
//! filtering (`NotificationFilter`, task 2.3), no generation logic
//! (`NotificationGenerator`, task 2.4), no serialization to Mastodon JSON
//! (`NotificationSerializer`, task 2.1), no business logic
//! (`NotificationService`, task 3.2), no HTTP surface (`NotificationEndpoints`,
//! task 4.1), and no `NotificationModule` composition/wiring (task 4.x) live
//! here — those consume the types defined in this module but are out of
//! scope for task 1.2 (`Boundary: model`).
//!
//! ## Relationship to `statuses::notification_sink` (deliberately not touched)
//! `src/statuses/notification_sink.rs` (task 9.2) already defines a
//! same-shaped `NotificationType`/`NotificationEvent` pair as a *temporary*
//! placeholder — its own doc comment explains it exists only because
//! notifications had zero implemented tasks when statuses-core needed to
//! emit events, and that "a future notifications implementation can
//! adopt/migrate this module's types without a breaking rewrite". This
//! task's own boundary is `model` only (not `ports`/`NotificationEventSink`,
//! task 2.2/3.1's job), so the two definitions deliberately coexist as
//! distinct types for now; migrating `statuses::notification_sink`'s and
//! `social_graph::transitions`'s many call sites onto this module's types
//! is out of this task's scope. See this module's own doc comment on
//! [`NotificationEvent`] for the field-shape correspondence that migration
//! will rely on.
//!
//! ## Closed v1 kind set (Requirement 1.5)
//! [`NotificationType`] is a fieldless enum with exactly the eight v1
//! variants design.md's "型定義（抜粋）" names — no `Other(String)` escape
//! hatch, no `#[non_exhaustive]`. Out-of-scope kinds (v2 grouping, admin
//! notifications) are therefore not representable as a value of this type
//! at all, which is what "範囲外の種別が型として表現できず" (this task's
//! completion definition) means at the type level; this module's tests
//! prove it by exhaustively `match`-ing all eight variants with no `_`
//! wildcard arm (mirrors `src/statuses/model.rs`'s identical exhaustive-
//! destructure technique for proving a closed field/variant set at compile
//! time rather than via a runtime check).
//!
//! ## `status_id` / `target_status_id` (Requirement 1.1, 5.1)
//! Both are `Option<Id>`, `None` for the two kinds that carry no target
//! post (`Follow`/`FollowRequest`) and `Some` for the six post-related
//! kinds. `Option<Id>` already makes "no target post" representable
//! without any dedicated per-kind struct or a placeholder `Id` value — no
//! separate enforcement is needed at this task's level (design.md: "type
//! を伴わない種別は status_id=None" is a documented invariant honored by
//! later construction sites — the generator, task 2.4 — not a constraint
//! this type itself enforces structurally, since a status-related kind
//! paired with `status_id: None` is not a type-level contradiction, only a
//! generator-level one).
//!
//! ## Derives: no `Serialize`/`Deserialize` here (mirrors `Visibility`'s split)
//! `crate::domain::Visibility` documents the precedent this module follows:
//! it owns only the enum and, because its serde representation *is* its
//! final wire representation, derives `Serialize`/`Deserialize` directly.
//! `NotificationType`/`Notification`/`NotificationEvent` are different: the
//! Mastodon-JSON wire shape (`NotificationSerializer`, task 2.1) embeds
//! upstream-delegated `Account`/`Status` JSON and applies its own null
//! discipline — it is not a thin derive over these fields. Deriving
//! `Serialize`/`Deserialize` here would speculatively commit to a JSON
//! shape this task's own boundary does not need and task 2.1 does not
//! reuse, so only `Debug`/`Clone`/`PartialEq`/`Eq` are derived (what this
//! task's own unit tests require), plus `Copy` on the fieldless
//! [`NotificationType`] (mirrors `crate::domain::Visibility` and
//! `social_graph::model::FollowRequestDirection`'s identical fieldless-enum
//! precedent).

use time::OffsetDateTime;

use crate::domain::{AccountRef, Id};

/// The closed v1 notification kind set (design.md's "型定義（抜粋）";
/// Requirement 1.5). See this module's doc comment, "Closed v1 kind set",
/// for why no out-of-scope kind is representable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationType {
    Mention,
    Follow,
    FollowRequest,
    Favourite,
    Reblog,
    Poll,
    Status,
    Update,
}

/// A persisted notification (design.md's "型定義（抜粋）"; Requirement
/// 1.1): `recipient_id` is the receiving local actor, `origin` is the
/// notification's source actor (local or remote, hence [`AccountRef`]
/// rather than a plain [`Id`]), and `status_id` is the associated post for
/// post-related kinds — `None` for `Follow`/`FollowRequest` (see this
/// module's doc comment, "`status_id` / `target_status_id`"). `dismissed`
/// is the消去 flag `migrations/0009_notifications.sql`'s dedup partial
/// index scopes on (`WHERE NOT dismissed`, Requirement 8.1) — not
/// interpreted or enforced by this type itself, only carried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub id: Id,
    pub recipient_id: Id,
    pub kind: NotificationType,
    pub origin: AccountRef,
    pub status_id: Option<Id>,
    pub dismissed: bool,
    pub created_at: OffsetDateTime,
}

/// An upstream-emitted, immutable notification-generation event
/// (design.md's "型定義（抜粋）"; Requirement 5.1: "上流が emit する不変
/// ペイロード"). `recipient`/`origin` are [`AccountRef`] because either may
/// be local or remote (Requirement 5.3 — only a local `recipient` is ever
/// turned into a [`Notification`], a later generator-level check, not
/// enforced by this type). `target_status_id` mirrors [`Notification::status_id`]'s
/// "`None` for `Follow`/`FollowRequest`" discipline.
///
/// Field-shape note: this matches `statuses::notification_sink::NotificationEvent`'s
/// shape verbatim by construction — see this module's doc comment,
/// "Relationship to `statuses::notification_sink`" — so a later task can
/// migrate that module's callers onto this type without a field-shape
/// change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationEvent {
    pub recipient: AccountRef,
    pub origin: AccountRef,
    pub kind: NotificationType,
    pub target_status_id: Option<Id>,
    pub occurred_at: OffsetDateTime,
}

// Unit tests live inline below, not in a sibling `model/tests.rs` file.
// `.kiro/steering/structure.md`'s general statement is a same-directory
// `tests.rs` submodule, but the actual established precedent for exactly
// this kind of file — a pure value-type `model.rs` with no I/O — is
// inline: `src/statuses/model.rs`, `src/social_graph/model.rs`, and
// `src/accounts/model.rs` all declare `#[cfg(test)] mod tests { .. }`
// in-file (there is no `src/statuses/model/tests.rs` etc. on disk). This
// module matches that specific, closer-matching precedent rather than the
// steering doc's general statement — see CONCERNS in this task's status
// report.
#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use super::*;

    fn sample_notification(kind: NotificationType, status_id: Option<Id>) -> Notification {
        Notification {
            id: Id::from_i64(1),
            recipient_id: Id::from_i64(10),
            kind,
            origin: AccountRef::Local(Id::from_i64(20)),
            status_id,
            dismissed: false,
            created_at: datetime!(2026-07-24 00:00:00 UTC),
        }
    }

    fn sample_event(kind: NotificationType, target_status_id: Option<Id>) -> NotificationEvent {
        NotificationEvent {
            recipient: AccountRef::Local(Id::from_i64(10)),
            origin: AccountRef::Remote(Id::from_i64(20)),
            kind,
            target_status_id,
            occurred_at: datetime!(2026-07-24 00:00:00 UTC),
        }
    }

    /// Requirement 1.5 / completion definition ("範囲外の種別が型として表現
    /// できず"): an exhaustive `match` over all eight `NotificationType`
    /// variants, with **no wildcard arm**, compiles. If a ninth variant were
    /// ever added, this match would fail to compile until updated — that is
    /// the proof the v1 kind set is closed at the type level, not merely by
    /// convention. This also proves nothing outside the eight named variants
    /// is constructible: there is no other arm a value of this type could
    /// ever take.
    #[test]
    fn notification_type_is_exhaustively_matched_by_exactly_eight_variants() {
        fn label(kind: NotificationType) -> &'static str {
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

        let all = [
            NotificationType::Mention,
            NotificationType::Follow,
            NotificationType::FollowRequest,
            NotificationType::Favourite,
            NotificationType::Reblog,
            NotificationType::Poll,
            NotificationType::Status,
            NotificationType::Update,
        ];
        let labels: Vec<&'static str> = all.into_iter().map(label).collect();
        assert_eq!(
            labels,
            vec![
                "mention",
                "follow",
                "follow_request",
                "favourite",
                "reblog",
                "poll",
                "status",
                "update",
            ]
        );
    }

    /// A status-less kind (`Follow`) is representable with `status_id: None`
    /// — no target post required — proving the completion definition's
    /// "status を伴わない種別が対象投稿なしで表現できる" half for
    /// [`Notification`].
    #[test]
    fn notification_follow_kind_is_representable_without_a_status() {
        let notification = sample_notification(NotificationType::Follow, None);
        assert_eq!(notification.kind, NotificationType::Follow);
        assert_eq!(notification.status_id, None);
    }

    /// Same proof for `FollowRequest`.
    #[test]
    fn notification_follow_request_kind_is_representable_without_a_status() {
        let notification = sample_notification(NotificationType::FollowRequest, None);
        assert_eq!(notification.kind, NotificationType::FollowRequest);
        assert_eq!(notification.status_id, None);
    }

    /// A post-related kind (`Favourite`) is representable *with* a target
    /// post, exercising the other half of `status_id`'s `Option<Id>` shape.
    #[test]
    fn notification_favourite_kind_is_representable_with_a_status() {
        let notification = sample_notification(NotificationType::Favourite, Some(Id::from_i64(99)));
        assert_eq!(notification.kind, NotificationType::Favourite);
        assert_eq!(notification.status_id, Some(Id::from_i64(99)));
    }

    /// Exhaustively destructures a [`Notification`] value (no `..` rest
    /// pattern) — mirrors `src/statuses/model.rs`'s identical technique for
    /// proving a required-or-absent field set at the type level: this test
    /// fails to compile the moment a field is added or removed without
    /// updating it here.
    #[test]
    fn notification_field_set_is_exhaustive() {
        let notification = sample_notification(NotificationType::Reblog, Some(Id::from_i64(5)));
        let Notification {
            id,
            recipient_id,
            kind,
            origin,
            status_id,
            dismissed,
            created_at,
        } = notification;
        assert_eq!(id, Id::from_i64(1));
        assert_eq!(recipient_id, Id::from_i64(10));
        assert_eq!(kind, NotificationType::Reblog);
        assert_eq!(origin, AccountRef::Local(Id::from_i64(20)));
        assert_eq!(status_id, Some(Id::from_i64(5)));
        assert!(!dismissed);
        assert_eq!(created_at, datetime!(2026-07-24 00:00:00 UTC));
    }

    /// [`NotificationEvent`]'s status-less kind (`FollowRequest`) is
    /// representable with `target_status_id: None`.
    #[test]
    fn notification_event_follow_request_kind_is_representable_without_a_target_status() {
        let event = sample_event(NotificationType::FollowRequest, None);
        assert_eq!(event.kind, NotificationType::FollowRequest);
        assert_eq!(event.target_status_id, None);
    }

    /// [`NotificationEvent`]'s post-related kind (`Mention`) is
    /// representable *with* a target post.
    #[test]
    fn notification_event_mention_kind_is_representable_with_a_target_status() {
        let event = sample_event(NotificationType::Mention, Some(Id::from_i64(7)));
        assert_eq!(event.kind, NotificationType::Mention);
        assert_eq!(event.target_status_id, Some(Id::from_i64(7)));
    }

    /// Exhaustively destructures a [`NotificationEvent`] value (no `..`
    /// rest pattern) for the same reason as `notification_field_set_is_exhaustive`.
    #[test]
    fn notification_event_field_set_is_exhaustive() {
        let event = sample_event(NotificationType::Poll, Some(Id::from_i64(3)));
        let NotificationEvent {
            recipient,
            origin,
            kind,
            target_status_id,
            occurred_at,
        } = event;
        assert_eq!(recipient, AccountRef::Local(Id::from_i64(10)));
        assert_eq!(origin, AccountRef::Remote(Id::from_i64(20)));
        assert_eq!(kind, NotificationType::Poll);
        assert_eq!(target_status_id, Some(Id::from_i64(3)));
        assert_eq!(occurred_at, datetime!(2026-07-24 00:00:00 UTC));
    }

    /// `AccountRef::Local`/`Remote` both remain distinguishable through
    /// `Notification::origin`/`NotificationEvent::origin` — a notification's
    /// source may be a local or a remote actor (design.md's model doc).
    #[test]
    fn origin_distinguishes_local_from_remote_on_both_types() {
        let local_origin =
            sample_notification(NotificationType::Reblog, Some(Id::from_i64(1))).origin;
        let remote_origin = sample_event(NotificationType::Reblog, Some(Id::from_i64(1))).origin;
        assert_eq!(local_origin, AccountRef::Local(Id::from_i64(20)));
        assert_eq!(remote_origin, AccountRef::Remote(Id::from_i64(20)));
        assert_ne!(local_origin, remote_origin);
    }
}
