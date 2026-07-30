//! Unit tests for `RelationshipMapper` (Requirements 8.3, 8.4, 8.5, 4.3),
//! per task 2.4's observable completion condition: "関係なしで全フラグ既
//! 定、フォロー/ミュート/ブロックの組合せが正しいフラグになり、
//! Relationship 契約を再定義していないことを単体テストで確認できる状態".
//!
//! Pure in-memory logic — no DB, no HTTP, no async; plain `#[test]` unit
//! tests constructing `RelationshipState` values directly and asserting on
//! the resulting `RelationshipView`. Confirms this module consumes
//! `crate::accounts::model::RelationshipView` verbatim (never a shadow
//! struct) by importing and constructing that exact type below.

use super::*;
use crate::accounts::model::RelationshipView;
use crate::domain::Id;
use crate::social_graph::model::{Follow, Mute};
use time::macros::datetime;

fn target_local(n: i64) -> AccountRef {
    AccountRef::Local(Id::from_i64(n))
}

fn viewer_local(n: i64) -> AccountRef {
    AccountRef::Local(Id::from_i64(n))
}

fn no_relationship_state(target: AccountRef) -> RelationshipState {
    RelationshipState {
        target,
        follow: None,
        followed_by: false,
        blocking: false,
        blocked_by: false,
        mute: None,
        requested: false,
        requested_by: false,
    }
}

fn some_follow(follower: AccountRef, followee: AccountRef) -> Follow {
    Follow {
        follower,
        followee,
        reblogs: true,
        notify: false,
        languages: vec!["en".to_string(), "ja".to_string()],
        activity_id: "https://example.social/activities/follow/1".to_string(),
        created_at: datetime!(2026-07-29 00:00:00 UTC),
    }
}

fn some_mute(muter: AccountRef, muted: AccountRef, notifications: bool) -> Mute {
    Mute {
        muter,
        muted,
        notifications,
        expires_at: None,
        created_at: datetime!(2026-07-29 00:00:00 UTC),
    }
}

// -- 8.4: no relationship at all -> every flag at its default ---------------

#[test]
fn no_relationship_maps_to_all_flags_at_default() {
    let target = target_local(42);
    let state = no_relationship_state(target);

    let view: RelationshipView = RelationshipMapper.to_view(&state);

    assert_eq!(view.id, Id::from_i64(42));
    assert!(!view.following);
    assert!(!view.showing_reblogs);
    assert!(!view.notifying);
    assert!(view.languages.is_empty());
    assert!(!view.followed_by);
    assert!(!view.blocking);
    assert!(!view.blocked_by);
    assert!(!view.muting);
    assert!(!view.muting_notifications);
    assert!(!view.requested);
    assert!(!view.requested_by);
    assert!(!view.domain_blocking);
    assert!(!view.endorsed);
    assert_eq!(view.note, "");
}

// -- id extraction: both AccountRef variants -------------------------------

#[test]
fn id_is_extracted_from_local_target() {
    let state = no_relationship_state(AccountRef::Local(Id::from_i64(7)));
    let view = RelationshipMapper.to_view(&state);
    assert_eq!(view.id, Id::from_i64(7));
}

#[test]
fn id_is_extracted_from_remote_target() {
    let state = no_relationship_state(AccountRef::Remote(Id::from_i64(99)));
    let view = RelationshipMapper.to_view(&state);
    assert_eq!(view.id, Id::from_i64(99));
}

// -- 8.4: follow present -> following/showing_reblogs/notifying/languages --

#[test]
fn established_follow_sets_following_and_carries_behavior_options() {
    let viewer = viewer_local(1);
    let target = target_local(2);
    let mut state = no_relationship_state(target);
    state.follow = Some(some_follow(viewer, target));

    let view = RelationshipMapper.to_view(&state);

    assert!(view.following);
    assert!(view.showing_reblogs);
    assert!(!view.notifying);
    assert_eq!(view.languages, vec!["en".to_string(), "ja".to_string()]);
}

#[test]
fn established_follow_with_notify_true_and_no_reblogs_maps_correctly() {
    let viewer = viewer_local(1);
    let target = target_local(2);
    let mut follow = some_follow(viewer, target);
    follow.reblogs = false;
    follow.notify = true;
    follow.languages = vec![];
    let mut state = no_relationship_state(target);
    state.follow = Some(follow);

    let view = RelationshipMapper.to_view(&state);

    assert!(view.following);
    assert!(!view.showing_reblogs);
    assert!(view.notifying);
    assert!(view.languages.is_empty());
}

// -- followed_by / blocking / blocked_by / requested / requested_by --------

#[test]
fn followed_by_blocking_blocked_by_requested_flags_pass_through_directly() {
    let target = target_local(3);
    let mut state = no_relationship_state(target);
    state.followed_by = true;
    state.blocking = true;
    state.blocked_by = true;
    state.requested = true;
    state.requested_by = true;

    let view = RelationshipMapper.to_view(&state);

    assert!(!view.following);
    assert!(view.followed_by);
    assert!(view.blocking);
    assert!(view.blocked_by);
    assert!(view.requested);
    assert!(view.requested_by);
}

// -- 4.3 / 8.4: mute present -> muting/muting_notifications ------------------

#[test]
fn active_mute_sets_muting_and_muting_notifications() {
    let viewer = viewer_local(1);
    let target = target_local(4);
    let mut state = no_relationship_state(target);
    state.mute = Some(some_mute(viewer, target, true));

    let view = RelationshipMapper.to_view(&state);

    assert!(view.muting);
    assert!(view.muting_notifications);
}

#[test]
fn mute_without_notifications_sets_muting_true_but_muting_notifications_false() {
    let viewer = viewer_local(1);
    let target = target_local(5);
    let mut state = no_relationship_state(target);
    state.mute = Some(some_mute(viewer, target, false));

    let view = RelationshipMapper.to_view(&state);

    assert!(view.muting);
    assert!(!view.muting_notifications);
}

#[test]
fn expired_mute_already_filtered_to_none_by_repository_maps_to_muting_false() {
    // RelationshipState::mute is documented as already expiry-filtered by
    // the repository layer (Requirements 4.3, 9.3): an expired mute is
    // represented as `None`, not `Some` with a past `expires_at`. This
    // mapper must not need to inspect `expires_at` at all -- `None` alone
    // is enough to prove `muting` is false.
    let target = target_local(6);
    let state = no_relationship_state(target); // mute: None (as if expired)

    let view = RelationshipMapper.to_view(&state);

    assert!(!view.muting);
    assert!(!view.muting_notifications);
}

// -- 8.5: domain_blocking always false, regardless of other state -----------

#[test]
fn domain_blocking_is_always_false_regardless_of_other_flags() {
    let viewer = viewer_local(1);
    let target = target_local(7);
    let mut state = no_relationship_state(target);
    state.follow = Some(some_follow(viewer, target));
    state.followed_by = true;
    state.blocking = true;
    state.blocked_by = true;
    state.mute = Some(some_mute(viewer, target, true));
    state.requested = true;
    state.requested_by = true;

    let view = RelationshipMapper.to_view(&state);

    assert!(!view.domain_blocking);
}

// -- endorsed/note always false/empty regardless of other state -------------

#[test]
fn endorsed_and_note_are_always_false_and_empty_regardless_of_other_flags() {
    let viewer = viewer_local(1);
    let target = target_local(8);
    let mut state = no_relationship_state(target);
    state.follow = Some(some_follow(viewer, target));
    state.blocking = true;
    state.mute = Some(some_mute(viewer, target, true));

    let view = RelationshipMapper.to_view(&state);

    assert!(!view.endorsed);
    assert_eq!(view.note, "");
}

// -- combination: blocking + mute + no follow --------------------------------

#[test]
fn blocking_and_mute_without_follow_yields_expected_combination() {
    let viewer = viewer_local(1);
    let target = target_local(9);
    let mut state = no_relationship_state(target);
    state.blocking = true;
    state.mute = Some(some_mute(viewer, target, false));

    let view = RelationshipMapper.to_view(&state);

    assert!(!view.following);
    assert!(!view.showing_reblogs);
    assert!(!view.notifying);
    assert!(view.languages.is_empty());
    assert!(view.blocking);
    assert!(view.muting);
    assert!(!view.muting_notifications);
    assert!(!view.domain_blocking);
}
