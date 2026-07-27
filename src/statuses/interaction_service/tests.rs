//! DB-backed tests for `InteractionService` (Requirements 9.1-9.5, 10.1-10.4,
//! 11.1-11.4, 12.1-12.4), per task 5.2's observable completion condition:
//! "reblog/favourite でカウンタが増減し Announce/Like が配送される、
//! bookmark/pin は連合せず状態が反映される、direct 投稿の pin が拒否される".
//!
//! Mirrors `status_service/tests.rs`'s established convention
//! (`crate::test_harness::spawn_test_app` for an isolated, migrated schema
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
use crate::statuses::status_repository;
use crate::statuses::visibility::ViewerRelation;
use crate::test_harness::{TestApp, spawn_test_app};

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

/// Builds a ready-to-use `InteractionService` against `app`'s own
/// pool/runtime. `known_actors` pre-registers every `(Id, handle)` this
/// test needs `ActorHandleLookup`/`LocalActorLookup` to resolve (both the
/// acting actor and any target-post author whose `ActorRef` this service
/// must resolve for `Like`/`Undo` delivery). `is_follower` controls
/// `MockRelationshipQuery`'s `private` visibility answer.
fn service(
    app: &TestApp,
    known_actors: &[(Id, &str)],
    is_follower: bool,
) -> (TestService, Arc<RecordingSink>, Arc<RecordingSink>) {
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

    let service = InteractionService::new(
        app.pool.clone(),
        app.runtime.clone(),
        urls,
        activity_builder,
        actor_lookup,
        MockRelationshipQuery::new(is_follower, followers),
    );
    (service, local_sink, http_sink)
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
async fn insert_test_status(app: &TestApp, actor_id: Id, visibility: Visibility) -> Status {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
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
    status_repository::insert_status(&app.pool, &status)
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
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();
    let booster = app.runtime.ids.next_id();
    let (service, local_sink, http_sink) =
        service(&app, &[(author, "alice"), (booster, "bob")], false);

    let target = insert_test_status(&app, author, Visibility::Public).await;

    let reblog = service
        .reblog(booster, target.id)
        .await
        .expect("reblog of a visible public status must succeed");

    assert_eq!(reblog.reblog_of_id, Some(target.id));
    assert_eq!(reblog.actor_id, booster);

    let updated_target = status_repository::find_by_id(&app.pool, target.id)
        .await
        .expect("find_by_id must succeed")
        .expect("target must still exist");
    assert_eq!(updated_target.reblogs_count, 1);

    assert_eq!(deliveries(&local_sink, &http_sink), 1);
    let calls = local_sink.calls();
    assert_eq!(activity_type(calls[0].1.as_value()), "Announce");

    app.cleanup().await;
}

/// Requirement 9.3: reblogging an already-reblogged status does not create a
/// duplicate boost row or double-count `reblogs_count`, and dispatches no
/// second `Announce`.
#[tokio::test]
async fn reblogging_twice_does_not_duplicate_or_double_count() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();
    let booster = app.runtime.ids.next_id();
    let (service, local_sink, http_sink) =
        service(&app, &[(author, "alice"), (booster, "bob")], false);

    let target = insert_test_status(&app, author, Visibility::Public).await;

    let first = service
        .reblog(booster, target.id)
        .await
        .expect("first reblog must succeed");
    let second = service
        .reblog(booster, target.id)
        .await
        .expect("second reblog request must succeed (idempotent), not error");

    assert_eq!(first.id, second.id, "must return the same existing boost");

    let updated_target = status_repository::find_by_id(&app.pool, target.id)
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

    app.cleanup().await;
}

/// Requirement 9.5: reblogging a `private` status invisible to the acting
/// actor (a non-follower) is rejected.
#[tokio::test]
async fn reblogging_an_invisible_private_status_is_rejected() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();
    let booster = app.runtime.ids.next_id();
    let (service, _local, _http) = service(
        &app,
        &[(author, "alice"), (booster, "bob")],
        false, // booster is not a follower of author
    );

    let target = insert_test_status(&app, author, Visibility::Private).await;

    let err = service
        .reblog(booster, target.id)
        .await
        .expect_err("a non-follower must not be able to reblog a private status");
    assert_eq!(err.kind, ErrorKind::Client);
    assert_eq!(err.status, StatusCode::NOT_FOUND);

    let updated_target = status_repository::find_by_id(&app.pool, target.id)
        .await
        .expect("find_by_id must succeed")
        .expect("target must still exist");
    assert_eq!(updated_target.reblogs_count, 0);

    app.cleanup().await;
}

/// Requirement 9.4: unreblog decrements `reblogs_count` and dispatches a
/// canonical `Undo(Announce)`.
#[tokio::test]
async fn unreblog_decrements_count_and_dispatches_undo_announce() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();
    let booster = app.runtime.ids.next_id();
    let (service, local_sink, http_sink) =
        service(&app, &[(author, "alice"), (booster, "bob")], false);

    let target = insert_test_status(&app, author, Visibility::Public).await;
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
        interaction_repository::find_reblog(&app.pool, booster, target.id)
            .await
            .expect("find_reblog must succeed")
            .is_none()
    );

    app.cleanup().await;
}

/// Un-reblogging a status that was never reblogged by this actor is a
/// no-op: no error, no dispatch, target unchanged.
#[tokio::test]
async fn unreblog_without_a_prior_reblog_is_a_no_op() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();
    let booster = app.runtime.ids.next_id();
    let (service, local_sink, http_sink) =
        service(&app, &[(author, "alice"), (booster, "bob")], false);

    let target = insert_test_status(&app, author, Visibility::Public).await;

    let result = service
        .unreblog(booster, target.id)
        .await
        .expect("unreblog with no prior reblog must succeed as a no-op");
    assert_eq!(result.reblogs_count, 0);
    assert_eq!(deliveries(&local_sink, &http_sink), 0);

    app.cleanup().await;
}

// -- favourite / unfavourite --------------------------------------------------

/// Requirements 10.1, 10.2: a successful favourite increments
/// `favourites_count` and dispatches a canonical `Like`.
#[tokio::test]
async fn favourite_increments_count_and_dispatches_like() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();
    let fan = app.runtime.ids.next_id();
    let (service, local_sink, http_sink) =
        service(&app, &[(author, "alice"), (fan, "carol")], false);

    let target = insert_test_status(&app, author, Visibility::Public).await;

    let favourited = service
        .favourite(fan, target.id)
        .await
        .expect("favourite of a visible public status must succeed");
    assert_eq!(favourited.favourites_count, 1);

    assert_eq!(deliveries(&local_sink, &http_sink), 1);
    let calls = local_sink.calls();
    assert_eq!(activity_type(calls[0].1.as_value()), "Like");

    app.cleanup().await;
}

/// Requirement 10.4: favouriting an already-favourited status does not
/// double-count `favourites_count` or dispatch a second `Like`.
#[tokio::test]
async fn favouriting_twice_does_not_double_count() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();
    let fan = app.runtime.ids.next_id();
    let (service, local_sink, http_sink) =
        service(&app, &[(author, "alice"), (fan, "carol")], false);

    let target = insert_test_status(&app, author, Visibility::Public).await;

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

    app.cleanup().await;
}

/// Requirement 10.3: unfavourite decrements `favourites_count` and
/// dispatches a canonical `Undo(Like)`.
#[tokio::test]
async fn unfavourite_decrements_count_and_dispatches_undo_like() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();
    let fan = app.runtime.ids.next_id();
    let (service, local_sink, http_sink) =
        service(&app, &[(author, "alice"), (fan, "carol")], false);

    let target = insert_test_status(&app, author, Visibility::Public).await;
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

    app.cleanup().await;
}

// -- bookmark / list_bookmarks ------------------------------------------------

/// Requirements 11.1, 11.2: bookmark/unbookmark state persists and is
/// idempotently reversible, with zero federation dispatch either way
/// (Requirement 11.4).
#[tokio::test]
async fn bookmark_and_unbookmark_persist_state_with_no_federation_dispatch() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();
    let reader = app.runtime.ids.next_id();
    let (service, local_sink, http_sink) =
        service(&app, &[(author, "alice"), (reader, "dave")], false);

    let target = insert_test_status(&app, author, Visibility::Public).await;

    service
        .bookmark(reader, target.id, true)
        .await
        .expect("bookmarking a visible status must succeed");
    assert!(
        interaction_repository::exists_bookmark(&app.pool, reader, target.id)
            .await
            .expect("exists_bookmark must succeed")
    );

    service
        .bookmark(reader, target.id, false)
        .await
        .expect("unbookmarking must succeed");
    assert!(
        !interaction_repository::exists_bookmark(&app.pool, reader, target.id)
            .await
            .expect("exists_bookmark must succeed")
    );

    assert_eq!(
        deliveries(&local_sink, &http_sink),
        0,
        "bookmark/unbookmark must never dispatch any federation Activity"
    );

    app.cleanup().await;
}

/// Requirement 11.3: `list_bookmarks` returns the actor's bookmarked
/// statuses, paginated.
#[tokio::test]
async fn list_bookmarks_returns_bookmarked_statuses() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();
    let reader = app.runtime.ids.next_id();
    let (service, _local, _http) = service(&app, &[(author, "alice"), (reader, "dave")], false);

    let first = insert_test_status(&app, author, Visibility::Public).await;
    let second = insert_test_status(&app, author, Visibility::Public).await;
    let _not_bookmarked = insert_test_status(&app, author, Visibility::Public).await;

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

    app.cleanup().await;
}

// -- pin ----------------------------------------------------------------------

/// Requirements 12.1, 12.2: pin succeeds for an actor's own status, and can
/// be reversed; no federation dispatch either way.
#[tokio::test]
async fn pin_and_unpin_own_status_with_no_federation_dispatch() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();
    let (service, local_sink, http_sink) = service(&app, &[(author, "alice")], false);

    let status = insert_test_status(&app, author, Visibility::Public).await;

    service
        .pin(author, status.id, true)
        .await
        .expect("pinning one's own status must succeed");
    assert!(
        interaction_repository::exists_pin(&app.pool, author, status.id)
            .await
            .expect("exists_pin must succeed")
    );

    service
        .pin(author, status.id, false)
        .await
        .expect("unpinning must succeed");
    assert!(
        !interaction_repository::exists_pin(&app.pool, author, status.id)
            .await
            .expect("exists_pin must succeed")
    );

    assert_eq!(
        deliveries(&local_sink, &http_sink),
        0,
        "pin/unpin must never dispatch any federation Activity"
    );

    app.cleanup().await;
}

/// Requirement 12.3: pinning a status not owned by the requesting actor is
/// rejected.
#[tokio::test]
async fn pinning_a_non_owned_status_is_rejected() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();
    let stranger = app.runtime.ids.next_id();
    let (service, _local, _http) = service(&app, &[(author, "alice"), (stranger, "eve")], false);

    let status = insert_test_status(&app, author, Visibility::Public).await;

    let err = service
        .pin(stranger, status.id, true)
        .await
        .expect_err("a non-owner must not be able to pin another actor's status");
    assert_eq!(err.kind, ErrorKind::Client);

    assert!(
        !interaction_repository::exists_pin(&app.pool, stranger, status.id)
            .await
            .expect("exists_pin must succeed")
    );

    app.cleanup().await;
}

/// Requirement 12.4: pinning a `direct`-visibility status is rejected, even
/// for its own owner.
#[tokio::test]
async fn pinning_a_direct_status_is_rejected() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();
    let (service, _local, _http) = service(&app, &[(author, "alice")], false);

    let status = insert_test_status(&app, author, Visibility::Direct).await;

    let err = service
        .pin(author, status.id, true)
        .await
        .expect_err("pinning a direct-visibility status must be rejected");
    assert_eq!(err.kind, ErrorKind::Client);
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);

    assert!(
        !interaction_repository::exists_pin(&app.pool, author, status.id)
            .await
            .expect("exists_pin must succeed")
    );

    app.cleanup().await;
}
