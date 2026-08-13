//! DB-backed tests for `InteractionService` (Requirements 9.1-9.5, 10.1-10.4,
//! 11.1-11.4, 12.1-12.4), per task 5.2's observable completion condition:
//! "reblog/favourite でカウンタが増減し Announce/Like が配送される、
//! bookmark/pin は連合せず状態が反映される、direct 投稿の pin が拒否される".
//!
//! Mirrors `status_service/tests.rs`'s established convention
//! (`crate::test_harness::db_fixture::spawn_test_db` for an isolated, migrated schema
//! plus a deterministic `RuntimeContext`, in-memory `ActorHandleLookup`/
//! `LocalActorLookup`/`DeliverySink` test doubles, a `RecordingSink` that
//! captures every dispatched Activity, and a configurable
//! `MockRelationshipQuery`). Status fixtures are inserted directly via
//! `status_repository::insert_status` (this service's own boundary does not
//! create posts — that is `StatusService`'s job, a separate boundary this
//! task does not depend on).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use axum::http::StatusCode;

use super::*;
use crate::actor::{ActorState, ActorType, Handle, ResolvedActor};
use crate::domain::Visibility;
use crate::error::ErrorKind;
use crate::federation::outbound::target::{DeliveryTarget, RecipientTargetResolver};
use crate::federation::{CanonicalActivity, DeliveryService};
use crate::runtime::SeqIdGenerator;
use crate::statuses::model::Status;
use crate::statuses::notification_sink::NotificationEventSink;
use crate::statuses::status_repository;
use crate::statuses::visibility::ViewerRelation;
use crate::test_harness::db_fixture::{TestDb, spawn_test_db};

// --- Test doubles (mirrors status_service/tests.rs) -----------------------

#[derive(Clone)]
struct MockActorLookup {
    by_id: HashMap<i64, Handle>,
}

impl MockActorLookup {
    fn with_actors(pairs: &[(Id, &str)]) -> Self {
        let mut by_id = HashMap::new();
        for (id, raw) in pairs {
            let handle = Handle::new(*raw).expect("valid test handle");
            by_id.insert(id.as_i64(), handle);
        }
        Self { by_id }
    }
}

impl ActorHandleLookup for MockActorLookup {
    async fn resolve_handle(&self, actor_id: Id) -> Result<Handle, AppError> {
        self.by_id.get(&actor_id.as_i64()).cloned().ok_or_else(|| {
            AppError::client(
                StatusCode::NOT_FOUND,
                format!("actor id {actor_id:?} not known to MockActorLookup"),
            )
        })
    }
}

struct MockLocalActorLookup {
    known_handles: std::collections::HashSet<String>,
}

impl MockLocalActorLookup {
    fn with_handles(handles: &[&str]) -> Self {
        Self {
            known_handles: handles.iter().map(|h| (*h).to_string()).collect(),
        }
    }
}

impl LocalActorLookup for MockLocalActorLookup {
    async fn resolve_actor_by_handle(
        &self,
        handle: &Handle,
    ) -> Result<Option<ResolvedActor>, AppError> {
        if self.known_handles.contains(handle.as_str()) {
            Ok(Some(ResolvedActor {
                id: Id::from_i64(1),
                handle: handle.clone(),
                actor_type: ActorType::Person,
                display_name: "Test Actor".to_string(),
                summary: String::new(),
                state: ActorState::Active,
            }))
        } else {
            Ok(None)
        }
    }
}

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

struct MockRelationshipQuery {
    is_follower: bool,
    followers: Vec<Recipient>,
}

impl MockRelationshipQuery {
    fn new(is_follower: bool, followers: Vec<Recipient>) -> Self {
        Self {
            is_follower,
            followers,
        }
    }
}

impl RelationshipQuery for MockRelationshipQuery {
    async fn viewer_relation(
        &self,
        _author: Id,
        viewer: Option<Id>,
    ) -> Result<ViewerRelation, AppError> {
        Ok(ViewerRelation {
            is_follower: viewer.is_some() && self.is_follower,
        })
    }

    async fn followers_of(&self, _author: Id) -> Result<Vec<Recipient>, AppError> {
        Ok(self.followers.clone())
    }
}

type TestService = InteractionService<
    MockActorLookup,
    MockLocalActorLookup,
    Arc<RecordingSink>,
    Arc<RecordingSink>,
    MockRelationshipQuery,
>;

/// Records every [`NotificationEventSink::emit`] call — mirrors
/// `status_service/tests.rs::RecordingNotificationSink`, for task 9.2's own
/// emit-site tests.
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
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), AppError>> + Send + 'a>>
    {
        Box::pin(async move {
            self.events.lock().unwrap().push(event);
            Ok(())
        })
    }
}

/// Builds a ready-to-use `InteractionService` against `db`'s own
/// pool/runtime. `known_actors` pre-registers every `(Id, handle)` this
/// test needs `ActorHandleLookup`/`LocalActorLookup` to resolve (both the
/// acting actor and any target-post author whose `ActorRef` this service
/// must resolve for `Like`/`Undo` delivery). `is_follower` controls
/// `MockRelationshipQuery`'s `private` visibility answer. The returned
/// [`RecordingNotificationSink`] handle lets a test assert on emitted
/// [`NotificationEvent`]s (task 9.2); most tests ignore it (`_notifications`).
fn build_service(
    db: &TestDb,
    known_actors: &[(Id, &str)],
    is_follower: bool,
) -> (
    TestService,
    Arc<RecordingSink>,
    Arc<RecordingSink>,
    Arc<RecordingNotificationSink>,
) {
    let actor_lookup = MockActorLookup::with_actors(known_actors);
    let handles: Vec<&str> = known_actors.iter().map(|(_, h)| *h).collect();
    let local_lookup = MockLocalActorLookup::with_handles(&handles);
    let local_sink = Arc::new(RecordingSink::new());
    let http_sink = Arc::new(RecordingSink::new());
    let delivery = DeliveryService::new(
        RecipientTargetResolver::new(local_lookup),
        Arc::clone(&local_sink),
        Arc::clone(&http_sink),
    );
    let urls = ActorUrls::new("kawasemi.example");
    let ids = Arc::new(SeqIdGenerator::new(90_000)) as Arc<dyn crate::runtime::IdGenerator>;
    let activity_builder =
        StatusActivityBuilder::new(urls.clone(), ids, actor_lookup.clone(), Arc::new(delivery));

    // A concrete "follower" recipient so public/unlisted/private addressing
    // (which all route followers through `derive_recipients`) has at least
    // one recipient to actually dispatch to — mirrors
    // `status_service/tests.rs::service`'s identical rationale.
    let followers = known_actors
        .first()
        .map(|(_, handle)| {
            vec![Recipient::Local(
                Handle::new(*handle).expect("valid handle"),
            )]
        })
        .unwrap_or_default();

    let notifications = Arc::new(RecordingNotificationSink::new());
    let notification_registry = NotificationSinkRegistry::new();
    notification_registry.set_sink(Arc::clone(&notifications) as Arc<dyn NotificationEventSink>);

    let service = InteractionService::new(
        db.pool.clone(),
        db.runtime.clone(),
        urls,
        activity_builder,
        actor_lookup,
        MockRelationshipQuery::new(is_follower, followers),
        notification_registry,
    );
    (service, local_sink, http_sink, notifications)
}

/// Existing-tests-facing wrapper: identical to `build_service`, minus the
/// notification handle most tests do not need.
fn service(
    db: &TestDb,
    known_actors: &[(Id, &str)],
    is_follower: bool,
) -> (TestService, Arc<RecordingSink>, Arc<RecordingSink>) {
    let (service, local_sink, http_sink, _notifications) =
        build_service(db, known_actors, is_follower);
    (service, local_sink, http_sink)
}

/// Task-9.2-facing wrapper: identical to `build_service`, returning the
/// notification handle for tests that assert on emitted events.
fn service_with_notifications(
    db: &TestDb,
    known_actors: &[(Id, &str)],
    is_follower: bool,
) -> (
    TestService,
    Arc<RecordingSink>,
    Arc<RecordingSink>,
    Arc<RecordingNotificationSink>,
) {
    build_service(db, known_actors, is_follower)
}

fn deliveries(local: &RecordingSink, http: &RecordingSink) -> usize {
    local.calls().len() + http.calls().len()
}

fn activity_type(activity: &serde_json::Value) -> &str {
    activity
        .get("type")
        .and_then(|v| v.as_str())
        .expect("activity must have a string 'type'")
}

/// Inserts a real `statuses` row directly (bypassing `StatusService`, a
/// separate boundary this task does not depend on), owned by `actor_id`,
/// with the given `visibility`.
async fn insert_test_status(db: &TestDb, actor_id: Id, visibility: Visibility) -> Status {
    let id = db.runtime.ids.next_id();
    let now = db.runtime.clock.now();
    let status = Status {
        id,
        actor_id,
        uri: format!("https://kawasemi.example/statuses/{}", id.as_i64()),
        url: Some(format!("https://kawasemi.example/@actor/{}", id.as_i64())),
        content: "hello world".to_string(),
        visibility,
        sensitive: false,
        spoiler_text: String::new(),
        in_reply_to_id: None,
        in_reply_to_account_id: None,
        reblog_of_id: None,
        poll_id: None,
        language: Some("en".to_string()),
        reblogs_count: 0,
        favourites_count: 0,
        replies_count: 0,
        local: true,
        created_at: now,
        edited_at: None,
    };
    status_repository::insert_status(&db.pool, &status)
        .await
        .expect("insert_status must succeed for a fresh id/uri");
    status
}

// -- reblog / unreblog -------------------------------------------------------

/// Requirements 9.1, 9.2: a successful reblog persists a boost row,
/// increments the target's `reblogs_count`, and dispatches a canonical
/// `Announce`.
#[tokio::test]
async fn reblog_increments_count_and_dispatches_announce() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let booster = db.runtime.ids.next_id();
    let (service, local_sink, http_sink) =
        service(&db, &[(author, "alice"), (booster, "bob")], false);

    let target = insert_test_status(&db, author, Visibility::Public).await;

    let reblog = service
        .reblog(booster, target.id)
        .await
        .expect("reblog of a visible public status must succeed");

    assert_eq!(reblog.reblog_of_id, Some(target.id));
    assert_eq!(reblog.actor_id, booster);

    let updated_target = status_repository::find_by_id(&db.pool, target.id)
        .await
        .expect("find_by_id must succeed")
        .expect("target must still exist");
    assert_eq!(updated_target.reblogs_count, 1);

    assert_eq!(deliveries(&local_sink, &http_sink), 1);
    let calls = local_sink.calls();
    assert_eq!(activity_type(calls[0].1.as_value()), "Announce");

    db.cleanup().await;
}

/// Requirement 9.3: reblogging an already-reblogged status does not create a
/// duplicate boost row or double-count `reblogs_count`, and dispatches no
/// second `Announce`.
#[tokio::test]
async fn reblogging_twice_does_not_duplicate_or_double_count() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let booster = db.runtime.ids.next_id();
    let (service, local_sink, http_sink) =
        service(&db, &[(author, "alice"), (booster, "bob")], false);

    let target = insert_test_status(&db, author, Visibility::Public).await;

    let first = service
        .reblog(booster, target.id)
        .await
        .expect("first reblog must succeed");
    let second = service
        .reblog(booster, target.id)
        .await
        .expect("second reblog request must succeed (idempotent), not error");

    assert_eq!(first.id, second.id, "must return the same existing boost");

    let updated_target = status_repository::find_by_id(&db.pool, target.id)
        .await
        .expect("find_by_id must succeed")
        .expect("target must still exist");
    assert_eq!(
        updated_target.reblogs_count, 1,
        "a duplicate reblog request must not double-count reblogs_count"
    );

    assert_eq!(
        deliveries(&local_sink, &http_sink),
        1,
        "a duplicate reblog request must not dispatch a second Announce"
    );

    db.cleanup().await;
}

/// Requirement 9.5: reblogging a `private` status invisible to the acting
/// actor (a non-follower) is rejected.
#[tokio::test]
async fn reblogging_an_invisible_private_status_is_rejected() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let booster = db.runtime.ids.next_id();
    let (service, _local, _http) = service(
        &db,
        &[(author, "alice"), (booster, "bob")],
        false, // booster is not a follower of author
    );

    let target = insert_test_status(&db, author, Visibility::Private).await;

    let err = service
        .reblog(booster, target.id)
        .await
        .expect_err("a non-follower must not be able to reblog a private status");
    assert_eq!(err.kind, ErrorKind::Client);
    assert_eq!(err.status, StatusCode::NOT_FOUND);

    let updated_target = status_repository::find_by_id(&db.pool, target.id)
        .await
        .expect("find_by_id must succeed")
        .expect("target must still exist");
    assert_eq!(updated_target.reblogs_count, 0);

    db.cleanup().await;
}

/// Requirement 9.4: unreblog decrements `reblogs_count` and dispatches a
/// canonical `Undo(Announce)`.
#[tokio::test]
async fn unreblog_decrements_count_and_dispatches_undo_announce() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let booster = db.runtime.ids.next_id();
    let (service, local_sink, http_sink) =
        service(&db, &[(author, "alice"), (booster, "bob")], false);

    let target = insert_test_status(&db, author, Visibility::Public).await;
    service
        .reblog(booster, target.id)
        .await
        .expect("reblog must succeed");
    local_sink.calls.lock().unwrap().clear();
    http_sink.calls.lock().unwrap().clear();

    let result = service
        .unreblog(booster, target.id)
        .await
        .expect("unreblog of an existing boost must succeed");
    assert_eq!(result.id, target.id);
    assert_eq!(result.reblogs_count, 0);

    assert_eq!(deliveries(&local_sink, &http_sink), 1);
    let calls = local_sink.calls();
    let (_, activity, _) = &calls[0];
    assert_eq!(activity_type(activity.as_value()), "Undo");

    // The boost row itself must be gone.
    assert!(
        interaction_repository::find_reblog(&db.pool, booster, target.id)
            .await
            .expect("find_reblog must succeed")
            .is_none()
    );

    db.cleanup().await;
}

/// Un-reblogging a status that was never reblogged by this actor is a
/// no-op: no error, no dispatch, target unchanged.
#[tokio::test]
async fn unreblog_without_a_prior_reblog_is_a_no_op() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let booster = db.runtime.ids.next_id();
    let (service, local_sink, http_sink) =
        service(&db, &[(author, "alice"), (booster, "bob")], false);

    let target = insert_test_status(&db, author, Visibility::Public).await;

    let result = service
        .unreblog(booster, target.id)
        .await
        .expect("unreblog with no prior reblog must succeed as a no-op");
    assert_eq!(result.reblogs_count, 0);
    assert_eq!(deliveries(&local_sink, &http_sink), 0);

    db.cleanup().await;
}

// -- favourite / unfavourite --------------------------------------------------

/// Requirements 10.1, 10.2: a successful favourite increments
/// `favourites_count` and dispatches a canonical `Like`.
#[tokio::test]
async fn favourite_increments_count_and_dispatches_like() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let fan = db.runtime.ids.next_id();
    let (service, local_sink, http_sink) =
        service(&db, &[(author, "alice"), (fan, "carol")], false);

    let target = insert_test_status(&db, author, Visibility::Public).await;

    let favourited = service
        .favourite(fan, target.id)
        .await
        .expect("favourite of a visible public status must succeed");
    assert_eq!(favourited.favourites_count, 1);

    assert_eq!(deliveries(&local_sink, &http_sink), 1);
    let calls = local_sink.calls();
    assert_eq!(activity_type(calls[0].1.as_value()), "Like");

    db.cleanup().await;
}

/// Requirement 10.4: favouriting an already-favourited status does not
/// double-count `favourites_count` or dispatch a second `Like`.
#[tokio::test]
async fn favouriting_twice_does_not_double_count() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let fan = db.runtime.ids.next_id();
    let (service, local_sink, http_sink) =
        service(&db, &[(author, "alice"), (fan, "carol")], false);

    let target = insert_test_status(&db, author, Visibility::Public).await;

    service
        .favourite(fan, target.id)
        .await
        .expect("first favourite must succeed");
    let second = service
        .favourite(fan, target.id)
        .await
        .expect("second favourite request must succeed (idempotent), not error");

    assert_eq!(second.favourites_count, 1);
    assert_eq!(
        deliveries(&local_sink, &http_sink),
        1,
        "a duplicate favourite must not dispatch a second Like"
    );

    db.cleanup().await;
}

/// Requirement 10.3: unfavourite decrements `favourites_count` and
/// dispatches a canonical `Undo(Like)`.
#[tokio::test]
async fn unfavourite_decrements_count_and_dispatches_undo_like() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let fan = db.runtime.ids.next_id();
    let (service, local_sink, http_sink) =
        service(&db, &[(author, "alice"), (fan, "carol")], false);

    let target = insert_test_status(&db, author, Visibility::Public).await;
    service
        .favourite(fan, target.id)
        .await
        .expect("favourite must succeed");
    local_sink.calls.lock().unwrap().clear();
    http_sink.calls.lock().unwrap().clear();

    let result = service
        .unfavourite(fan, target.id)
        .await
        .expect("unfavourite of an existing favourite must succeed");
    assert_eq!(result.favourites_count, 0);

    assert_eq!(deliveries(&local_sink, &http_sink), 1);
    let calls = local_sink.calls();
    assert_eq!(activity_type(calls[0].1.as_value()), "Undo");

    db.cleanup().await;
}

// -- bookmark / list_bookmarks ------------------------------------------------

/// Requirements 11.1, 11.2: bookmark/unbookmark state persists and is
/// idempotently reversible, with zero federation dispatch either way
/// (Requirement 11.4).
#[tokio::test]
async fn bookmark_and_unbookmark_persist_state_with_no_federation_dispatch() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let reader = db.runtime.ids.next_id();
    let (service, local_sink, http_sink) =
        service(&db, &[(author, "alice"), (reader, "dave")], false);

    let target = insert_test_status(&db, author, Visibility::Public).await;

    service
        .bookmark(reader, target.id, true)
        .await
        .expect("bookmarking a visible status must succeed");
    assert!(
        interaction_repository::exists_bookmark(&db.pool, reader, target.id)
            .await
            .expect("exists_bookmark must succeed")
    );

    service
        .bookmark(reader, target.id, false)
        .await
        .expect("unbookmarking must succeed");
    assert!(
        !interaction_repository::exists_bookmark(&db.pool, reader, target.id)
            .await
            .expect("exists_bookmark must succeed")
    );

    assert_eq!(
        deliveries(&local_sink, &http_sink),
        0,
        "bookmark/unbookmark must never dispatch any federation Activity"
    );

    db.cleanup().await;
}

/// Requirement 11.3: `list_bookmarks` returns the actor's bookmarked
/// statuses, paginated.
#[tokio::test]
async fn list_bookmarks_returns_bookmarked_statuses() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let reader = db.runtime.ids.next_id();
    let (service, _local, _http) = service(&db, &[(author, "alice"), (reader, "dave")], false);

    let first = insert_test_status(&db, author, Visibility::Public).await;
    let second = insert_test_status(&db, author, Visibility::Public).await;
    let _not_bookmarked = insert_test_status(&db, author, Visibility::Public).await;

    service
        .bookmark(reader, first.id, true)
        .await
        .expect("bookmark first must succeed");
    service
        .bookmark(reader, second.id, true)
        .await
        .expect("bookmark second must succeed");

    let page = service
        .list_bookmarks(reader, PageParams::default())
        .await
        .expect("list_bookmarks must succeed");

    let ids: Vec<Id> = page.items.iter().map(|s| s.id).collect();
    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&first.id));
    assert!(ids.contains(&second.id));

    db.cleanup().await;
}

// -- pin ----------------------------------------------------------------------

/// Requirements 12.1, 12.2: pin succeeds for an actor's own status, and can
/// be reversed; no federation dispatch either way.
#[tokio::test]
async fn pin_and_unpin_own_status_with_no_federation_dispatch() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let (service, local_sink, http_sink) = service(&db, &[(author, "alice")], false);

    let status = insert_test_status(&db, author, Visibility::Public).await;

    service
        .pin(author, status.id, true)
        .await
        .expect("pinning one's own status must succeed");
    assert!(
        interaction_repository::exists_pin(&db.pool, author, status.id)
            .await
            .expect("exists_pin must succeed")
    );

    service
        .pin(author, status.id, false)
        .await
        .expect("unpinning must succeed");
    assert!(
        !interaction_repository::exists_pin(&db.pool, author, status.id)
            .await
            .expect("exists_pin must succeed")
    );

    assert_eq!(
        deliveries(&local_sink, &http_sink),
        0,
        "pin/unpin must never dispatch any federation Activity"
    );

    db.cleanup().await;
}

/// Requirement 12.3: pinning a status not owned by the requesting actor is
/// rejected.
#[tokio::test]
async fn pinning_a_non_owned_status_is_rejected() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let stranger = db.runtime.ids.next_id();
    let (service, _local, _http) = service(&db, &[(author, "alice"), (stranger, "eve")], false);

    let status = insert_test_status(&db, author, Visibility::Public).await;

    let err = service
        .pin(stranger, status.id, true)
        .await
        .expect_err("a non-owner must not be able to pin another actor's status");
    assert_eq!(err.kind, ErrorKind::Client);

    assert!(
        !interaction_repository::exists_pin(&db.pool, stranger, status.id)
            .await
            .expect("exists_pin must succeed")
    );

    db.cleanup().await;
}

/// Requirement 12.4: pinning a `direct`-visibility status is rejected, even
/// for its own owner.
#[tokio::test]
async fn pinning_a_direct_status_is_rejected() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let (service, _local, _http) = service(&db, &[(author, "alice")], false);

    let status = insert_test_status(&db, author, Visibility::Direct).await;

    let err = service
        .pin(author, status.id, true)
        .await
        .expect_err("pinning a direct-visibility status must be rejected");
    assert_eq!(err.kind, ErrorKind::Client);
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);

    assert!(
        !interaction_repository::exists_pin(&db.pool, author, status.id)
            .await
            .expect("exists_pin must succeed")
    );

    db.cleanup().await;
}

// -- NotificationEvent emit (task 9.2) ---------------------------------------

/// Requirements 10.1, 10.2: a new favourite emits exactly one `Favourite`
/// `NotificationEvent`, tagged with the target author as `recipient`, the
/// favouriting actor as `origin`, and the target status as
/// `target_status_id`.
#[tokio::test]
async fn favourite_emits_a_favourite_notification_event() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let fan = db.runtime.ids.next_id();
    let (service, _local, _http, notifications) =
        service_with_notifications(&db, &[(author, "alice"), (fan, "carol")], false);

    let target = insert_test_status(&db, author, Visibility::Public).await;

    service
        .favourite(fan, target.id)
        .await
        .expect("favourite of a visible public status must succeed");

    let events = notifications.events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, NotificationType::Favourite);
    assert_eq!(events[0].recipient, AccountRef::Local(author));
    assert_eq!(events[0].origin, AccountRef::Local(fan));
    assert_eq!(events[0].target_status_id, Some(target.id));

    db.cleanup().await;
}

/// Requirement 10.4 / task 9.2's own idempotency requirement: favouriting
/// the same status twice by the same actor emits exactly one
/// `NotificationEvent`, not two — the emit call sits only on
/// `add_favourite`'s `is_new` branch, never the already-favourited no-op
/// branch.
#[tokio::test]
async fn favouriting_twice_emits_only_one_notification_event() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let fan = db.runtime.ids.next_id();
    let (service, _local, _http, notifications) =
        service_with_notifications(&db, &[(author, "alice"), (fan, "carol")], false);

    let target = insert_test_status(&db, author, Visibility::Public).await;

    service
        .favourite(fan, target.id)
        .await
        .expect("first favourite must succeed");
    service
        .favourite(fan, target.id)
        .await
        .expect("second (duplicate) favourite request must succeed idempotently");

    assert_eq!(
        notifications.events().len(),
        1,
        "a duplicate favourite must not re-emit a NotificationEvent"
    );

    db.cleanup().await;
}

/// Unfavouriting never emits a `NotificationEvent` (task 9.2's own emit
/// list names favourite/reblog/mention/edit — not their inverses).
#[tokio::test]
async fn unfavourite_does_not_emit_a_notification_event() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let fan = db.runtime.ids.next_id();
    let (service, _local, _http, notifications) =
        service_with_notifications(&db, &[(author, "alice"), (fan, "carol")], false);

    let target = insert_test_status(&db, author, Visibility::Public).await;
    service
        .favourite(fan, target.id)
        .await
        .expect("favourite must succeed");
    assert_eq!(notifications.events().len(), 1);

    service
        .unfavourite(fan, target.id)
        .await
        .expect("unfavourite must succeed");

    assert_eq!(
        notifications.events().len(),
        1,
        "unfavourite must not emit a NotificationEvent"
    );

    db.cleanup().await;
}

/// A self-favourite (favouriting your own post) does not emit a
/// `NotificationEvent` — documented judgment call (this module's own doc
/// comment, "Notification emit (task 9.2)").
#[tokio::test]
async fn self_favourite_does_not_emit_a_notification_event() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let (service, _local, _http, notifications) =
        service_with_notifications(&db, &[(author, "alice")], false);

    let own_status = insert_test_status(&db, author, Visibility::Public).await;

    service
        .favourite(author, own_status.id)
        .await
        .expect("favouriting your own status must succeed");

    assert_eq!(
        notifications.events().len(),
        0,
        "a self-favourite must not emit a NotificationEvent"
    );

    db.cleanup().await;
}

/// Requirements 9.1, 9.2: a new reblog emits exactly one `Reblog`
/// `NotificationEvent`, tagged with the target author as `recipient`, the
/// booster as `origin`, and the target status as `target_status_id`.
#[tokio::test]
async fn reblog_emits_a_reblog_notification_event() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let booster = db.runtime.ids.next_id();
    let (service, _local, _http, notifications) =
        service_with_notifications(&db, &[(author, "alice"), (booster, "bob")], false);

    let target = insert_test_status(&db, author, Visibility::Public).await;

    service
        .reblog(booster, target.id)
        .await
        .expect("reblog of a visible public status must succeed");

    let events = notifications.events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, NotificationType::Reblog);
    assert_eq!(events[0].recipient, AccountRef::Local(author));
    assert_eq!(events[0].origin, AccountRef::Local(booster));
    assert_eq!(events[0].target_status_id, Some(target.id));

    db.cleanup().await;
}

/// Requirement 9.3 / task 9.2's own idempotency requirement: reblogging the
/// same status twice by the same actor emits exactly one `NotificationEvent`.
#[tokio::test]
async fn reblogging_twice_emits_only_one_notification_event() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let booster = db.runtime.ids.next_id();
    let (service, _local, _http, notifications) =
        service_with_notifications(&db, &[(author, "alice"), (booster, "bob")], false);

    let target = insert_test_status(&db, author, Visibility::Public).await;

    service
        .reblog(booster, target.id)
        .await
        .expect("first reblog must succeed");
    service
        .reblog(booster, target.id)
        .await
        .expect("second (duplicate) reblog request must succeed idempotently");

    assert_eq!(
        notifications.events().len(),
        1,
        "a duplicate reblog must not re-emit a NotificationEvent"
    );

    db.cleanup().await;
}

/// Un-reblogging never emits a `NotificationEvent` (same rationale as
/// `unfavourite_does_not_emit_a_notification_event`).
#[tokio::test]
async fn unreblog_does_not_emit_a_notification_event() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let booster = db.runtime.ids.next_id();
    let (service, _local, _http, notifications) =
        service_with_notifications(&db, &[(author, "alice"), (booster, "bob")], false);

    let target = insert_test_status(&db, author, Visibility::Public).await;
    service
        .reblog(booster, target.id)
        .await
        .expect("reblog must succeed");
    assert_eq!(notifications.events().len(), 1);

    service
        .unreblog(booster, target.id)
        .await
        .expect("unreblog must succeed");

    assert_eq!(
        notifications.events().len(),
        1,
        "unreblog must not emit a NotificationEvent"
    );

    db.cleanup().await;
}

/// A self-reblog (boosting your own post) does not emit a
/// `NotificationEvent` — same documented judgment call as
/// `self_favourite_does_not_emit_a_notification_event`.
#[tokio::test]
async fn self_reblog_does_not_emit_a_notification_event() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let (service, _local, _http, notifications) =
        service_with_notifications(&db, &[(author, "alice")], false);

    let own_status = insert_test_status(&db, author, Visibility::Public).await;

    service
        .reblog(author, own_status.id)
        .await
        .expect("reblogging your own status must succeed");

    assert_eq!(
        notifications.events().len(),
        0,
        "a self-reblog must not emit a NotificationEvent"
    );

    db.cleanup().await;
}

// -- atomicity of the record/counter pair ---------------------------------

/// Installs a `BEFORE UPDATE` trigger on this test instance's own `statuses`
/// table that unconditionally raises, so the counter half of a composite
/// write (`status_repository::adjust_counts`, always an `UPDATE statuses`)
/// fails *after* the record half (the boost row `INSERT`, the `favourites`
/// `INSERT`/`DELETE`) has already run.
///
/// This is the mid-operation failure injection the atomicity guarantee needs
/// to be tested against: without a shared transaction the record half stays
/// committed while the counter never moves; with one, both roll back
/// together. The trigger is created inside
/// `spawn_test_db`'s own isolated schema (`search_path`-pinned), so it can
/// never affect a concurrently running test.
async fn fail_every_counter_update(db: &TestDb) {
    sqlx::query(
        "CREATE FUNCTION kawasemi_test_fail_counter_update() RETURNS trigger \
         LANGUAGE plpgsql AS $fn$ \
         BEGIN RAISE EXCEPTION 'injected counter-update failure'; END; \
         $fn$",
    )
    .execute(&db.pool)
    .await
    .expect("creating the failure-injection trigger function must succeed");

    sqlx::query(
        "CREATE TRIGGER kawasemi_test_fail_counter_update \
         BEFORE UPDATE ON statuses FOR EACH ROW \
         EXECUTE FUNCTION kawasemi_test_fail_counter_update()",
    )
    .execute(&db.pool)
    .await
    .expect("creating the failure-injection trigger must succeed");
}

/// When the `reblogs_count` update fails mid-operation, the boost row must
/// not survive — neither the record nor the counter may be
/// left changed on its own, and the caller must see the error.
#[tokio::test]
async fn reblog_leaves_neither_boost_row_nor_counter_changed_when_the_counter_update_fails() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let booster = db.runtime.ids.next_id();
    let (service, local_sink, http_sink, notifications) =
        service_with_notifications(&db, &[(author, "alice"), (booster, "bob")], false);

    let target = insert_test_status(&db, author, Visibility::Public).await;
    fail_every_counter_update(&db).await;

    let err = service
        .reblog(booster, target.id)
        .await
        .expect_err("a failed counter update must surface as an error");
    assert_eq!(err.kind, ErrorKind::Server);

    assert!(
        interaction_repository::find_reblog(&db.pool, booster, target.id)
            .await
            .expect("find_reblog must succeed")
            .is_none(),
        "the boost row must have rolled back with the counter update"
    );
    let reloaded = status_repository::find_by_id(&db.pool, target.id)
        .await
        .expect("find_by_id must succeed")
        .expect("target must still exist");
    assert_eq!(reloaded.reblogs_count, 0);

    assert_eq!(
        deliveries(&local_sink, &http_sink),
        0,
        "delivery happens strictly after commit, so a rolled-back reblog dispatches nothing"
    );
    assert_eq!(notifications.events().len(), 0);

    db.cleanup().await;
}

/// When the `favourites_count` update fails mid-operation, the `favourites`
/// row must not survive.
#[tokio::test]
async fn favourite_leaves_neither_row_nor_counter_changed_when_the_counter_update_fails() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let fan = db.runtime.ids.next_id();
    let (service, local_sink, http_sink, notifications) =
        service_with_notifications(&db, &[(author, "alice"), (fan, "carol")], false);

    let target = insert_test_status(&db, author, Visibility::Public).await;
    fail_every_counter_update(&db).await;

    let err = service
        .favourite(fan, target.id)
        .await
        .expect_err("a failed counter update must surface as an error");
    assert_eq!(err.kind, ErrorKind::Server);

    assert!(
        !interaction_repository::exists_favourite(&db.pool, fan, target.id)
            .await
            .expect("exists_favourite must succeed"),
        "the favourites row must have rolled back with the counter update"
    );
    let reloaded = status_repository::find_by_id(&db.pool, target.id)
        .await
        .expect("find_by_id must succeed")
        .expect("target must still exist");
    assert_eq!(reloaded.favourites_count, 0);

    assert_eq!(deliveries(&local_sink, &http_sink), 0);
    assert_eq!(notifications.events().len(), 0);

    db.cleanup().await;
}

/// The un-favourite direction is symmetric — a failed
/// counter decrement must not leave the `favourites` row deleted.
#[tokio::test]
async fn unfavourite_leaves_neither_row_nor_counter_changed_when_the_counter_update_fails() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let fan = db.runtime.ids.next_id();
    let (service, local_sink, http_sink) =
        service(&db, &[(author, "alice"), (fan, "carol")], false);

    let target = insert_test_status(&db, author, Visibility::Public).await;
    service
        .favourite(fan, target.id)
        .await
        .expect("the initial favourite must succeed");
    local_sink.calls.lock().unwrap().clear();
    http_sink.calls.lock().unwrap().clear();

    fail_every_counter_update(&db).await;

    let err = service
        .unfavourite(fan, target.id)
        .await
        .expect_err("a failed counter update must surface as an error");
    assert_eq!(err.kind, ErrorKind::Server);

    assert!(
        interaction_repository::exists_favourite(&db.pool, fan, target.id)
            .await
            .expect("exists_favourite must succeed"),
        "the favourites row deletion must have rolled back with the counter update"
    );
    let reloaded = status_repository::find_by_id(&db.pool, target.id)
        .await
        .expect("find_by_id must succeed")
        .expect("target must still exist");
    assert_eq!(
        reloaded.favourites_count, 1,
        "counter and row must stay consistent"
    );

    assert_eq!(deliveries(&local_sink, &http_sink), 0);

    db.cleanup().await;
}

// -- counter/record agreement on the success path -------------------------
//
// The rollback tests above pin the *failure* side of composite writes. The
// success side — a committed composite write leaves the counter equal to the
// real number of rows it summarises — needs its own coverage: the success
// tests only ever assert the counter (e.g. `favourites_count == 1`) without
// ever counting the rows that counter is supposed to summarise. A counter
// that drifted to a value no row backs would sail straight through them.

/// Asserts that `statuses.favourites_count` and the real number of
/// `favourites` rows for `status_id` are both `expected`.
async fn assert_favourite_counter_matches_rows(
    db: &TestDb,
    status_id: Id,
    expected: i64,
    step: &str,
) {
    let reloaded = status_repository::find_by_id(&db.pool, status_id)
        .await
        .expect("find_by_id must succeed")
        .expect("the target status must still exist");
    let (rows,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM favourites WHERE status_id = $1")
        .bind(status_id.as_i64())
        .fetch_one(&db.pool)
        .await
        .expect("counting favourites rows must succeed");

    assert_eq!(
        (reloaded.favourites_count, rows),
        (expected, expected),
        "after {step}, favourites_count and the real row count must agree"
    );
}

/// After successful favourite composite writes, the cached
/// `favourites_count` equals the real number of `favourites` rows — checked
/// across a first favourite, a duplicate (the idempotent no-op branch, which
/// must move neither half), a second distinct favouriter, and an
/// un-favourite.
#[tokio::test]
async fn favourite_counter_matches_the_actual_favourite_row_count_at_every_step() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let fan = db.runtime.ids.next_id();
    let other_fan = db.runtime.ids.next_id();
    let (service, _local, _http) = service(
        &db,
        &[(author, "alice"), (fan, "carol"), (other_fan, "dave")],
        false,
    );

    let target = insert_test_status(&db, author, Visibility::Public).await;
    assert_favourite_counter_matches_rows(&db, target.id, 0, "no favourite yet").await;

    service
        .favourite(fan, target.id)
        .await
        .expect("the first favourite must succeed");
    assert_favourite_counter_matches_rows(&db, target.id, 1, "the first favourite").await;

    // The idempotent repeat takes `add_favourite`'s `is_new == false` branch
    // inside the transaction, so it must leave both halves alone.
    service
        .favourite(fan, target.id)
        .await
        .expect("a duplicate favourite must be an accepted no-op");
    assert_favourite_counter_matches_rows(&db, target.id, 1, "a duplicate favourite").await;

    service
        .favourite(other_fan, target.id)
        .await
        .expect("a second actor's favourite must succeed");
    assert_favourite_counter_matches_rows(&db, target.id, 2, "a second distinct favouriter").await;

    service
        .unfavourite(fan, target.id)
        .await
        .expect("the un-favourite must succeed");
    assert_favourite_counter_matches_rows(&db, target.id, 1, "un-favouriting one of the two").await;

    db.cleanup().await;
}
