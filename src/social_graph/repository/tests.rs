//! Integration-style, DB-backed tests for `RelationshipRepository`
//! (Requirements 1.6, 2.2, 4.3, 8.1, 8.4, 9.1, 9.2, 9.3), per task 1.3's
//! observable completion condition: "同一関係の二重 upsert が一意制約で冪
//! 等になり、期限切れミュートが集合・導出から除外されることをリポジトリ
//! 単体で確認できる状態".
//!
//! Mirrors `statuses/interaction_repository/tests.rs`'s and
//! `statuses/status_repository/tests.rs`'s established convention: reuses
//! `crate::test_harness::spawn_test_app` for an isolated, already-migrated
//! schema and a deterministic `RuntimeContext`. Every account reference used
//! here is a plain synthetic `AccountRef` minted from `app.runtime.ids` —
//! `follows`/`follow_requests`/`mutes`/`blocks` hold only *logical*
//! `(kind, id)` account references (`migrations/0012_social_graph.sql`'s own
//! doc comment), so nothing in this repository depends on a real
//! `local_actors`/`remote_accounts` row existing.

use time::Duration;

use crate::api::pagination::PageParams;
use crate::domain::AccountRef;
use crate::social_graph::model::{Block, Follow, FollowRequest, FollowRequestDirection, Mute};
use crate::test_harness::spawn_test_app;

use super::{
    blocked_by, blocked_targets, count_followers, count_following, delete_block, delete_follow,
    delete_mute, delete_request, following_targets, list_inbound_requests, load_states,
    muted_targets, upsert_block, upsert_follow, upsert_mute, upsert_request,
};

// -- follows ------------------------------------------------------------

#[tokio::test]
async fn upsert_follow_inserts_a_new_follow() {
    let app = spawn_test_app().await;
    let follower = AccountRef::Local(app.runtime.ids.next_id());
    let followee = AccountRef::Remote(app.runtime.ids.next_id());
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();

    let follow = Follow {
        follower,
        followee,
        reblogs: true,
        notify: false,
        languages: vec!["en".to_string()],
        activity_id: "https://example.test/activities/1".to_string(),
        created_at: now,
    };
    upsert_follow(&app.pool, id, &follow)
        .await
        .expect("upsert_follow must succeed for a fresh pair");

    assert_eq!(
        count_followers(&app.pool, &followee)
            .await
            .expect("count_followers must succeed"),
        1
    );
    assert_eq!(
        count_following(&app.pool, &follower)
            .await
            .expect("count_following must succeed"),
        1
    );

    app.cleanup().await;
}

/// Requirement 1.6: a duplicate `upsert_follow` for the same
/// (follower, followee) pair must not create a second row (the unique
/// constraint keeps it idempotent), but must still refresh the mutable
/// option fields (`reblogs`/`notify`/`languages`) rather than silently
/// keeping stale values.
#[tokio::test]
async fn upsert_follow_is_idempotent_and_refreshes_changed_options() {
    let app = spawn_test_app().await;
    let follower = AccountRef::Local(app.runtime.ids.next_id());
    let followee = AccountRef::Remote(app.runtime.ids.next_id());
    let now = app.runtime.clock.now();

    let first_id = app.runtime.ids.next_id();
    let first = Follow {
        follower,
        followee,
        reblogs: true,
        notify: false,
        languages: vec!["en".to_string()],
        activity_id: "https://example.test/activities/1".to_string(),
        created_at: now,
    };
    upsert_follow(&app.pool, first_id, &first)
        .await
        .expect("first upsert_follow must succeed");

    // A second upsert for the same pair, with different options and a
    // different caller-minted id (the caller doesn't know the row already
    // exists) must not duplicate the relationship.
    let second_id = app.runtime.ids.next_id();
    let second = Follow {
        follower,
        followee,
        reblogs: false,
        notify: true,
        languages: vec!["ja".to_string(), "fr".to_string()],
        activity_id: "https://example.test/activities/2".to_string(),
        created_at: now,
    };
    upsert_follow(&app.pool, second_id, &second)
        .await
        .expect("second upsert_follow for the same pair must succeed idempotently");

    assert_eq!(
        count_followers(&app.pool, &followee)
            .await
            .expect("count_followers must succeed"),
        1,
        "a duplicate upsert must not create a second follows row"
    );

    let states = load_states(&app.pool, &follower, &[followee], now)
        .await
        .expect("load_states must succeed");
    let follow = states[0]
        .follow
        .as_ref()
        .expect("the follow must still be present after the duplicate upsert");
    assert!(!follow.reblogs, "reblogs must reflect the refreshed value");
    assert!(follow.notify, "notify must reflect the refreshed value");
    assert_eq!(follow.languages, vec!["ja".to_string(), "fr".to_string()]);
    assert_eq!(follow.activity_id, "https://example.test/activities/2");

    app.cleanup().await;
}

#[tokio::test]
async fn delete_follow_removes_the_row_and_is_idempotent() {
    let app = spawn_test_app().await;
    let follower = AccountRef::Local(app.runtime.ids.next_id());
    let followee = AccountRef::Remote(app.runtime.ids.next_id());
    let now = app.runtime.clock.now();
    let id = app.runtime.ids.next_id();
    let follow = Follow {
        follower,
        followee,
        reblogs: true,
        notify: false,
        languages: vec![],
        activity_id: "https://example.test/activities/1".to_string(),
        created_at: now,
    };
    upsert_follow(&app.pool, id, &follow)
        .await
        .expect("upsert_follow must succeed");

    let deleted = delete_follow(&app.pool, &follower, &followee)
        .await
        .expect("delete_follow must succeed");
    assert!(deleted, "the just-inserted follow must be deleted");

    let deleted_again = delete_follow(&app.pool, &follower, &followee)
        .await
        .expect("deleting an absent follow must succeed idempotently");
    assert!(!deleted_again, "a second delete must be a no-op");

    app.cleanup().await;
}

// -- follow_requests ------------------------------------------------------

#[tokio::test]
async fn upsert_request_records_a_pending_request_and_list_inbound_requests_finds_it() {
    let app = spawn_test_app().await;
    let requester = AccountRef::Remote(app.runtime.ids.next_id());
    let target = AccountRef::Local(app.runtime.ids.next_id());
    let now = app.runtime.clock.now();
    let id = app.runtime.ids.next_id();

    let req = FollowRequest {
        requester,
        target,
        direction: FollowRequestDirection::Inbound,
        activity_id: "https://example.test/activities/follow/1".to_string(),
        created_at: now,
    };
    upsert_request(&app.pool, id, &req)
        .await
        .expect("upsert_request must succeed");

    let page = list_inbound_requests(&app.pool, &target, &PageParams::default())
        .await
        .expect("list_inbound_requests must succeed");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].requester, requester);
    assert_eq!(page.items[0].direction, FollowRequestDirection::Inbound);

    app.cleanup().await;
}

/// Requirement 2.1: an outbound and an inbound pending request for the same
/// (requester, target) pair coexist without colliding — the unique
/// constraint is scoped by `direction`.
#[tokio::test]
async fn outbound_and_inbound_requests_for_the_same_pair_coexist() {
    let app = spawn_test_app().await;
    let a = AccountRef::Local(app.runtime.ids.next_id());
    let b = AccountRef::Local(app.runtime.ids.next_id());
    let now = app.runtime.clock.now();

    let outbound = FollowRequest {
        requester: a,
        target: b,
        direction: FollowRequestDirection::Outbound,
        activity_id: "act-out".to_string(),
        created_at: now,
    };
    let inbound = FollowRequest {
        requester: a,
        target: b,
        direction: FollowRequestDirection::Inbound,
        activity_id: "act-in".to_string(),
        created_at: now,
    };
    upsert_request(&app.pool, app.runtime.ids.next_id(), &outbound)
        .await
        .expect("upsert_request (outbound) must succeed");
    upsert_request(&app.pool, app.runtime.ids.next_id(), &inbound)
        .await
        .expect("upsert_request (inbound) must succeed");

    let page = list_inbound_requests(&app.pool, &b, &PageParams::default())
        .await
        .expect("list_inbound_requests must succeed");
    assert_eq!(
        page.items.len(),
        1,
        "only the inbound-direction row must be listed"
    );

    app.cleanup().await;
}

/// Requirement 1.6/idempotency: a duplicate `upsert_request` for the same
/// (requester, target, direction) must not create a second row.
#[tokio::test]
async fn upsert_request_is_idempotent_per_direction() {
    let app = spawn_test_app().await;
    let requester = AccountRef::Remote(app.runtime.ids.next_id());
    let target = AccountRef::Local(app.runtime.ids.next_id());
    let now = app.runtime.clock.now();

    let first = FollowRequest {
        requester,
        target,
        direction: FollowRequestDirection::Inbound,
        activity_id: "act-1".to_string(),
        created_at: now,
    };
    upsert_request(&app.pool, app.runtime.ids.next_id(), &first)
        .await
        .expect("first upsert_request must succeed");

    let second = FollowRequest {
        requester,
        target,
        direction: FollowRequestDirection::Inbound,
        activity_id: "act-2".to_string(),
        created_at: now,
    };
    upsert_request(&app.pool, app.runtime.ids.next_id(), &second)
        .await
        .expect("second upsert_request for the same pair+direction must succeed idempotently");

    let page = list_inbound_requests(&app.pool, &target, &PageParams::default())
        .await
        .expect("list_inbound_requests must succeed");
    assert_eq!(page.items.len(), 1, "no duplicate row must be created");
    assert_eq!(
        page.items[0].activity_id, "act-2",
        "the activity_id must be refreshed by the duplicate upsert"
    );

    app.cleanup().await;
}

#[tokio::test]
async fn delete_request_removes_the_row_and_is_idempotent() {
    let app = spawn_test_app().await;
    let requester = AccountRef::Remote(app.runtime.ids.next_id());
    let target = AccountRef::Local(app.runtime.ids.next_id());
    let now = app.runtime.clock.now();
    let req = FollowRequest {
        requester,
        target,
        direction: FollowRequestDirection::Inbound,
        activity_id: "act-1".to_string(),
        created_at: now,
    };
    upsert_request(&app.pool, app.runtime.ids.next_id(), &req)
        .await
        .expect("upsert_request must succeed");

    let deleted = delete_request(
        &app.pool,
        &requester,
        &target,
        FollowRequestDirection::Inbound,
    )
    .await
    .expect("delete_request must succeed");
    assert!(deleted);

    let deleted_again = delete_request(
        &app.pool,
        &requester,
        &target,
        FollowRequestDirection::Inbound,
    )
    .await
    .expect("deleting an absent request must succeed idempotently");
    assert!(!deleted_again);

    app.cleanup().await;
}

#[tokio::test]
async fn list_inbound_requests_paginates_and_is_scoped_to_the_target() {
    let app = spawn_test_app().await;
    let target = AccountRef::Local(app.runtime.ids.next_id());
    let other_target = AccountRef::Local(app.runtime.ids.next_id());
    let now = app.runtime.clock.now();

    for i in 0..3 {
        let requester = AccountRef::Remote(app.runtime.ids.next_id());
        let req = FollowRequest {
            requester,
            target,
            direction: FollowRequestDirection::Inbound,
            activity_id: format!("act-{i}"),
            created_at: now,
        };
        upsert_request(&app.pool, app.runtime.ids.next_id(), &req)
            .await
            .expect("upsert_request must succeed");
    }
    // A request for a different target must not leak into `target`'s page.
    let unrelated = FollowRequest {
        requester: AccountRef::Remote(app.runtime.ids.next_id()),
        target: other_target,
        direction: FollowRequestDirection::Inbound,
        activity_id: "act-other".to_string(),
        created_at: now,
    };
    upsert_request(&app.pool, app.runtime.ids.next_id(), &unrelated)
        .await
        .expect("upsert_request must succeed");

    let params = PageParams {
        limit: Some(2),
        ..Default::default()
    };
    let first_page = list_inbound_requests(&app.pool, &target, &params)
        .await
        .expect("list_inbound_requests must succeed");
    assert_eq!(first_page.items.len(), 2);
    assert!(first_page.next_cursor.is_some());

    let next_params = PageParams {
        max_id: first_page.next_cursor.clone(),
        limit: Some(2),
        ..Default::default()
    };
    let second_page = list_inbound_requests(&app.pool, &target, &next_params)
        .await
        .expect("list_inbound_requests must succeed");
    assert_eq!(second_page.items.len(), 1);

    app.cleanup().await;
}

// -- mutes ------------------------------------------------------------

#[tokio::test]
async fn upsert_mute_is_idempotent_and_refreshes_changed_options() {
    let app = spawn_test_app().await;
    let muter = AccountRef::Local(app.runtime.ids.next_id());
    let muted = AccountRef::Remote(app.runtime.ids.next_id());
    let now = app.runtime.clock.now();

    let first = Mute {
        muter,
        muted,
        notifications: false,
        expires_at: None,
        created_at: now,
    };
    upsert_mute(&app.pool, app.runtime.ids.next_id(), &first)
        .await
        .expect("first upsert_mute must succeed");

    let expires_at = now + Duration::hours(1);
    let second = Mute {
        muter,
        muted,
        notifications: true,
        expires_at: Some(expires_at),
        created_at: now,
    };
    upsert_mute(&app.pool, app.runtime.ids.next_id(), &second)
        .await
        .expect("second upsert_mute for the same pair must succeed idempotently");

    let targets = muted_targets(&app.pool, &muter, now, false)
        .await
        .expect("muted_targets must succeed");
    assert_eq!(
        targets.len(),
        1,
        "a duplicate upsert must not create a second mutes row"
    );

    let notif_only = muted_targets(&app.pool, &muter, now, true)
        .await
        .expect("muted_targets must succeed");
    assert_eq!(
        notif_only.len(),
        1,
        "notifications must have been refreshed to true by the duplicate upsert"
    );

    app.cleanup().await;
}

/// Requirements 4.3, 9.3: an expired mute must be excluded from
/// `muted_targets` and from `load_states`'s derived `mute` field.
#[tokio::test]
async fn expired_mute_is_excluded_from_the_filter_set_and_from_load_states() {
    let app = spawn_test_app().await;
    let muter = AccountRef::Local(app.runtime.ids.next_id());
    let muted = AccountRef::Remote(app.runtime.ids.next_id());
    let now = app.runtime.clock.now();
    let already_expired = now - Duration::seconds(1);

    let mute = Mute {
        muter,
        muted,
        notifications: true,
        expires_at: Some(already_expired),
        created_at: now - Duration::hours(1),
    };
    upsert_mute(&app.pool, app.runtime.ids.next_id(), &mute)
        .await
        .expect("upsert_mute must succeed");

    let targets = muted_targets(&app.pool, &muter, now, false)
        .await
        .expect("muted_targets must succeed");
    assert!(
        targets.is_empty(),
        "an expired mute must not appear in the filter set"
    );

    let states = load_states(&app.pool, &muter, &[muted], now)
        .await
        .expect("load_states must succeed");
    assert!(
        states[0].mute.is_none(),
        "an expired mute must not be derived as an active mute in load_states"
    );

    // Sanity: before expiry, the same mute IS included.
    let before_expiry = already_expired - Duration::seconds(10);
    let targets_before = muted_targets(&app.pool, &muter, before_expiry, false)
        .await
        .expect("muted_targets must succeed");
    assert_eq!(targets_before.len(), 1);

    app.cleanup().await;
}

#[tokio::test]
async fn mute_with_no_expiry_is_never_excluded() {
    let app = spawn_test_app().await;
    let muter = AccountRef::Local(app.runtime.ids.next_id());
    let muted = AccountRef::Remote(app.runtime.ids.next_id());
    let now = app.runtime.clock.now();
    let mute = Mute {
        muter,
        muted,
        notifications: false,
        expires_at: None,
        created_at: now,
    };
    upsert_mute(&app.pool, app.runtime.ids.next_id(), &mute)
        .await
        .expect("upsert_mute must succeed");

    let far_future = now + Duration::days(3650);
    let targets = muted_targets(&app.pool, &muter, far_future, false)
        .await
        .expect("muted_targets must succeed");
    assert_eq!(targets.len(), 1, "an unbounded mute never expires");

    app.cleanup().await;
}

#[tokio::test]
async fn delete_mute_removes_the_row_and_is_idempotent() {
    let app = spawn_test_app().await;
    let muter = AccountRef::Local(app.runtime.ids.next_id());
    let muted = AccountRef::Remote(app.runtime.ids.next_id());
    let now = app.runtime.clock.now();
    let mute = Mute {
        muter,
        muted,
        notifications: false,
        expires_at: None,
        created_at: now,
    };
    upsert_mute(&app.pool, app.runtime.ids.next_id(), &mute)
        .await
        .expect("upsert_mute must succeed");

    let deleted = delete_mute(&app.pool, &muter, &muted)
        .await
        .expect("delete_mute must succeed");
    assert!(deleted);
    let deleted_again = delete_mute(&app.pool, &muter, &muted)
        .await
        .expect("deleting an absent mute must succeed idempotently");
    assert!(!deleted_again);

    app.cleanup().await;
}

// -- blocks -----------------------------------------------------------

#[tokio::test]
async fn upsert_block_is_idempotent_and_blocked_targets_and_blocked_by_agree() {
    let app = spawn_test_app().await;
    let blocker = AccountRef::Local(app.runtime.ids.next_id());
    let blocked = AccountRef::Remote(app.runtime.ids.next_id());
    let now = app.runtime.clock.now();

    let first = Block {
        blocker,
        blocked,
        activity_id: "act-1".to_string(),
        created_at: now,
    };
    upsert_block(&app.pool, app.runtime.ids.next_id(), &first)
        .await
        .expect("first upsert_block must succeed");
    let second = Block {
        blocker,
        blocked,
        activity_id: "act-2".to_string(),
        created_at: now,
    };
    upsert_block(&app.pool, app.runtime.ids.next_id(), &second)
        .await
        .expect("second upsert_block for the same pair must succeed idempotently");

    let blocker_view = blocked_targets(&app.pool, &blocker)
        .await
        .expect("blocked_targets must succeed");
    assert_eq!(
        blocker_view.len(),
        1,
        "a duplicate upsert must not create a second blocks row"
    );
    assert_eq!(blocker_view[0], blocked);

    let blocked_view = blocked_by(&app.pool, &blocked)
        .await
        .expect("blocked_by must succeed");
    assert_eq!(blocked_view.len(), 1);
    assert_eq!(blocked_view[0], blocker);

    app.cleanup().await;
}

#[tokio::test]
async fn delete_block_removes_the_row_and_is_idempotent() {
    let app = spawn_test_app().await;
    let blocker = AccountRef::Local(app.runtime.ids.next_id());
    let blocked = AccountRef::Remote(app.runtime.ids.next_id());
    let now = app.runtime.clock.now();
    let block = Block {
        blocker,
        blocked,
        activity_id: "act-1".to_string(),
        created_at: now,
    };
    upsert_block(&app.pool, app.runtime.ids.next_id(), &block)
        .await
        .expect("upsert_block must succeed");

    let deleted = delete_block(&app.pool, &blocker, &blocked)
        .await
        .expect("delete_block must succeed");
    assert!(deleted);
    let deleted_again = delete_block(&app.pool, &blocker, &blocked)
        .await
        .expect("deleting an absent block must succeed idempotently");
    assert!(!deleted_again);

    app.cleanup().await;
}

// -- following_targets / counts ------------------------------------------

#[tokio::test]
async fn following_targets_and_counts_reflect_established_follows_only() {
    let app = spawn_test_app().await;
    let viewer = AccountRef::Local(app.runtime.ids.next_id());
    let a = AccountRef::Remote(app.runtime.ids.next_id());
    let b = AccountRef::Local(app.runtime.ids.next_id());
    let now = app.runtime.clock.now();

    for followee in [a, b] {
        let follow = Follow {
            follower: viewer,
            followee,
            reblogs: true,
            notify: false,
            languages: vec![],
            activity_id: "act".to_string(),
            created_at: now,
        };
        upsert_follow(&app.pool, app.runtime.ids.next_id(), &follow)
            .await
            .expect("upsert_follow must succeed");
    }

    let mut targets = following_targets(&app.pool, &viewer)
        .await
        .expect("following_targets must succeed");
    targets.sort_by_key(|t| match t {
        AccountRef::Local(id) => id.as_i64(),
        AccountRef::Remote(id) => id.as_i64(),
    });
    let mut expected = vec![a, b];
    expected.sort_by_key(|t| match t {
        AccountRef::Local(id) => id.as_i64(),
        AccountRef::Remote(id) => id.as_i64(),
    });
    assert_eq!(targets, expected);

    assert_eq!(
        count_following(&app.pool, &viewer)
            .await
            .expect("count_following must succeed"),
        2
    );
    assert_eq!(
        count_followers(&app.pool, &a)
            .await
            .expect("count_followers must succeed"),
        1
    );

    app.cleanup().await;
}

// -- load_states (Requirement 8.4) --------------------------------------

/// Exercises every flag `load_states` derives, across a batch of multiple
/// targets in one call, including a target with no relationship rows at
/// all (must still get a default, all-false/None state, not be omitted).
#[tokio::test]
async fn load_states_derives_every_flag_across_a_batch_of_targets() {
    let app = spawn_test_app().await;
    let viewer = AccountRef::Local(app.runtime.ids.next_id());

    // target_following: viewer follows them, they don't follow back.
    let target_following = AccountRef::Remote(app.runtime.ids.next_id());
    // target_mutual: established follow both ways.
    let target_mutual = AccountRef::Local(app.runtime.ids.next_id());
    // target_blocking: viewer blocks them.
    let target_blocking = AccountRef::Remote(app.runtime.ids.next_id());
    // target_blocked_by: they block viewer.
    let target_blocked_by = AccountRef::Remote(app.runtime.ids.next_id());
    // target_muted: viewer mutes them (with notifications).
    let target_muted = AccountRef::Remote(app.runtime.ids.next_id());
    // target_requested: viewer has an outbound pending request to them.
    let target_requested = AccountRef::Remote(app.runtime.ids.next_id());
    // target_requested_by: they have an inbound pending request to viewer.
    let target_requested_by = AccountRef::Remote(app.runtime.ids.next_id());
    // target_none: no relationship rows at all.
    let target_none = AccountRef::Remote(app.runtime.ids.next_id());

    let now = app.runtime.clock.now();

    let follow = |follower: AccountRef, followee: AccountRef| Follow {
        follower,
        followee,
        reblogs: true,
        notify: true,
        languages: vec!["en".to_string()],
        activity_id: "act".to_string(),
        created_at: now,
    };

    upsert_follow(
        &app.pool,
        app.runtime.ids.next_id(),
        &follow(viewer, target_following),
    )
    .await
    .expect("upsert_follow must succeed");

    upsert_follow(
        &app.pool,
        app.runtime.ids.next_id(),
        &follow(viewer, target_mutual),
    )
    .await
    .expect("upsert_follow must succeed");
    upsert_follow(
        &app.pool,
        app.runtime.ids.next_id(),
        &follow(target_mutual, viewer),
    )
    .await
    .expect("upsert_follow must succeed");

    upsert_block(
        &app.pool,
        app.runtime.ids.next_id(),
        &Block {
            blocker: viewer,
            blocked: target_blocking,
            activity_id: "act".to_string(),
            created_at: now,
        },
    )
    .await
    .expect("upsert_block must succeed");

    upsert_block(
        &app.pool,
        app.runtime.ids.next_id(),
        &Block {
            blocker: target_blocked_by,
            blocked: viewer,
            activity_id: "act".to_string(),
            created_at: now,
        },
    )
    .await
    .expect("upsert_block must succeed");

    upsert_mute(
        &app.pool,
        app.runtime.ids.next_id(),
        &Mute {
            muter: viewer,
            muted: target_muted,
            notifications: true,
            expires_at: None,
            created_at: now,
        },
    )
    .await
    .expect("upsert_mute must succeed");

    upsert_request(
        &app.pool,
        app.runtime.ids.next_id(),
        &FollowRequest {
            requester: viewer,
            target: target_requested,
            direction: FollowRequestDirection::Outbound,
            activity_id: "act".to_string(),
            created_at: now,
        },
    )
    .await
    .expect("upsert_request must succeed");

    upsert_request(
        &app.pool,
        app.runtime.ids.next_id(),
        &FollowRequest {
            requester: target_requested_by,
            target: viewer,
            direction: FollowRequestDirection::Inbound,
            activity_id: "act".to_string(),
            created_at: now,
        },
    )
    .await
    .expect("upsert_request must succeed");

    let targets = vec![
        target_following,
        target_mutual,
        target_blocking,
        target_blocked_by,
        target_muted,
        target_requested,
        target_requested_by,
        target_none,
    ];
    let states = load_states(&app.pool, &viewer, &targets, now)
        .await
        .expect("load_states must succeed");
    assert_eq!(states.len(), targets.len());

    let by_target = |t: AccountRef| {
        states
            .iter()
            .find(|s| s.target == t)
            .expect("load_states must return one entry per requested target")
    };

    let following = by_target(target_following);
    assert!(following.follow.is_some());
    assert!(!following.followed_by);
    assert!(!following.blocking);
    assert!(!following.blocked_by);
    assert!(following.mute.is_none());
    assert!(!following.requested);
    assert!(!following.requested_by);
    let f = following.follow.as_ref().unwrap();
    assert!(f.reblogs);
    assert!(f.notify);
    assert_eq!(f.languages, vec!["en".to_string()]);

    let mutual = by_target(target_mutual);
    assert!(mutual.follow.is_some());
    assert!(mutual.followed_by);

    let blocking = by_target(target_blocking);
    assert!(blocking.blocking);
    assert!(!blocking.blocked_by);

    let blocked_by_state = by_target(target_blocked_by);
    assert!(blocked_by_state.blocked_by);
    assert!(!blocked_by_state.blocking);

    let muted = by_target(target_muted);
    assert!(muted.mute.is_some());
    assert!(muted.mute.as_ref().unwrap().notifications);

    let requested = by_target(target_requested);
    assert!(requested.requested);
    assert!(!requested.requested_by);

    let requested_by_state = by_target(target_requested_by);
    assert!(requested_by_state.requested_by);
    assert!(!requested_by_state.requested);

    let none = by_target(target_none);
    assert!(none.follow.is_none());
    assert!(!none.followed_by);
    assert!(!none.blocking);
    assert!(!none.blocked_by);
    assert!(none.mute.is_none());
    assert!(!none.requested);
    assert!(!none.requested_by);

    app.cleanup().await;
}

#[tokio::test]
async fn load_states_returns_empty_vec_for_an_empty_target_slice() {
    let app = spawn_test_app().await;
    let viewer = AccountRef::Local(app.runtime.ids.next_id());
    let now = app.runtime.clock.now();

    let states = load_states(&app.pool, &viewer, &[], now)
        .await
        .expect("load_states must succeed on an empty target slice");
    assert!(states.is_empty());

    app.cleanup().await;
}
