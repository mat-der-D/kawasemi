//! DB-backed tests for `FollowRequestService` (Requirements 2.2, 2.3, 2.4),
//! per task 3.2's own observable completion condition: "ロック済みアクター宛
//! フォローが保留として一覧に現れ、承認でフォロー確立 + Accept 配送、拒否で
//! 削除 + Reject 配送が起こる状態".
//!
//! Mirrors `follow_service/tests.rs`'s established conventions
//! (`crate::test_harness::spawn_test_app`, a `RecordingSink` `DeliverySink`
//! double capturing every dispatched Activity, real `actor`/
//! `remote_accounts` rows) closely — see that module's own doc comment for
//! the base rationale this file does not repeat. This service only ever
//! deals with **inbound**-direction pending requests, which (per
//! `transitions.rs`'s own documented convention) are only ever recorded with
//! a `Remote` `requester`/`Local` `target` in production — so every fixture
//! here seeds a pending row directly via `Transitions::record_pending`
//! (mirroring `transitions/tests.rs`'s own established fixture technique)
//! rather than trying to reach this state through `FollowService::follow`
//! (task 3.1's own territory, and structurally incapable of producing an
//! `Inbound`-direction row at all).

use std::sync::{Arc, Mutex};

use axum::http::StatusCode;

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
use crate::social_graph::model::FollowRequest;
use crate::statuses::notification_sink::NotificationSinkRegistry;
use crate::test_harness::{TestApp, spawn_test_app};
use time::OffsetDateTime;

// --- Test fixtures ----------------------------------------------------------

/// Creates a real owner + local actor row, returning the actor's `Id` — an
/// exact copy of `follow_service/tests.rs::create_test_actor`.
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

fn sample_remote_account(
    id: Id,
    actor_uri: &str,
    username: &str,
    fetched_at: OffsetDateTime,
) -> RemoteAccount {
    RemoteAccount {
        id,
        actor_uri: actor_uri.to_string(),
        username: username.to_string(),
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
async fn create_test_remote(app: &TestApp, actor_uri: &str, username: &str) -> Id {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    upsert_remote(
        &app.pool,
        &sample_remote_account(id, actor_uri, username, now),
    )
    .await
    .expect("upsert_remote must succeed");
    id
}

/// Records a pending **inbound** follow request from `requester` to `target`
/// directly via `Transitions::record_pending` — the only way this crate can
/// currently reach this state (task 4.1's `InboundHandler`, which would
/// normally produce this row from a received Follow Activity, does not exist
/// yet). Mirrors `transitions/tests.rs`'s own established fixture technique.
async fn record_inbound_request(
    app: &TestApp,
    requester: AccountRef,
    target: AccountRef,
    activity_id: &str,
) {
    let transitions = Transitions::new(
        app.pool.clone(),
        app.runtime.clone(),
        NotificationSinkRegistry::new(),
    );
    let req = FollowRequest {
        requester,
        target,
        direction: crate::social_graph::model::FollowRequestDirection::Inbound,
        activity_id: activity_id.to_string(),
        created_at: app.runtime.clock.now(),
    };
    transitions
        .record_pending(&req)
        .await
        .expect("record_pending must succeed");
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

type TestService = FollowRequestService<
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
    let ids = Arc::new(SeqIdGenerator::new(90_000)) as Arc<dyn crate::runtime::IdGenerator>;
    let activity_builder = ActivityBuilder::new(
        urls,
        ids,
        ActorDirectory::new(app.pool.clone()),
        PgRemoteActorLookup::new(app.pool.clone()),
    );

    let service = FollowRequestService::new(
        app.pool.clone(),
        app.runtime.clone(),
        ActorDirectory::new(app.pool.clone()),
        activity_builder,
        Transitions::new(
            app.pool.clone(),
            app.runtime.clone(),
            NotificationSinkRegistry::new(),
        ),
        Arc::new(delivery),
        app.state.accounts().service(),
        app.state.config().server.domain.clone(),
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

// --- list_requests -----------------------------------------------------------

#[tokio::test]
async fn list_requests_is_empty_when_no_pending_requests() {
    let app = spawn_test_app().await;
    let (service, _local_sink, _http_sink) = build_service(&app);

    let owner = create_test_actor(&app, "owner1").await;

    let page = service
        .list_requests(owner, PageParams::default())
        .await
        .expect("list_requests must succeed");

    assert!(page.items.is_empty());
}

#[tokio::test]
async fn list_requests_returns_the_pending_requesters_account_json() {
    let app = spawn_test_app().await;
    let (service, _local_sink, _http_sink) = build_service(&app);

    let owner = create_test_actor(&app, "owner2").await;
    let requester = create_test_remote(&app, "https://remote.example/users/alice", "alice").await;

    record_inbound_request(
        &app,
        AccountRef::Remote(requester),
        AccountRef::Local(owner),
        "https://remote.example/acts/follow-1",
    )
    .await;

    let page = service
        .list_requests(owner, PageParams::default())
        .await
        .expect("list_requests must succeed");

    assert_eq!(page.items.len(), 1);
    let account = &page.items[0];
    assert_eq!(
        account.get("username").and_then(|v| v.as_str()),
        Some("alice")
    );
    assert_eq!(
        account.get("id").and_then(|v| v.as_str()),
        Some(requester.as_i64().to_string()).as_deref()
    );
}

#[tokio::test]
async fn list_requests_paginates_with_the_given_limit() {
    let app = spawn_test_app().await;
    let (service, _local_sink, _http_sink) = build_service(&app);

    let owner = create_test_actor(&app, "owner3").await;
    let requester1 = create_test_remote(&app, "https://remote.example/users/bob", "bob").await;
    let requester2 = create_test_remote(&app, "https://remote.example/users/carol", "carol").await;

    record_inbound_request(
        &app,
        AccountRef::Remote(requester1),
        AccountRef::Local(owner),
        "https://remote.example/acts/follow-2",
    )
    .await;
    record_inbound_request(
        &app,
        AccountRef::Remote(requester2),
        AccountRef::Local(owner),
        "https://remote.example/acts/follow-3",
    )
    .await;

    let page = service
        .list_requests(
            owner,
            PageParams {
                limit: Some(1),
                ..Default::default()
            },
        )
        .await
        .expect("list_requests must succeed");

    assert_eq!(page.items.len(), 1);
    assert!(
        page.next_cursor.is_some(),
        "a second page must still be available"
    );
    // Newest-first ordering (`repository::list_inbound_requests`'s own `ORDER
    // BY id DESC`): `requester2`'s row was recorded after `requester1`'s, so
    // it has the higher id and appears first.
    assert_eq!(
        page.items[0].get("username").and_then(|v| v.as_str()),
        Some("carol")
    );
}

// --- authorize_request --------------------------------------------------------

#[tokio::test]
async fn authorize_request_establishes_follow_and_delivers_accept_to_a_remote_requester() {
    let app = spawn_test_app().await;
    let (service, _local_sink, http_sink) = build_service(&app);

    let owner = create_test_actor(&app, "owner4").await;
    let requester = create_test_remote(&app, "https://remote.example/users/dave", "dave").await;

    record_inbound_request(
        &app,
        AccountRef::Remote(requester),
        AccountRef::Local(owner),
        "https://remote.example/acts/follow-4",
    )
    .await;

    let relationship = service
        .authorize_request(owner, &requester.as_i64().to_string())
        .await
        .expect("authorize_request must succeed");

    // Viewer is `owner`: `owner` did not follow `requester`, but `requester`
    // now follows `owner` -- so `following` is false and `followed_by` is
    // true.
    assert!(!as_bool(&relationship, "following"));
    assert!(as_bool(&relationship, "followed_by"));

    assert_eq!(http_sink.calls().len(), 1);
    let (delivery_target, activity, _) = &http_sink.calls()[0];
    assert_eq!(activity.parsed().activity_type, "Accept");
    match delivery_target {
        DeliveryTarget::Remote { inbox } => {
            assert_eq!(inbox, "https://remote.example/users/dave/inbox");
        }
        DeliveryTarget::Local { .. } => panic!("expected a remote delivery target"),
    }

    // The pending request must be consumed -- a repeat authorize must now
    // 404.
    let err = service
        .authorize_request(owner, &requester.as_i64().to_string())
        .await
        .expect_err("a repeat authorize must fail: nothing left to authorize");
    assert_eq!(err.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn authorize_request_delivers_locally_when_the_requester_is_local() {
    // Regression/symmetry test for `resolve_requester`'s local branch and
    // the common `DeliveryService` path's local-recipient dispatch.
    // `Transitions::promote_pending`/`drop_pending` derive the pending row's
    // direction purely from `requester`'s own `AccountRef` variant
    // (`pending_direction_for`, task 2.2): a `Local` requester's pending row
    // is *always* interpreted as `Outbound`, never `Inbound`, regardless of
    // what direction was actually written -- so, unlike every other fixture
    // in this file, this one records the row as `Outbound` (not `Inbound`)
    // to reach a state `authorize_request` can actually consume. This is a
    // structurally unusual case for `FollowRequestService` (in production,
    // `FollowRequestService` only ever consumes `Remote`-requester rows --
    // see this file's own module doc comment), but it is the only way to
    // exercise `resolve_requester`'s `AccountRef::Local` branch and the
    // local in-process delivery path at all, mirroring
    // `follow_service/tests.rs`'s own local/remote delivery-symmetry
    // coverage.
    let app = spawn_test_app().await;
    let (service, local_sink, http_sink) = build_service(&app);

    let owner = create_test_actor(&app, "owner5").await;
    let requester = create_test_actor(&app, "requester5").await;

    let transitions = Transitions::new(
        app.pool.clone(),
        app.runtime.clone(),
        NotificationSinkRegistry::new(),
    );
    transitions
        .record_pending(&FollowRequest {
            requester: AccountRef::Local(requester),
            target: AccountRef::Local(owner),
            direction: crate::social_graph::model::FollowRequestDirection::Outbound,
            activity_id: "https://kawasemi.example/acts/follow-5".to_string(),
            created_at: app.runtime.clock.now(),
        })
        .await
        .expect("record_pending must succeed");

    let relationship = service
        .authorize_request(owner, &requester.as_i64().to_string())
        .await
        .expect("authorize_request must succeed");

    assert!(as_bool(&relationship, "followed_by"));
    assert_eq!(local_sink.calls().len(), 1);
    assert_eq!(http_sink.calls().len(), 0);
    let (_, activity, _) = &local_sink.calls()[0];
    assert_eq!(activity.parsed().activity_type, "Accept");
}

#[tokio::test]
async fn authorize_request_returns_not_found_when_no_pending_request_exists() {
    let app = spawn_test_app().await;
    let (service, _local_sink, http_sink) = build_service(&app);

    let owner = create_test_actor(&app, "owner6").await;
    let requester = create_test_remote(&app, "https://remote.example/users/erin", "erin").await;

    let err = service
        .authorize_request(owner, &requester.as_i64().to_string())
        .await
        .expect_err("authorizing a nonexistent pending request must fail");

    assert_eq!(err.kind, ErrorKind::Client);
    assert_eq!(err.status, StatusCode::NOT_FOUND);
    assert_eq!(http_sink.calls().len(), 0);
}

// --- reject_request ------------------------------------------------------------

#[tokio::test]
async fn reject_request_drops_the_pending_request_and_delivers_reject() {
    let app = spawn_test_app().await;
    let (service, _local_sink, http_sink) = build_service(&app);

    let owner = create_test_actor(&app, "owner7").await;
    let requester = create_test_remote(&app, "https://remote.example/users/frank", "frank").await;

    record_inbound_request(
        &app,
        AccountRef::Remote(requester),
        AccountRef::Local(owner),
        "https://remote.example/acts/follow-7",
    )
    .await;

    let relationship = service
        .reject_request(owner, &requester.as_i64().to_string())
        .await
        .expect("reject_request must succeed");

    assert!(!as_bool(&relationship, "following"));
    assert!(!as_bool(&relationship, "followed_by"));

    assert_eq!(http_sink.calls().len(), 1);
    let (_, activity, _) = &http_sink.calls()[0];
    assert_eq!(activity.parsed().activity_type, "Reject");

    // Idempotency check: nothing pending is left, a repeat reject must 404.
    let err = service
        .reject_request(owner, &requester.as_i64().to_string())
        .await
        .expect_err("a repeat reject must fail: nothing left to reject");
    assert_eq!(err.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn reject_request_returns_not_found_when_no_pending_request_exists() {
    let app = spawn_test_app().await;
    let (service, _local_sink, http_sink) = build_service(&app);

    let owner = create_test_actor(&app, "owner8").await;
    let requester = create_test_remote(&app, "https://remote.example/users/grace", "grace").await;

    let err = service
        .reject_request(owner, &requester.as_i64().to_string())
        .await
        .expect_err("rejecting a nonexistent pending request must fail");

    assert_eq!(err.kind, ErrorKind::Client);
    assert_eq!(err.status, StatusCode::NOT_FOUND);
    assert_eq!(http_sink.calls().len(), 0);
}
