//! DB-backed tests for `Transitions` (Requirements 1.1, 1.4, 2.5, 2.6, 3.1,
//! 3.2, 5.1, 5.2, 7.2, 7.3, 7.7), per task 2.2's own observable completion
//! condition: "ブロック適用で双方向フォロー・両方向保留が消え、同一遷移の
//! 二重適用が状態を壊さないこと、およびフォロー確立・受信保留記録のコミッ
//! ト後にシンクへ 1 度だけイベントが渡り既定 no-op のため notifications
//! 未配線でも成功すること".
//!
//! Mirrors `repository/tests.rs`'s `spawn_test_app`-based harness
//! convention (an isolated, already-migrated schema plus a deterministic
//! `RuntimeContext`) and `interaction_service/tests.rs`'s
//! `RecordingNotificationSink` convention (records every
//! `NotificationEventSink::emit` call so a test can assert on emitted
//! `NotificationEvent`s).

use std::sync::{Arc, Mutex};

use crate::domain::AccountRef;
use crate::social_graph::model::{FollowOptions, FollowRequest, FollowRequestDirection};
use crate::social_graph::repository;
use crate::social_graph::transitions::Transitions;
use crate::statuses::notification_sink::{
    NotificationEvent, NotificationEventSink, NotificationSinkRegistry, NotificationType,
};
use crate::test_harness::spawn_test_app;

/// Records every [`NotificationEventSink::emit`] call — mirrors
/// `interaction_service/tests.rs::RecordingNotificationSink` exactly.
struct RecordingNotificationSink {
    events: Mutex<Vec<NotificationEvent>>,
}

impl RecordingNotificationSink {
    fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
        }
    }

    fn events(&self) -> Vec<NotificationEvent> {
        self.events.lock().unwrap().clone()
    }
}

impl NotificationEventSink for RecordingNotificationSink {
    fn emit<'a>(
        &'a self,
        event: NotificationEvent,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), crate::error::AppError>> + Send + 'a>,
    > {
        Box::pin(async move {
            self.events.lock().unwrap().push(event);
            Ok(())
        })
    }
}

fn default_opts() -> FollowOptions {
    FollowOptions {
        reblogs: true,
        notify: false,
        languages: Vec::new(),
    }
}

// -- establish_follow -------------------------------------------------------

#[tokio::test]
async fn establish_follow_creates_a_new_follow_and_emits_once_for_local_followee() {
    let app = spawn_test_app().await;
    let follower = AccountRef::Remote(app.runtime.ids.next_id());
    let followee = AccountRef::Local(app.runtime.ids.next_id());

    let sink = Arc::new(RecordingNotificationSink::new());
    let registry = NotificationSinkRegistry::new();
    registry.set_sink(Arc::clone(&sink) as Arc<dyn NotificationEventSink>);
    let transitions = Transitions::new(app.pool.clone(), app.runtime.clone(), registry);

    transitions
        .establish_follow(
            &follower,
            &followee,
            &default_opts(),
            "https://remote.test/acts/1",
        )
        .await
        .expect("establish_follow must succeed for a fresh pair");

    let states = repository::load_states(
        &app.pool,
        &follower,
        std::slice::from_ref(&followee),
        app.runtime.clock.now(),
    )
    .await
    .expect("load_states must succeed");
    assert!(
        states[0].follow.is_some(),
        "establish_follow must persist a follows row"
    );

    let events = sink.events();
    assert_eq!(events.len(), 1, "establish_follow must emit exactly once");
    assert_eq!(events[0].recipient, followee);
    assert_eq!(events[0].origin, follower);
    assert_eq!(events[0].kind, NotificationType::Follow);
    assert_eq!(events[0].target_status_id, None);
}

#[tokio::test]
async fn establish_follow_is_idempotent_and_does_not_reemit_on_repeat_call() {
    let app = spawn_test_app().await;
    let follower = AccountRef::Remote(app.runtime.ids.next_id());
    let followee = AccountRef::Local(app.runtime.ids.next_id());

    let sink = Arc::new(RecordingNotificationSink::new());
    let registry = NotificationSinkRegistry::new();
    registry.set_sink(Arc::clone(&sink) as Arc<dyn NotificationEventSink>);
    let transitions = Transitions::new(app.pool.clone(), app.runtime.clone(), registry);

    transitions
        .establish_follow(
            &follower,
            &followee,
            &default_opts(),
            "https://remote.test/acts/1",
        )
        .await
        .expect("first establish_follow must succeed");

    // Repeat with different options -- must refresh the row, not duplicate
    // it, and must not re-emit.
    let changed_opts = FollowOptions {
        reblogs: false,
        notify: true,
        languages: vec!["ja".to_string()],
    };
    transitions
        .establish_follow(
            &follower,
            &followee,
            &changed_opts,
            "https://remote.test/acts/1",
        )
        .await
        .expect("repeat establish_follow must succeed idempotently");

    let states = repository::load_states(
        &app.pool,
        &follower,
        std::slice::from_ref(&followee),
        app.runtime.clock.now(),
    )
    .await
    .expect("load_states must succeed");
    let follow = states[0]
        .follow
        .as_ref()
        .expect("follow must still exist after the repeat call");
    assert!(!follow.reblogs, "repeat call must refresh reblogs");
    assert!(follow.notify, "repeat call must refresh notify");

    assert_eq!(
        sink.events().len(),
        1,
        "a repeat establish_follow call for an already-established follow must not re-emit"
    );
}

#[tokio::test]
async fn establish_follow_does_not_emit_when_followee_is_remote() {
    let app = spawn_test_app().await;
    let follower = AccountRef::Local(app.runtime.ids.next_id());
    let followee = AccountRef::Remote(app.runtime.ids.next_id());

    let sink = Arc::new(RecordingNotificationSink::new());
    let registry = NotificationSinkRegistry::new();
    registry.set_sink(Arc::clone(&sink) as Arc<dyn NotificationEventSink>);
    let transitions = Transitions::new(app.pool.clone(), app.runtime.clone(), registry);

    transitions
        .establish_follow(
            &follower,
            &followee,
            &default_opts(),
            "https://local.test/acts/1",
        )
        .await
        .expect("establish_follow must succeed for a remote followee");

    assert!(
        sink.events().is_empty(),
        "establish_follow must never emit for a remote followee"
    );
}

#[tokio::test]
async fn establish_follow_succeeds_with_default_noop_sink_when_notifications_unwired() {
    let app = spawn_test_app().await;
    let follower = AccountRef::Remote(app.runtime.ids.next_id());
    let followee = AccountRef::Local(app.runtime.ids.next_id());

    // No `set_sink` call: this registry is exactly what a `Transitions`
    // built before `SocialGraphModule` wiring (task 5.2) looks like today.
    let registry = NotificationSinkRegistry::new();
    let transitions = Transitions::new(app.pool.clone(), app.runtime.clone(), registry);

    transitions
        .establish_follow(
            &follower,
            &followee,
            &default_opts(),
            "https://remote.test/acts/1",
        )
        .await
        .expect("establish_follow must succeed against the default no-op sink");
}

// -- remove_follow ------------------------------------------------------

#[tokio::test]
async fn remove_follow_deletes_and_is_idempotent_when_nothing_exists() {
    let app = spawn_test_app().await;
    let follower = AccountRef::Local(app.runtime.ids.next_id());
    let followee = AccountRef::Remote(app.runtime.ids.next_id());
    let transitions = Transitions::new(
        app.pool.clone(),
        app.runtime.clone(),
        NotificationSinkRegistry::new(),
    );

    transitions
        .establish_follow(
            &follower,
            &followee,
            &default_opts(),
            "https://local.test/acts/2",
        )
        .await
        .expect("establish_follow must succeed");

    transitions
        .remove_follow(&follower, &followee)
        .await
        .expect("remove_follow must succeed");
    let states = repository::load_states(
        &app.pool,
        &follower,
        std::slice::from_ref(&followee),
        app.runtime.clock.now(),
    )
    .await
    .expect("load_states must succeed");
    assert!(states[0].follow.is_none(), "follow must be gone");

    // Idempotent repeat on an already-absent follow.
    transitions
        .remove_follow(&follower, &followee)
        .await
        .expect("repeat remove_follow on an absent follow must still succeed");
}

// -- record_pending -------------------------------------------------------

#[tokio::test]
async fn record_pending_inbound_emits_once_and_is_idempotent() {
    let app = spawn_test_app().await;
    let requester = AccountRef::Remote(app.runtime.ids.next_id());
    let target = AccountRef::Local(app.runtime.ids.next_id());

    let sink = Arc::new(RecordingNotificationSink::new());
    let registry = NotificationSinkRegistry::new();
    registry.set_sink(Arc::clone(&sink) as Arc<dyn NotificationEventSink>);
    let transitions = Transitions::new(app.pool.clone(), app.runtime.clone(), registry);

    let req = FollowRequest {
        requester,
        target,
        direction: FollowRequestDirection::Inbound,
        activity_id: "https://remote.test/acts/follow-1".to_string(),
        created_at: app.runtime.clock.now(),
    };

    transitions
        .record_pending(&req)
        .await
        .expect("record_pending must succeed");
    transitions
        .record_pending(&req)
        .await
        .expect("repeat record_pending must succeed idempotently");

    let events = sink.events();
    assert_eq!(
        events.len(),
        1,
        "record_pending must emit exactly once even across a repeat call"
    );
    assert_eq!(events[0].recipient, target);
    assert_eq!(events[0].origin, requester);
    assert_eq!(events[0].kind, NotificationType::FollowRequest);
}

#[tokio::test]
async fn record_pending_outbound_never_emits() {
    let app = spawn_test_app().await;
    let requester = AccountRef::Local(app.runtime.ids.next_id());
    let target = AccountRef::Remote(app.runtime.ids.next_id());

    let sink = Arc::new(RecordingNotificationSink::new());
    let registry = NotificationSinkRegistry::new();
    registry.set_sink(Arc::clone(&sink) as Arc<dyn NotificationEventSink>);
    let transitions = Transitions::new(app.pool.clone(), app.runtime.clone(), registry);

    let req = FollowRequest {
        requester,
        target,
        direction: FollowRequestDirection::Outbound,
        activity_id: "https://local.test/acts/follow-2".to_string(),
        created_at: app.runtime.clock.now(),
    };

    transitions
        .record_pending(&req)
        .await
        .expect("record_pending must succeed for an outbound request");

    assert!(
        sink.events().is_empty(),
        "record_pending must never emit for an outbound pending request"
    );
}

// -- promote_pending / drop_pending --------------------------------------

#[tokio::test]
async fn promote_pending_establishes_follow_from_existing_outbound_request_and_preserves_activity_id()
 {
    let app = spawn_test_app().await;
    let requester = AccountRef::Local(app.runtime.ids.next_id());
    let target = AccountRef::Remote(app.runtime.ids.next_id());
    let transitions = Transitions::new(
        app.pool.clone(),
        app.runtime.clone(),
        NotificationSinkRegistry::new(),
    );

    let req = FollowRequest {
        requester,
        target,
        direction: FollowRequestDirection::Outbound,
        activity_id: "https://local.test/acts/follow-3".to_string(),
        created_at: app.runtime.clock.now(),
    };
    transitions
        .record_pending(&req)
        .await
        .expect("record_pending must succeed");

    transitions
        .promote_pending(&requester, &target)
        .await
        .expect("promote_pending must succeed");

    let states = repository::load_states(
        &app.pool,
        &requester,
        std::slice::from_ref(&target),
        app.runtime.clock.now(),
    )
    .await
    .expect("load_states must succeed");
    let follow = states[0]
        .follow
        .as_ref()
        .expect("promote_pending must establish a follow");
    assert_eq!(follow.activity_id, "https://local.test/acts/follow-3");
    assert!(
        !states[0].requested,
        "the outbound pending request must be consumed"
    );

    // Idempotent repeat: no pending request left, must not error or corrupt
    // the already-established follow.
    transitions
        .promote_pending(&requester, &target)
        .await
        .expect("repeat promote_pending must be a no-op success");
    let states_again = repository::load_states(
        &app.pool,
        &requester,
        std::slice::from_ref(&target),
        app.runtime.clock.now(),
    )
    .await
    .expect("load_states must succeed");
    assert!(states_again[0].follow.is_some());
}

#[tokio::test]
async fn promote_pending_is_idempotent_noop_when_no_pending_request_exists() {
    let app = spawn_test_app().await;
    let requester = AccountRef::Local(app.runtime.ids.next_id());
    let target = AccountRef::Remote(app.runtime.ids.next_id());
    let transitions = Transitions::new(
        app.pool.clone(),
        app.runtime.clone(),
        NotificationSinkRegistry::new(),
    );

    transitions
        .promote_pending(&requester, &target)
        .await
        .expect("promote_pending with nothing pending must succeed as a no-op");

    let states = repository::load_states(
        &app.pool,
        &requester,
        std::slice::from_ref(&target),
        app.runtime.clock.now(),
    )
    .await
    .expect("load_states must succeed");
    assert!(states[0].follow.is_none(), "no follow must be created");
}

#[tokio::test]
async fn drop_pending_deletes_outbound_request_and_is_idempotent() {
    let app = spawn_test_app().await;
    let requester = AccountRef::Local(app.runtime.ids.next_id());
    let target = AccountRef::Remote(app.runtime.ids.next_id());
    let transitions = Transitions::new(
        app.pool.clone(),
        app.runtime.clone(),
        NotificationSinkRegistry::new(),
    );

    let req = FollowRequest {
        requester,
        target,
        direction: FollowRequestDirection::Outbound,
        activity_id: "https://local.test/acts/follow-4".to_string(),
        created_at: app.runtime.clock.now(),
    };
    transitions
        .record_pending(&req)
        .await
        .expect("record_pending must succeed");

    transitions
        .drop_pending(&requester, &target)
        .await
        .expect("drop_pending must succeed");

    let states = repository::load_states(
        &app.pool,
        &requester,
        std::slice::from_ref(&target),
        app.runtime.clock.now(),
    )
    .await
    .expect("load_states must succeed");
    assert!(!states[0].requested, "the pending request must be gone");
    assert!(
        states[0].follow.is_none(),
        "drop_pending must never establish a follow"
    );

    transitions
        .drop_pending(&requester, &target)
        .await
        .expect("repeat drop_pending must still succeed");
}

// -- apply_block / clear_block --------------------------------------------

#[tokio::test]
async fn apply_block_clears_bidirectional_follows_and_pending_requests_and_creates_block_row() {
    let app = spawn_test_app().await;
    let blocker = AccountRef::Local(app.runtime.ids.next_id());
    let blocked = AccountRef::Remote(app.runtime.ids.next_id());
    let transitions = Transitions::new(
        app.pool.clone(),
        app.runtime.clone(),
        NotificationSinkRegistry::new(),
    );

    // Mutual follow both directions.
    transitions
        .establish_follow(
            &blocker,
            &blocked,
            &default_opts(),
            "https://local.test/acts/f1",
        )
        .await
        .expect("establish_follow (blocker->blocked) must succeed");
    transitions
        .establish_follow(
            &blocked,
            &blocker,
            &default_opts(),
            "https://remote.test/acts/f2",
        )
        .await
        .expect("establish_follow (blocked->blocker) must succeed");

    // A pending outbound request from blocker to blocked, and a pending
    // inbound request the blocker's own side recorded from blocked.
    transitions
        .record_pending(&FollowRequest {
            requester: blocker,
            target: blocked,
            direction: FollowRequestDirection::Outbound,
            activity_id: "https://local.test/acts/req1".to_string(),
            created_at: app.runtime.clock.now(),
        })
        .await
        .expect("record_pending (outbound) must succeed");
    transitions
        .record_pending(&FollowRequest {
            requester: blocked,
            target: blocker,
            direction: FollowRequestDirection::Inbound,
            activity_id: "https://remote.test/acts/req2".to_string(),
            created_at: app.runtime.clock.now(),
        })
        .await
        .expect("record_pending (inbound) must succeed");

    transitions
        .apply_block(&blocker, &blocked, "https://local.test/acts/block1")
        .await
        .expect("apply_block must succeed");

    let states = repository::load_states(
        &app.pool,
        &blocker,
        std::slice::from_ref(&blocked),
        app.runtime.clock.now(),
    )
    .await
    .expect("load_states must succeed");
    assert!(
        states[0].follow.is_none(),
        "blocker->blocked follow must be cleared"
    );
    assert!(
        !states[0].followed_by,
        "blocked->blocker follow must be cleared"
    );
    assert!(!states[0].requested, "outbound pending must be cleared");
    assert!(!states[0].requested_by, "inbound pending must be cleared");
    assert!(states[0].blocking, "the block row must be recorded");

    // Idempotent repeat: nothing left to clear, block row simply
    // re-affirmed, no error.
    transitions
        .apply_block(&blocker, &blocked, "https://local.test/acts/block1")
        .await
        .expect("repeat apply_block must succeed idempotently");
    let states_again = repository::load_states(
        &app.pool,
        &blocker,
        std::slice::from_ref(&blocked),
        app.runtime.clock.now(),
    )
    .await
    .expect("load_states must succeed");
    assert!(states_again[0].blocking);
    assert!(states_again[0].follow.is_none());
    assert!(!states_again[0].followed_by);
}

#[tokio::test]
async fn clear_block_removes_block_row_and_is_idempotent() {
    let app = spawn_test_app().await;
    let blocker = AccountRef::Local(app.runtime.ids.next_id());
    let blocked = AccountRef::Remote(app.runtime.ids.next_id());
    let transitions = Transitions::new(
        app.pool.clone(),
        app.runtime.clone(),
        NotificationSinkRegistry::new(),
    );

    transitions
        .apply_block(&blocker, &blocked, "https://local.test/acts/block2")
        .await
        .expect("apply_block must succeed");

    transitions
        .clear_block(&blocker, &blocked)
        .await
        .expect("clear_block must succeed");

    let states = repository::load_states(
        &app.pool,
        &blocker,
        std::slice::from_ref(&blocked),
        app.runtime.clock.now(),
    )
    .await
    .expect("load_states must succeed");
    assert!(!states[0].blocking, "block row must be gone");

    transitions
        .clear_block(&blocker, &blocked)
        .await
        .expect("repeat clear_block must still succeed");
}

// -- mark_blocked_by / clear_blocked_by ------------------------------------

#[tokio::test]
async fn mark_blocked_by_clears_relationships_and_creates_block_row_without_activity_id() {
    let app = spawn_test_app().await;
    let source = AccountRef::Remote(app.runtime.ids.next_id());
    let target = AccountRef::Local(app.runtime.ids.next_id());
    let transitions = Transitions::new(
        app.pool.clone(),
        app.runtime.clone(),
        NotificationSinkRegistry::new(),
    );

    transitions
        .establish_follow(
            &source,
            &target,
            &default_opts(),
            "https://remote.test/acts/f3",
        )
        .await
        .expect("establish_follow (source->target) must succeed");
    transitions
        .establish_follow(
            &target,
            &source,
            &default_opts(),
            "https://local.test/acts/f4",
        )
        .await
        .expect("establish_follow (target->source) must succeed");

    transitions
        .mark_blocked_by(&source, &target)
        .await
        .expect("mark_blocked_by must succeed");

    let states = repository::load_states(
        &app.pool,
        &target,
        std::slice::from_ref(&source),
        app.runtime.clock.now(),
    )
    .await
    .expect("load_states must succeed");
    assert!(
        states[0].blocked_by,
        "target must see itself as blocked_by source"
    );
    assert!(
        states[0].follow.is_none(),
        "target->source follow must be cleared"
    );
    assert!(
        !states[0].followed_by,
        "source->target follow must be cleared"
    );

    // Idempotent repeat.
    transitions
        .mark_blocked_by(&source, &target)
        .await
        .expect("repeat mark_blocked_by must succeed idempotently");
}

#[tokio::test]
async fn clear_blocked_by_removes_block_row() {
    let app = spawn_test_app().await;
    let source = AccountRef::Remote(app.runtime.ids.next_id());
    let target = AccountRef::Local(app.runtime.ids.next_id());
    let transitions = Transitions::new(
        app.pool.clone(),
        app.runtime.clone(),
        NotificationSinkRegistry::new(),
    );

    transitions
        .mark_blocked_by(&source, &target)
        .await
        .expect("mark_blocked_by must succeed");

    transitions
        .clear_blocked_by(&source, &target)
        .await
        .expect("clear_blocked_by must succeed");

    let states = repository::load_states(
        &app.pool,
        &target,
        std::slice::from_ref(&source),
        app.runtime.clock.now(),
    )
    .await
    .expect("load_states must succeed");
    assert!(!states[0].blocked_by, "blocked_by must be cleared");

    transitions
        .clear_blocked_by(&source, &target)
        .await
        .expect("repeat clear_blocked_by must still succeed");
}
