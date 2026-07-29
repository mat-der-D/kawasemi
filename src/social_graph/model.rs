//! Social graph domain types (`model` component, design.md "Social Graph
//! Domain / ドメイン層" -> `model`, Requirements 1.5, 2.1, 4.2, 4.3, 5.1,
//! 8.1; task 1.2, `Boundary: model`).
//!
//! Scope: this module owns exactly the domain value types design.md's
//! model excerpt names — [`FollowOptions`], [`Follow`],
//! [`FollowRequestDirection`], [`FollowRequest`], [`MuteOptions`],
//! [`Mute`], and [`Block`]. [`AccountRef`] is *not* redefined here: it is
//! imported from `crate::domain` (core-runtime's canonical shared
//! primitives module, mirroring `src/accounts/model.rs`'s and
//! `src/statuses/model.rs`'s identical precedent), per this task's own
//! explicit instruction and design.md's model excerpt (`use core_runtime::
//! domain_primitives::AccountRef;` — `core_runtime::domain_primitives` is
//! that document's illustrative name for what this crate exposes as
//! `crate::domain`).
//!
//! Field names match design.md's Rust excerpt exactly (`reblogs`/`notify`,
//! not `showing_reblogs`/`notifying` — those are the *Relationship JSON
//! contract*'s field names, owned and applied by accounts-and-instance's
//! `RelationshipMapper`/`RelationshipSerializer`, out of this task's
//! boundary; Requirement 1.5's `showing_reblogs`/`notifying`/`languages`
//! flags are the semantic targets these fields back).
//!
//! No persistence (`RelationshipRepository`, task 1.3), no approval policy,
//! no Activity generation, no state-transition functions, no
//! relationship -> contract mapping, no business services, no inbound
//! Activity handlers, no delegation-port implementations, and no HTTP
//! surface live here — those consume the types defined in this module but
//! are out of scope for task 1.2 (`Boundary: model`).
//!
//! ## What this module deliberately does not do
//! - Does not resolve [`MuteOptions::duration`] (an optional seconds-based
//!   duration *input*) into [`Mute::expires_at`] (a resolved absolute
//!   timestamp). That resolution needs an injected `Clock` (steering's
//!   "決定性" principle: time is never read directly, only via
//!   `RuntimeContext`) and belongs to the repository/service layer
//!   (`RelationshipRepository`/`MuteService`, later tasks), not this pure
//!   value-type layer.
//! - Does not enforce the "(subject, target) uniqueness, no secret values"
//!   invariant design.md's model excerpt states in code — that invariant is
//!   a documentation/design constraint on what fields these types must
//!   *not* grow (e.g. no bearer-token-like secret field), not a runtime
//!   check a value type can itself perform. `follows` / `follow_requests`
//!   (scoped by `direction`) / `mutes` / `blocks`' own `UNIQUE` constraints
//!   (`migrations/0012_social_graph.sql`, task 1.1) are the actual
//!   enforcement point.

use time::OffsetDateTime;

use crate::domain::AccountRef;

/// The follow-behavior options a follow request may carry (Requirement
/// 1.5): whether to show the followee's reblogs, whether to be notified of
/// their new posts, and which of their languages to include. Mirrored 1:1
/// by [`Follow`]'s own `reblogs`/`notify`/`languages` fields once a follow
/// is established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowOptions {
    pub reblogs: bool,
    pub notify: bool,
    pub languages: Vec<String>,
}

/// An established follow relationship: `follower` follows `followee`
/// (Requirement 8.1's single source of truth for the `following`/
/// `followed_by` Relationship flags). Holds the follow-behavior options
/// (`reblogs`/`notify`/`languages`, Requirement 1.5) and the outbound
/// Follow Activity id (`activity_id`) this relationship's eventual
/// Undo(Follow) must reference (Requirement 1.4) — mirrors
/// `follows.activity_id` (`migrations/0012_social_graph.sql`, task 1.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Follow {
    pub follower: AccountRef,
    pub followee: AccountRef,
    pub reblogs: bool,
    pub notify: bool,
    pub languages: Vec<String>,
    pub activity_id: String,
    pub created_at: OffsetDateTime,
}

/// Distinguishes a locally-initiated pending follow request (`Outbound`,
/// awaiting the target's Accept/Reject) from one recorded on receipt of a
/// remote Follow Activity this instance has not yet accepted or rejected
/// (`Inbound`) — Requirement 2.1. Matches `follow_requests.direction`'s
/// `'outbound'`/`'inbound'` TEXT values (`migrations/0012_social_graph.sql`,
/// task 1.1): both variants may coexist for the same (requester, target)
/// pair without colliding, since the underlying table's own uniqueness
/// constraint is scoped by direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FollowRequestDirection {
    Outbound,
    Inbound,
}

/// A pending follow request: `requester` has asked to follow `target`, not
/// yet established as a [`Follow`] (Requirement 2.1). `direction`
/// distinguishes the locally-initiated ("送信中") case from the
/// received-and-pending ("受信保留") case (see [`FollowRequestDirection`]).
/// `activity_id` is the Follow Activity id the eventual Accept/Reject
/// Activity references (Requirements 2.3, 2.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowRequest {
    pub requester: AccountRef,
    pub target: AccountRef,
    pub direction: FollowRequestDirection,
    pub activity_id: String,
    pub created_at: OffsetDateTime,
}

/// The mute options a mute request may carry (Requirement 4.2, 4.3):
/// whether to also mute notifications, and an optional duration in seconds
/// after which the mute should be lifted. `duration` is an *input* only —
/// this type does not resolve it against a clock (see this module's doc
/// comment). `None` means an unbounded/default duration; the repository/
/// service layer decides how an absent duration is treated when computing
/// [`Mute::expires_at`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MuteOptions {
    pub notifications: bool,
    pub duration: Option<i64>,
}

/// A mute relationship: `muter` has muted `muted` (Requirement 8.1's single
/// source of truth for the `muting`/`muting_notifications` Relationship
/// flags). `expires_at` is the *resolved*, absolute expiration timestamp
/// (`None` = no expiration) — mirrors `mutes.expires_at`'s nullable column
/// (`migrations/0012_social_graph.sql`, task 1.1) and backs the
/// duration-based auto-lift behavior (Requirements 4.3, 9.3). Unlike
/// [`MuteOptions::duration`], this field is already resolved: computing it
/// from a duration input is the repository/service layer's job (an
/// injected `Clock`, not this module).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mute {
    pub muter: AccountRef,
    pub muted: AccountRef,
    pub notifications: bool,
    pub expires_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
}

/// A block relationship: `blocker` has blocked `blocked` (Requirement 8.1's
/// single source of truth for the `blocking` Relationship flag; a signer's
/// `blocked_by` status is derived from the *opposite*-direction `Block` row
/// — no separate "blocked by" type, mirroring `migrations/
/// 0012_social_graph.sql`'s naming-note precedent). Holds the outbound
/// Block Activity id (`activity_id`) this relationship's eventual
/// Undo(Block) must reference (Requirement 5.4) — mirrors
/// `blocks.activity_id` (task 1.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub blocker: AccountRef,
    pub blocked: AccountRef,
    pub activity_id: String,
    pub created_at: OffsetDateTime,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Id;
    use time::macros::datetime;

    // -- Follow ---------------------------------------------------------------

    #[test]
    fn follow_holds_activity_id_and_behavior_options_and_round_trips_fields() {
        let follower = AccountRef::Local(Id::from_i64(1));
        let followee = AccountRef::Remote(Id::from_i64(2));
        let follow = Follow {
            follower,
            followee,
            reblogs: true,
            notify: false,
            languages: vec!["en".to_string(), "ja".to_string()],
            activity_id: "https://example.social/activities/follow/1".to_string(),
            created_at: datetime!(2026-07-29 00:00:00 UTC),
        };

        assert_eq!(follow.follower, follower);
        assert_eq!(follow.followee, followee);
        assert!(follow.reblogs);
        assert!(!follow.notify);
        assert_eq!(follow.languages, vec!["en".to_string(), "ja".to_string()]);
        assert_eq!(
            follow.activity_id,
            "https://example.social/activities/follow/1"
        );
        assert_eq!(follow.created_at, datetime!(2026-07-29 00:00:00 UTC));
    }

    #[test]
    fn follow_options_carry_reblogs_notify_and_languages_verbatim() {
        let opts = FollowOptions {
            reblogs: false,
            notify: true,
            languages: vec!["fr".to_string()],
        };
        assert!(!opts.reblogs);
        assert!(opts.notify);
        assert_eq!(opts.languages, vec!["fr".to_string()]);
    }

    // -- FollowRequest / FollowRequestDirection --------------------------------

    #[test]
    fn follow_request_direction_outbound_and_inbound_are_distinct_variants() {
        assert_ne!(
            FollowRequestDirection::Outbound,
            FollowRequestDirection::Inbound
        );
        assert_eq!(
            FollowRequestDirection::Outbound,
            FollowRequestDirection::Outbound
        );
        assert_eq!(
            FollowRequestDirection::Inbound,
            FollowRequestDirection::Inbound
        );
    }

    #[test]
    fn follow_request_holds_direction_and_activity_id_for_outbound_case() {
        let requester = AccountRef::Local(Id::from_i64(10));
        let target = AccountRef::Remote(Id::from_i64(20));
        let req = FollowRequest {
            requester,
            target,
            direction: FollowRequestDirection::Outbound,
            activity_id: "https://example.social/activities/follow/10".to_string(),
            created_at: datetime!(2026-07-29 00:00:00 UTC),
        };

        assert_eq!(req.requester, requester);
        assert_eq!(req.target, target);
        assert_eq!(req.direction, FollowRequestDirection::Outbound);
        assert_eq!(
            req.activity_id,
            "https://example.social/activities/follow/10"
        );
    }

    #[test]
    fn follow_request_holds_direction_and_activity_id_for_inbound_case() {
        let requester = AccountRef::Remote(Id::from_i64(30));
        let target = AccountRef::Local(Id::from_i64(40));
        let req = FollowRequest {
            requester,
            target,
            direction: FollowRequestDirection::Inbound,
            activity_id: "https://remote.example/activities/follow/99".to_string(),
            created_at: datetime!(2026-07-29 00:00:00 UTC),
        };

        assert_eq!(req.requester, requester);
        assert_eq!(req.target, target);
        assert_eq!(req.direction, FollowRequestDirection::Inbound);
        assert_eq!(
            req.activity_id,
            "https://remote.example/activities/follow/99"
        );
    }

    #[test]
    fn otherwise_identical_follow_requests_differing_only_in_direction_are_not_equal() {
        // Requirement 2.1: an outbound and an inbound pending request for the
        // same (requester, target) pair must be able to coexist as distinct
        // values (mirrors `follow_requests`'s own direction-scoped UNIQUE
        // constraint, migrations/0012_social_graph.sql).
        let requester = AccountRef::Local(Id::from_i64(1));
        let target = AccountRef::Local(Id::from_i64(2));
        let created_at = datetime!(2026-07-29 00:00:00 UTC);
        let outbound = FollowRequest {
            requester,
            target,
            direction: FollowRequestDirection::Outbound,
            activity_id: "same-activity-id".to_string(),
            created_at,
        };
        let inbound = FollowRequest {
            requester,
            target,
            direction: FollowRequestDirection::Inbound,
            activity_id: "same-activity-id".to_string(),
            created_at,
        };
        assert_ne!(outbound, inbound);
    }

    // -- Mute / MuteOptions -----------------------------------------------------

    #[test]
    fn mute_options_carry_notifications_and_optional_duration_verbatim() {
        let with_duration = MuteOptions {
            notifications: true,
            duration: Some(3600),
        };
        assert!(with_duration.notifications);
        assert_eq!(with_duration.duration, Some(3600));

        let without_duration = MuteOptions {
            notifications: false,
            duration: None,
        };
        assert!(!without_duration.notifications);
        assert_eq!(without_duration.duration, None);
    }

    #[test]
    fn mute_with_expires_at_some_holds_a_resolved_absolute_timestamp() {
        let muter = AccountRef::Local(Id::from_i64(1));
        let muted = AccountRef::Remote(Id::from_i64(2));
        let expires_at = datetime!(2026-08-05 00:00:00 UTC);
        let mute = Mute {
            muter,
            muted,
            notifications: true,
            expires_at: Some(expires_at),
            created_at: datetime!(2026-07-29 00:00:00 UTC),
        };

        assert_eq!(mute.muter, muter);
        assert_eq!(mute.muted, muted);
        assert!(mute.notifications);
        assert_eq!(mute.expires_at, Some(expires_at));
    }

    #[test]
    fn mute_with_expires_at_none_represents_an_unbounded_mute() {
        let muter = AccountRef::Local(Id::from_i64(3));
        let muted = AccountRef::Local(Id::from_i64(4));
        let mute = Mute {
            muter,
            muted,
            notifications: false,
            expires_at: None,
            created_at: datetime!(2026-07-29 00:00:00 UTC),
        };

        assert!(!mute.notifications);
        assert_eq!(mute.expires_at, None);
    }

    // -- Block --------------------------------------------------------------

    #[test]
    fn block_holds_the_outbound_activity_id_needed_for_undo() {
        let blocker = AccountRef::Local(Id::from_i64(5));
        let blocked = AccountRef::Remote(Id::from_i64(6));
        let block = Block {
            blocker,
            blocked,
            activity_id: "https://example.social/activities/block/5".to_string(),
            created_at: datetime!(2026-07-29 00:00:00 UTC),
        };

        assert_eq!(block.blocker, blocker);
        assert_eq!(block.blocked, blocked);
        assert_eq!(
            block.activity_id,
            "https://example.social/activities/block/5"
        );
        assert_eq!(block.created_at, datetime!(2026-07-29 00:00:00 UTC));
    }

    #[test]
    fn block_is_directional_blocker_and_blocked_are_not_interchangeable() {
        // Requirement 8.1: being blocked ("blocked_by") is represented by an
        // opposite-direction Block row, not a separate type/flag — so a Block
        // with subject/target swapped must be a distinct value.
        let a = AccountRef::Local(Id::from_i64(1));
        let b = AccountRef::Local(Id::from_i64(2));
        let created_at = datetime!(2026-07-29 00:00:00 UTC);
        let forward = Block {
            blocker: a,
            blocked: b,
            activity_id: "act-1".to_string(),
            created_at,
        };
        let reverse = Block {
            blocker: b,
            blocked: a,
            activity_id: "act-1".to_string(),
            created_at,
        };
        assert_ne!(forward, reverse);
    }
}
