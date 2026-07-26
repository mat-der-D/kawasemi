//! DB-backed tests for `PollService` (Requirements 13.2-13.6), per task
//! 5.3's observable completion condition: "締切前の有効投票で集計が更新され
//! 投票 Activity が配送される、無効投票が拒否される".
//!
//! Mirrors `interaction_service/tests.rs`'s established convention
//! (`crate::test_harness::spawn_test_app` for an isolated, migrated schema
//! plus a deterministic `RuntimeContext`, in-memory `ActorHandleLookup`/
//! `LocalActorLookup`/`DeliverySink` test doubles, a `RecordingSink` that
//! captures every dispatched Activity, and a configurable
//! `MockRelationshipQuery`). Status/poll fixtures are inserted directly via
//! `status_repository::insert_status`/`poll_repository::insert_poll` (this
//! service's own boundary does not create posts or polls — that is
//! `StatusService`'s/a future task's job, a separate boundary this task does
//! not depend on — see `poll_service.rs`'s own doc comment, "Poll creation
//! (Requirement 13.1) is out of this task's scope").

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use axum::http::StatusCode;
use time::Duration;

use super::*;
use crate::actor::{ActorState, ActorType, Handle, ResolvedActor};
use crate::domain::Visibility;
use crate::error::ErrorKind;
use crate::federation::outbound::target::{DeliveryTarget, RecipientTargetResolver};
use crate::federation::{CanonicalActivity, DeliveryService};
use crate::runtime::SeqIdGenerator;
use crate::statuses::model::{PollOption, Status};
use crate::statuses::poll_repository::insert_poll;
use crate::statuses::status_repository;
use crate::statuses::visibility::ViewerRelation;
use crate::test_harness::{TestApp, spawn_test_app};

// --- Test doubles (mirrors interaction_service/tests.rs) -------------------

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

type TestService = PollService<
    MockActorLookup,
    MockLocalActorLookup,
    Arc<RecordingSink>,
    Arc<RecordingSink>,
    MockRelationshipQuery,
>;

/// Builds a ready-to-use `PollService` against `app`'s own pool/runtime.
/// `known_actors` pre-registers every `(Id, handle)` this test needs
/// `ActorHandleLookup`/`LocalActorLookup` to resolve (both the voting actor
/// and the poll-owning status's author, whose `ActorRef` this service must
/// resolve for `deliver_vote` delivery). `is_follower` controls
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
        StatusActivityBuilder::new(urls.clone(), ids, actor_lookup.clone(), delivery);

    let service = PollService::new(
        app.pool.clone(),
        app.runtime.clone(),
        urls,
        activity_builder,
        actor_lookup,
        MockRelationshipQuery::new(is_follower, Vec::new()),
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

/// Inserts a real `statuses` row directly, owned by `actor_id`, with the
/// given `visibility` — mirrors `interaction_service/tests.rs`'s identical
/// `insert_test_status` helper (a small, deliberate duplicate: this task's
/// Boundary forbids modifying `interaction_service.rs`/`status_service.rs`
/// just to share a test helper).
async fn insert_test_status(app: &TestApp, actor_id: Id, visibility: Visibility) -> Status {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    let status = Status {
        id,
        actor_id,
        uri: format!("https://kawasemi.example/statuses/{}", id.as_i64()),
        url: Some(format!("https://kawasemi.example/@actor/{}", id.as_i64())),
        content: "what should we have for lunch?".to_string(),
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

/// Builds and persists a poll with `titles.len()` options, attached to a
/// fresh status owned by `actor_id`. Returns `(Status, Poll)`.
async fn insert_test_poll(
    app: &TestApp,
    actor_id: Id,
    visibility: Visibility,
    titles: &[&str],
    multiple: bool,
    expires_at: Option<time::OffsetDateTime>,
) -> (Status, Poll) {
    let status = insert_test_status(app, actor_id, visibility).await;
    let poll = Poll {
        id: app.runtime.ids.next_id(),
        status_id: status.id,
        expires_at,
        multiple,
    };
    let options: Vec<PollOption> = titles
        .iter()
        .enumerate()
        .map(|(idx, title)| PollOption {
            poll_id: poll.id,
            idx: idx as i32,
            title: title.to_string(),
            votes_count: 0,
        })
        .collect();

    insert_poll(&app.pool, &poll, &options)
        .await
        .expect("insert_poll must succeed for a fresh poll");

    (status, poll)
}

// -- vote: happy path ---------------------------------------------------------

/// Requirements 13.2, 13.6: a valid vote before the deadline updates the
/// tally and dispatches a canonical vote Activity (`Create{Note,
/// name=<title>}`).
#[tokio::test]
async fn valid_vote_before_deadline_updates_tally_and_dispatches_vote_activity() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();
    let voter = app.runtime.ids.next_id();
    let (service, local_sink, http_sink) =
        service(&app, &[(author, "alice"), (voter, "bob")], false);

    let expires_at = app.runtime.clock.now() + Duration::hours(1);
    let (_status, poll) = insert_test_poll(
        &app,
        author,
        Visibility::Public,
        &["Pizza", "Sushi", "Tacos"],
        false,
        Some(expires_at),
    )
    .await;

    let (returned_poll, tally) = service
        .vote(voter, poll.id, &[1])
        .await
        .expect("a valid vote before the deadline must succeed");

    assert_eq!(returned_poll.id, poll.id);
    assert_eq!(tally.own_votes, vec![1]);
    let sushi = tally
        .options
        .iter()
        .find(|o| o.idx == 1)
        .expect("option idx 1 must exist");
    assert_eq!(sushi.votes_count, 1);
    assert_eq!(tally.voters_count, 1);

    assert_eq!(deliveries(&local_sink, &http_sink), 1);
    let calls = local_sink.calls();
    let activity = calls[0].1.as_value();
    assert_eq!(activity_type(activity), "Create");
    let object = activity
        .get("object")
        .expect("Create activity must carry an object");
    assert_eq!(activity_type(object), "Note");
    assert_eq!(
        object.get("name").and_then(|v| v.as_str()),
        Some("Sushi"),
        "the dispatched vote Note must name the actually-chosen option's title"
    );

    app.cleanup().await;
}

/// Requirement 13.4 (single-choice half): a multiple-choice poll's vote
/// dispatches one independent `Create{Note,...}` Activity per selected
/// option (task 4.1's own established per-title dispatch convention).
#[tokio::test]
async fn multiple_choice_vote_dispatches_one_activity_per_selection() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();
    let voter = app.runtime.ids.next_id();
    let (service, local_sink, http_sink) =
        service(&app, &[(author, "alice"), (voter, "bob")], false);

    let (_status, poll) = insert_test_poll(
        &app,
        author,
        Visibility::Public,
        &["Pizza", "Sushi", "Tacos"],
        true,
        None,
    )
    .await;

    let (_returned_poll, tally) = service
        .vote(voter, poll.id, &[0, 2])
        .await
        .expect("a valid multi-choice vote must succeed");

    let mut own_votes = tally.own_votes.clone();
    own_votes.sort_unstable();
    assert_eq!(own_votes, vec![0, 2]);

    assert_eq!(deliveries(&local_sink, &http_sink), 2);
    let mut titles: Vec<String> = local_sink
        .calls()
        .iter()
        .map(|(_, activity, _)| {
            activity
                .as_value()
                .get("object")
                .and_then(|o| o.get("name"))
                .and_then(|n| n.as_str())
                .expect("each dispatched Note must carry a name")
                .to_string()
        })
        .collect();
    titles.sort();
    assert_eq!(titles, vec!["Pizza".to_string(), "Tacos".to_string()]);

    app.cleanup().await;
}

// -- vote: rejections ----------------------------------------------------------

/// Requirement 13.3: voting after the poll's deadline is rejected, with no
/// tally change and no dispatch.
#[tokio::test]
async fn vote_after_deadline_is_rejected_with_no_tally_change_or_dispatch() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();
    let voter = app.runtime.ids.next_id();
    let (service, local_sink, http_sink) =
        service(&app, &[(author, "alice"), (voter, "bob")], false);

    let expired_at = app.runtime.clock.now() - Duration::hours(1);
    let (_status, poll) = insert_test_poll(
        &app,
        author,
        Visibility::Public,
        &["Pizza", "Sushi"],
        false,
        Some(expired_at),
    )
    .await;

    let err = service
        .vote(voter, poll.id, &[0])
        .await
        .expect_err("voting after the deadline must be rejected");
    assert_eq!(err.kind, ErrorKind::Client);
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);

    let tally = poll_repository::tally(&app.pool, poll.id, Some(voter))
        .await
        .expect("tally must still succeed");
    assert!(tally.own_votes.is_empty());
    assert_eq!(tally.options[0].votes_count, 0);
    assert_eq!(deliveries(&local_sink, &http_sink), 0);

    app.cleanup().await;
}

/// Requirement 13.4 (range half): an out-of-range choice index is rejected.
#[tokio::test]
async fn out_of_range_choice_is_rejected() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();
    let voter = app.runtime.ids.next_id();
    let (service, local_sink, http_sink) =
        service(&app, &[(author, "alice"), (voter, "bob")], false);

    let (_status, poll) = insert_test_poll(
        &app,
        author,
        Visibility::Public,
        &["Pizza", "Sushi"],
        false,
        None,
    )
    .await;

    let err = service
        .vote(voter, poll.id, &[7])
        .await
        .expect_err("an out-of-range choice index must be rejected");
    assert_eq!(err.kind, ErrorKind::Client);
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(deliveries(&local_sink, &http_sink), 0);

    app.cleanup().await;
}

/// Requirement 13.4 (single-choice half): a single-choice poll rejects a
/// vote naming multiple selected indices.
#[tokio::test]
async fn single_choice_poll_rejects_multiple_selected_indices() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();
    let voter = app.runtime.ids.next_id();
    let (service, local_sink, http_sink) =
        service(&app, &[(author, "alice"), (voter, "bob")], false);

    let (_status, poll) = insert_test_poll(
        &app,
        author,
        Visibility::Public,
        &["Pizza", "Sushi", "Tacos"],
        false,
        None,
    )
    .await;

    let err = service
        .vote(voter, poll.id, &[0, 1])
        .await
        .expect_err("a single-choice poll must reject multiple selections");
    assert_eq!(err.kind, ErrorKind::Client);
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(deliveries(&local_sink, &http_sink), 0);

    app.cleanup().await;
}

/// Requirement 13.5: a duplicate vote by the same actor is rejected and does
/// not double-count.
#[tokio::test]
async fn duplicate_vote_by_the_same_actor_is_rejected_and_does_not_double_count() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();
    let voter = app.runtime.ids.next_id();
    let (service, local_sink, http_sink) =
        service(&app, &[(author, "alice"), (voter, "bob")], false);

    let (_status, poll) = insert_test_poll(
        &app,
        author,
        Visibility::Public,
        &["Pizza", "Sushi"],
        false,
        None,
    )
    .await;

    service
        .vote(voter, poll.id, &[0])
        .await
        .expect("first vote must succeed");
    local_sink.calls.lock().unwrap().clear();
    http_sink.calls.lock().unwrap().clear();

    let err = service
        .vote(voter, poll.id, &[1])
        .await
        .expect_err("a second vote by the same actor must be rejected");
    assert_eq!(err.kind, ErrorKind::Client);
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);

    let tally = poll_repository::tally(&app.pool, poll.id, Some(voter))
        .await
        .expect("tally must still succeed");
    assert_eq!(
        tally.own_votes,
        vec![0],
        "the original vote must be unchanged"
    );
    assert_eq!(tally.options[0].votes_count, 1);
    assert_eq!(tally.options[1].votes_count, 0);
    assert_eq!(
        deliveries(&local_sink, &http_sink),
        0,
        "a rejected duplicate vote must not dispatch a second Activity"
    );

    app.cleanup().await;
}

/// Requirement 13.2 (visibility gate): voting on a `private` poll's owning
/// status is rejected for a non-follower viewer.
#[tokio::test]
async fn voting_on_an_invisible_private_poll_is_rejected() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();
    let voter = app.runtime.ids.next_id();
    let (service, local_sink, http_sink) = service(
        &app,
        &[(author, "alice"), (voter, "bob")],
        false, // voter is not a follower of author
    );

    let (_status, poll) = insert_test_poll(
        &app,
        author,
        Visibility::Private,
        &["Pizza", "Sushi"],
        false,
        None,
    )
    .await;

    let err = service
        .vote(voter, poll.id, &[0])
        .await
        .expect_err("a non-follower must not be able to vote on a private poll");
    assert_eq!(err.kind, ErrorKind::Client);
    assert_eq!(err.status, StatusCode::NOT_FOUND);

    let tally = poll_repository::tally(&app.pool, poll.id, Some(voter))
        .await
        .expect("tally must still succeed");
    assert!(tally.own_votes.is_empty());
    assert_eq!(deliveries(&local_sink, &http_sink), 0);

    app.cleanup().await;
}

// -- poll (get) ------------------------------------------------------------

/// The "get" half: `poll()` returns the poll+tally for a visible poll.
#[tokio::test]
async fn poll_returns_poll_and_tally_for_a_visible_poll() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();
    let voter = app.runtime.ids.next_id();
    let (service, _local, _http) = service(&app, &[(author, "alice"), (voter, "bob")], false);

    let (_status, poll) = insert_test_poll(
        &app,
        author,
        Visibility::Public,
        &["Pizza", "Sushi"],
        false,
        None,
    )
    .await;
    service
        .vote(voter, poll.id, &[1])
        .await
        .expect("vote must succeed");

    let (returned_poll, tally) = service
        .poll(Some(voter), poll.id)
        .await
        .expect("poll() must succeed for a visible poll");
    assert_eq!(returned_poll.id, poll.id);
    assert_eq!(tally.own_votes, vec![1]);
    assert_eq!(tally.options[1].votes_count, 1);

    app.cleanup().await;
}

/// The "get" half: `poll()` rejects (uniform not-found) an invisible poll —
/// a `private` poll's owning status, viewed by a non-follower.
#[tokio::test]
async fn poll_rejects_an_invisible_private_poll() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();
    let stranger = app.runtime.ids.next_id();
    let (service, _local, _http) = service(
        &app,
        &[(author, "alice"), (stranger, "eve")],
        false, // stranger is not a follower of author
    );

    let (_status, poll) = insert_test_poll(
        &app,
        author,
        Visibility::Private,
        &["Pizza", "Sushi"],
        false,
        None,
    )
    .await;

    let err = service
        .poll(Some(stranger), poll.id)
        .await
        .expect_err("poll() must reject an invisible private poll");
    assert_eq!(err.kind, ErrorKind::Client);
    assert_eq!(err.status, StatusCode::NOT_FOUND);

    app.cleanup().await;
}

/// `poll()` returns a not-found for a nonexistent poll id.
#[tokio::test]
async fn poll_returns_not_found_for_a_nonexistent_poll_id() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();
    let (service, _local, _http) = service(&app, &[(author, "alice")], false);

    let missing_id = app.runtime.ids.next_id();
    let err = service
        .poll(Some(author), missing_id)
        .await
        .expect_err("poll() must reject a nonexistent poll id");
    assert_eq!(err.kind, ErrorKind::Client);
    assert_eq!(err.status, StatusCode::NOT_FOUND);

    app.cleanup().await;
}
