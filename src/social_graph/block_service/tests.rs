//! DB-backed tests for `BlockService` (Requirements 5.1-5.5), per task 3.4's
//! own observable completion condition: "ブロックで双方向フォロー・保留が解
//! 消され blocking が真、Block が配送され、アンブロックで Undo(Block) が配
//! 送される状態".
//!
//! Mirrors `follow_service/tests.rs`'s established convention
//! (`crate::test_harness::spawn_test_app`, a `RecordingSink` `DeliverySink`
//! double capturing every dispatched Activity, `create_test_actor`/
//! `create_test_remote` fixtures) — this service needs *real*
//! `actor`/`remote_accounts`/`follows`/`follow_requests`/`blocks` rows, not
//! mocks, since target existence resolution and relationship-clearing are
//! real business logic under test here.

use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use time::OffsetDateTime;

use super::*;
use crate::accounts::model::{ProfileField, RemoteAccount};
use crate::accounts::remote_repository::upsert_remote;
use crate::actor::owner::create_owner;
use crate::actor::repository::insert_actor;
use crate::actor::{ActorDirectory, ActorState, ActorType, Handle};
use crate::domain::Id;
use crate::error::ErrorKind;
use crate::federation::outbound::target::RecipientTargetResolver;
use crate::federation::{CanonicalActivity, DeliveryTarget};
use crate::runtime::SeqIdGenerator;
use crate::social_graph::activity_builder::PgRemoteActorLookup;
use crate::social_graph::model::{FollowOptions, FollowRequest, FollowRequestDirection};
use crate::social_graph::transitions::Transitions;
use crate::test_harness::{TestApp, spawn_test_app};

// --- Test fixtures ----------------------------------------------------------

/// Creates a real owner + local actor row, returning the actor's `Id` — an
/// exact copy of `follow_service/tests.rs::create_test_actor` (this module's
/// own tests need the identical real-actor shape `ActorDirectory` resolves
/// against).
async fn create_test_actor(app: &TestApp, handle: &str) -> Id {
    let now = app.runtime.clock.now();
    let owner_id = app.runtime.ids.next_id();
    create_owner(&app.pool, owner_id, now)
        .await
        .expect("creating the owner must succeed");

    let actor_id = app.runtime.ids.next_id();
    let actor = crate::actor::model::LocalActor {
        id: actor_id,
        owner_id,
        handle: Handle::new(handle).expect("test handle must be valid"),
        actor_type: ActorType::Person,
        display_name: "Test Actor".to_string(),
        summary: "a test actor".to_string(),
        state: ActorState::Active,
        created_at: now,
        updated_at: now,
    };
    let mut tx = app
        .pool
        .begin()
        .await
        .expect("opening a transaction must succeed");
    insert_actor(&mut tx, &actor)
        .await
        .expect("insert_actor must succeed");
    tx.commit().await.expect("committing must succeed");

    actor_id
}

fn sample_remote_account(id: Id, actor_uri: &str, fetched_at: OffsetDateTime) -> RemoteAccount {
    RemoteAccount {
        id,
        actor_uri: actor_uri.to_string(),
        username: "alice".to_string(),
        domain: "remote.example".to_string(),
        display_name: "Alice".to_string(),
        note: String::new(),
        url: actor_uri.to_string(),
        avatar_url: None,
        header_url: None,
        fields: Vec::<ProfileField>::new(),
        bot: false,
        locked: false,
        fetched_at,
    }
}

/// Creates a real `remote_accounts` row, returning its `Id`.
async fn create_test_remote(app: &TestApp, actor_uri: &str) -> Id {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    upsert_remote(&app.pool, &sample_remote_account(id, actor_uri, now))
        .await
        .expect("upsert_remote must succeed");
    id
}

/// A `DeliverySink` double recording every dispatched Activity — mirrors
/// `follow_service/tests.rs::RecordingSink` exactly.
struct RecordingSink {
    calls: Mutex<Vec<(DeliveryTarget, CanonicalActivity, Handle)>>,
}

impl RecordingSink {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<(DeliveryTarget, CanonicalActivity, Handle)> {
        self.calls.lock().unwrap().clone()
    }
}

impl DeliverySink for RecordingSink {
    async fn dispatch(
        &self,
        target: DeliveryTarget,
        activity: &CanonicalActivity,
        sender: &Handle,
    ) -> Result<(), AppError> {
        self.calls
            .lock()
            .unwrap()
            .push((target, activity.clone(), sender.clone()));
        Ok(())
    }
}

impl DeliverySink for Arc<RecordingSink> {
    async fn dispatch(
        &self,
        target: DeliveryTarget,
        activity: &CanonicalActivity,
        sender: &Handle,
    ) -> Result<(), AppError> {
        (**self).dispatch(target, activity, sender).await
    }
}

type TestService = BlockService<
    ActorDirectory,
    PgRemoteActorLookup,
    ActorDirectory,
    Arc<RecordingSink>,
    Arc<RecordingSink>,
>;

fn build_service(app: &TestApp) -> (TestService, Arc<RecordingSink>, Arc<RecordingSink>) {
    let local_sink = Arc::new(RecordingSink::new());
    let http_sink = Arc::new(RecordingSink::new());
    let delivery = DeliveryService::new(
        RecipientTargetResolver::new(ActorDirectory::new(app.pool.clone())),
        Arc::clone(&local_sink),
        Arc::clone(&http_sink),
    );
    let urls = crate::federation::urls::ActorUrls::new("kawasemi.example");
    let ids = Arc::new(SeqIdGenerator::new(80_000)) as Arc<dyn crate::runtime::IdGenerator>;
    let activity_builder = ActivityBuilder::new(
        urls,
        ids,
        ActorDirectory::new(app.pool.clone()),
        PgRemoteActorLookup::new(app.pool.clone()),
    );

    let service = BlockService::new(
        app.pool.clone(),
        app.runtime.clone(),
        ActorDirectory::new(app.pool.clone()),
        activity_builder,
        Transitions::new(
            app.pool.clone(),
            app.runtime.clone(),
            crate::statuses::notification_sink::NotificationSinkRegistry::new(),
        ),
        Arc::new(delivery),
    );
    (service, local_sink, http_sink)
}

fn as_bool(value: &serde_json::Value, key: &str) -> bool {
    value
        .get(key)
        .unwrap_or_else(|| panic!("relationship JSON missing '{key}'"))
        .as_bool()
        .unwrap_or_else(|| panic!("relationship JSON '{key}' was not a bool"))
}

fn default_opts() -> FollowOptions {
    FollowOptions {
        reblogs: true,
        notify: false,
        languages: Vec::new(),
    }
}

// --- block: local target -----------------------------------------------------

#[tokio::test]
async fn block_records_the_relationship_and_delivers_block_locally() {
    let app = spawn_test_app().await;
    let (service, local_sink, http_sink) = build_service(&app);

    let viewer = create_test_actor(&app, "blocker1").await;
    let target = create_test_actor(&app, "blocked1").await;

    let relationship = service
        .block(viewer, &target.as_i64().to_string())
        .await
        .expect("block must succeed");

    assert!(as_bool(&relationship, "blocking"));

    // Requirement 5.3/5.5: same Block Activity delivered in-process for a
    // local target -- the local sink got exactly one call, the http sink
    // none.
    assert_eq!(local_sink.calls().len(), 1);
    assert_eq!(http_sink.calls().len(), 0);
    let (_, activity, _) = &local_sink.calls()[0];
    assert_eq!(activity.parsed().activity_type, "Block");
}

#[tokio::test]
async fn block_clears_bidirectional_follows_and_pending_requests() {
    // Requirement 5.2: blocking clears both-direction established follows
    // and both-direction pending follow requests.
    let app = spawn_test_app().await;
    let (service, _local_sink, _http_sink) = build_service(&app);

    let viewer = create_test_actor(&app, "blocker2").await;
    let target = create_test_actor(&app, "blocked2").await;

    let viewer_ref = AccountRef::Local(viewer);
    let target_ref = AccountRef::Local(target);

    let transitions = Transitions::new(
        app.pool.clone(),
        app.runtime.clone(),
        crate::statuses::notification_sink::NotificationSinkRegistry::new(),
    );
    // Mutual follow both directions.
    transitions
        .establish_follow(
            &viewer_ref,
            &target_ref,
            &default_opts(),
            "https://local.test/acts/f1",
        )
        .await
        .expect("establish_follow (viewer->target) must succeed");
    transitions
        .establish_follow(
            &target_ref,
            &viewer_ref,
            &default_opts(),
            "https://local.test/acts/f2",
        )
        .await
        .expect("establish_follow (target->viewer) must succeed");
    // A pending inbound request the viewer's own side recorded from target.
    transitions
        .record_pending(&FollowRequest {
            requester: target_ref,
            target: viewer_ref,
            direction: FollowRequestDirection::Inbound,
            activity_id: "https://local.test/acts/req1".to_string(),
            created_at: app.runtime.clock.now(),
        })
        .await
        .expect("record_pending must succeed");

    let relationship = service
        .block(viewer, &target.as_i64().to_string())
        .await
        .expect("block must succeed");

    assert!(as_bool(&relationship, "blocking"));
    assert!(!as_bool(&relationship, "following"));
    assert!(!as_bool(&relationship, "followed_by"));
    assert!(!as_bool(&relationship, "requested_by"));
}

#[tokio::test]
async fn block_is_idempotent_and_does_not_redeliver() {
    let app = spawn_test_app().await;
    let (service, local_sink, _http_sink) = build_service(&app);

    let viewer = create_test_actor(&app, "blocker3").await;
    let target = create_test_actor(&app, "blocked3").await;

    let first = service
        .block(viewer, &target.as_i64().to_string())
        .await
        .expect("first block must succeed");
    assert!(as_bool(&first, "blocking"));
    assert_eq!(local_sink.calls().len(), 1);

    // A second block request for the same pair must not dispatch a
    // duplicate Activity (this module's own documented idempotency
    // decision).
    let second = service
        .block(viewer, &target.as_i64().to_string())
        .await
        .expect("second block must succeed idempotently");
    assert!(as_bool(&second, "blocking"));
    assert_eq!(
        local_sink.calls().len(),
        1,
        "a repeat block must not dispatch a second Activity"
    );
}

#[tokio::test]
async fn block_rejects_a_self_block() {
    let app = spawn_test_app().await;
    let (service, local_sink, http_sink) = build_service(&app);

    let viewer = create_test_actor(&app, "loner_blocker").await;

    let err = service
        .block(viewer, &viewer.as_i64().to_string())
        .await
        .expect_err("blocking yourself must fail");

    assert_eq!(err.kind, ErrorKind::Client);
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(local_sink.calls().len(), 0);
    assert_eq!(http_sink.calls().len(), 0);
}

#[tokio::test]
async fn block_returns_not_found_for_a_nonexistent_target() {
    let app = spawn_test_app().await;
    let (service, _local_sink, _http_sink) = build_service(&app);

    let viewer = create_test_actor(&app, "seeker_blocker").await;

    let err = service
        .block(viewer, "999999999")
        .await
        .expect_err("a nonexistent target must fail");

    assert_eq!(err.kind, ErrorKind::Client);
    assert_eq!(err.status, StatusCode::NOT_FOUND);
}

// --- block: remote target ----------------------------------------------------

#[tokio::test]
async fn block_delivers_via_http_for_a_remote_target() {
    let app = spawn_test_app().await;
    let (service, local_sink, http_sink) = build_service(&app);

    let viewer = create_test_actor(&app, "blocker4").await;
    let target = create_test_remote(&app, "https://remote.example/users/dave").await;

    let relationship = service
        .block(viewer, &target.as_i64().to_string())
        .await
        .expect("block must succeed");

    assert!(as_bool(&relationship, "blocking"));

    // Requirement 5.3: delivered via the http sink (remote target), not the
    // local sink, but the exact same Activity type as the local case.
    assert_eq!(local_sink.calls().len(), 0);
    assert_eq!(http_sink.calls().len(), 1);
    let (delivery_target, activity, _) = &http_sink.calls()[0];
    assert_eq!(activity.parsed().activity_type, "Block");
    match delivery_target {
        DeliveryTarget::Remote { inbox } => {
            assert_eq!(inbox, "https://remote.example/users/dave/inbox");
        }
        DeliveryTarget::Local { .. } => panic!("expected a remote delivery target"),
    }
}

// --- unblock ------------------------------------------------------------------

#[tokio::test]
async fn unblock_removes_an_existing_block_and_delivers_undo() {
    let app = spawn_test_app().await;
    let (service, local_sink, _http_sink) = build_service(&app);

    let viewer = create_test_actor(&app, "blocker5").await;
    let target = create_test_actor(&app, "blocked5").await;

    service
        .block(viewer, &target.as_i64().to_string())
        .await
        .expect("block must succeed");
    assert_eq!(local_sink.calls().len(), 1);

    let relationship = service
        .unblock(viewer, &target.as_i64().to_string())
        .await
        .expect("unblock must succeed");

    assert!(!as_bool(&relationship, "blocking"));
    assert_eq!(
        local_sink.calls().len(),
        2,
        "unblock must dispatch an Undo(Block) Activity"
    );
    let (_, undo_activity, _) = &local_sink.calls()[1];
    assert_eq!(undo_activity.parsed().activity_type, "Undo");
}

#[tokio::test]
async fn unblock_is_a_noop_when_no_block_exists() {
    let app = spawn_test_app().await;
    let (service, local_sink, http_sink) = build_service(&app);

    let viewer = create_test_actor(&app, "blocker6").await;
    let target = create_test_actor(&app, "blocked6").await;

    let relationship = service
        .unblock(viewer, &target.as_i64().to_string())
        .await
        .expect("unblock must succeed even with no prior block");

    assert!(!as_bool(&relationship, "blocking"));
    assert_eq!(local_sink.calls().len(), 0);
    assert_eq!(http_sink.calls().len(), 0);
}

#[tokio::test]
async fn unblock_removes_an_existing_block_for_a_remote_target_via_http() {
    let app = spawn_test_app().await;
    let (service, _local_sink, http_sink) = build_service(&app);

    let viewer = create_test_actor(&app, "blocker7").await;
    let target = create_test_remote(&app, "https://remote.example/users/erin").await;

    service
        .block(viewer, &target.as_i64().to_string())
        .await
        .expect("block must succeed");
    assert_eq!(http_sink.calls().len(), 1);

    let relationship = service
        .unblock(viewer, &target.as_i64().to_string())
        .await
        .expect("unblock must succeed");

    assert!(!as_bool(&relationship, "blocking"));
    assert_eq!(http_sink.calls().len(), 2);
    let (_, undo_activity, _) = &http_sink.calls()[1];
    assert_eq!(undo_activity.parsed().activity_type, "Undo");
}

#[tokio::test]
async fn unblock_returns_not_found_for_a_nonexistent_target() {
    let app = spawn_test_app().await;
    let (service, _local_sink, _http_sink) = build_service(&app);

    let viewer = create_test_actor(&app, "seeker_unblocker").await;

    let err = service
        .unblock(viewer, "999999999")
        .await
        .expect_err("a nonexistent target must fail");

    assert_eq!(err.kind, ErrorKind::Client);
    assert_eq!(err.status, StatusCode::NOT_FOUND);
}
