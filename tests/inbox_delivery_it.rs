//! Integration tests for federation-core's inbound dispatch/dedup/block
//! pipeline and outbound delivery-queue/worker lifecycle
//! (`.kiro/specs/federation-core/tasks.md`, task 6.2, `_Boundary:
//! inbox_delivery_it_`; Requirements 7.3, 7.4, 11.1, 11.2, 11.3, 11.4, 11.5,
//! 12.1, 12.2).
//!
//! Every underlying component this file exercises already has its own
//! dedicated unit/integration-style coverage in isolation:
//! `inbound/dispatcher/tests.rs` (dispatch fan-out logic against an
//! in-memory stub), `inbound/dedup/tests.rs` (the idempotency ledger alone),
//! `inbound/block_policy/tests.rs` (`NoopBlockPolicy`'s own contract),
//! `outbound/queue/tests.rs` (`DeliveryQueue`'s CRUD/state-transition
//! primitives against a real Postgres schema), `outbound/worker/tests.rs`
//! (`DeliveryWorker` driving a real queue through send/reschedule/fail), and
//! `outbound/target/tests.rs` (`RecipientTargetResolver`'s dedup rule in
//! memory). This file's own job is different: prove these compose correctly
//! *through* the same assembled pipelines task 5.3/5.4 wire together --
//!
//! - A destination-aware [`BlockPolicy`] test double, queried through a
//!   directly-constructed [`InboxService`] exactly the way `endpoints/inbox.rs`'s
//!   real handlers build it (mirrors `endpoints/inbox/tests.rs`'s and this
//!   spec's `NoopBlockPolicy` doc comment's own established convention for
//!   this exact scenario), proves a signer blocked from one local actor's
//!   per-actor inbox is genuinely rejected (Requirements 12.1, 12.2) while
//!   the *identical* signer and Activity, addressed to the shared inbox
//!   instead, is not bulk-rejected (this task's own "shared inbox 宛は既定
//!   契約どおり一括拒否されないこと" completion condition) -- and, since the
//!   rejected per-actor attempt must not have marked the Activity as seen,
//!   this simultaneously proves the pipeline's block-before-dedup ordering.
//! - A stub [`InboundActivityHandler`] registered on a real
//!   [`InboundActivityDispatcher`] proves `InboxService::process_inbound`
//!   genuinely hands an accepted Activity off to it (Requirement 7.3), and
//!   that re-posting the identical Activity id does not run that handoff a
//!   second time (Requirement 7.4).
//! - [`kawasemi::federation::FederationModule::delivery_service`] (the same
//!   port `tests/federation_bootstrap_it.rs` already proves enqueues a
//!   `delivery_jobs` row for a single remote recipient) is exercised with
//!   *multiple* remote recipients sharing one shared inbox, proving
//!   `RecipientTargetResolver`'s dedup rule collapses them into exactly one
//!   persisted job before a directly-built [`DeliveryWorker`] (mirroring
//!   `outbound/worker/tests.rs`'s own `worker_for` convention) sends to that
//!   shared inbox exactly once (Requirements 11.1, 11.2, 11.4).
//! - A directly-enqueued job's full retry lifecycle -- one real transient
//!   failure, a simulated time-passing gap, a second real transient failure
//!   with a strictly wider backoff delay, then attempts fast-forwarded to
//!   one below the documented cap and a third real failure that permanently
//!   fails the job -- proves both the progressive-widening contract
//!   (Requirement 11.3) and the permanent-failure contract (Requirement
//!   11.5) through the real `DeliveryWorker`/`DeliveryQueue` composition, not
//!   `backoff_delay`'s own already-unit-tested pure formula in isolation.
//!
//! Every network boundary this file touches is [`MockFederationHttpClient`]
//! (mirrors `signatures_it.rs`/`worker/tests.rs`'s own convention) -- no real
//! HTTP call is ever made by this file's own directly-built
//! `HttpSignatureVerifier`/`DeliveryWorker` instances. `spawn_test_app()`
//! does also start its own live background delivery worker backed by the
//! real `ReqwestFederationHttpClient` (`TEST_DELIVERY_POLL_INTERVAL`); this
//! is the same precedent `outbound/worker/tests.rs` already accepts for its
//! own directly-enqueued-job scenarios, not something this file introduces.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use axum::http::{Method, StatusCode};
use serde_json::json;

use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor, ResolvedActor};
use kawasemi::domain::Id;
use kawasemi::error::AppError;
use kawasemi::federation::jsonld::ParsedActivity;
use kawasemi::federation::outbound::{DeliveryWorker, WorkerRunSummary};
use kawasemi::federation::signatures::{
    DbFederationPublicKeyResolver, HttpResponse, HttpSignatureVerifier, IncomingRequest,
    MockFederationHttpClient, OutboundRequest, PublicKeyResolver, RequestSigner, SignatureFormat,
    SignatureNegotiator,
};
use kawasemi::federation::urls::ActorUrls;
use kawasemi::federation::{
    BlockPolicy, DEFAULT_MAX_DELIVERY_ATTEMPTS, DEFAULT_RECEIVED_ACTIVITY_RETENTION,
    DbDeliveryQueue, DbReceivedActivityStore, DeliveryQueue, DeliveryRequest, HandleOutcome,
    InboundActivityDispatcher, InboundActivityHandler, InboundContext, InboxOutcome, InboxService,
    LocalRecipientContext, NewDeliveryJob, NoopBlockPolicy, Recipient, backoff_delay,
};
use kawasemi::test_harness::{TestApp, spawn_test_app};

// ==========================================================================
// Fixtures (each integration test file is its own compiled crate, so these
// deliberately duplicate `signatures_it.rs`/`worker/tests.rs`'s own
// conventions rather than sharing across files).
// ==========================================================================

/// Creates a real owner + a real local actor via `ActorService::create_actor`
/// (real RSA-2048 signing key provisioning), resolved back through
/// `ActorDirectory` -- mirrors `signatures_it.rs`'s/`worker/tests.rs`'s own
/// identical helper.
async fn insert_actor_fixture(app: &TestApp, handle_str: &str) -> ResolvedActor {
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
            handle: Handle::new(handle_str).expect("test handle must be valid"),
            actor_type: ActorType::Person,
            display_name: format!("Inbox Delivery IT {handle_str}"),
            summary: "an actor used by the inbox/delivery integration test".to_string(),
        })
        .await
        .expect("create_actor (with signing key provisioning) must succeed");

    app.actor
        .directory()
        .resolve_actor_by_handle(&actor.handle)
        .await
        .expect("resolving the just-created actor must succeed")
        .expect("the just-created actor must be resolvable")
}

fn test_domain(app: &TestApp) -> String {
    app.state.config().server.domain.clone()
}

/// Builds a `RequestSigner` wired against `app`'s own real
/// `ActorDirectory`/`SigningKeyProvider`/`Clock` -- mirrors
/// `signatures_it.rs`'s `signer_for`.
fn signer_for(app: &TestApp) -> RequestSigner {
    RequestSigner::new(
        app.actor.directory().clone(),
        app.runtime.keys.clone(),
        ActorUrls::new(test_domain(app)),
        app.runtime.clock.clone(),
    )
}

/// Builds a `DbFederationPublicKeyResolver` against `app`'s own real,
/// migrated pool, with `mock` as the fetch boundary -- mirrors
/// `signatures_it.rs`'s `resolver_for`.
fn resolver_for(
    app: &TestApp,
    mock: Arc<MockFederationHttpClient>,
) -> DbFederationPublicKeyResolver<MockFederationHttpClient> {
    DbFederationPublicKeyResolver::new(
        app.pool.clone(),
        mock,
        app.runtime.clock.clone(),
        kawasemi::federation::DEFAULT_PUBLIC_KEY_CACHE_TTL,
    )
}

/// Builds an `HttpSignatureVerifier` against `resolver` -- mirrors
/// `signatures_it.rs`'s `verifier_for`.
fn verifier_for<R: PublicKeyResolver>(app: &TestApp, resolver: Arc<R>) -> HttpSignatureVerifier<R> {
    HttpSignatureVerifier::new(
        resolver,
        app.runtime.clock.clone(),
        kawasemi::federation::DEFAULT_SIGNATURE_MAX_AGE,
    )
}

/// A minimal `200 OK` actor document body carrying exactly the fields
/// `DbFederationPublicKeyResolver::parse_public_key_document` reads --
/// mirrors `signatures_it.rs`'s identical helper.
fn actor_document_response(actor_uri: &str, key_id: &str, public_key_pem: &str) -> HttpResponse {
    let body = json!({
        "id": actor_uri,
        "publicKey": {
            "id": key_id,
            "owner": actor_uri,
            "publicKeyPem": public_key_pem,
        }
    })
    .to_string()
    .into_bytes();
    HttpResponse {
        status: StatusCode::OK,
        headers: axum::http::HeaderMap::new(),
        body,
    }
}

/// The real, currently-valid public key PEM for `actor` -- mirrors
/// `signatures_it.rs`'s identical helper.
async fn actor_public_key_pem(app: &TestApp, actor: &ResolvedActor) -> String {
    app.actor
        .directory()
        .actor_public_key(actor.id)
        .await
        .expect("actor_public_key query must succeed")
        .expect("a freshly created actor must have an active signing key")
        .public_key_pem
}

/// Converts a signed `OutboundRequest` into the `IncomingRequest` shape
/// `InboxService::process_inbound` expects -- mirrors `signatures_it.rs`'s
/// identical helper.
fn to_incoming(req: &OutboundRequest) -> IncomingRequest {
    IncomingRequest {
        method: req.method.clone(),
        url: req.url.clone(),
        headers: req.headers.clone(),
        body: req.body.clone(),
    }
}

/// Builds a signable inbound-Activity `OutboundRequest` carrying the given
/// `activity_id`/`activity_type` as its JSON-LD body.
fn signable_activity_request(url: &str, activity_id: &str, activity_type: &str) -> OutboundRequest {
    OutboundRequest::new(Method::POST, url).with_body(
        json!({ "id": activity_id, "type": activity_type })
            .to_string()
            .into_bytes(),
    )
}

/// A stub [`InboundActivityHandler`] that records every Activity id it was
/// actually invoked with -- mirrors `inbound/dispatcher/tests.rs`'s own
/// `StubHandler` pattern, adapted to record ids (not just a count) so a test
/// can assert *which* Activity reached dispatch, not merely how many did.
struct RecordingHandler {
    types: Vec<&'static str>,
    seen: Arc<Mutex<Vec<String>>>,
}

impl RecordingHandler {
    fn new(types: Vec<&'static str>) -> (Arc<Self>, Arc<Mutex<Vec<String>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        (
            Arc::new(Self {
                types,
                seen: Arc::clone(&seen),
            }),
            seen,
        )
    }
}

impl InboundActivityHandler for RecordingHandler {
    fn activity_types(&self) -> &[&str] {
        &self.types
    }

    fn handle<'a>(
        &'a self,
        activity: &'a ParsedActivity,
        _ctx: &'a InboundContext,
    ) -> Pin<Box<dyn Future<Output = Result<HandleOutcome, AppError>> + Send + 'a>> {
        Box::pin(async move {
            self.seen.lock().unwrap().push(activity.id.clone());
            Ok(HandleOutcome::Handled)
        })
    }
}

/// A destination-aware `BlockPolicy` test double that blocks exactly one
/// actor URI, and only when queried with `LocalRecipientContext::Actor` --
/// never for `LocalRecipientContext::SharedInbox`, mirroring this spec's own
/// `NoopBlockPolicy` contract (`block_policy.rs`'s doc comment: "querying
/// with `LocalRecipientContext::SharedInbox` must never be used to
/// bulk-reject"). This is what lets a single signer be provably rejected
/// from one destination context and provably accepted from the other in the
/// same test.
struct BlocksOneActorFromPerActorInboxOnly {
    blocked_actor_uri: String,
}

impl BlockPolicy for BlocksOneActorFromPerActorInboxOnly {
    async fn is_blocked(
        &self,
        actor_uri: &str,
        local_recipient: LocalRecipientContext,
    ) -> Result<bool, AppError> {
        Ok(match local_recipient {
            LocalRecipientContext::Actor { .. } => actor_uri == self.blocked_actor_uri,
            LocalRecipientContext::SharedInbox => false,
        })
    }
}

/// Builds an `InboxService` composing a real `HttpSignatureVerifier`
/// (backed by `mock` for public-key fetches), `block_policy`, a real
/// `DbReceivedActivityStore` against `app`'s own pool, and `dispatcher` --
/// the same construction `endpoints/inbox.rs`'s real handlers use, built
/// directly here (bypassing `spawn_test_app()`'s live, already-`NoopBlockPolicy`-wired
/// router) so a test-double `BlockPolicy` can be substituted for this
/// task's own 12.1/12.2 scenario.
fn build_inbox_service<B: BlockPolicy>(
    app: &TestApp,
    mock: Arc<MockFederationHttpClient>,
    block_policy: B,
    dispatcher: InboundActivityDispatcher,
) -> InboxService<
    HttpSignatureVerifier<DbFederationPublicKeyResolver<MockFederationHttpClient>>,
    B,
    DbReceivedActivityStore,
> {
    let resolver = Arc::new(resolver_for(app, mock));
    let verifier = verifier_for(app, resolver);
    let dedup = DbReceivedActivityStore::new(
        app.pool.clone(),
        app.runtime.clock.clone(),
        DEFAULT_RECEIVED_ACTIVITY_RETENTION,
    );
    InboxService::new(
        verifier,
        block_policy,
        dedup,
        dispatcher,
        ActorUrls::new(test_domain(app)),
    )
}

// ==========================================================================
// (1) Requirement 7.3: an accepted Activity is handed off to the registered
// dispatch handler.
// ==========================================================================

#[tokio::test]
async fn accepted_activity_is_handed_off_to_the_registered_dispatch_handler() {
    let app = spawn_test_app().await;
    let urls = ActorUrls::new(test_domain(&app));
    let signer_actor = insert_actor_fixture(&app, "dispatch_signer").await;
    let recipient_actor = insert_actor_fixture(&app, "dispatch_recipient").await;
    let signer_uri = urls.actor_url(&signer_actor.handle);
    let key_id = urls.key_id(&signer_actor.handle);
    let public_key_pem = actor_public_key_pem(&app, &signer_actor).await;

    let (handler, seen) = RecordingHandler::new(vec!["Follow"]);
    let mut dispatcher = InboundActivityDispatcher::new();
    dispatcher.register(handler);

    let mock = Arc::new(MockFederationHttpClient::new());
    mock.queue_fetch_response(actor_document_response(
        &signer_uri,
        &key_id,
        &public_key_pem,
    ));
    let inbox = build_inbox_service(&app, mock, NoopBlockPolicy, dispatcher);

    let activity_id = "https://sender.example/activities/dispatch-1";
    let mut req = signable_activity_request(
        &urls.inbox_url(&recipient_actor.handle),
        activity_id,
        "Follow",
    );
    signer_for(&app)
        .sign_request(&signer_actor.handle, SignatureFormat::DraftCavage, &mut req)
        .await
        .expect("signing must succeed");

    let destination = LocalRecipientContext::Actor {
        actor_uri: urls.actor_url(&recipient_actor.handle),
    };
    let outcome = inbox
        .process_inbound(to_incoming(&req), destination)
        .await
        .expect("a verified, non-blocked, newly-seen Activity must be accepted");

    assert_eq!(outcome, InboxOutcome::Accepted);
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        [activity_id.to_string()],
        "the registered handler must receive exactly the dispatched Activity (Requirement 7.3)"
    );

    app.cleanup().await;
}

// ==========================================================================
// (2) Requirement 7.4: a duplicate Activity id is not re-dispatched.
// ==========================================================================

#[tokio::test]
async fn a_duplicate_activity_id_is_accepted_only_once_and_not_redispatched() {
    let app = spawn_test_app().await;
    let urls = ActorUrls::new(test_domain(&app));
    let signer_actor = insert_actor_fixture(&app, "dedup_signer").await;
    let recipient_actor = insert_actor_fixture(&app, "dedup_recipient").await;
    let signer_uri = urls.actor_url(&signer_actor.handle);
    let key_id = urls.key_id(&signer_actor.handle);
    let public_key_pem = actor_public_key_pem(&app, &signer_actor).await;

    let (handler, seen) = RecordingHandler::new(vec!["Follow"]);
    let mut dispatcher = InboundActivityDispatcher::new();
    dispatcher.register(handler);

    let mock = Arc::new(MockFederationHttpClient::new());
    // Only ONE fetch outcome is queued: the resolver's cache serves the
    // second `process_inbound` call's verification from cache (mirrors
    // `signatures_it.rs`'s `cached_public_key_is_reused_across_verifications_without_a_second_fetch`).
    mock.queue_fetch_response(actor_document_response(
        &signer_uri,
        &key_id,
        &public_key_pem,
    ));
    let inbox = build_inbox_service(&app, mock, NoopBlockPolicy, dispatcher);

    let activity_id = "https://sender.example/activities/dedup-1";
    let mut req = signable_activity_request(
        &urls.inbox_url(&recipient_actor.handle),
        activity_id,
        "Follow",
    );
    signer_for(&app)
        .sign_request(&signer_actor.handle, SignatureFormat::DraftCavage, &mut req)
        .await
        .expect("signing must succeed");
    let incoming = to_incoming(&req);
    let destination = LocalRecipientContext::Actor {
        actor_uri: urls.actor_url(&recipient_actor.handle),
    };

    let first = inbox
        .process_inbound(incoming.clone(), destination.clone())
        .await
        .expect("the first receipt of a newly-seen Activity must be accepted");
    assert_eq!(first, InboxOutcome::Accepted);
    assert_eq!(seen.lock().unwrap().len(), 1);

    let second = inbox
        .process_inbound(incoming, destination)
        .await
        .expect("re-posting an already-accepted Activity id must not itself be an error");
    assert_eq!(
        second,
        InboxOutcome::Duplicate,
        "an already-accepted Activity id must be reported as a duplicate (Requirement 7.4)"
    );
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "a duplicate Activity must not be re-dispatched to the registered handler (Requirement 7.4)"
    );

    app.cleanup().await;
}

// ==========================================================================
// (3) Requirements 12.1, 12.2, plus this task's own "shared inbox 宛は既定
// 契約どおり一括拒否されないこと" completion condition: a signer blocked from
// one local actor's per-actor inbox is rejected, while the identical signer
// and Activity addressed to the shared inbox is not bulk-rejected.
// ==========================================================================

#[tokio::test]
async fn actor_inbox_rejects_a_blocked_signer_while_shared_inbox_accepts_the_identical_signer() {
    let app = spawn_test_app().await;
    let urls = ActorUrls::new(test_domain(&app));
    let signer_actor = insert_actor_fixture(&app, "blocked_signer").await;
    let recipient_actor = insert_actor_fixture(&app, "block_recipient").await;
    let signer_uri = urls.actor_url(&signer_actor.handle);
    let key_id = urls.key_id(&signer_actor.handle);
    let public_key_pem = actor_public_key_pem(&app, &signer_actor).await;

    let (handler, seen) = RecordingHandler::new(vec!["Follow"]);
    let mut dispatcher = InboundActivityDispatcher::new();
    dispatcher.register(handler);

    let mock = Arc::new(MockFederationHttpClient::new());
    mock.queue_fetch_response(actor_document_response(
        &signer_uri,
        &key_id,
        &public_key_pem,
    ));
    let block_policy = BlocksOneActorFromPerActorInboxOnly {
        blocked_actor_uri: signer_uri.clone(),
    };
    let inbox = build_inbox_service(&app, mock, block_policy, dispatcher);

    let activity_id = "https://sender.example/activities/block-1";
    let mut req = signable_activity_request(
        &urls.inbox_url(&recipient_actor.handle),
        activity_id,
        "Follow",
    );
    signer_for(&app)
        .sign_request(&signer_actor.handle, SignatureFormat::DraftCavage, &mut req)
        .await
        .expect("signing must succeed");
    let incoming = to_incoming(&req);

    // (12.1, 12.2): the per-actor inbox destination queries the BlockPolicy
    // about this exact signer/destination pair and rejects it.
    let actor_destination = LocalRecipientContext::Actor {
        actor_uri: urls.actor_url(&recipient_actor.handle),
    };
    let err = inbox
        .process_inbound(incoming.clone(), actor_destination)
        .await
        .expect_err("a signer blocked from this destination actor's perspective must be rejected");
    assert_eq!(err.status, StatusCode::FORBIDDEN);
    assert!(
        seen.lock().unwrap().is_empty(),
        "a blocked signer's Activity must never reach dispatch"
    );

    // Task's own completion condition: the identical signer and Activity,
    // addressed to the shared inbox instead, must NOT be bulk-rejected.
    // This also proves the rejected per-actor attempt above never recorded
    // this Activity id into the dedup ledger: if it had, this call would
    // observe `Duplicate` instead of `Accepted` (`InboxService::process_verified`'s
    // block-before-dedup ordering).
    let outcome = inbox
        .process_inbound(incoming, LocalRecipientContext::SharedInbox)
        .await
        .expect("the same signer addressed to the shared inbox must not be bulk-rejected");
    assert_eq!(
        outcome,
        InboxOutcome::Accepted,
        "shared inbox must accept the identical signer under the default BlockPolicy contract"
    );
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        [activity_id.to_string()],
        "the shared-inbox delivery must reach dispatch since it was never rejected or deduped"
    );

    app.cleanup().await;
}

// ==========================================================================
// Delivery queue / worker fixtures.
// ==========================================================================

/// Builds a `DeliveryWorker` wired against `app`'s own real
/// `DbDeliveryQueue`/`ActorDirectory`/`Clock`-backed `SignatureNegotiator`,
/// with `mock` as the send boundary -- mirrors `outbound/worker/tests.rs`'s
/// own `worker_for`.
fn worker_for(
    app: &TestApp,
    mock: Arc<MockFederationHttpClient>,
) -> DeliveryWorker<DbDeliveryQueue, MockFederationHttpClient> {
    let signer = signer_for(app);
    let negotiator =
        SignatureNegotiator::new(app.pool.clone(), mock, signer, app.runtime.clock.clone());
    DeliveryWorker::new(
        DbDeliveryQueue::new(app.pool.clone()),
        negotiator,
        app.runtime.clock.clone(),
        app.actor.directory().clone(),
    )
}

fn ok_response() -> HttpResponse {
    HttpResponse {
        status: StatusCode::OK,
        headers: axum::http::HeaderMap::new(),
        body: Vec::new(),
    }
}

fn server_error_response() -> HttpResponse {
    HttpResponse {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        headers: axum::http::HeaderMap::new(),
        body: Vec::new(),
    }
}

async fn delivery_job_count(app: &TestApp, target_inbox: &str, activity_id: &str) -> i64 {
    let row: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM delivery_jobs WHERE target_inbox = $1 AND activity->>'id' = $2",
    )
    .bind(target_inbox)
    .bind(activity_id)
    .fetch_one(&app.pool)
    .await
    .expect("counting delivery_jobs rows must succeed");
    row.0
}

async fn read_job_state(pool: &sqlx::PgPool, job_id: i64) -> (String, i32, time::OffsetDateTime) {
    sqlx::query_as("SELECT status, attempts, next_attempt_at FROM delivery_jobs WHERE id = $1")
        .bind(job_id)
        .fetch_one(pool)
        .await
        .expect("the job must exist in delivery_jobs")
}

// ==========================================================================
// (4) Requirements 11.1, 11.2, 11.4: `DeliveryService`, reached through
// `FederationModule`, collapses multiple remote recipients sharing one
// shared inbox into a single persisted job without blocking on delivery
// completion, and a delivery worker sends to that shared inbox exactly once.
// ==========================================================================

#[tokio::test]
async fn shared_inbox_recipients_collapse_into_one_job_and_the_worker_sends_to_it_exactly_once() {
    let app = spawn_test_app().await;
    let sender = insert_actor_fixture(&app, "shared_inbox_sender").await;

    let activity_id = "https://kawasemi.inbox-delivery-it.internal/activities/shared-1";
    let shared_inbox = "https://shared.inbox-delivery-it.example/inbox";
    let request = DeliveryRequest {
        activity: json!({ "id": activity_id, "type": "Create" }),
        sender: sender.handle.clone(),
        recipients: vec![
            Recipient::Remote {
                inbox: "https://shared.inbox-delivery-it.example/users/bob/inbox".to_string(),
                shared_inbox: Some(shared_inbox.to_string()),
            },
            Recipient::Remote {
                inbox: "https://shared.inbox-delivery-it.example/users/carol/inbox".to_string(),
                shared_inbox: Some(shared_inbox.to_string()),
            },
            Recipient::Remote {
                inbox: "https://shared.inbox-delivery-it.example/users/dave/inbox".to_string(),
                shared_inbox: Some(shared_inbox.to_string()),
            },
        ],
    };

    // Requirement 11.1: `deliver()` persists the job(s) and returns without
    // waiting for delivery to complete -- structurally guaranteed by
    // `HttpDeliverySink` never touching a `FederationHttpClient` at all
    // (`sink.rs`), and observable here by `deliver()` itself succeeding with
    // no `FederationHttpClient` of any kind involved in this call at all.
    app.state
        .federation()
        .delivery_service()
        .deliver(request)
        .await
        .expect("deliver() must succeed for three remote recipients sharing one shared inbox");

    // Requirement 11.4: three recipients sharing one shared inbox must
    // collapse into exactly one persisted delivery_jobs row, not three.
    assert_eq!(
        delivery_job_count(&app, shared_inbox, activity_id).await,
        1,
        "three remote recipients sharing the same shared inbox must persist as exactly one \
         delivery job, never three"
    );

    // Requirement 11.2: a delivery worker picks up the persisted job and
    // performs a signed HTTP send -- and, since only one job exists at all,
    // the shared inbox necessarily receives exactly one send, never three
    // (the send-side half of Requirement 11.4's "重複させない").
    let mock = Arc::new(MockFederationHttpClient::new());
    mock.queue_send_response(ok_response());
    let worker = worker_for(&app, mock.clone());
    let summary = worker.run_once(10).await.expect("run_once must succeed");

    assert_eq!(
        summary.done, 1,
        "exactly one job must have been claimed and delivered"
    );
    assert_eq!(
        mock.sent_requests().len(),
        1,
        "the shared inbox must receive exactly one signed send, not one per recipient"
    );

    app.cleanup().await;
}

// ==========================================================================
// (5) Requirements 11.3, 11.5: a job's retry lifecycle -- two consecutive
// transient failures reschedule with a strictly widening backoff delay, and
// a third failure once attempts reach the documented cap permanently fails
// the job instead of rescheduling again.
// ==========================================================================

#[tokio::test]
async fn a_job_reschedules_with_widening_backoff_then_permanently_fails_at_the_attempt_cap() {
    let app = spawn_test_app().await;
    let sender = insert_actor_fixture(&app, "backoff_sender").await;
    let now = app.runtime.clock.now();
    let target_inbox = "https://backoff.inbox-delivery-it.example/inbox";
    let job_id: i64 = 987_654_321;

    let queue_setup = DbDeliveryQueue::new(app.pool.clone());
    queue_setup
        .enqueue(NewDeliveryJob {
            id: Id::from_i64(job_id),
            sender_actor_id: sender.id,
            target_inbox: target_inbox.to_string(),
            activity: json!({
                "@context": "https://www.w3.org/ns/activitystreams",
                "id": "https://kawasemi.inbox-delivery-it.internal/activities/backoff-1",
                "type": "Create",
            }),
            next_attempt_at: now,
        })
        .await
        .expect("enqueue must succeed");

    // --- First transient failure: attempts 0 -> 1, delay = backoff_delay(1). ---
    let mock = Arc::new(MockFederationHttpClient::new());
    mock.queue_send_response(server_error_response());
    let worker = worker_for(&app, mock.clone());
    let summary = worker.run_once(10).await.expect("run_once must succeed");
    assert_eq!(summary.rescheduled, 1);

    let (status, attempts, next_attempt_at_1) = read_job_state(&app.pool, job_id).await;
    assert_eq!(status, "pending");
    assert_eq!(attempts, 1);
    let first_delay = next_attempt_at_1 - now;
    assert_eq!(
        next_attempt_at_1,
        now + backoff_delay(1),
        "the first reschedule must push next_attempt_at forward by exactly backoff_delay(1)"
    );

    // Simulate the retry interval having elapsed (this queue/worker take
    // every timestamp as an explicit caller-supplied parameter -- see
    // `queue.rs`'s own doc comment -- so there is no wall-clock to actually
    // wait on; this directly bypasses the component under test the same way
    // `signatures_it.rs`'s `seed_cached_public_key` and
    // `worker/tests.rs`'s attempts fast-forward already do) by winding
    // `next_attempt_at` back to `now` so the second `run_once` call below
    // finds this job due again.
    sqlx::query("UPDATE delivery_jobs SET next_attempt_at = $1 WHERE id = $2")
        .bind(now)
        .bind(job_id)
        .execute(&app.pool)
        .await
        .expect("winding next_attempt_at back to simulate elapsed time must succeed");

    // --- Second transient failure: attempts 1 -> 2, delay = backoff_delay(2),
    // strictly wider than the first (Requirement 11.3's "widening... 進行的
    // に広げながら"). ---
    let mock2 = Arc::new(MockFederationHttpClient::new());
    mock2.queue_send_response(server_error_response());
    let worker2 = worker_for(&app, mock2.clone());
    let summary2 = worker2.run_once(10).await.expect("run_once must succeed");
    assert_eq!(summary2.rescheduled, 1);

    let (status, attempts, next_attempt_at_2) = read_job_state(&app.pool, job_id).await;
    assert_eq!(status, "pending");
    assert_eq!(attempts, 2);
    let second_delay = next_attempt_at_2 - now;
    assert_eq!(
        next_attempt_at_2,
        now + backoff_delay(2),
        "the second reschedule must push next_attempt_at forward by exactly backoff_delay(2)"
    );
    assert!(
        second_delay > first_delay,
        "the retry interval must widen progressively across consecutive failures \
         (Requirement 11.3): first delay {first_delay:?}, second delay {second_delay:?}"
    );

    // --- Fast-forward attempts to one below the documented cap, and wind
    // next_attempt_at back to now again, so the next failure is the one
    // that exhausts the retry budget (mirrors `worker/tests.rs`'s own
    // attempts-fast-forward convention for this exact edge). ---
    sqlx::query("UPDATE delivery_jobs SET attempts = $1, next_attempt_at = $2 WHERE id = $3")
        .bind(DEFAULT_MAX_DELIVERY_ATTEMPTS - 1)
        .bind(now)
        .bind(job_id)
        .execute(&app.pool)
        .await
        .expect("fast-forwarding the job's attempts count must succeed");

    let mock3 = Arc::new(MockFederationHttpClient::new());
    mock3.queue_send_response(server_error_response());
    let worker3 = worker_for(&app, mock3.clone());
    let summary3 = worker3.run_once(10).await.expect("run_once must succeed");

    assert_eq!(
        summary3,
        WorkerRunSummary {
            claimed: 1,
            done: 0,
            rescheduled: 0,
            failed: 1,
        },
        "a job whose incremented attempts reaches the documented cap must be marked \
         permanently failed instead of rescheduled again (Requirement 11.5)"
    );

    let (status, attempts, _) = read_job_state(&app.pool, job_id).await;
    assert_eq!(status, "failed");
    assert_eq!(
        attempts,
        DEFAULT_MAX_DELIVERY_ATTEMPTS - 1,
        "mark_failed must not itself alter the persisted attempts count"
    );

    app.cleanup().await;
}
