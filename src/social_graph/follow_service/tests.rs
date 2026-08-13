//! DB-backed tests for `FollowService` (Requirements 1.1-1.7), per task
//! 3.1's own observable completion condition: "ローカル/リモート対象いずれ
//! もフォローで関係（または保留）が作られ Relationship を返し、重複フォロー
//! が冪等、アンフォローで Undo が配送される状態".
//!
//! Mirrors `interaction_service/tests.rs`'s established convention
//! (`crate::test_harness::db_fixture::spawn_test_db`, a `RecordingSink` `DeliverySink`
//! double capturing every dispatched Activity) and `account_service/
//! tests.rs`'s `create_test_actor`/`sample_remote_account` helpers (this
//! service needs *real* `actor`/`account_profiles`/`remote_accounts` rows,
//! not mocks, since target existence/lock resolution is real business logic
//! under test here — unlike `InteractionService`'s tests, which mock actor
//! lookup entirely).

use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use time::OffsetDateTime;

use super::*;
use crate::accounts::model::{ProfileField, ProfilePatch, RemoteAccount};
use crate::accounts::profile_repository::upsert_profile;
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
use crate::test_harness::db_fixture::{TestDb, spawn_test_db};

// --- Test fixtures ----------------------------------------------------------

/// Creates a real owner + local actor row, returning the actor's `Id` — an
/// exact copy of `account_service/tests.rs::create_test_actor` (this
/// module's own tests need the identical real-actor shape `ActorDirectory`
/// resolves against).
async fn create_test_actor(db: &TestDb, handle: &str) -> Id {
    let now = db.runtime.clock.now();
    let owner_id = db.runtime.ids.next_id();
    create_owner(&db.pool, owner_id, now)
        .await
        .expect("creating the owner must succeed");

    let actor_id = db.runtime.ids.next_id();
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
    let mut tx = db
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

/// Marks `actor_id`'s profile as locked (manually-approves-followers), for
/// Requirement 3.x-adjacent approval-necessity scenarios.
async fn lock_actor(db: &TestDb, actor_id: Id) {
    upsert_profile(
        &db.pool,
        actor_id,
        ProfilePatch {
            locked: Some(true),
            ..Default::default()
        },
        db.runtime.clock.now(),
    )
    .await
    .expect("locking the test actor's profile must succeed");
}

fn sample_remote_account(
    id: Id,
    actor_uri: &str,
    fetched_at: OffsetDateTime,
    locked: bool,
) -> RemoteAccount {
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
        locked,
        fetched_at,
    }
}

/// Creates a real `remote_accounts` row, returning its `Id`.
async fn create_test_remote(db: &TestDb, actor_uri: &str, locked: bool) -> Id {
    let id = db.runtime.ids.next_id();
    let now = db.runtime.clock.now();
    upsert_remote(&db.pool, &sample_remote_account(id, actor_uri, now, locked))
        .await
        .expect("upsert_remote must succeed");
    id
}

/// A `DeliverySink` double recording every dispatched Activity — mirrors
/// `interaction_service/tests.rs::RecordingSink` exactly.
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

type TestService = FollowService<
    ActorDirectory,
    PgRemoteActorLookup,
    ActorDirectory,
    Arc<RecordingSink>,
    Arc<RecordingSink>,
>;

fn build_service(db: &TestDb) -> (TestService, Arc<RecordingSink>, Arc<RecordingSink>) {
    let local_sink = Arc::new(RecordingSink::new());
    let http_sink = Arc::new(RecordingSink::new());
    let delivery = DeliveryService::new(
        RecipientTargetResolver::new(ActorDirectory::new(db.pool.clone())),
        Arc::clone(&local_sink),
        Arc::clone(&http_sink),
    );
    let urls = crate::federation::urls::ActorUrls::new("kawasemi.example");
    let ids = Arc::new(SeqIdGenerator::new(70_000)) as Arc<dyn crate::runtime::IdGenerator>;
    let activity_builder = ActivityBuilder::new(
        urls,
        ids,
        ActorDirectory::new(db.pool.clone()),
        PgRemoteActorLookup::new(db.pool.clone()),
    );

    let service = FollowService::new(
        db.pool.clone(),
        db.runtime.clone(),
        ActorDirectory::new(db.pool.clone()),
        activity_builder,
        Transitions::new(
            db.pool.clone(),
            db.runtime.clone(),
            crate::statuses::notification_sink::NotificationSinkRegistry::new(),
        ),
        Arc::new(delivery),
    );
    (service, local_sink, http_sink)
}

fn default_opts() -> FollowOptions {
    FollowOptions {
        reblogs: true,
        notify: false,
        languages: Vec::new(),
    }
}

fn as_bool(value: &serde_json::Value, key: &str) -> bool {
    value
        .get(key)
        .unwrap_or_else(|| panic!("relationship JSON missing '{key}'"))
        .as_bool()
        .unwrap_or_else(|| panic!("relationship JSON '{key}' was not a bool"))
}

// --- follow: local target ----------------------------------------------------

#[tokio::test]
async fn follow_establishes_a_relationship_for_an_unlocked_local_target_and_delivers_follow() {
    let db = spawn_test_db().await;
    let (service, local_sink, http_sink) = build_service(&db);

    let viewer = create_test_actor(&db, "follower1").await;
    let target = create_test_actor(&db, "target1").await;

    let relationship = service
        .follow(viewer, &target.as_i64().to_string(), default_opts())
        .await
        .expect("follow must succeed");

    assert!(as_bool(&relationship, "following"));
    assert!(!as_bool(&relationship, "requested"));

    // Requirement 1.2/1.3: same Follow Activity delivered in-process for a
    // local target -- the local sink got exactly one call, the http sink
    // none.
    assert_eq!(local_sink.calls().len(), 1);
    assert_eq!(http_sink.calls().len(), 0);
    let (_, activity, _) = &local_sink.calls()[0];
    assert_eq!(activity.parsed().activity_type, "Follow");
}

#[tokio::test]
async fn follow_establishes_immediately_for_a_locked_local_target_same_server_privilege() {
    // Requirement 3.1/3.2: the same-server ("both local") admin privilege
    // is applied by `FollowApprovalPolicy::requires_approval` unconditionally
    // for two local actors -- a locked *local* target must still establish
    // immediately, never go pending (unlike a locked *remote* target, see
    // `follow_records_a_pending_request_for_a_locked_remote_target` below).
    let db = spawn_test_db().await;
    let (service, local_sink, _http_sink) = build_service(&db);

    let viewer = create_test_actor(&db, "follower2").await;
    let target = create_test_actor(&db, "target2").await;
    lock_actor(&db, target).await;

    let relationship = service
        .follow(viewer, &target.as_i64().to_string(), default_opts())
        .await
        .expect("follow must succeed");

    assert!(as_bool(&relationship, "following"));
    assert!(!as_bool(&relationship, "requested"));
    assert_eq!(local_sink.calls().len(), 1);
}

#[tokio::test]
async fn follow_applies_follow_options() {
    let db = spawn_test_db().await;
    let (service, _local_sink, _http_sink) = build_service(&db);

    let viewer = create_test_actor(&db, "follower3").await;
    let target = create_test_actor(&db, "target3").await;

    let opts = FollowOptions {
        reblogs: false,
        notify: true,
        languages: vec!["en".to_string(), "ja".to_string()],
    };
    let relationship = service
        .follow(viewer, &target.as_i64().to_string(), opts)
        .await
        .expect("follow must succeed");

    assert!(!as_bool(&relationship, "showing_reblogs"));
    assert!(as_bool(&relationship, "notifying"));
    assert_eq!(
        relationship.get("languages").and_then(|v| v.as_array()),
        Some(&vec![
            serde_json::Value::String("en".to_string()),
            serde_json::Value::String("ja".to_string())
        ])
    );
}

#[tokio::test]
async fn follow_is_idempotent_for_an_already_established_follow() {
    let db = spawn_test_db().await;
    let (service, local_sink, _http_sink) = build_service(&db);

    let viewer = create_test_actor(&db, "follower4").await;
    let target = create_test_actor(&db, "target4").await;

    let first = service
        .follow(viewer, &target.as_i64().to_string(), default_opts())
        .await
        .expect("first follow must succeed");
    assert!(as_bool(&first, "following"));
    assert_eq!(local_sink.calls().len(), 1);

    // A second follow request for the same pair must not create a
    // duplicate relationship/Activity (Requirement 1.6).
    let second = service
        .follow(viewer, &target.as_i64().to_string(), default_opts())
        .await
        .expect("second follow must succeed idempotently");
    assert!(as_bool(&second, "following"));
    assert_eq!(
        local_sink.calls().len(),
        1,
        "a repeat follow must not dispatch a second Activity"
    );
}

#[tokio::test]
async fn follow_is_idempotent_for_an_already_pending_request() {
    // A locked *remote* target (unlike a locked local one, see
    // `follow_establishes_immediately_for_a_locked_local_target_same_server_privilege`
    // above) genuinely requires approval, so it is the reachable case for
    // testing "already pending -> idempotent, no duplicate Activity".
    let db = spawn_test_db().await;
    let (service, _local_sink, http_sink) = build_service(&db);

    let viewer = create_test_actor(&db, "follower5").await;
    let target = create_test_remote(&db, "https://remote.example/users/dave", true).await;

    let first = service
        .follow(viewer, &target.as_i64().to_string(), default_opts())
        .await
        .expect("first follow must succeed");
    assert!(as_bool(&first, "requested"));
    assert_eq!(http_sink.calls().len(), 1);

    let second = service
        .follow(viewer, &target.as_i64().to_string(), default_opts())
        .await
        .expect("second follow must succeed idempotently");
    assert!(as_bool(&second, "requested"));
    assert_eq!(http_sink.calls().len(), 1);
}

#[tokio::test]
async fn follow_rejects_a_self_follow() {
    let db = spawn_test_db().await;
    let (service, local_sink, http_sink) = build_service(&db);

    let viewer = create_test_actor(&db, "loner").await;

    let err = service
        .follow(viewer, &viewer.as_i64().to_string(), default_opts())
        .await
        .expect_err("following yourself must fail");

    assert_eq!(err.kind, ErrorKind::Client);
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(local_sink.calls().len(), 0);
    assert_eq!(http_sink.calls().len(), 0);
}

#[tokio::test]
async fn follow_returns_not_found_for_a_nonexistent_target() {
    let db = spawn_test_db().await;
    let (service, _local_sink, _http_sink) = build_service(&db);

    let viewer = create_test_actor(&db, "seeker").await;

    let err = service
        .follow(viewer, "999999999", default_opts())
        .await
        .expect_err("a nonexistent target must fail");

    assert_eq!(err.kind, ErrorKind::Client);
    assert_eq!(err.status, StatusCode::NOT_FOUND);
}

// --- follow: remote target ---------------------------------------------------

#[tokio::test]
async fn follow_establishes_a_relationship_for_an_unlocked_remote_target_and_delivers_via_http() {
    let db = spawn_test_db().await;
    let (service, local_sink, http_sink) = build_service(&db);

    let viewer = create_test_actor(&db, "follower6").await;
    let target = create_test_remote(&db, "https://remote.example/users/alice", false).await;

    let relationship = service
        .follow(viewer, &target.as_i64().to_string(), default_opts())
        .await
        .expect("follow must succeed");

    assert!(as_bool(&relationship, "following"));

    // Requirement 1.2: delivered via the http sink (remote target), not the
    // local sink, but the exact same Activity type as the local case.
    assert_eq!(local_sink.calls().len(), 0);
    assert_eq!(http_sink.calls().len(), 1);
    let (delivery_target, activity, _) = &http_sink.calls()[0];
    assert_eq!(activity.parsed().activity_type, "Follow");
    match delivery_target {
        DeliveryTarget::Remote { inbox } => {
            assert_eq!(inbox, "https://remote.example/users/alice/inbox");
        }
        DeliveryTarget::Local { .. } => panic!("expected a remote delivery target"),
    }
}

#[tokio::test]
async fn follow_records_a_pending_request_for_a_locked_remote_target() {
    let db = spawn_test_db().await;
    let (service, _local_sink, http_sink) = build_service(&db);

    let viewer = create_test_actor(&db, "follower7").await;
    let target = create_test_remote(&db, "https://remote.example/users/bob", true).await;

    let relationship = service
        .follow(viewer, &target.as_i64().to_string(), default_opts())
        .await
        .expect("follow must succeed");

    assert!(!as_bool(&relationship, "following"));
    assert!(as_bool(&relationship, "requested"));
    assert_eq!(http_sink.calls().len(), 1);
}

// --- unfollow -----------------------------------------------------------------

#[tokio::test]
async fn unfollow_removes_an_established_follow_and_delivers_undo() {
    let db = spawn_test_db().await;
    let (service, local_sink, _http_sink) = build_service(&db);

    let viewer = create_test_actor(&db, "follower8").await;
    let target = create_test_actor(&db, "target8").await;

    service
        .follow(viewer, &target.as_i64().to_string(), default_opts())
        .await
        .expect("follow must succeed");
    assert_eq!(local_sink.calls().len(), 1);

    let relationship = service
        .unfollow(viewer, &target.as_i64().to_string())
        .await
        .expect("unfollow must succeed");

    assert!(!as_bool(&relationship, "following"));
    assert_eq!(
        local_sink.calls().len(),
        2,
        "unfollow must dispatch an Undo(Follow) Activity"
    );
    let (_, undo_activity, _) = &local_sink.calls()[1];
    assert_eq!(undo_activity.parsed().activity_type, "Undo");
}

#[tokio::test]
async fn unfollow_removes_a_pending_outbound_request_and_delivers_undo() {
    let db = spawn_test_db().await;
    let (service, local_sink, _http_sink) = build_service(&db);

    let viewer = create_test_actor(&db, "follower9").await;
    let target = create_test_actor(&db, "target9").await;
    lock_actor(&db, target).await;

    service
        .follow(viewer, &target.as_i64().to_string(), default_opts())
        .await
        .expect("follow must succeed");

    let relationship = service
        .unfollow(viewer, &target.as_i64().to_string())
        .await
        .expect("unfollow must succeed");

    assert!(!as_bool(&relationship, "requested"));
    assert_eq!(local_sink.calls().len(), 2);
    let (_, undo_activity, _) = &local_sink.calls()[1];
    assert_eq!(undo_activity.parsed().activity_type, "Undo");
}

#[tokio::test]
async fn unfollow_is_a_noop_when_no_relationship_exists() {
    let db = spawn_test_db().await;
    let (service, local_sink, http_sink) = build_service(&db);

    let viewer = create_test_actor(&db, "follower10").await;
    let target = create_test_actor(&db, "target10").await;

    let relationship = service
        .unfollow(viewer, &target.as_i64().to_string())
        .await
        .expect("unfollow must succeed even with no prior relationship");

    assert!(!as_bool(&relationship, "following"));
    assert_eq!(local_sink.calls().len(), 0);
    assert_eq!(http_sink.calls().len(), 0);
}

#[tokio::test]
async fn unfollow_removes_an_established_follow_for_a_remote_target_via_http() {
    let db = spawn_test_db().await;
    let (service, _local_sink, http_sink) = build_service(&db);

    let viewer = create_test_actor(&db, "follower11").await;
    let target = create_test_remote(&db, "https://remote.example/users/carol", false).await;

    service
        .follow(viewer, &target.as_i64().to_string(), default_opts())
        .await
        .expect("follow must succeed");
    assert_eq!(http_sink.calls().len(), 1);

    let relationship = service
        .unfollow(viewer, &target.as_i64().to_string())
        .await
        .expect("unfollow must succeed");

    assert!(!as_bool(&relationship, "following"));
    assert_eq!(http_sink.calls().len(), 2);
    let (_, undo_activity, _) = &http_sink.calls()[1];
    assert_eq!(undo_activity.parsed().activity_type, "Undo");
}
