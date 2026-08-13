//! DB-backed tests for `StatusService` (Requirements 3.1-3.6, 5.1-5.3,
//! 6.1-6.4, 7.1-7.4, 8.1-8.5), per task 5.1's observable completion
//! condition: "投稿が作成され配送依頼が発行される、同一冪等キー再送が同一
//! 投稿を返す、不可視投稿は取得で404相当、削除/編集でDelete/Updateが配送
//! される".
//!
//! Mirrors `status_repository/tests.rs`'s established convention
//! (`crate::test_harness::db_fixture::spawn_test_db` for an isolated, migrated schema
//! plus a deterministic `RuntimeContext`) for every DB-touching operation,
//! and `activity_builder/tests.rs`'s established convention (in-memory
//! `ActorHandleLookup`/`LocalActorLookup`/`DeliverySink` test doubles plus a
//! `RecordingSink` that captures every dispatched Activity) for the
//! delivery seam this task's own instructions call out ("dispatch to a
//! fake/mock `StatusActivityBuilder`/`DeliveryService` seam").
//!
//! [`extraction_tests`] (pure, no DB/Postgres at all) covers
//! [`super::extract_content_tokens`] directly.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use axum::http::StatusCode;

use super::*;
use crate::actor::{ActorState, ActorType, ResolvedActor};
use crate::domain::Visibility;
use crate::error::ErrorKind;
use crate::federation::outbound::target::{DeliveryTarget, RecipientTargetResolver};
use crate::federation::{CanonicalActivity, DeliveryService};
use crate::media::model::{Focus, Media, MediaState, MediaType};
use crate::runtime::{DeterministicSeed, SeqIdGenerator};
use crate::statuses::notification_sink::NotificationEventSink;
use crate::statuses::visibility::ViewerRelation;
use crate::test_harness::db_fixture::{TestDb, spawn_test_db};

// --- Test doubles --------------------------------------------------------

/// In-memory [`ActorHandleLookup`] + [`MentionLookup`] double: knows a fixed
/// set of `(Id, handle)` pairs, resolvable in either direction. One type
/// serves both `StatusService`'s own `M` and its embedded
/// `StatusActivityBuilder`'s `A`.
#[derive(Clone)]
struct MockActorLookup {
    by_id: HashMap<i64, Handle>,
    by_handle: HashMap<String, Id>,
}

impl MockActorLookup {
    fn with_actors(pairs: &[(Id, &str)]) -> Self {
        let mut by_id = HashMap::new();
        let mut by_handle = HashMap::new();
        for (id, raw) in pairs {
            let handle = Handle::new(*raw).expect("valid test handle");
            by_id.insert(id.as_i64(), handle.clone());
            by_handle.insert(raw.to_string(), *id);
        }
        Self { by_id, by_handle }
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

impl MentionLookup for MockActorLookup {
    async fn resolve_local_handle(&self, handle: &Handle) -> Result<Option<Id>, AppError> {
        Ok(self.by_handle.get(handle.as_str()).copied())
    }
}

/// In-memory [`LocalActorLookup`] for `DeliveryService`'s own recipient
/// resolution — mirrors `activity_builder/tests.rs::MockLocalActorLookup`.
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

/// Records every [`DeliverySink::dispatch`] call — mirrors
/// `activity_builder/tests.rs::RecordingSink`.
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

/// A configurable [`RelationshipQuery`] fake: a fixed `is_follower` answer
/// for any authenticated viewer, and a fixed `followers_of` result.
struct MockRelationshipQuery {
    is_follower: bool,
    followers: Vec<Recipient>,
}

impl MockRelationshipQuery {
    /// `followers` is deliberately non-empty in every test that exercises
    /// delivery: `DeliveryService::deliver` dispatches to zero sinks for
    /// zero resolved recipients (the abstract "public collection" `to`/`cc`
    /// entry is not itself a concrete recipient — see
    /// `federation/outbound/delivery.rs`'s own doc comment, "one canonical
    /// Activity, one resolution"), so a test asserting a `Create`/`Delete`/
    /// `Update` actually reached a `DeliverySink` needs at least one
    /// concrete follower/mention recipient for `public`/`unlisted`/
    /// `private` addressing to resolve to.
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

type TestService = StatusService<
    MockActorLookup,
    MockLocalActorLookup,
    Arc<RecordingSink>,
    Arc<RecordingSink>,
    MockRelationshipQuery,
    MockActorLookup,
>;

/// Records every [`NotificationEventSink::emit`] call — mirrors
/// `RecordingSink`'s identical "capture every call" shape, for task 9.2's
/// own emit-site tests.
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

/// Builds a ready-to-use `StatusService` against `db`'s own pool/runtime,
/// with `author_id` pre-registered under `author_handle` (so
/// `deliver_create`/`deliver_delete`/`deliver_update` can resolve a sender),
/// plus every `(Id, handle)` in `extra_actors` also registered (task 9.2's
/// own mention-notification tests need a second, independently-resolvable
/// local actor as the mention target — most callers pass `&[]`), and
/// `is_follower` controlling `MockRelationshipQuery`'s `private` visibility
/// answer. The returned [`NotificationSinkRegistry`]-backed
/// [`RecordingNotificationSink`] handle lets a test assert on emitted
/// [`NotificationEvent`]s (task 9.2); most tests ignore it (`_notifications`).
fn build_service(
    db: &TestDb,
    author_id: Id,
    author_handle: &str,
    extra_actors: &[(Id, &str)],
    is_follower: bool,
) -> (
    TestService,
    Arc<RecordingSink>,
    Arc<RecordingSink>,
    Arc<RecordingNotificationSink>,
) {
    let mut known_actors = vec![(author_id, author_handle)];
    known_actors.extend_from_slice(extra_actors);
    let actor_lookup = MockActorLookup::with_actors(&known_actors);
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

    // A concrete "follower" recipient — reusing the author's own known
    // handle purely as a resolvable local target — so `public`/`unlisted`/
    // `private` addressing (which all route followers through
    // `derive_recipients`) has at least one recipient to actually dispatch
    // to. See `MockRelationshipQuery::new`'s own doc comment for why this
    // is necessary for a delivery-count assertion to be meaningful.
    let followers = vec![Recipient::Local(
        Handle::new(author_handle).expect("valid test handle"),
    )];

    let notifications = Arc::new(RecordingNotificationSink::new());
    let notification_registry = NotificationSinkRegistry::new();
    notification_registry.set_sink(Arc::clone(&notifications) as Arc<dyn NotificationEventSink>);

    let service = StatusService::new(
        db.pool.clone(),
        db.runtime.clone(),
        "kawasemi.example",
        urls,
        activity_builder,
        MockRelationshipQuery::new(is_follower, followers),
        actor_lookup,
        notification_registry,
    );
    (service, local_sink, http_sink, notifications)
}

/// Existing-tests-facing wrapper: identical to `build_service`, minus the
/// notification handle most tests do not need and with no extra
/// mention-target actors.
fn service(
    db: &TestDb,
    author_id: Id,
    author_handle: &str,
    is_follower: bool,
) -> (TestService, Arc<RecordingSink>, Arc<RecordingSink>) {
    let (service, local_sink, http_sink, _notifications) =
        build_service(db, author_id, author_handle, &[], is_follower);
    (service, local_sink, http_sink)
}

/// Mention-notification-facing wrapper: identical to `build_service`,
/// additionally registering `extra_actors` (task 9.2's own mention target)
/// and returning the notification handle.
fn service_with_mentions(
    db: &TestDb,
    author_id: Id,
    author_handle: &str,
    extra_actors: &[(Id, &str)],
    is_follower: bool,
) -> (
    TestService,
    Arc<RecordingSink>,
    Arc<RecordingSink>,
    Arc<RecordingNotificationSink>,
) {
    build_service(db, author_id, author_handle, extra_actors, is_follower)
}

fn create_input(content: &str, visibility: Visibility) -> CreateStatus {
    CreateStatus {
        content: content.to_string(),
        visibility,
        spoiler_text: String::new(),
        sensitive: false,
        media_ids: Vec::new(),
        in_reply_to_id: None,
        language: Some("en".to_string()),
        poll: None,
    }
}

/// Inserts a real, actor-owned `media` row directly via
/// `media_repository::insert_media` (mirrors
/// `media_repository/tests.rs::sample_media`'s fixture shape: a freshly
/// accepted, still-`Processing` row — `find_owned`'s ownership check does
/// not filter by state, so this is a sufficient fixture without waiting on
/// a real processing pipeline), so a test can exercise `attach_media`/
/// `replace_media`'s successful path with a genuinely valid, owned media id
/// rather than only the *rejection* path
/// (`media_not_owned_by_actor_is_rejected` above exercises only that half).
async fn insert_test_media(db: &TestDb, actor_id: Id) -> Id {
    let media_id = db.runtime.ids.next_id();
    let now = db.runtime.clock.now();
    let media = Media {
        id: media_id,
        actor_id,
        media_type: MediaType::Image,
        state: MediaState::Processing,
        description: None,
        focus: Focus::default(),
        meta: None,
        blurhash: None,
        created_at: now,
    };
    media_repository::insert_media(&db.pool, &media, "1/original", "image/png")
        .await
        .expect("insert_media must succeed for a fresh id/actor");
    media_id
}

fn activity_type(activity: &serde_json::Value) -> &str {
    activity
        .get("type")
        .and_then(|v| v.as_str())
        .expect("activity must have a string 'type'")
}

fn deliveries(local: &RecordingSink, http: &RecordingSink) -> usize {
    local.calls().len() + http.calls().len()
}

// -- create_status --------------------------------------------------------

/// Requirements 3.1, 4.2, 4.3: a successful create both persists the
/// `Status` and dispatches a canonical `Create` Activity.
#[tokio::test]
async fn create_status_persists_and_dispatches_create_activity() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let (service, local_sink, http_sink) = service(&db, author, "alice", false);

    let created = service
        .create_status(
            author,
            create_input("hello world", Visibility::Public),
            None,
        )
        .await
        .expect("create_status must succeed for a well-formed request");

    assert_eq!(created.content, "hello world");
    assert_eq!(created.actor_id, author);

    let fetched = status_repository::find_by_id(&db.pool, created.id)
        .await
        .expect("find_by_id must succeed")
        .expect("the created status must be persisted");
    assert_eq!(fetched.id, created.id);

    assert_eq!(deliveries(&local_sink, &http_sink), 1);
    let calls = local_sink.calls();
    let (_, activity, _) = &calls[0];
    assert_eq!(activity_type(activity.as_value()), "Create");

    db.cleanup().await;
}

/// Requirement 5.1, 5.2: a resend under the same `(actor, idempotency key)`
/// returns the *same* status rather than creating a second one.
#[tokio::test]
async fn idempotent_resubmission_returns_the_same_status() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let (service, local_sink, http_sink) = service(&db, author, "alice", false);

    let first = service
        .create_status(
            author,
            create_input("idempotent post", Visibility::Public),
            Some("client-key-1"),
        )
        .await
        .expect("first create_status with a fresh idempotency key must succeed");

    let second = service
        .create_status(
            author,
            create_input("a different body — must be ignored", Visibility::Public),
            Some("client-key-1"),
        )
        .await
        .expect("resend with the same idempotency key must succeed, not create a new post");

    assert_eq!(first.id, second.id);
    assert_eq!(second.content, "idempotent post");

    // Only the first request actually created a post + dispatched Create.
    assert_eq!(deliveries(&local_sink, &http_sink), 1);

    db.cleanup().await;
}

/// Requirement 3.2: content-less, media-less, poll-less create requests are
/// rejected as a client (422) error.
#[tokio::test]
async fn empty_status_is_rejected() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let (service, _local, _http) = service(&db, author, "alice", false);

    let err = service
        .create_status(author, create_input("   ", Visibility::Public), None)
        .await
        .expect_err("empty content, no media, no poll must be rejected");

    assert_eq!(err.kind, ErrorKind::Client);
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);

    db.cleanup().await;
}

/// Requirement 3.4: a media id not owned by the requesting actor (here:
/// simply unknown, since no `media` row exists at all) is rejected rather
/// than silently attached.
#[tokio::test]
async fn media_not_owned_by_actor_is_rejected() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let (service, _local, _http) = service(&db, author, "alice", false);

    let mut input = create_input("look at this", Visibility::Public);
    input.media_ids = vec![db.runtime.ids.next_id()]; // no such media row exists

    let err = service
        .create_status(author, input, None)
        .await
        .expect_err("a nonexistent/non-owned media id must be rejected");

    assert_eq!(err.kind, ErrorKind::Client);
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);

    db.cleanup().await;
}

/// Requirements 3.4, 8.1 (the successful counterpart to
/// `media_not_owned_by_actor_is_rejected` above, which only exercises the
/// *rejection* half): real, actor-owned media ids are persisted via
/// `attach_media` and readable back via `media_ids_for_status`, in the
/// given order.
#[tokio::test]
async fn create_status_attaches_owned_media_in_given_order() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let (service, _local, _http) = service(&db, author, "alice", false);

    let media_a = insert_test_media(&db, author).await;
    let media_b = insert_test_media(&db, author).await;

    let mut input = create_input("look at these", Visibility::Public);
    input.media_ids = vec![media_a, media_b];

    let created = service
        .create_status(author, input, None)
        .await
        .expect("create_status must succeed with valid, actor-owned media");

    let attached = status_repository::media_ids_for_status(&db.pool, created.id)
        .await
        .expect("media_ids_for_status must succeed");
    assert_eq!(
        attached,
        vec![media_a, media_b],
        "attach_media must persist the given media ids, in the given order"
    );

    db.cleanup().await;
}

/// Requirement 13.1: a poll and media attachments together are rejected as
/// mutually exclusive.
#[tokio::test]
async fn poll_and_media_together_are_rejected() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let (service, _local, _http) = service(&db, author, "alice", false);

    let mut input = create_input("pick one", Visibility::Public);
    input.media_ids = vec![db.runtime.ids.next_id()];
    input.poll = Some(CreateStatusPoll {
        options: vec!["yes".to_string(), "no".to_string()],
        multiple: false,
        expires_at: None,
    });

    let err = service
        .create_status(author, input, None)
        .await
        .expect_err("poll + media must be rejected as mutually exclusive");

    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);

    db.cleanup().await;
}

/// Requirement 13.1: a caller-supplied poll (without media) is genuinely
/// created and associated with the new post — a real `polls`/`poll_options`
/// row exists afterward, not silently dropped and not rejected.
#[tokio::test]
async fn poll_without_media_is_created_and_associated_with_the_post() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let (service, local_sink, http_sink) = service(&db, author, "alice", false);

    let mut input = create_input("pick one", Visibility::Public);
    input.poll = Some(CreateStatusPoll {
        options: vec!["yes".to_string(), "no".to_string()],
        multiple: false,
        expires_at: None,
    });

    let created = service
        .create_status(author, input, None)
        .await
        .expect("a well-formed poll (>= 2 options, no media) must be created");

    let poll_id = created
        .poll_id
        .expect("the created status must carry a poll_id");

    let poll = crate::statuses::poll_repository::find_poll_by_id(&db.pool, poll_id)
        .await
        .expect("find_poll_by_id must succeed")
        .expect("the poll row must actually exist");
    assert_eq!(poll.status_id, created.id);
    assert!(!poll.multiple);

    let tally = crate::statuses::poll_repository::tally(&db.pool, poll_id, Some(author))
        .await
        .expect("tally must succeed for a freshly-created poll");
    let titles: Vec<&str> = tally
        .options
        .iter()
        .map(|option| option.title.as_str())
        .collect();
    assert_eq!(titles, vec!["yes", "no"]);
    assert!(tally.options.iter().all(|option| option.votes_count == 0));

    // A poll-bearing post still dispatches a normal `Create` — no separate
    // "poll creation" Activity type exists (see this module's own doc
    // comment, "Poll handling").
    assert_eq!(deliveries(&local_sink, &http_sink), 1);

    db.cleanup().await;
}

/// Requirement 13.1 (implied by 13.4's "範囲外の選択肢インデックス" only
/// making sense for a poll with >= 2 options — see this module's doc
/// comment, "Poll creation validation"): fewer than 2 options is rejected.
#[tokio::test]
async fn poll_with_fewer_than_two_options_is_rejected() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let (service, _local, _http) = service(&db, author, "alice", false);

    let mut input = create_input("pick one", Visibility::Public);
    input.poll = Some(CreateStatusPoll {
        options: vec!["only one".to_string()],
        multiple: false,
        expires_at: None,
    });

    let err = service
        .create_status(author, input, None)
        .await
        .expect_err("a poll with a single option must be rejected");

    assert_eq!(err.kind, ErrorKind::Client);
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);

    db.cleanup().await;
}

/// A blank (whitespace-only) option title is rejected, matching the same
/// "degenerate poll" reasoning as the too-few-options case above.
#[tokio::test]
async fn poll_with_a_blank_option_is_rejected() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let (service, _local, _http) = service(&db, author, "alice", false);

    let mut input = create_input("pick one", Visibility::Public);
    input.poll = Some(CreateStatusPoll {
        options: vec!["yes".to_string(), "   ".to_string()],
        multiple: false,
        expires_at: None,
    });

    let err = service
        .create_status(author, input, None)
        .await
        .expect_err("a blank poll option must be rejected");

    assert_eq!(err.kind, ErrorKind::Client);
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);

    db.cleanup().await;
}

// -- create_status atomicity ----------------------------------------------
//
// A create that fails part-way through must leave nothing behind — no status
// row, media attachment, poll, tag, or parent reply-count change — and must
// surface the error to the caller rather than appearing to succeed.
//
// Failure injection: the parent post's `replies_count` (a `BIGINT`, see
// `migrations/0007_statuses.sql`) is pre-set to `i64::MAX`, so
// `status_repository::adjust_counts(.., Replies, 1)` — the *last* write in
// `create_status`'s composite write — overflows and Postgres raises
// `22003 numeric_value_out_of_range`. Every earlier write (status row,
// media attachment, poll, tags) has already been issued at that point, so
// the assertions below genuinely observe a rollback rather than
// never-attempted work. No production code path is stubbed or bypassed to
// produce this failure.

/// Row counts of every table `create_status` writes to, plus the parent's
/// own `replies_count` — the exact set a create must roll back together.
#[derive(Debug, PartialEq, Eq)]
struct WriteFootprint {
    statuses: i64,
    status_media: i64,
    polls: i64,
    poll_options: i64,
    tags: i64,
    status_tags: i64,
    parent_replies_count: i64,
}

/// `sql` is always one of this module's own literal `SELECT COUNT(*)`
/// statements (sqlx 0.9 only accepts `&'static str` here anyway).
async fn count_rows(db: &TestDb, sql: &'static str) -> i64 {
    let (n,): (i64,) = sqlx::query_as(sql)
        .fetch_one(&db.pool)
        .await
        .unwrap_or_else(|e| panic!("`{sql}` must succeed: {e}"));
    n
}

async fn write_footprint(db: &TestDb, parent_id: Id) -> WriteFootprint {
    let (parent_replies_count,): (i64,) =
        sqlx::query_as("SELECT replies_count FROM statuses WHERE id = $1")
            .bind(parent_id.as_i64())
            .fetch_one(&db.pool)
            .await
            .expect("the parent status row must exist");

    WriteFootprint {
        statuses: count_rows(db, "SELECT COUNT(*) FROM statuses").await,
        status_media: count_rows(db, "SELECT COUNT(*) FROM status_media").await,
        polls: count_rows(db, "SELECT COUNT(*) FROM polls").await,
        poll_options: count_rows(db, "SELECT COUNT(*) FROM poll_options").await,
        tags: count_rows(db, "SELECT COUNT(*) FROM tags").await,
        status_tags: count_rows(db, "SELECT COUNT(*) FROM status_tags").await,
        parent_replies_count,
    }
}

/// Creates a real parent post and saturates its `replies_count`, so the
/// reply-count increment at the end of `create_status`'s composite write is
/// guaranteed to fail. Returns the parent's id.
async fn parent_with_saturated_reply_count(db: &TestDb, service: &TestService, author: Id) -> Id {
    let parent = service
        .create_status(
            author,
            create_input("parent post", Visibility::Public),
            None,
        )
        .await
        .expect("the parent post itself must be created normally");

    sqlx::query("UPDATE statuses SET replies_count = $1 WHERE id = $2")
        .bind(i64::MAX)
        .bind(parent.id.as_i64())
        .execute(&db.pool)
        .await
        .expect("saturating the parent's replies_count must succeed");

    parent.id
}

/// When a write late in `create_status` fails, the
/// status row, its media attachments, its tags, and the parent's reply
/// count are all left exactly as they were.
#[tokio::test]
async fn create_status_leaves_no_partial_write_when_a_later_write_fails_with_media() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let (service, local_sink, http_sink) = service(&db, author, "alice", false);

    let parent_id = parent_with_saturated_reply_count(&db, &service, author).await;
    let media_id = insert_test_media(&db, author).await;
    let before = write_footprint(&db, parent_id).await;
    let deliveries_before = deliveries(&local_sink, &http_sink);

    let mut input = create_input("a reply with #rollback and media", Visibility::Public);
    input.in_reply_to_id = Some(parent_id);
    input.media_ids = vec![media_id];

    let err = service
        .create_status(author, input, None)
        .await
        .expect_err("a failing reply-count increment must surface as an error");
    assert_eq!(err.kind, ErrorKind::Server);

    assert_eq!(
        write_footprint(&db, parent_id).await,
        before,
        "no status row, media attachment, tag, or reply-count change may survive a failed \
         create_status"
    );
    assert_eq!(
        deliveries(&local_sink, &http_sink),
        deliveries_before,
        "a failed create must not have dispatched a Create Activity"
    );

    db.cleanup().await;
}

/// The poll half of the same guarantee — a poll (and
/// its options) written earlier in the same composite write is rolled back
/// too. Polls and media are mutually exclusive, hence the separate test.
#[tokio::test]
async fn create_status_leaves_no_partial_write_when_a_later_write_fails_with_poll() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let (service, local_sink, http_sink) = service(&db, author, "alice", false);

    let parent_id = parent_with_saturated_reply_count(&db, &service, author).await;
    let before = write_footprint(&db, parent_id).await;
    let deliveries_before = deliveries(&local_sink, &http_sink);

    let mut input = create_input("a reply with #rollback and a poll", Visibility::Public);
    input.in_reply_to_id = Some(parent_id);
    input.poll = Some(CreateStatusPoll {
        options: vec!["yes".to_string(), "no".to_string()],
        multiple: false,
        expires_at: None,
    });

    let err = service
        .create_status(author, input, None)
        .await
        .expect_err("a failing reply-count increment must surface as an error");
    assert_eq!(err.kind, ErrorKind::Server);

    assert_eq!(
        write_footprint(&db, parent_id).await,
        before,
        "no status row, poll, poll option, tag, or reply-count change may survive a failed \
         create_status"
    );
    assert_eq!(
        deliveries(&local_sink, &http_sink),
        deliveries_before,
        "a failed create must not have dispatched a Create Activity"
    );

    db.cleanup().await;
}

// -- counter/record agreement on the success path -------------------------

/// After successful `create_status` composite writes, the
/// parent's cached `replies_count` equals the real number of reply rows, and
/// the status's tag associations match the tags actually extracted. The two
/// rollback tests above pin the failure side; the existing success tests
/// assert response shape but never reconcile a counter against the rows it
/// summarises, so a counter that drifted free of its records would pass them.
#[tokio::test]
async fn reply_counter_matches_the_actual_reply_row_count_after_successful_creates() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let (service, _local, _http) = service(&db, author, "alice", false);

    let parent = service
        .create_status(
            author,
            create_input("parent post", Visibility::Public),
            None,
        )
        .await
        .expect("the parent post must be created");

    for n in 1..=3 {
        let mut input = create_input(&format!("reply {n} with #tagged"), Visibility::Public);
        input.in_reply_to_id = Some(parent.id);
        service
            .create_status(author, input, None)
            .await
            .unwrap_or_else(|e| panic!("reply {n} must be created: {e:?}"));

        let (replies_count,): (i64,) =
            sqlx::query_as("SELECT replies_count FROM statuses WHERE id = $1")
                .bind(parent.id.as_i64())
                .fetch_one(&db.pool)
                .await
                .expect("reading the parent's replies_count must succeed");
        let (rows,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM statuses WHERE in_reply_to_id = $1")
                .bind(parent.id.as_i64())
                .fetch_one(&db.pool)
                .await
                .expect("counting reply rows must succeed");

        assert_eq!(
            (replies_count, rows),
            (n, n),
            "after reply {n}, the parent's replies_count and the real reply row count must \
             agree"
        );
    }

    // The tag half of the same composite write: every reply carried one
    // hashtag, so the association rows must number exactly one per reply.
    assert_eq!(
        count_rows(&db, "SELECT COUNT(*) FROM status_tags").await,
        3,
        "each committed reply must have contributed exactly one tag association"
    );

    db.cleanup().await;
}

// -- show / context ---------------------------------------------------------

/// Requirement 6.4: an unauthenticated viewer only sees `public` posts —
/// not `unlisted` (task 3.1's documented, spec-intended stricter behavior
/// this task routes through, replacing `status_repository`'s own
/// provisional stand-in which incorrectly admits `unlisted` here).
#[tokio::test]
async fn unauthenticated_viewer_sees_only_public_not_unlisted() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let (service, _local, _http) = service(&db, author, "alice", false);

    let public = service
        .create_status(
            author,
            create_input("public post", Visibility::Public),
            None,
        )
        .await
        .expect("create public post");
    let unlisted = service
        .create_status(
            author,
            create_input("unlisted post", Visibility::Unlisted),
            None,
        )
        .await
        .expect("create unlisted post");

    assert!(
        service
            .show(None, public.id)
            .await
            .expect("show must succeed")
            .is_some()
    );
    assert!(
        service
            .show(None, unlisted.id)
            .await
            .expect("show must succeed")
            .is_none(),
        "unauthenticated viewers must not see unlisted posts (Requirement 6.4)"
    );

    db.cleanup().await;
}

/// Requirement 6.1: a `private` post is invisible to a non-follower viewer,
/// visible to a follower, and always visible to its own author.
#[tokio::test]
async fn private_status_is_visible_to_follower_and_author_only() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let other_viewer = db.runtime.ids.next_id();

    let (service_stranger, _l1, _h1) = service(&db, author, "alice", false);
    let private = service_stranger
        .create_status(
            author,
            create_input("just for followers", Visibility::Private),
            None,
        )
        .await
        .expect("create private post");

    // Non-follower viewer: invisible.
    assert!(
        service_stranger
            .show(Some(other_viewer), private.id)
            .await
            .expect("show must succeed")
            .is_none()
    );

    // Author: always visible.
    assert!(
        service_stranger
            .show(Some(author), private.id)
            .await
            .expect("show must succeed")
            .is_some()
    );

    // A follower: visible.
    let (service_follower, _l2, _h2) = service(&db, author, "alice", true);
    assert!(
        service_follower
            .show(Some(other_viewer), private.id)
            .await
            .expect("show must succeed")
            .is_some()
    );

    db.cleanup().await;
}

/// Requirement 6.1: an unknown id returns `None` (404-equivalent), same as
/// an invisible one.
#[tokio::test]
async fn show_returns_none_for_unknown_id() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let (service, _local, _http) = service(&db, author, "alice", false);

    let unknown_id = db.runtime.ids.next_id();
    assert!(
        service
            .show(None, unknown_id)
            .await
            .expect("show must succeed even for an unknown id")
            .is_none()
    );

    db.cleanup().await;
}

/// Requirements 6.2, 6.3: `context` returns ancestors/descendants filtered
/// to what the viewer may see.
#[tokio::test]
async fn context_returns_ancestors_and_descendants_filtered_by_visibility() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let (service, _local, _http) = service(&db, author, "alice", false);

    let root = service
        .create_status(author, create_input("root", Visibility::Public), None)
        .await
        .expect("create root");

    let mut reply_input = create_input("a public reply", Visibility::Public);
    reply_input.in_reply_to_id = Some(root.id);
    let reply = service
        .create_status(author, reply_input, None)
        .await
        .expect("create reply");

    let mut private_reply_input = create_input("a private reply", Visibility::Private);
    private_reply_input.in_reply_to_id = Some(root.id);
    let _private_reply = service
        .create_status(author, private_reply_input, None)
        .await
        .expect("create private reply");

    let context = service
        .context(None, root.id)
        .await
        .expect("context must succeed for a visible root");

    assert!(context.ancestors.is_empty());
    assert_eq!(
        context.descendants.len(),
        1,
        "the private reply must be excluded for an unauthenticated viewer"
    );
    assert_eq!(context.descendants[0].id, reply.id);

    db.cleanup().await;
}

// -- delete_status ----------------------------------------------------------

/// Requirement 7.2: deleting someone else's post is rejected.
#[tokio::test]
async fn delete_requires_ownership() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let stranger = db.runtime.ids.next_id();
    let (service, _local, _http) = service(&db, author, "alice", false);

    let status = service
        .create_status(author, create_input("mine", Visibility::Public), None)
        .await
        .expect("create status");

    let err = service
        .delete_status(stranger, status.id)
        .await
        .expect_err("a non-owner must not be able to delete another actor's post");
    assert_eq!(err.status, StatusCode::NOT_FOUND);

    // Still there afterwards.
    assert!(
        status_repository::find_by_id(&db.pool, status.id)
            .await
            .expect("find_by_id must succeed")
            .is_some()
    );

    db.cleanup().await;
}

/// Requirements 7.1, 7.3: a successful delete removes the row and
/// dispatches a canonical `Delete` Activity.
#[tokio::test]
async fn delete_removes_status_and_dispatches_delete_activity() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let (service, local_sink, http_sink) = service(&db, author, "alice", false);

    let status = service
        .create_status(author, create_input("temporary", Visibility::Public), None)
        .await
        .expect("create status");
    // Reset delivery recording so only the delete's own dispatch counts.
    local_sink.calls.lock().unwrap().clear();
    http_sink.calls.lock().unwrap().clear();

    let deleted = service
        .delete_status(author, status.id)
        .await
        .expect("owner must be able to delete their own post");
    assert_eq!(deleted.id, status.id);
    assert_eq!(deleted.content, "temporary");

    assert!(
        status_repository::find_by_id(&db.pool, status.id)
            .await
            .expect("find_by_id must succeed")
            .is_none(),
        "the status row must be gone after delete"
    );

    assert_eq!(deliveries(&local_sink, &http_sink), 1);
    let calls = local_sink.calls();
    assert_eq!(activity_type(calls[0].1.as_value()), "Delete");

    db.cleanup().await;
}

/// Cross-task boundary fix (Group 5 remediation, see this module's doc
/// comment "`delete_status` rejects reblog rows"): a boost row (`Status`
/// with `reblog_of_id` set, the same shape `InteractionService::reblog`
/// persists) must not be deletable through the generic
/// `delete_status` — only `InteractionService::unreblog` may retire it,
/// since only that path knows to decrement the *target's* `reblogs_count`
/// and dispatch `Undo(Announce)` rather than `Delete`. Builds the boost row
/// directly via `status_repository::insert_status`/`adjust_counts` (the same
/// two calls `InteractionService::reblog` itself makes) rather than via
/// `InteractionService` (out of this task's file boundary), then asserts
/// `delete_status` rejects it with a `422`, leaves the row and the original
/// post's `reblogs_count` untouched, and dispatches no Activity at all.
#[tokio::test]
async fn delete_status_rejects_a_reblog_row() {
    let db = spawn_test_db().await;
    let original_author = db.runtime.ids.next_id();
    let booster = db.runtime.ids.next_id();
    let (author_service, _author_local, _author_http) =
        service(&db, original_author, "alice", false);
    let (booster_service, local_sink, http_sink) = service(&db, booster, "bob", false);

    let original = author_service
        .create_status(
            original_author,
            create_input("original", Visibility::Public),
            None,
        )
        .await
        .expect("create original status");

    // Build the boost row exactly the way `InteractionService::reblog` does
    // (see `interaction_service.rs::reblog`), without going through
    // `InteractionService` itself.
    let reblog_id = db.runtime.ids.next_id();
    let now = db.runtime.clock.now();
    let uri = format!("https://kawasemi.example/statuses/{}", reblog_id.as_i64());
    let reblog = Status {
        id: reblog_id,
        actor_id: booster,
        uri: uri.clone(),
        url: Some(uri),
        content: String::new(),
        visibility: original.visibility,
        sensitive: false,
        spoiler_text: String::new(),
        in_reply_to_id: None,
        in_reply_to_account_id: None,
        reblog_of_id: Some(original.id),
        poll_id: None,
        language: None,
        reblogs_count: 0,
        favourites_count: 0,
        replies_count: 0,
        local: true,
        created_at: now,
        edited_at: None,
    };
    status_repository::insert_status(&db.pool, &reblog)
        .await
        .expect("insert reblog row");
    status_repository::adjust_counts(&db.pool, original.id, CountKind::Reblogs, 1)
        .await
        .expect("bump the original's reblogs_count, mirroring InteractionService::reblog");

    // Reset delivery recording so only this call's own (non-)dispatch counts.
    local_sink.calls.lock().unwrap().clear();
    http_sink.calls.lock().unwrap().clear();

    let err = booster_service
        .delete_status(booster, reblog.id)
        .await
        .expect_err("delete_status must reject a reblog row, not silently delete it");
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);

    // The boost row itself must still be there.
    assert!(
        status_repository::find_by_id(&db.pool, reblog.id)
            .await
            .expect("find_by_id must succeed")
            .is_some(),
        "a rejected delete must not remove the reblog row"
    );

    // The original post's reblogs_count must be untouched.
    let refetched_original = status_repository::find_by_id(&db.pool, original.id)
        .await
        .expect("find_by_id must succeed")
        .expect("original status must still exist");
    assert_eq!(
        refetched_original.reblogs_count, 1,
        "a rejected delete must not touch the original's reblogs_count"
    );

    // No Delete/Undo (or any) Activity must have been dispatched.
    assert_eq!(
        deliveries(&local_sink, &http_sink),
        0,
        "a rejected delete must not dispatch any Activity"
    );

    db.cleanup().await;
}

// -- edit_status / history / source ------------------------------------------

/// Requirements 8.1, 8.2, 8.4: an edit updates `edited_at`, records history,
/// and dispatches a canonical `Update` Activity.
#[tokio::test]
async fn edit_updates_edited_at_and_creates_history_and_dispatches_update() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let (service, local_sink, http_sink) = service(&db, author, "alice", false);

    let status = service
        .create_status(
            author,
            create_input("original body", Visibility::Public),
            None,
        )
        .await
        .expect("create status");
    assert!(status.edited_at.is_none());
    local_sink.calls.lock().unwrap().clear();
    http_sink.calls.lock().unwrap().clear();

    let edited = service
        .edit_status(
            author,
            status.id,
            EditStatus {
                content: "edited body".to_string(),
                spoiler_text: String::new(),
                sensitive: false,
                media_ids: Vec::new(),
            },
        )
        .await
        .expect("owner must be able to edit their own post");

    assert_eq!(edited.content, "edited body");
    assert!(edited.edited_at.is_some());

    let history = service
        .history(Some(author), status.id)
        .await
        .expect("history must succeed");
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].content, "original body");

    assert_eq!(deliveries(&local_sink, &http_sink), 1);
    let calls = local_sink.calls();
    assert_eq!(activity_type(calls[0].1.as_value()), "Update");

    db.cleanup().await;
}

/// Requirement 8.5: editing someone else's post is rejected.
#[tokio::test]
async fn edit_requires_ownership() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let stranger = db.runtime.ids.next_id();
    let (service, _local, _http) = service(&db, author, "alice", false);

    let status = service
        .create_status(author, create_input("mine", Visibility::Public), None)
        .await
        .expect("create status");

    let err = service
        .edit_status(
            stranger,
            status.id,
            EditStatus {
                content: "hijacked".to_string(),
                spoiler_text: String::new(),
                sensitive: false,
                media_ids: Vec::new(),
            },
        )
        .await
        .expect_err("a non-owner must not be able to edit another actor's post");
    assert_eq!(err.status, StatusCode::NOT_FOUND);

    db.cleanup().await;
}

/// Requirement 8.1: editing a post's media set fully *replaces* the prior
/// attachment set (via `replace_media`) — the old ids are gone, only the
/// newly-given ones remain, and an empty `media_ids` clears every
/// attachment — not merely appended to on top of the old set.
#[tokio::test]
async fn edit_status_fully_replaces_media_set_including_clearing_to_empty() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let (service, _local, _http) = service(&db, author, "alice", false);

    let media_a = insert_test_media(&db, author).await;
    let media_b = insert_test_media(&db, author).await;
    let media_c = insert_test_media(&db, author).await;

    let mut input = create_input("original body", Visibility::Public);
    input.media_ids = vec![media_a, media_b];
    let status = service
        .create_status(author, input, None)
        .await
        .expect("create status with an initial media set");

    let initial = status_repository::media_ids_for_status(&db.pool, status.id)
        .await
        .expect("media_ids_for_status must succeed");
    assert_eq!(initial, vec![media_a, media_b]);

    // Replace with a disjoint set: media_a/media_b must be gone, only
    // media_c must remain — proves this is a replace, not an append.
    let edited = service
        .edit_status(
            author,
            status.id,
            EditStatus {
                content: "edited body".to_string(),
                spoiler_text: String::new(),
                sensitive: false,
                media_ids: vec![media_c],
            },
        )
        .await
        .expect("edit_status must succeed with a different, valid media set");
    assert_eq!(edited.content, "edited body");

    let after_replace = status_repository::media_ids_for_status(&db.pool, status.id)
        .await
        .expect("media_ids_for_status must succeed");
    assert_eq!(
        after_replace,
        vec![media_c],
        "edit_status must fully replace the media set, not append to the prior one"
    );

    // Clearing to an empty media_ids must remove every attachment.
    service
        .edit_status(
            author,
            status.id,
            EditStatus {
                content: "edited again, no media".to_string(),
                spoiler_text: String::new(),
                sensitive: false,
                media_ids: Vec::new(),
            },
        )
        .await
        .expect("edit_status must succeed clearing media to empty");

    let after_clear = status_repository::media_ids_for_status(&db.pool, status.id)
        .await
        .expect("media_ids_for_status must succeed");
    assert!(
        after_clear.is_empty(),
        "an empty media_ids on edit must clear all attachments, not leave the prior set intact"
    );

    db.cleanup().await;
}

/// Requirement 8.3: `source` returns the raw text + spoiler_text, and is
/// owner-scoped.
#[tokio::test]
async fn source_returns_raw_text_and_spoiler_and_is_owner_scoped() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let stranger = db.runtime.ids.next_id();
    let (service, _local, _http) = service(&db, author, "alice", false);

    let mut input = create_input("body text", Visibility::Public);
    input.spoiler_text = "cw text".to_string();
    let status = service
        .create_status(author, input, None)
        .await
        .expect("create status");

    let source = service
        .source(author, status.id)
        .await
        .expect("owner must be able to fetch source");
    assert_eq!(source.text, "body text");
    assert_eq!(source.spoiler_text, "cw text");

    let err = service
        .source(stranger, status.id)
        .await
        .expect_err("a non-owner must not be able to fetch another actor's source");
    assert_eq!(err.status, StatusCode::NOT_FOUND);

    db.cleanup().await;
}

// -- NotificationEvent emit (task 9.2) ---------------------------------------

/// Requirement 3.6 / task 9.2: a post mentioning a registered local actor
/// emits exactly one `Mention` `NotificationEvent`, tagged with the
/// mentioned actor as `recipient`, the posting actor as `origin`, and the
/// new status as `target_status_id`.
#[tokio::test]
async fn create_status_with_a_local_mention_emits_a_mention_notification_event() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let mentioned = db.runtime.ids.next_id();
    let (service, _local, _http, notifications) =
        service_with_mentions(&db, author, "alice", &[(mentioned, "bob")], false);

    let status = service
        .create_status(
            author,
            create_input("hello @bob, welcome!", Visibility::Public),
            None,
        )
        .await
        .expect("create_status with a valid local mention must succeed");

    let events = notifications.events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, NotificationType::Mention);
    assert_eq!(events[0].recipient, AccountRef::Local(mentioned));
    assert_eq!(events[0].origin, AccountRef::Local(author));
    assert_eq!(events[0].target_status_id, Some(status.id));

    db.cleanup().await;
}

/// A post mentioning two distinct registered local actors emits one
/// `Mention` `NotificationEvent` per mentioned actor.
#[tokio::test]
async fn create_status_with_two_local_mentions_emits_two_mention_notification_events() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let bob = db.runtime.ids.next_id();
    let carol = db.runtime.ids.next_id();
    let (service, _local, _http, notifications) = service_with_mentions(
        &db,
        author,
        "alice",
        &[(bob, "bob"), (carol, "carol")],
        false,
    );

    service
        .create_status(
            author,
            create_input("hey @bob and @carol", Visibility::Public),
            None,
        )
        .await
        .expect("create_status with two valid local mentions must succeed");

    let events = notifications.events();
    assert_eq!(events.len(), 2);
    let recipients: std::collections::HashSet<AccountRef> =
        events.iter().map(|e| e.recipient).collect();
    assert!(recipients.contains(&AccountRef::Local(bob)));
    assert!(recipients.contains(&AccountRef::Local(carol)));
    assert!(events.iter().all(|e| e.kind == NotificationType::Mention));

    db.cleanup().await;
}

/// Requirement 5.1's own "Mention resolution: local only" gap: a mention
/// naming a different domain is extracted but never resolved to a local
/// actor, so it emits no `NotificationEvent` (there is no `Id` to tag it
/// with).
#[tokio::test]
async fn create_status_with_a_remote_domain_mention_emits_no_notification_event() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let (service, _local, _http, notifications) =
        service_with_mentions(&db, author, "alice", &[], false);

    service
        .create_status(
            author,
            create_input("hi @bob@remote.example", Visibility::Direct),
            None,
        )
        .await
        .expect("create_status with an unresolvable remote mention must still succeed");

    assert!(
        notifications.events().is_empty(),
        "a remote-domain mention must not emit a NotificationEvent (never resolved to an Id)"
    );

    db.cleanup().await;
}

/// A self-mention (mentioning your own handle) does not emit a
/// `NotificationEvent` — documented judgment call (this module's own doc
/// comment, "Notification emit (task 9.2)").
#[tokio::test]
async fn create_status_with_a_self_mention_emits_no_notification_event() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let (service, _local, _http, notifications) =
        service_with_mentions(&db, author, "alice", &[], false);

    service
        .create_status(
            author,
            create_input("talking to myself @alice", Visibility::Public),
            None,
        )
        .await
        .expect("create_status with a self-mention must succeed");

    assert!(
        notifications.events().is_empty(),
        "a self-mention must not emit a NotificationEvent"
    );

    db.cleanup().await;
}

/// task 9.2's own idempotency requirement: a resend under the same
/// `(actor, idempotency key)` (Requirements 5.1, 5.2) must not re-emit a
/// `Mention` `NotificationEvent` — the idempotent-resend branch returns
/// early, before `create_status`'s mention-emit loop is ever reached.
#[tokio::test]
async fn idempotent_resubmission_does_not_re_emit_a_mention_notification_event() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let mentioned = db.runtime.ids.next_id();
    let (service, _local, _http, notifications) =
        service_with_mentions(&db, author, "alice", &[(mentioned, "bob")], false);

    service
        .create_status(
            author,
            create_input("hi @bob", Visibility::Public),
            Some("client-key-1"),
        )
        .await
        .expect("first create_status must succeed");
    service
        .create_status(
            author,
            create_input("a different body — must be ignored", Visibility::Public),
            Some("client-key-1"),
        )
        .await
        .expect("resend with the same idempotency key must succeed, not create a new post");

    assert_eq!(
        notifications.events().len(),
        1,
        "an idempotent resend must not re-emit a Mention NotificationEvent"
    );

    db.cleanup().await;
}

/// `delete_status`/`edit_status` never emit a `NotificationEvent` (delete:
/// out of task 9.2's own emit list; edit: a documented "not implemented"
/// judgment call — see this module's doc comment, "Notification emit (task
/// 9.2)").
#[tokio::test]
async fn delete_and_edit_do_not_emit_notification_events() {
    let db = spawn_test_db().await;
    let author = db.runtime.ids.next_id();
    let (service, _local, _http, notifications) =
        service_with_mentions(&db, author, "alice", &[], false);

    let status = service
        .create_status(
            author,
            create_input("hello world", Visibility::Public),
            None,
        )
        .await
        .expect("create_status must succeed");
    // The create itself emitted no events (no mentions in this content).
    assert_eq!(notifications.events().len(), 0);

    service
        .edit_status(
            author,
            status.id,
            EditStatus {
                content: "edited body".to_string(),
                spoiler_text: String::new(),
                sensitive: false,
                media_ids: Vec::new(),
            },
        )
        .await
        .expect("edit_status must succeed");
    assert_eq!(
        notifications.events().len(),
        0,
        "edit_status must not emit a NotificationEvent"
    );

    service
        .delete_status(author, status.id)
        .await
        .expect("delete_status must succeed");
    assert_eq!(
        notifications.events().len(),
        0,
        "delete_status must not emit a NotificationEvent"
    );

    db.cleanup().await;
}

// -- runtime sanity: deterministic seed is exercised (no behavior asserted) --

#[test]
fn deterministic_seed_constant_is_reachable() {
    // Smoke-checks this test module's own imports compile; the real
    // determinism guarantee is `RuntimeContext::deterministic`'s own test in
    // `runtime.rs`.
    let _ = DeterministicSeed::new(1);
}

// --- extraction_tests: pure, no DB ------------------------------------------

mod extraction_tests {
    use super::super::extract_content_tokens;

    #[test]
    fn extracts_a_bare_local_mention() {
        let extracted = extract_content_tokens("hello @alice how are you");
        assert_eq!(extracted.mentions.len(), 1);
        assert_eq!(extracted.mentions[0].local, "alice");
        assert_eq!(extracted.mentions[0].domain, None);
    }

    #[test]
    fn extracts_a_mention_with_domain() {
        let extracted = extract_content_tokens("cc @bob@remote.example ping");
        assert_eq!(extracted.mentions.len(), 1);
        assert_eq!(extracted.mentions[0].local, "bob");
        assert_eq!(
            extracted.mentions[0].domain.as_deref(),
            Some("remote.example")
        );
    }

    #[test]
    fn does_not_match_an_email_address_embedded_mid_word() {
        let extracted = extract_content_tokens("contact me at foo@example.com please");
        assert!(
            extracted.mentions.is_empty(),
            "an '@' preceded by an alphanumeric character must not be treated as a mention start"
        );
    }

    #[test]
    fn extracts_hashtags_lowercased_and_deduplicated() {
        let extracted = extract_content_tokens("#Rust #rust #WebDev");
        assert_eq!(
            extracted.hashtags,
            vec!["rust".to_string(), "webdev".to_string()]
        );
    }

    #[test]
    fn extracts_emoji_shortcodes() {
        let extracted = extract_content_tokens("nice work :blobcat: :+1 not-this: :blobcat:");
        assert_eq!(extracted.emoji_shortcodes, vec!["blobcat".to_string()]);
    }

    #[test]
    fn does_not_match_a_time_like_colon_pair() {
        let extracted = extract_content_tokens("meet at 10:30 sharp");
        assert!(
            extracted.emoji_shortcodes.is_empty(),
            "a ':' preceded by an alphanumeric character must not start an emoji shortcode scan"
        );
    }

    #[test]
    fn mentions_hashtags_and_emoji_extracted_together_in_one_pass() {
        let extracted = extract_content_tokens("@alice check out #rust :blobcat:");
        assert_eq!(extracted.mentions.len(), 1);
        assert_eq!(extracted.hashtags, vec!["rust".to_string()]);
        assert_eq!(extracted.emoji_shortcodes, vec!["blobcat".to_string()]);
    }

    #[test]
    fn empty_content_extracts_nothing() {
        let extracted = extract_content_tokens("");
        assert!(extracted.mentions.is_empty());
        assert!(extracted.hashtags.is_empty());
        assert!(extracted.emoji_shortcodes.is_empty());
    }
}
