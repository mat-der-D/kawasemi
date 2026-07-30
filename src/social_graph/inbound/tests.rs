//! DB-backed tests for `SocialGraphInboundHandler` (Requirements 7.1-7.7,
//! 2.5, 2.6, 3.2), per task 4.1's own observable completion condition:
//! "受信 Follow がロック有無で確立/保留に分かれ、受信 Accept で送信中リクエス
//! トが確立、受信 Block で被ブロック + 関係解消、受信 Undo で逆操作が起こり、
//! 再受信で状態が二重変更されない状態".
//!
//! Mirrors `follow_service/tests.rs`'s established convention
//! (`crate::test_harness::spawn_test_app`, `create_test_actor`/
//! `create_test_remote` real-row fixtures, a `RecordingSink` `DeliverySink`
//! double). Activities fed to `handle()` are built via a *real*
//! `ActivityBuilder` (the same production Follow/Accept/Block/Undo JSON
//! shapes `activity_builder.rs` emits), mirroring this task's own brief
//! ("for symmetry: local in-process delivery presumably calls this handler
//! with a JSON activity shaped like what `build_undo` emits").
//!
//! `ActorUriResolver` is a small in-memory [`FakeActorUriResolver`] (mirrors
//! `statuses::poll_service/tests.rs::MockActorLookup`'s precedent, cited by
//! `statuses::inbound_handlers.rs`'s own doc comment) rather than
//! `ProdActorUriResolver`: these are unit/behavioral tests of this handler's
//! own state-transition logic, not of `ProdActorUriResolver`'s real-network
//! remote-fetch path (which needs a live HTTP peer, out of this task's own
//! boundary -- `tests/inbound_activities_it.rs`, design.md's Testing
//! Strategy file list, is the eventual full-federation integration
//! counterpart).

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use axum::http::StatusCode;

use super::*;
use crate::accounts::model::{ProfileField, ProfilePatch, RemoteAccount};
use crate::accounts::profile_repository::upsert_profile;
use crate::accounts::remote_repository::upsert_remote;
use crate::actor::owner::create_owner;
use crate::actor::repository::insert_actor;
use crate::actor::{ActorDirectory, ActorState, ActorType, Handle};
use crate::domain::Id;
use crate::federation::outbound::target::RecipientTargetResolver;
use crate::federation::signatures::VerifiedSigner;
use crate::federation::{CanonicalActivity, DeliveryService, DeliverySink, DeliveryTarget};
use crate::runtime::SeqIdGenerator;
use crate::social_graph::activity_builder::PgRemoteActorLookup;
use crate::social_graph::model::FollowRequestDirection;
use crate::social_graph::repository as sg_repository;
use crate::statuses::notification_sink::NotificationSinkRegistry;
use crate::test_harness::{TestApp, spawn_test_app};

const TEST_DOMAIN: &str = "kawasemi.example";

// --- Test fixtures ----------------------------------------------------------

/// Creates a real owner + local actor row, returning the actor's `Id` --
/// mirrors `follow_service/tests.rs::create_test_actor` exactly.
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

/// Marks `actor_id`'s profile as locked (manually-approves-followers) --
/// mirrors `follow_service/tests.rs::lock_actor` exactly.
async fn lock_actor(app: &TestApp, actor_id: Id) {
    upsert_profile(
        &app.pool,
        actor_id,
        ProfilePatch {
            locked: Some(true),
            ..Default::default()
        },
        app.runtime.clock.now(),
    )
    .await
    .expect("locking the test actor's profile must succeed");
}

fn sample_remote_account(id: Id, actor_uri: &str, locked: bool) -> RemoteAccount {
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
        fetched_at: time::OffsetDateTime::now_utc(),
    }
}

/// Creates a real `remote_accounts` row, returning its `Id`.
async fn create_test_remote(app: &TestApp, actor_uri: &str, locked: bool) -> Id {
    let id = app.runtime.ids.next_id();
    upsert_remote(&app.pool, &sample_remote_account(id, actor_uri, locked))
        .await
        .expect("upsert_remote must succeed");
    id
}

fn actor_url(handle: &str) -> String {
    format!("https://{TEST_DOMAIN}/users/{handle}")
}

/// An in-memory [`ActorUriResolver`] test double -- see this module's own
/// doc comment for why a fake, not `ProdActorUriResolver`, is used here.
#[derive(Clone, Default)]
struct FakeActorUriResolver {
    map: Arc<Mutex<HashMap<String, AccountRef>>>,
}

impl FakeActorUriResolver {
    fn new() -> Self {
        Self::default()
    }

    fn with(self, uri: impl Into<String>, account: AccountRef) -> Self {
        self.map.lock().unwrap().insert(uri.into(), account);
        self
    }
}

impl ActorUriResolver for FakeActorUriResolver {
    fn resolve_account_ref(
        &self,
        actor_uri: &str,
    ) -> impl Future<Output = Result<AccountRef, AppError>> + Send {
        let result = self
            .map
            .lock()
            .unwrap()
            .get(actor_uri)
            .copied()
            .ok_or_else(|| {
                AppError::client(
                    StatusCode::NOT_FOUND,
                    format!("unknown actor uri '{actor_uri}' in FakeActorUriResolver"),
                )
            });
        async move { result }
    }
}

/// A `DeliverySink` double recording every dispatched Activity -- mirrors
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

type TestHandler =
    SocialGraphInboundHandler<ActorDirectory, PgRemoteActorLookup, FakeActorUriResolver>;

type TestActivityBuilder = ActivityBuilder<ActorDirectory, PgRemoteActorLookup>;

fn build_activity_builder(app: &TestApp, seed: i64) -> TestActivityBuilder {
    let urls = crate::federation::urls::ActorUrls::new(TEST_DOMAIN);
    let ids = Arc::new(SeqIdGenerator::new(seed)) as Arc<dyn crate::runtime::IdGenerator>;
    ActivityBuilder::new(
        urls,
        ids,
        ActorDirectory::new(app.pool.clone()),
        PgRemoteActorLookup::new(app.pool.clone()),
    )
}

/// Builds a `BoxedDeliver` wrapping a concrete, test-only `DeliveryService`
/// (`ActorDirectory` + `Arc<RecordingSink>` local/http sinks) -- this
/// function is deliberately *not* generic (see `inbound.rs`'s own doc
/// comment, "`deliver: BoxedDeliver`": the type-erasure closure must be built
/// at a call site where the `DeliveryService`'s own type parameters are
/// already fully concrete, never inside a function still generic over them).
fn build_boxed_deliver(
    app: &TestApp,
    local_sink: Arc<RecordingSink>,
    http_sink: Arc<RecordingSink>,
) -> BoxedDeliver {
    let delivery = Arc::new(DeliveryService::new(
        RecipientTargetResolver::new(ActorDirectory::new(app.pool.clone())),
        local_sink,
        http_sink,
    ));
    Arc::new(move |req: DeliveryRequest| {
        let delivery = Arc::clone(&delivery);
        Box::pin(async move { delivery.deliver(req).await })
            as std::pin::Pin<Box<dyn Future<Output = Result<(), AppError>> + Send>>
    })
}

fn build_handler(
    app: &TestApp,
    actor_uris: FakeActorUriResolver,
    seed: i64,
) -> (TestHandler, Arc<RecordingSink>, Arc<RecordingSink>) {
    let local_sink = Arc::new(RecordingSink::new());
    let http_sink = Arc::new(RecordingSink::new());
    let deliver = build_boxed_deliver(app, Arc::clone(&local_sink), Arc::clone(&http_sink));

    let handler = SocialGraphInboundHandler::new(
        app.pool.clone(),
        app.runtime.clone(),
        ActorDirectory::new(app.pool.clone()),
        build_activity_builder(app, seed),
        Transitions::new(
            app.pool.clone(),
            app.runtime.clone(),
            NotificationSinkRegistry::new(),
        ),
        deliver,
        actor_uris,
    );
    (handler, local_sink, http_sink)
}

fn signer(actor_uri: &str) -> InboundContext {
    InboundContext {
        signer: VerifiedSigner {
            key_id: format!("{actor_uri}#main-key"),
            actor_uri: actor_uri.to_string(),
        },
    }
}

async fn load_state(
    app: &TestApp,
    viewer: AccountRef,
    target: AccountRef,
) -> sg_repository::RelationshipState {
    let now = app.runtime.clock.now();
    let mut states =
        sg_repository::load_states(&app.pool, &viewer, std::slice::from_ref(&target), now)
            .await
            .expect("load_states must succeed");
    states.pop().expect("exactly one state")
}

// --- activity_types ----------------------------------------------------------

#[tokio::test]
async fn activity_types_names_all_five_owned_outer_types() {
    let app = spawn_test_app().await;
    let (handler, _local, _http) = build_handler(&app, FakeActorUriResolver::new(), 1_000);
    assert_eq!(
        handler.activity_types(),
        &["Follow", "Accept", "Reject", "Block", "Undo"]
    );
}

// --- Follow: establish (unlocked local target, remote source) ---------------

#[tokio::test]
async fn inbound_follow_establishes_for_unlocked_local_target_and_delivers_accept() {
    let app = spawn_test_app().await;

    let target_id = create_test_actor(&app, "target_unlocked").await;
    let target_url = actor_url("target_unlocked");
    let remote_uri = "https://remote.example/users/alice";
    let remote_id = create_test_remote(&app, remote_uri, false).await;

    let resolver = FakeActorUriResolver::new()
        .with(target_url.clone(), AccountRef::Local(target_id))
        .with(remote_uri, AccountRef::Remote(remote_id));
    let (handler, local_sink, http_sink) = build_handler(&app, resolver, 2_000);

    let builder = build_activity_builder(&app, 2_100);
    let (activity_id, follow_json) = builder
        .build_follow(
            &AccountRef::Remote(remote_id),
            &AccountRef::Local(target_id),
        )
        .await
        .expect("build_follow must succeed");

    let activity = ParsedActivity {
        id: activity_id,
        activity_type: "Follow".to_string(),
        raw: follow_json,
    };
    let ctx = signer(remote_uri);

    let outcome = handler
        .handle(&activity, &ctx)
        .await
        .expect("handling an inbound Follow must succeed");
    assert_eq!(outcome, HandleOutcome::Handled);

    let state = load_state(
        &app,
        AccountRef::Local(target_id),
        AccountRef::Remote(remote_id),
    )
    .await;
    assert!(state.followed_by, "the remote actor must now follow us");
    assert!(!state.requested_by, "no pending request should remain");

    // Requirement 7.2: Accept(Follow) delivered back to the remote source
    // via the common DeliveryService path.
    assert_eq!(local_sink.calls().len(), 0);
    assert_eq!(http_sink.calls().len(), 1);
    let (_, accept, _) = &http_sink.calls()[0];
    assert_eq!(accept.parsed().activity_type, "Accept");
}

// --- Follow: pending (locked local target, remote source) -------------------

#[tokio::test]
async fn inbound_follow_records_pending_for_locked_local_target_and_sends_no_accept() {
    let app = spawn_test_app().await;

    let target_id = create_test_actor(&app, "target_locked").await;
    lock_actor(&app, target_id).await;
    let target_url = actor_url("target_locked");
    let remote_uri = "https://remote.example/users/bob";
    let remote_id = create_test_remote(&app, remote_uri, false).await;

    let resolver = FakeActorUriResolver::new()
        .with(target_url, AccountRef::Local(target_id))
        .with(remote_uri, AccountRef::Remote(remote_id));
    let (handler, local_sink, http_sink) = build_handler(&app, resolver, 3_000);

    let builder = build_activity_builder(&app, 3_100);
    let (activity_id, follow_json) = builder
        .build_follow(
            &AccountRef::Remote(remote_id),
            &AccountRef::Local(target_id),
        )
        .await
        .expect("build_follow must succeed");
    let activity = ParsedActivity {
        id: activity_id,
        activity_type: "Follow".to_string(),
        raw: follow_json,
    };
    let ctx = signer(remote_uri);

    let outcome = handler
        .handle(&activity, &ctx)
        .await
        .expect("handling an inbound Follow must succeed");
    assert_eq!(outcome, HandleOutcome::Handled);

    let state = load_state(
        &app,
        AccountRef::Local(target_id),
        AccountRef::Remote(remote_id),
    )
    .await;
    assert!(!state.followed_by, "must not be established yet");
    assert!(state.requested_by, "must be recorded as pending inbound");

    assert_eq!(local_sink.calls().len(), 0);
    assert_eq!(http_sink.calls().len(), 0, "no Accept sent while pending");
}

// --- Follow: same-server privilege (locked local target, local source) ------

#[tokio::test]
async fn inbound_follow_establishes_immediately_for_locked_local_target_same_server_privilege() {
    let app = spawn_test_app().await;

    let target_id = create_test_actor(&app, "target_ss").await;
    lock_actor(&app, target_id).await;
    let target_url = actor_url("target_ss");
    let source_id = create_test_actor(&app, "source_ss").await;
    let source_url = actor_url("source_ss");

    let resolver = FakeActorUriResolver::new()
        .with(target_url, AccountRef::Local(target_id))
        .with(source_url.clone(), AccountRef::Local(source_id));
    let (handler, local_sink, http_sink) = build_handler(&app, resolver, 4_000);

    let builder = build_activity_builder(&app, 4_100);
    let (activity_id, follow_json) = builder
        .build_follow(&AccountRef::Local(source_id), &AccountRef::Local(target_id))
        .await
        .expect("build_follow must succeed");
    let activity = ParsedActivity {
        id: activity_id,
        activity_type: "Follow".to_string(),
        raw: follow_json,
    };
    let ctx = signer(&source_url);

    let outcome = handler
        .handle(&activity, &ctx)
        .await
        .expect("handling an inbound Follow must succeed");
    assert_eq!(outcome, HandleOutcome::Handled);

    let state = load_state(
        &app,
        AccountRef::Local(target_id),
        AccountRef::Local(source_id),
    )
    .await;
    assert!(
        state.followed_by,
        "Requirement 3.1/3.2: locked target must still establish immediately for a same-server source"
    );

    // Accept is delivered in-process to the (local) source.
    assert_eq!(local_sink.calls().len(), 1);
    assert_eq!(http_sink.calls().len(), 0);
}

// --- Follow: not addressed to us --------------------------------------------

#[tokio::test]
async fn inbound_follow_is_ignored_when_object_does_not_resolve_locally() {
    let app = spawn_test_app().await;

    let other_remote_uri = "https://other.example/users/carol";
    let other_remote_id = create_test_remote(&app, other_remote_uri, false).await;
    let remote_uri = "https://remote.example/users/dave";
    let remote_id = create_test_remote(&app, remote_uri, false).await;

    let resolver = FakeActorUriResolver::new()
        .with(other_remote_uri, AccountRef::Remote(other_remote_id))
        .with(remote_uri, AccountRef::Remote(remote_id));
    let (handler, _local, _http) = build_handler(&app, resolver, 5_000);

    let activity = ParsedActivity {
        id: "https://remote.example/activities/follow/1".to_string(),
        activity_type: "Follow".to_string(),
        raw: serde_json::json!({
            "id": "https://remote.example/activities/follow/1",
            "type": "Follow",
            "actor": remote_uri,
            "object": other_remote_uri,
        }),
    };
    let ctx = signer(remote_uri);

    let outcome = handler
        .handle(&activity, &ctx)
        .await
        .expect("handling must succeed");
    assert_eq!(outcome, HandleOutcome::Ignored);
}

// --- Follow: idempotent re-receipt ------------------------------------------

#[tokio::test]
async fn inbound_follow_received_twice_does_not_double_apply_or_redeliver_accept() {
    let app = spawn_test_app().await;

    let target_id = create_test_actor(&app, "target_dup").await;
    let target_url = actor_url("target_dup");
    let remote_uri = "https://remote.example/users/eve";
    let remote_id = create_test_remote(&app, remote_uri, false).await;

    let resolver = FakeActorUriResolver::new()
        .with(target_url, AccountRef::Local(target_id))
        .with(remote_uri, AccountRef::Remote(remote_id));
    let (handler, _local_sink, http_sink) = build_handler(&app, resolver, 6_000);

    let builder = build_activity_builder(&app, 6_100);

    for i in 0..2 {
        let (activity_id, follow_json) = builder
            .build_follow(
                &AccountRef::Remote(remote_id),
                &AccountRef::Local(target_id),
            )
            .await
            .expect("build_follow must succeed");
        let activity = ParsedActivity {
            id: format!("{activity_id}-{i}"),
            activity_type: "Follow".to_string(),
            raw: follow_json,
        };
        let ctx = signer(remote_uri);
        let outcome = handler
            .handle(&activity, &ctx)
            .await
            .expect("handling an inbound Follow must succeed");
        assert_eq!(outcome, HandleOutcome::Handled);
    }

    let state = load_state(
        &app,
        AccountRef::Local(target_id),
        AccountRef::Remote(remote_id),
    )
    .await;
    assert!(state.followed_by);

    // Requirement 7.7: a second Accept must not be built/delivered for the
    // already-established pair.
    assert_eq!(http_sink.calls().len(), 1);

    let follow_rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM follows WHERE follower_kind = 'remote' AND follower_id = $1 \
             AND followee_kind = 'local' AND followee_id = $2",
    )
    .bind(remote_id.as_i64())
    .bind(target_id.as_i64())
    .fetch_one(&app.pool)
    .await
    .expect("counting follows must succeed");
    assert_eq!(follow_rows, 1, "no duplicate follow row");
}

// --- Accept ------------------------------------------------------------------

#[tokio::test]
async fn inbound_accept_promotes_our_outbound_pending_request_to_established() {
    let app = spawn_test_app().await;

    let requester_id = create_test_actor(&app, "requester_accept").await;
    let requester_url = actor_url("requester_accept");
    let remote_uri = "https://remote.example/users/frank";
    let remote_id = create_test_remote(&app, remote_uri, false).await;

    let resolver = FakeActorUriResolver::new()
        .with(requester_url.clone(), AccountRef::Local(requester_id))
        .with(remote_uri, AccountRef::Remote(remote_id));
    let (handler, _local, _http) = build_handler(&app, resolver, 7_000);

    // Seed an outbound pending FollowRequest (as `FollowService::follow`
    // would have already recorded before this Accept arrives).
    let now = app.runtime.clock.now();
    sg_repository::upsert_request(
        &app.pool,
        app.runtime.ids.next_id(),
        &crate::social_graph::model::FollowRequest {
            requester: AccountRef::Local(requester_id),
            target: AccountRef::Remote(remote_id),
            direction: FollowRequestDirection::Outbound,
            activity_id: "https://kawasemi.example/activities/follow/orig".to_string(),
            created_at: now,
        },
    )
    .await
    .expect("seeding the pending request must succeed");

    let builder = build_activity_builder(&app, 7_100);
    let accept_json = builder
        .build_accept(
            &AccountRef::Remote(remote_id),
            "https://kawasemi.example/activities/follow/orig",
            &AccountRef::Local(requester_id),
        )
        .await
        .expect("build_accept must succeed");

    let activity = ParsedActivity {
        id: "https://remote.example/activities/accept/1".to_string(),
        activity_type: "Accept".to_string(),
        raw: accept_json,
    };
    let ctx = signer(remote_uri);

    let outcome = handler
        .handle(&activity, &ctx)
        .await
        .expect("handling an inbound Accept must succeed");
    assert_eq!(outcome, HandleOutcome::Handled);

    let state = load_state(
        &app,
        AccountRef::Local(requester_id),
        AccountRef::Remote(remote_id),
    )
    .await;
    assert!(state.follow.is_some(), "the follow must now be established");
    assert!(!state.requested, "the pending request must be consumed");
}

#[tokio::test]
async fn inbound_accept_with_no_matching_pending_request_is_an_idempotent_no_op() {
    let app = spawn_test_app().await;

    let requester_id = create_test_actor(&app, "requester_noop").await;
    let requester_url = actor_url("requester_noop");
    let remote_uri = "https://remote.example/users/gina";
    let remote_id = create_test_remote(&app, remote_uri, false).await;

    let resolver = FakeActorUriResolver::new()
        .with(requester_url, AccountRef::Local(requester_id))
        .with(remote_uri, AccountRef::Remote(remote_id));
    let (handler, _local, _http) = build_handler(&app, resolver, 7_500);

    let builder = build_activity_builder(&app, 7_600);
    let accept_json = builder
        .build_accept(
            &AccountRef::Remote(remote_id),
            "https://kawasemi.example/activities/follow/never-sent",
            &AccountRef::Local(requester_id),
        )
        .await
        .expect("build_accept must succeed");
    let activity = ParsedActivity {
        id: "https://remote.example/activities/accept/2".to_string(),
        activity_type: "Accept".to_string(),
        raw: accept_json,
    };
    let ctx = signer(remote_uri);

    let outcome = handler
        .handle(&activity, &ctx)
        .await
        .expect("handling an inbound Accept with nothing pending must still succeed");
    assert_eq!(outcome, HandleOutcome::Handled);

    let state = load_state(
        &app,
        AccountRef::Local(requester_id),
        AccountRef::Remote(remote_id),
    )
    .await;
    assert!(state.follow.is_none());
}

// --- Reject -------------------------------------------------------------------

#[tokio::test]
async fn inbound_reject_drops_our_outbound_pending_request_without_establishing() {
    let app = spawn_test_app().await;

    let requester_id = create_test_actor(&app, "requester_reject").await;
    let requester_url = actor_url("requester_reject");
    let remote_uri = "https://remote.example/users/hank";
    let remote_id = create_test_remote(&app, remote_uri, false).await;

    let resolver = FakeActorUriResolver::new()
        .with(requester_url, AccountRef::Local(requester_id))
        .with(remote_uri, AccountRef::Remote(remote_id));
    let (handler, _local, _http) = build_handler(&app, resolver, 8_000);

    let now = app.runtime.clock.now();
    sg_repository::upsert_request(
        &app.pool,
        app.runtime.ids.next_id(),
        &crate::social_graph::model::FollowRequest {
            requester: AccountRef::Local(requester_id),
            target: AccountRef::Remote(remote_id),
            direction: FollowRequestDirection::Outbound,
            activity_id: "https://kawasemi.example/activities/follow/orig2".to_string(),
            created_at: now,
        },
    )
    .await
    .expect("seeding the pending request must succeed");

    let builder = build_activity_builder(&app, 8_100);
    let reject_json = builder
        .build_reject(
            &AccountRef::Remote(remote_id),
            "https://kawasemi.example/activities/follow/orig2",
            &AccountRef::Local(requester_id),
        )
        .await
        .expect("build_reject must succeed");
    let activity = ParsedActivity {
        id: "https://remote.example/activities/reject/1".to_string(),
        activity_type: "Reject".to_string(),
        raw: reject_json,
    };
    let ctx = signer(remote_uri);

    let outcome = handler
        .handle(&activity, &ctx)
        .await
        .expect("handling an inbound Reject must succeed");
    assert_eq!(outcome, HandleOutcome::Handled);

    let state = load_state(
        &app,
        AccountRef::Local(requester_id),
        AccountRef::Remote(remote_id),
    )
    .await;
    assert!(state.follow.is_none(), "no follow must be established");
    assert!(!state.requested, "the pending request must be dropped");
}

// --- Block ---------------------------------------------------------------------

#[tokio::test]
async fn inbound_block_marks_blocked_by_and_clears_existing_follows() {
    let app = spawn_test_app().await;

    let target_id = create_test_actor(&app, "target_block").await;
    let target_url = actor_url("target_block");
    let remote_uri = "https://remote.example/users/ivan";
    let remote_id = create_test_remote(&app, remote_uri, false).await;

    let resolver = FakeActorUriResolver::new()
        .with(target_url, AccountRef::Local(target_id))
        .with(remote_uri, AccountRef::Remote(remote_id));
    let (handler, _local, _http) = build_handler(&app, resolver, 9_000);

    // Seed a pre-existing follow both directions so this Block's
    // relationship-clearing side effect is observable.
    let now = app.runtime.clock.now();
    sg_repository::upsert_follow(
        &app.pool,
        app.runtime.ids.next_id(),
        &crate::social_graph::model::Follow {
            follower: AccountRef::Remote(remote_id),
            followee: AccountRef::Local(target_id),
            reblogs: true,
            notify: false,
            languages: Vec::new(),
            activity_id: "https://remote.example/activities/follow/pre".to_string(),
            created_at: now,
        },
    )
    .await
    .expect("seeding the pre-existing follow must succeed");

    let builder = build_activity_builder(&app, 9_100);
    let (_activity_id, block_json) = builder
        .build_block(
            &AccountRef::Remote(remote_id),
            &AccountRef::Local(target_id),
        )
        .await
        .expect("build_block must succeed");
    let activity = ParsedActivity {
        id: "https://remote.example/activities/block/1".to_string(),
        activity_type: "Block".to_string(),
        raw: block_json,
    };
    let ctx = signer(remote_uri);

    let outcome = handler
        .handle(&activity, &ctx)
        .await
        .expect("handling an inbound Block must succeed");
    assert_eq!(outcome, HandleOutcome::Handled);

    let state = load_state(
        &app,
        AccountRef::Local(target_id),
        AccountRef::Remote(remote_id),
    )
    .await;
    assert!(state.blocked_by, "Requirement 7.4: blocked_by must be set");
    assert!(
        !state.followed_by,
        "the pre-existing follow must be cleared by the block"
    );
}

// --- Undo(Follow) ---------------------------------------------------------------

#[tokio::test]
async fn inbound_undo_follow_removes_the_established_follow() {
    let app = spawn_test_app().await;

    let target_id = create_test_actor(&app, "target_undo_follow").await;
    let target_url = actor_url("target_undo_follow");
    let remote_uri = "https://remote.example/users/judy";
    let remote_id = create_test_remote(&app, remote_uri, false).await;

    let resolver = FakeActorUriResolver::new()
        .with(target_url, AccountRef::Local(target_id))
        .with(remote_uri, AccountRef::Remote(remote_id));
    let (handler, _local, _http) = build_handler(&app, resolver, 10_000);

    let now = app.runtime.clock.now();
    let orig_activity_id = "https://remote.example/activities/follow/pre2".to_string();
    sg_repository::upsert_follow(
        &app.pool,
        app.runtime.ids.next_id(),
        &crate::social_graph::model::Follow {
            follower: AccountRef::Remote(remote_id),
            followee: AccountRef::Local(target_id),
            reblogs: true,
            notify: false,
            languages: Vec::new(),
            activity_id: orig_activity_id.clone(),
            created_at: now,
        },
    )
    .await
    .expect("seeding the established follow must succeed");

    let builder = build_activity_builder(&app, 10_100);
    let (_wrapped_id, wrapped) = builder
        .build_follow(
            &AccountRef::Remote(remote_id),
            &AccountRef::Local(target_id),
        )
        .await
        .expect("build_follow must succeed");
    let undo_json = builder
        .build_undo(&AccountRef::Remote(remote_id), &orig_activity_id, wrapped)
        .await
        .expect("build_undo must succeed");
    let activity = ParsedActivity {
        id: "https://remote.example/activities/undo/1".to_string(),
        activity_type: "Undo".to_string(),
        raw: undo_json,
    };
    let ctx = signer(remote_uri);

    let outcome = handler
        .handle(&activity, &ctx)
        .await
        .expect("handling an inbound Undo(Follow) must succeed");
    assert_eq!(outcome, HandleOutcome::Handled);

    let state = load_state(
        &app,
        AccountRef::Local(target_id),
        AccountRef::Remote(remote_id),
    )
    .await;
    assert!(
        !state.followed_by,
        "Requirement 7.5: the follow must be removed"
    );
}

// --- Undo(Block) ------------------------------------------------------------

#[tokio::test]
async fn inbound_undo_block_clears_blocked_by() {
    let app = spawn_test_app().await;

    let target_id = create_test_actor(&app, "target_undo_block").await;
    let target_url = actor_url("target_undo_block");
    let remote_uri = "https://remote.example/users/karl";
    let remote_id = create_test_remote(&app, remote_uri, false).await;

    let resolver = FakeActorUriResolver::new()
        .with(target_url, AccountRef::Local(target_id))
        .with(remote_uri, AccountRef::Remote(remote_id));
    let (handler, _local, _http) = build_handler(&app, resolver, 11_000);

    let now = app.runtime.clock.now();
    let orig_activity_id = "https://remote.example/activities/block/pre".to_string();
    sg_repository::upsert_block(
        &app.pool,
        app.runtime.ids.next_id(),
        &crate::social_graph::model::Block {
            blocker: AccountRef::Remote(remote_id),
            blocked: AccountRef::Local(target_id),
            activity_id: orig_activity_id.clone(),
            created_at: now,
        },
    )
    .await
    .expect("seeding the block must succeed");

    let builder = build_activity_builder(&app, 11_100);
    let (_wrapped_id, wrapped) = builder
        .build_block(
            &AccountRef::Remote(remote_id),
            &AccountRef::Local(target_id),
        )
        .await
        .expect("build_block must succeed");
    let undo_json = builder
        .build_undo(&AccountRef::Remote(remote_id), &orig_activity_id, wrapped)
        .await
        .expect("build_undo must succeed");
    let activity = ParsedActivity {
        id: "https://remote.example/activities/undo/2".to_string(),
        activity_type: "Undo".to_string(),
        raw: undo_json,
    };
    let ctx = signer(remote_uri);

    let outcome = handler
        .handle(&activity, &ctx)
        .await
        .expect("handling an inbound Undo(Block) must succeed");
    assert_eq!(outcome, HandleOutcome::Handled);

    let state = load_state(
        &app,
        AccountRef::Local(target_id),
        AccountRef::Remote(remote_id),
    )
    .await;
    assert!(
        !state.blocked_by,
        "Requirement 7.6: blocked_by must be cleared"
    );
}

// --- Undo: unrelated inner type is left to other handlers --------------------

#[tokio::test]
async fn inbound_undo_with_unrelated_inner_type_is_ignored() {
    let app = spawn_test_app().await;
    let (handler, _local, _http) = build_handler(&app, FakeActorUriResolver::new(), 12_000);

    let activity = ParsedActivity {
        id: "https://remote.example/activities/undo/3".to_string(),
        activity_type: "Undo".to_string(),
        raw: serde_json::json!({
            "id": "https://remote.example/activities/undo/3",
            "type": "Undo",
            "actor": "https://remote.example/users/leo",
            "object": {
                "id": "https://remote.example/activities/like/1",
                "type": "Like",
                "actor": "https://remote.example/users/leo",
                "object": "https://kawasemi.example/statuses/1",
            },
        }),
    };
    let ctx = signer("https://remote.example/users/leo");

    let outcome = handler
        .handle(&activity, &ctx)
        .await
        .expect("handling must succeed");
    assert_eq!(outcome, HandleOutcome::Ignored);
}
