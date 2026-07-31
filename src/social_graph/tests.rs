//! Wiring-assembly tests for task 5.2 (`Boundary: SocialGraphModule`) — see
//! `src/social_graph.rs`'s own doc comment ("Task 5.2") for what
//! [`build_social_graph_module`]/[`register_downstream_handlers`] do.
//!
//! Per this task's own instructions these are deliberately narrow: "does the
//! wiring actually connect the pieces", not a re-verification of already-
//! approved task 3.x/4.x business logic (that is `follow_service/tests.rs`'
//! /`inbound/tests.rs`'s/`providers/tests.rs`'s own job). Three things are
//! proven here, matching this task's own completion condition
//! ("起動後に受信 Activity がハンドラへ届き、accounts の relationships と
//! counts…が実値を返し、ブロック判定が連合受信に効き、フォロー確立/受信保留
//! で notifications へイベントが渡る状態"):
//!
//! 1. [`follow_via_the_live_router_wires_endpoints_and_providers`][]: `POST
//!    /api/v1/accounts/:id/follow` through the *real*, fully-assembled
//!    router (`crate::server::build_router`, the exact `AppState`
//!    `spawn_test_app` itself serves — mirrors
//!    `tests/statuses_bootstrap_wiring_it.rs`'s own established "drive the
//!    real router in-process via `tower::ServiceExt::oneshot`" technique,
//!    adapted to an inline `#[cfg(test)]` module per this task's own
//!    instruction not to add a new `tests/*_it.rs` file) actually reaches
//!    [`crate::social_graph::follow_service::FollowService::follow`]
//!    (proving `SocialGraphEndpointsState`/the router mount/`AppState`'s new
//!    `social_graph` field all connect), and the resulting relationship
//!    state is then independently observed through
//!    `AppState::accounts().ports()` — the *real*
//!    `RelationshipStateProvider`/`AccountCountsProvider` registries this
//!    task registers into (Requirements 8.2, 10.1).
//! 2. [`blocking_via_the_service_makes_the_live_block_policy_report_blocked`][]:
//!    `BlockService::block` (via [`SocialGraphModule::block`]) followed by a
//!    direct query against `AppState::federation().block_policy()` — the
//!    *real* `BlockPolicyRegistry` this task registers `BlockPolicyImpl`
//!    into (Requirement 6.1) — proves the registration is live and its
//!    verdict flips on block/unblock.
//! 3. [`register_downstream_handlers_wires_a_dispatch_reachable_handler`][]:
//!    calls [`register_downstream_handlers`] directly against a freshly
//!    built `InboundActivityDispatcher` (mirroring how
//!    `federation::build_federation_module`'s own "DOWNSTREAM DISPATCHER
//!    REGISTRATION POINT" uses it) and dispatches a hand-built Follow
//!    Activity from a genuinely *remote* signer (a pre-seeded
//!    `remote_accounts` cache row, so `ProdActorUriResolver`'s resolution
//!    needs no live network fetch) addressed to a locked local target —
//!    proving this task's own registration closure produces a real,
//!    dispatch-reachable [`SocialGraphInboundHandler`] (Requirement 7.1),
//!    distinct from test 1's local-to-local scenario (which does not by
//!    itself distinguish "the sender's own direct `establish_follow` call"
//!    from "the inbound handler's own establish_follow call", since both
//!    are idempotent no-ops the second time — a genuinely remote signer
//!    with a locked target does distinguish them: only the inbound handler
//!    can create the pending inbound `follow_requests` row this test
//!    asserts on).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::Value;
use tower::ServiceExt;

use crate::accounts::DEFAULT_REMOTE_ACCOUNT_CACHE_TTL;
use crate::accounts::RemoteAccountFetcher;
use crate::accounts::model::{ProfileField, ProfilePatch, RemoteAccount};
use crate::accounts::profile_repository::upsert_profile;
use crate::accounts::remote_repository::upsert_remote;
use crate::actor::owner::create_owner;
use crate::actor::{ActorType, Handle, NewActor};
use crate::domain::{AccountRef, Id, Visibility};
use crate::error::AppError;
use crate::federation::inbound::{InboundActivityDispatcher, InboundContext};
use crate::federation::jsonld::ParsedActivity;
use crate::federation::signatures::{ReqwestFederationHttpClient, VerifiedSigner};
use crate::federation::{BlockPolicy, LocalRecipientContext};
use crate::oauth::app_repository::{self, NewApp};
use crate::oauth::model::ScopeSet as ModelScopeSet;
use crate::oauth::token_repository::{self, NewAccessToken};
use crate::server;
use crate::social_graph::repository;
use crate::social_graph::{self};
use crate::statuses::model::Status;
use crate::statuses::notification_sink::{
    NotificationEvent, NotificationEventSink, NotificationSinkRegistry, NotificationType,
};
use crate::statuses::status_repository;
use crate::test_harness::{TestApp, spawn_test_app};
use time::OffsetDateTime;

// ---- Shared fixtures (mirrors `follow_service/tests.rs`'s established
// helpers) --------------------------------------------------------------

async fn create_test_actor(app: &TestApp, handle: &str) -> Id {
    let owner_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    create_owner(&app.pool, owner_id, now)
        .await
        .expect("creating the owner fixture must succeed");

    let actor = app
        .actor
        .actor_service()
        .create_actor(NewActor {
            owner_id,
            handle: Handle::new(handle).expect("test handle must be valid"),
            actor_type: ActorType::Person,
            display_name: "Social Graph Wiring Test Actor".to_string(),
            summary: "an actor used by social-graph's own task 5.2 wiring tests".to_string(),
        })
        .await
        .expect("create_actor (with signing key provisioning) must succeed");

    actor.id
}

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

fn sample_remote_account(id: Id, actor_uri: &str, fetched_at: OffsetDateTime) -> RemoteAccount {
    RemoteAccount {
        id,
        actor_uri: actor_uri.to_string(),
        username: "wiring_remote".to_string(),
        domain: "remote.example".to_string(),
        display_name: "Wiring Remote".to_string(),
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

/// Pre-seeds a real `remote_accounts` cache row with a fresh `fetched_at`,
/// so `ProdActorUriResolver::resolve_account_ref`'s genuinely-remote
/// fallback (`RemoteAccountFetcher::fetch_and_normalize`) resolves it from
/// the DB cache — never a live network fetch (that method's own doc
/// comment: cache-hit-and-not-stale short-circuits before ever touching
/// `FederationHttpClient`).
async fn create_test_remote(app: &TestApp, actor_uri: &str) -> Id {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    upsert_remote(&app.pool, &sample_remote_account(id, actor_uri, now))
        .await
        .expect("upsert_remote must succeed");
    id
}

async fn register_test_app(app: &TestApp) -> Id {
    let key = app.state.oauth().token_hash_key().clone();
    let now = app.runtime.clock.now();
    let registered = app_repository::register_app(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        &key,
        now,
        NewApp {
            name: "Social Graph Wiring Test Client".to_string(),
            redirect_uris: vec!["https://client.example/callback".to_string()],
            scopes: ModelScopeSet::new(["read", "write"]),
        },
    )
    .await
    .expect("register_app must succeed");
    registered.id
}

async fn issue_test_token(app: &TestApp, app_id: Id, actor_id: Id, scopes: &[&str]) -> String {
    let key = app.state.oauth().token_hash_key().clone();
    let now = app.runtime.clock.now();
    let issued = token_repository::issue_token(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        &key,
        now,
        NewAccessToken {
            app_id,
            actor_id,
            scopes: ModelScopeSet::new(scopes.iter().copied()),
        },
    )
    .await
    .expect("issue_token must succeed");
    issued.plaintext.expose_secret().to_string()
}

fn real_router(app: &TestApp) -> Router {
    server::build_router(app.state.clone())
}

async fn post_json(router: &Router, path: &str, token: &str) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("POST")
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .expect("build request");
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("router must not fail to produce a response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read response body");
    let value: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("response body must be valid JSON")
    };
    (status, value)
}

// ---- 1: HTTP endpoint reachability + real provider registration ---------

#[tokio::test]
async fn follow_via_the_live_router_wires_endpoints_and_providers() {
    let app = spawn_test_app().await;
    let router = real_router(&app);

    let alice = create_test_actor(&app, "sg_wiring_alice").await;
    let bob = create_test_actor(&app, "sg_wiring_bob").await;
    let oauth_app_id = register_test_app(&app).await;
    let token = issue_test_token(&app, oauth_app_id, alice, &["follow"]).await;

    let (status, body) = post_json(
        &router,
        &format!("/api/v1/accounts/{}/follow", bob.as_i64()),
        &token,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "POST /api/v1/accounts/:id/follow must succeed once task 5.2 mounts the route: {body:?}"
    );
    assert_eq!(
        body["following"], true,
        "the live router must actually reach FollowService::follow: {body:?}"
    );

    // Requirement 8.2: the real `RelationshipStateProvider`/
    // `AccountCountsProvider` this task registers must reflect the write
    // above -- queried independently of the HTTP response body, directly
    // through `AppState::accounts().ports()` (the exact registry
    // `crate::accounts::endpoints::relationships`/`show_account` also
    // consult).
    let ports = app.state.accounts().ports();
    let relationships = ports
        .relationships(alice, &[AccountRef::Local(bob)])
        .await
        .expect("relationships must succeed");
    assert_eq!(relationships.len(), 1);
    assert!(
        relationships[0].following,
        "the registered RelationshipStateProvider (RelProviderImpl) must report the real \
         follow: {relationships:?}"
    );

    let counts = ports
        .counts(&AccountRef::Local(bob))
        .await
        .expect("counts must succeed");
    assert_eq!(
        counts.followers, 1,
        "the registered AccountCountsProvider must report bob's real followers_count: \
         {counts:?}"
    );

    app.cleanup().await;
}

// ---- 2: BlockPolicy registration is live ---------------------------------

#[tokio::test]
async fn blocking_via_the_service_makes_the_live_block_policy_report_blocked() {
    let app = spawn_test_app().await;

    let blocker = create_test_actor(&app, "sg_wiring_blocker").await;
    let blocked = create_test_actor(&app, "sg_wiring_blocked").await;
    let blocker_url = format!(
        "https://{}/users/sg_wiring_blocker",
        app.state.config().server.domain
    );
    let blocked_url = format!(
        "https://{}/users/sg_wiring_blocked",
        app.state.config().server.domain
    );

    // Not blocked before any Block operation -- the registered BlockPolicyImpl
    // must genuinely query live state, not report a hardcoded verdict.
    let before = app
        .state
        .federation()
        .block_policy()
        .is_blocked(
            &blocked_url,
            LocalRecipientContext::Actor {
                actor_uri: blocker_url.clone(),
            },
        )
        .await
        .expect("is_blocked must succeed");
    assert!(!before, "must not report blocked before any block exists");

    app.state
        .social_graph()
        .block()
        .block(blocker, &blocked.as_i64().to_string())
        .await
        .expect("BlockService::block must succeed");

    // Requirement 6.1, 6.2: the live BlockPolicyRegistry (federation-core)
    // this task registers ConcreteBlockPolicyImpl into must now report the
    // blocked signer as blocked from the blocker's own destination
    // perspective.
    let after = app
        .state
        .federation()
        .block_policy()
        .is_blocked(
            &blocked_url,
            LocalRecipientContext::Actor {
                actor_uri: blocker_url.clone(),
            },
        )
        .await
        .expect("is_blocked must succeed");
    assert!(
        after,
        "the registered BlockPolicyImpl must report the real block through the live \
         BlockPolicyRegistry"
    );

    app.state
        .social_graph()
        .block()
        .unblock(blocker, &blocked.as_i64().to_string())
        .await
        .expect("BlockService::unblock must succeed");

    // Requirement 6.4: unblocking must be observed on the very next query.
    let after_unblock = app
        .state
        .federation()
        .block_policy()
        .is_blocked(
            &blocked_url,
            LocalRecipientContext::Actor {
                actor_uri: blocker_url,
            },
        )
        .await
        .expect("is_blocked must succeed");
    assert!(
        !after_unblock,
        "the registered BlockPolicyImpl must stop reporting blocked once unblocked"
    );

    app.cleanup().await;
}

// ---- 3: register_downstream_handlers wires a dispatch-reachable handler --

#[tokio::test]
async fn register_downstream_handlers_wires_a_dispatch_reachable_handler() {
    let app = spawn_test_app().await;

    let target_id = create_test_actor(&app, "sg_wiring_target_locked").await;
    lock_actor(&app, target_id).await;
    let target_url = format!(
        "https://{}/users/sg_wiring_target_locked",
        app.state.config().server.domain
    );

    let remote_uri = "https://remote.example/users/sg_wiring_remote_signer";
    let remote_id = create_test_remote(&app, remote_uri).await;

    let fetcher = Arc::new(RemoteAccountFetcher::new(
        app.pool.clone(),
        Arc::new(ReqwestFederationHttpClient::new()),
        app.runtime.clone(),
        DEFAULT_REMOTE_ACCOUNT_CACHE_TTL,
    ));
    let (register, _pending_delivery) = social_graph::register_downstream_handlers(
        app.pool.clone(),
        app.runtime.clone(),
        app.state.config().server.domain.clone(),
        Arc::clone(app.actor.directory()),
        fetcher,
        NotificationSinkRegistry::new(),
    );
    // `_pending_delivery` is deliberately left unresolved: this scenario
    // (locked target, requires approval) never delivers an Accept -- see
    // `inbound.rs`'s own doc comment, "受信 Follow: ... 要承認なら
    // record_pending（inbound）" -- so the deferred delivery cell is never
    // read.

    let mut dispatcher = InboundActivityDispatcher::new();
    register(&mut dispatcher);

    let activity_id = "https://remote.example/activities/sg-wiring-follow-1".to_string();
    let activity = ParsedActivity {
        id: activity_id.clone(),
        activity_type: "Follow".to_string(),
        raw: serde_json::json!({
            "id": activity_id,
            "type": "Follow",
            "actor": remote_uri,
            "object": target_url,
        }),
    };
    let ctx = InboundContext {
        signer: VerifiedSigner {
            key_id: format!("{remote_uri}#main-key"),
            actor_uri: remote_uri.to_string(),
        },
    };

    dispatcher
        .dispatch(&activity, &ctx)
        .await
        .expect("dispatching the Follow must succeed");

    let now = app.runtime.clock.now();
    let mut states = repository::load_states(
        &app.pool,
        &AccountRef::Local(target_id),
        std::slice::from_ref(&AccountRef::Remote(remote_id)),
        now,
    )
    .await
    .expect("load_states must succeed");
    let state = states.pop().expect("exactly one state");

    assert!(
        !state.followed_by,
        "a locked target must not establish immediately"
    );
    assert!(
        state.requested_by,
        "the registration closure built by register_downstream_handlers must have produced a \
         real, dispatch-reachable SocialGraphInboundHandler that recorded the pending inbound \
         follow request -- state: {state:?}"
    );

    app.cleanup().await;
}

// ---- 4/5: notifications actually thread through the real wiring ---------
//
// Neither of the three tests above registers a `NotificationEventSink` and
// observes an emitted event -- `Transitions::new(pool, runtime,
// notifications)` (this module's own doc comment, task 5.2's completion
// condition's own "...で notifications へイベントが渡る状態" clause) is
// constructed inside `register_downstream_handlers`/
// `build_social_graph_module` themselves, so proving it is genuinely wired
// requires a registry built and passed in from *outside*, mirroring test 3's
// own "call register_downstream_handlers directly, dispatch a hand-built
// Activity" technique -- not a re-verification of `record_pending`/
// `establish_follow`'s own emit logic (already covered by
// `transitions/tests.rs`, task 2.2, not this task's concern).

/// A minimal recording [`NotificationEventSink`] double. Reimplemented here
/// (rather than imported) because
/// `crate::statuses::notification_sink::tests::RecordingSink` is private to
/// its own module's `#[cfg(test)]` block.
struct RecordingNotificationSink {
    events: std::sync::Mutex<Vec<NotificationEvent>>,
}

impl RecordingNotificationSink {
    fn new() -> Self {
        Self {
            events: std::sync::Mutex::new(Vec::new()),
        }
    }
}

impl NotificationEventSink for RecordingNotificationSink {
    fn emit<'a>(
        &'a self,
        event: NotificationEvent,
    ) -> Pin<Box<dyn Future<Output = Result<(), AppError>> + Send + 'a>> {
        Box::pin(async move {
            self.events.lock().unwrap().push(event);
            Ok(())
        })
    }
}

/// Requirement 7.1/Boundary Commitments (notifications emit point):
/// dispatching a remote Follow at an *unlocked* local target takes
/// `SocialGraphInboundHandler`'s `Establish` branch
/// (`Transitions::establish_follow`), which must emit a
/// `NotificationType::Follow` event through whatever `NotificationSinkRegistry`
/// `register_downstream_handlers` was actually constructed with. Unlike the
/// locked-target scenario below, `Establish` also delivers an Accept back to
/// the remote follower, so (unlike test 3's own deliberately-unresolved
/// `_pending_delivery`) the pending delivery cell here must be resolved --
/// reusing the app's own already-live `ConcreteDeliveryService`
/// (`app.state.federation().delivery_service()`) is safe and performs no
/// live network call: the Accept's recipient is remote, so delivery only
/// enqueues a `DbDeliveryQueue` row (`HttpDeliverySink::dispatch`), it never
/// sends synchronously.
#[tokio::test]
async fn register_downstream_handlers_emits_a_follow_notification_for_an_unlocked_target() {
    let app = spawn_test_app().await;

    let target_id = create_test_actor(&app, "sg_wiring_notif_follow_target").await;
    let target_url = format!(
        "https://{}/users/sg_wiring_notif_follow_target",
        app.state.config().server.domain
    );

    let remote_uri = "https://remote.example/users/sg_wiring_notif_follow_signer";
    let remote_id = create_test_remote(&app, remote_uri).await;

    let fetcher = Arc::new(RemoteAccountFetcher::new(
        app.pool.clone(),
        Arc::new(ReqwestFederationHttpClient::new()),
        app.runtime.clone(),
        DEFAULT_REMOTE_ACCOUNT_CACHE_TTL,
    ));

    let notifications = NotificationSinkRegistry::new();
    let sink = Arc::new(RecordingNotificationSink::new());
    notifications.set_sink(Arc::clone(&sink) as Arc<dyn NotificationEventSink>);

    let (register, pending) = social_graph::register_downstream_handlers(
        app.pool.clone(),
        app.runtime.clone(),
        app.state.config().server.domain.clone(),
        Arc::clone(app.actor.directory()),
        fetcher,
        notifications,
    );
    pending.resolve(Arc::clone(app.state.federation().delivery_service()));

    let mut dispatcher = InboundActivityDispatcher::new();
    register(&mut dispatcher);

    let activity_id = "https://remote.example/activities/sg-wiring-notif-follow-1".to_string();
    let activity = ParsedActivity {
        id: activity_id.clone(),
        activity_type: "Follow".to_string(),
        raw: serde_json::json!({
            "id": activity_id,
            "type": "Follow",
            "actor": remote_uri,
            "object": target_url,
        }),
    };
    let ctx = InboundContext {
        signer: VerifiedSigner {
            key_id: format!("{remote_uri}#main-key"),
            actor_uri: remote_uri.to_string(),
        },
    };

    dispatcher
        .dispatch(&activity, &ctx)
        .await
        .expect("dispatching the Follow must succeed");

    {
        let events = sink.events.lock().unwrap();
        assert_eq!(
            events.len(),
            1,
            "an unlocked target's Establish path must emit exactly one notification event \
             through the real registration closure: {events:?}"
        );
        assert_eq!(events[0].kind, NotificationType::Follow);
        assert_eq!(events[0].recipient, AccountRef::Local(target_id));
        assert_eq!(events[0].origin, AccountRef::Remote(remote_id));
    }

    app.cleanup().await;
}

/// Requirement 7.1/Boundary Commitments (notifications emit point):
/// dispatching a remote Follow at a *locked* local target takes
/// `SocialGraphInboundHandler`'s `RequireApproval` branch
/// (`Transitions::record_pending`), which must emit a
/// `NotificationType::FollowRequest` event through the real registration
/// closure's `NotificationSinkRegistry`. Mirrors
/// `register_downstream_handlers_wires_a_dispatch_reachable_handler`'s own
/// locked-target setup exactly (including leaving `_pending_delivery`
/// unresolved -- `RequireApproval` never delivers), adding only the
/// recording sink and its assertion.
#[tokio::test]
async fn register_downstream_handlers_emits_a_follow_request_notification_for_a_locked_target() {
    let app = spawn_test_app().await;

    let target_id = create_test_actor(&app, "sg_wiring_notif_pending_target").await;
    lock_actor(&app, target_id).await;
    let target_url = format!(
        "https://{}/users/sg_wiring_notif_pending_target",
        app.state.config().server.domain
    );

    let remote_uri = "https://remote.example/users/sg_wiring_notif_pending_signer";
    let remote_id = create_test_remote(&app, remote_uri).await;

    let fetcher = Arc::new(RemoteAccountFetcher::new(
        app.pool.clone(),
        Arc::new(ReqwestFederationHttpClient::new()),
        app.runtime.clone(),
        DEFAULT_REMOTE_ACCOUNT_CACHE_TTL,
    ));

    let notifications = NotificationSinkRegistry::new();
    let sink = Arc::new(RecordingNotificationSink::new());
    notifications.set_sink(Arc::clone(&sink) as Arc<dyn NotificationEventSink>);

    let (register, _pending_delivery) = social_graph::register_downstream_handlers(
        app.pool.clone(),
        app.runtime.clone(),
        app.state.config().server.domain.clone(),
        Arc::clone(app.actor.directory()),
        fetcher,
        notifications,
    );
    // `RequireApproval` never delivers (see this test's own doc comment) --
    // `_pending_delivery` deliberately left unresolved, mirroring test 3.

    let mut dispatcher = InboundActivityDispatcher::new();
    register(&mut dispatcher);

    let activity_id = "https://remote.example/activities/sg-wiring-notif-pending-1".to_string();
    let activity = ParsedActivity {
        id: activity_id.clone(),
        activity_type: "Follow".to_string(),
        raw: serde_json::json!({
            "id": activity_id,
            "type": "Follow",
            "actor": remote_uri,
            "object": target_url,
        }),
    };
    let ctx = InboundContext {
        signer: VerifiedSigner {
            key_id: format!("{remote_uri}#main-key"),
            actor_uri: remote_uri.to_string(),
        },
    };

    dispatcher
        .dispatch(&activity, &ctx)
        .await
        .expect("dispatching the Follow must succeed");

    {
        let events = sink.events.lock().unwrap();
        assert_eq!(
            events.len(),
            1,
            "a locked target's RequireApproval path must emit exactly one notification event \
             through the real registration closure: {events:?}"
        );
        assert_eq!(events[0].kind, NotificationType::FollowRequest);
        assert_eq!(events[0].recipient, AccountRef::Local(target_id));
        assert_eq!(events[0].origin, AccountRef::Remote(remote_id));
    }

    app.cleanup().await;
}

// ---- 6: CombinedAccountCountsProvider composes additively, not clobbering

/// Boundary Commitments (`CombinedAccountCountsProvider`'s own doc comment
/// in `src/social_graph.rs`): the composed `AccountCountsProvider`
/// registered by `build_social_graph_module` must merge
/// `AccountCountsProviderImpl`'s followers/following with statuses-core's
/// `AccountCountsContribution`'s statuses/last_status_at additively, not
/// clobber one with the other. Seeds a status for the target *before*
/// establishing a follow (through the live router, mirroring
/// `follow_via_the_live_router_wires_endpoints_and_providers`'s own
/// technique), then asserts both sub-count families survive in the single
/// queried `AccountCounts` value.
#[tokio::test]
async fn combined_account_counts_provider_composes_statuses_and_social_graph_sub_counts() {
    let app = spawn_test_app().await;
    let router = real_router(&app);

    let alice = create_test_actor(&app, "sg_wiring_counts_alice").await;
    let bob = create_test_actor(&app, "sg_wiring_counts_bob").await;

    // Seed a status authored by bob *before* the follow below, so
    // statuses-core's own statuses/last_status_at sub-counts are already
    // non-default -- proving social_graph's own later
    // `account_ports.set_counts_provider` registration composes additively
    // rather than clobbering them.
    let status_id = app.runtime.ids.next_id();
    let status_created_at = app.runtime.clock.now();
    let status = Status {
        id: status_id,
        actor_id: bob,
        uri: format!("https://example.test/statuses/{}", status_id.as_i64()),
        url: Some(format!("https://example.test/@bob/{}", status_id.as_i64())),
        content: "a pre-existing status, to prove the composed provider does not clobber it"
            .to_string(),
        visibility: Visibility::Public,
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
        created_at: status_created_at,
        edited_at: None,
    };
    status_repository::insert_status(&app.pool, &status)
        .await
        .expect("insert_status must succeed");

    let oauth_app_id = register_test_app(&app).await;
    let token = issue_test_token(&app, oauth_app_id, alice, &["follow"]).await;

    let (status_code, body) = post_json(
        &router,
        &format!("/api/v1/accounts/{}/follow", bob.as_i64()),
        &token,
    )
    .await;
    assert_eq!(status_code, StatusCode::OK, "follow must succeed: {body:?}");

    let ports = app.state.accounts().ports();
    let counts = ports
        .counts(&AccountRef::Local(bob))
        .await
        .expect("counts must succeed");

    assert_eq!(
        counts.followers, 1,
        "the composed provider must still report social_graph's own followers sub-count: \
         {counts:?}"
    );
    assert_eq!(
        counts.statuses, 1,
        "the composed provider must not clobber statuses-core's own statuses sub-count \
         (registered before social_graph's own, see build_social_graph_module's own doc \
         comment): {counts:?}"
    );
    assert_eq!(
        counts.last_status_at,
        Some(status_created_at),
        "the composed provider must not clobber statuses-core's own last_status_at sub-count: \
         {counts:?}"
    );

    app.cleanup().await;
}
