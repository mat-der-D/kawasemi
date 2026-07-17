//! Unit tests for [`LocalDeliverySink`]/[`HttpDeliverySink`] (Requirements
//! 10.3, 10.4, 11.1), per task 4.2's observable completion condition
//! ("local はキューを介さず受信意味論経路へ、remote はキューへ投入される")
//! — each sink is exercised against its *real* dependency
//! ([`InboxService`]/[`DeliveryQueue`]), not a further mock of that
//! dependency, so this file proves the actual wiring described in
//! design.md's Service Interface comment (`LocalDeliverySink ->
//! InboxService::process_local`, `HttpDeliverySink -> DeliveryQueue::enqueue`).
//! `DeliveryService`'s own branching/one-canonical-activity contract is
//! covered separately in `delivery/tests.rs` against plain [`DeliverySink`]
//! test doubles.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;
use time::OffsetDateTime;
use time::macros::datetime;

use super::*;
use crate::actor::{ActorState, ActorType, ResolvedActor};
use crate::domain::Id;
use crate::error::ErrorKind;
use crate::federation::inbound::{
    HandleOutcome, InboundActivityDispatcher, InboundActivityHandler, InboundContext,
    NoopBlockPolicy,
};
use crate::federation::outbound::queue::DeliveryJob;
use crate::federation::signatures::IncomingRequest;
use crate::runtime::{FixedClock, SeqIdGenerator};

fn handle(raw: &str) -> Handle {
    Handle::new(raw).expect("valid test handle")
}

fn actor_urls() -> ActorUrls {
    ActorUrls::new("kawasemi.example")
}

fn parsed_activity(id: &str) -> ParsedActivity {
    ParsedActivity {
        id: id.to_string(),
        activity_type: "Create".to_string(),
        raw: json!({
            "id": id,
            "type": "Create",
            "@context": "https://www.w3.org/ns/activitystreams",
        }),
    }
}

// --- Test doubles shared by LocalDeliverySink's tests ---

/// A `SignatureVerifier` that records how many times it was called and
/// always fails -- `LocalDeliverySink`/`process_local` must never call it at
/// all (Requirement 10.3: in-process hand-off, no HTTP, no signature
/// verification).
struct NeverCalledVerifier {
    calls: Arc<AtomicUsize>,
}

impl SignatureVerifier for NeverCalledVerifier {
    async fn verify_request(&self, _req: &IncomingRequest) -> Result<VerifiedSigner, AppError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(AppError::client(
            StatusCode::UNAUTHORIZED,
            "NeverCalledVerifier must not be invoked by process_local",
        ))
    }
}

/// A trivial in-memory `ReceivedActivityStore` -- no DB needed for this
/// file's own tests (`inbound/dedup.rs` already owns the real, Postgres-
/// backed implementation's own tests).
struct InMemoryDedupStore {
    seen: Mutex<std::collections::HashSet<String>>,
}

impl InMemoryDedupStore {
    fn new() -> Self {
        Self {
            seen: Mutex::new(std::collections::HashSet::new()),
        }
    }
}

impl ReceivedActivityStore for InMemoryDedupStore {
    async fn record_if_new(&self, activity_id: &str) -> Result<bool, AppError> {
        Ok(self.seen.lock().unwrap().insert(activity_id.to_string()))
    }

    async fn prune_expired(&self) -> Result<u64, AppError> {
        Ok(0)
    }
}

/// Counting `InboundActivityHandler` stub that also records the last
/// `InboundContext`/`ParsedActivity` it was handed, so a test can assert the
/// `VerifiedSigner` `LocalDeliverySink` synthesizes from `ActorUrls` reaches
/// the dispatcher unchanged, and (for the combined-convergence test below)
/// compare the exact Activity content the local path observed against what
/// the remote path enqueued.
struct CountingHandler {
    invocations: AtomicUsize,
    last_ctx: Mutex<Option<InboundContext>>,
    last_activity: Mutex<Option<ParsedActivity>>,
}

impl InboundActivityHandler for CountingHandler {
    fn activity_types(&self) -> &[&str] {
        &["Create"]
    }

    fn handle<'a>(
        &'a self,
        activity: &'a ParsedActivity,
        ctx: &'a InboundContext,
    ) -> Pin<Box<dyn Future<Output = Result<HandleOutcome, AppError>> + Send + 'a>> {
        Box::pin(async move {
            self.invocations.fetch_add(1, Ordering::SeqCst);
            *self.last_ctx.lock().unwrap() = Some(ctx.clone());
            *self.last_activity.lock().unwrap() = Some(activity.clone());
            Ok(HandleOutcome::Handled)
        })
    }
}

/// Builds a real `InboxService` (the same generic composition production
/// wiring will use) plus shared handles onto its verifier's call counter and
/// its registered handler, so a test can assert on both without reaching
/// into `InboxService`'s own private fields (a different module's
/// privacy boundary).
fn build_inbox_service() -> (
    InboxService<NeverCalledVerifier, NoopBlockPolicy, InMemoryDedupStore>,
    Arc<AtomicUsize>,
    Arc<CountingHandler>,
) {
    let verifier_calls = Arc::new(AtomicUsize::new(0));
    let verifier = NeverCalledVerifier {
        calls: Arc::clone(&verifier_calls),
    };
    let handler = Arc::new(CountingHandler {
        invocations: AtomicUsize::new(0),
        last_ctx: Mutex::new(None),
        last_activity: Mutex::new(None),
    });
    let mut dispatcher = InboundActivityDispatcher::new();
    dispatcher.register(Arc::clone(&handler) as Arc<dyn InboundActivityHandler>);

    let inbox = InboxService::new(
        verifier,
        NoopBlockPolicy,
        InMemoryDedupStore::new(),
        dispatcher,
        actor_urls(),
    );
    (inbox, verifier_calls, handler)
}

// --- 1: LocalDeliverySink reaches InboxService::process_local in-process,
// never invoking signature verification, and builds the sender's
// VerifiedSigner from ActorUrls (Requirement 10.3). ---

#[tokio::test]
async fn local_delivery_sink_dispatches_in_process_via_process_local() {
    let (inbox, verifier_calls, handler) = build_inbox_service();
    let sink = LocalDeliverySink::new(Arc::new(inbox), actor_urls());

    let canonical =
        CanonicalActivity::from_parsed(parsed_activity("https://kawasemi.example/activities/1"));
    let sender = handle("owner");
    let target = DeliveryTarget::Local {
        handle: handle("alice"),
    };

    sink.dispatch(target, &canonical, &sender)
        .await
        .expect("a local target dispatch must succeed");

    assert_eq!(
        handler.invocations.load(Ordering::SeqCst),
        1,
        "process_local must reach the dispatcher exactly once"
    );
    assert_eq!(
        verifier_calls.load(Ordering::SeqCst),
        0,
        "LocalDeliverySink must never trigger signature verification (in-process, no HTTP)"
    );

    let urls = actor_urls();
    let expected_signer = VerifiedSigner {
        key_id: urls.key_id(&sender),
        actor_uri: urls.actor_url(&sender),
    };
    assert_eq!(
        handler.last_ctx.lock().unwrap().clone(),
        Some(InboundContext {
            signer: expected_signer
        }),
        "the synthetic VerifiedSigner handed to the dispatcher must be built from ActorUrls for \
         the sending local actor, not a remote cryptographic claim"
    );
}

// --- 2: LocalDeliverySink rejects a non-local target defensively. ---

#[tokio::test]
async fn local_delivery_sink_rejects_a_remote_target() {
    let (inbox, _verifier_calls, _handler) = build_inbox_service();
    let sink = LocalDeliverySink::new(Arc::new(inbox), actor_urls());

    let canonical =
        CanonicalActivity::from_parsed(parsed_activity("https://kawasemi.example/activities/2"));
    let sender = handle("owner");
    let target = DeliveryTarget::Remote {
        inbox: "https://remote.example/inbox".to_string(),
    };

    let err = sink
        .dispatch(target, &canonical, &sender)
        .await
        .expect_err("a non-local target handed to LocalDeliverySink must be rejected");
    assert_eq!(err.kind, ErrorKind::Server);
}

// --- Test doubles for HttpDeliverySink's tests ---

/// A minimal `DeliveryQueue` test double recording every `enqueue`d job.
/// Every other method is unreachable by `HttpDeliverySink::dispatch` (which
/// only ever calls `enqueue`), so they return harmless placeholder values.
///
/// Backed by an `Arc<Mutex<..>>` (rather than a bare `Mutex<..>`) so a test
/// can retain a cloned [`MockDeliveryQueue::handle`] to the same underlying
/// storage *before* moving this value into an `HttpDeliverySink` (and, from
/// there, into a `DeliveryService`) — needed once the queue is no longer
/// reachable through a private field from outside `HttpDeliverySink`'s own
/// module (e.g. `delivery_service_with_real_sinks_converges_local_and_remote_on_the_same_canonical_activity`,
/// which composes a real `DeliveryService` and cannot reach
/// `DeliveryService.http_sink.queue` -- a private field of a sibling
/// module's type -- directly).
struct MockDeliveryQueue {
    jobs: Arc<Mutex<Vec<NewDeliveryJob>>>,
}

impl MockDeliveryQueue {
    fn new() -> Self {
        Self {
            jobs: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn jobs(&self) -> Vec<NewDeliveryJob> {
        self.jobs.lock().unwrap().clone()
    }

    /// A cloned handle onto this queue's own recorded jobs, independent of
    /// wherever `self` itself ends up being moved.
    fn handle(&self) -> Arc<Mutex<Vec<NewDeliveryJob>>> {
        Arc::clone(&self.jobs)
    }
}

impl DeliveryQueue for MockDeliveryQueue {
    async fn enqueue(&self, job: NewDeliveryJob) -> Result<(), AppError> {
        self.jobs.lock().unwrap().push(job);
        Ok(())
    }

    async fn claim_due(
        &self,
        _limit: i64,
        _now: OffsetDateTime,
    ) -> Result<Vec<DeliveryJob>, AppError> {
        Ok(vec![])
    }

    async fn mark_done(&self, _job_id: Id) -> Result<(), AppError> {
        Ok(())
    }

    async fn reschedule(
        &self,
        _job_id: Id,
        _next_attempt_at: OffsetDateTime,
        _attempts: i32,
    ) -> Result<(), AppError> {
        Ok(())
    }

    async fn mark_failed(&self, _job_id: Id) -> Result<(), AppError> {
        Ok(())
    }
}

/// A minimal `LocalActorLookup` test double resolving exactly the handles it
/// was constructed with, mirroring `target/tests.rs`'s own
/// `MockLocalActorLookup` convention.
struct MockSenderLookup {
    known: HashMap<String, i64>,
}

impl MockSenderLookup {
    fn with_handle(handle_str: &str, id: i64) -> Self {
        let mut known = HashMap::new();
        known.insert(handle_str.to_string(), id);
        Self { known }
    }

    fn empty() -> Self {
        Self {
            known: HashMap::new(),
        }
    }
}

impl LocalActorLookup for MockSenderLookup {
    async fn resolve_actor_by_handle(
        &self,
        handle: &Handle,
    ) -> Result<Option<ResolvedActor>, AppError> {
        Ok(self.known.get(handle.as_str()).map(|id| ResolvedActor {
            id: Id::from_i64(*id),
            handle: handle.clone(),
            actor_type: ActorType::Person,
            display_name: "Test Actor".to_string(),
            summary: String::new(),
            state: ActorState::Active,
        }))
    }
}

// --- 3: HttpDeliverySink enqueues a NewDeliveryJob with the expected
// fields, minted from the injected Clock/IdGenerator (Requirement 10.4,
// 11.1). ---

#[tokio::test]
async fn http_delivery_sink_enqueues_a_delivery_job_with_expected_fields() {
    let queue = MockDeliveryQueue::new();
    let lookup = MockSenderLookup::with_handle("owner", 42);
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(datetime!(2026-07-17 00:00:00 UTC)));
    let ids: Arc<dyn IdGenerator> = Arc::new(SeqIdGenerator::new(100));
    let sink = HttpDeliverySink::new(queue, lookup, Arc::clone(&clock), Arc::clone(&ids));

    let canonical =
        CanonicalActivity::from_parsed(parsed_activity("https://kawasemi.example/activities/3"));
    let sender = handle("owner");
    let target = DeliveryTarget::Remote {
        inbox: "https://remote.example/inbox".to_string(),
    };

    sink.dispatch(target, &canonical, &sender)
        .await
        .expect("a remote target dispatch must succeed");

    let jobs = sink.queue.jobs();
    assert_eq!(jobs.len(), 1, "exactly one job must be enqueued");
    let job = &jobs[0];
    assert_eq!(
        job.id,
        Id::from_i64(100),
        "the job's id must come from the injected IdGenerator"
    );
    assert_eq!(
        job.sender_actor_id,
        Id::from_i64(42),
        "sender_actor_id must be resolved from the sender Handle via LocalActorLookup"
    );
    assert_eq!(job.target_inbox, "https://remote.example/inbox");
    assert_eq!(
        job.activity,
        *canonical.as_value(),
        "the enqueued activity must be the canonical value, not a re-derived copy"
    );
    assert_eq!(
        job.next_attempt_at,
        datetime!(2026-07-17 00:00:00 UTC),
        "next_attempt_at must come from the injected Clock, never wall-clock time"
    );
}

// --- 4: HttpDeliverySink rejects a non-remote target defensively, without
// enqueuing anything. ---

#[tokio::test]
async fn http_delivery_sink_rejects_a_local_target() {
    let queue = MockDeliveryQueue::new();
    let lookup = MockSenderLookup::with_handle("owner", 42);
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(datetime!(2026-07-17 00:00:00 UTC)));
    let ids: Arc<dyn IdGenerator> = Arc::new(SeqIdGenerator::new(1));
    let sink = HttpDeliverySink::new(queue, lookup, clock, ids);

    let canonical =
        CanonicalActivity::from_parsed(parsed_activity("https://kawasemi.example/activities/4"));
    let sender = handle("owner");
    let target = DeliveryTarget::Local {
        handle: handle("alice"),
    };

    let err = sink
        .dispatch(target, &canonical, &sender)
        .await
        .expect_err("a non-remote target handed to HttpDeliverySink must be rejected");
    assert_eq!(err.kind, ErrorKind::Server);
    assert_eq!(sink.queue.jobs().len(), 0);
}

// --- 5: HttpDeliverySink fails when the sender no longer resolves to an
// existing local actor, and never enqueues a job in that case. ---

#[tokio::test]
async fn http_delivery_sink_rejects_an_unresolvable_sender() {
    let queue = MockDeliveryQueue::new();
    let lookup = MockSenderLookup::empty();
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(datetime!(2026-07-17 00:00:00 UTC)));
    let ids: Arc<dyn IdGenerator> = Arc::new(SeqIdGenerator::new(1));
    let sink = HttpDeliverySink::new(queue, lookup, clock, ids);

    let canonical =
        CanonicalActivity::from_parsed(parsed_activity("https://kawasemi.example/activities/5"));
    let sender = handle("ghost");
    let target = DeliveryTarget::Remote {
        inbox: "https://remote.example/inbox".to_string(),
    };

    let err = sink
        .dispatch(target, &canonical, &sender)
        .await
        .expect_err("an unresolvable sender handle must be rejected");
    assert_eq!(err.status, StatusCode::NOT_FOUND);
    assert_eq!(sink.queue.jobs().len(), 0);
}

// --- 6: DeliveryService, composed with *real* LocalDeliverySink (backed by
// a real InboxService) and *real* HttpDeliverySink (backed by a real
// DeliveryQueue test double), proves within a single `deliver()` call that
// the local and remote paths converge on the identical canonical Activity
// (Requirements 10.1-10.5, 11.1) -- this is task 4.2's own literal
// observable completion condition: "同一配送依頼で local 経路と remote 経路
// が同一の正規 Activity を扱い、local はキューを介さず受信意味論経路へ、
// remote はキューへ投入される統合テストが通る". Mirrors task 4.1's own
// `process_inbound_and_process_local_converge_on_the_same_dedup_and_dispatch_state`
// test (`inbound/service/tests.rs`), which proves the analogous convergence
// for `InboxService`'s two entry points by driving both through one real
// instance in one test, rather than through independently-faked doubles. ---

#[tokio::test]
async fn delivery_service_with_real_sinks_converges_local_and_remote_on_the_same_canonical_activity()
 {
    let (inbox, verifier_calls, handler) = build_inbox_service();
    let local_sink = LocalDeliverySink::new(Arc::new(inbox), actor_urls());

    let queue = MockDeliveryQueue::new();
    let jobs_handle = queue.handle();
    let http_sender_lookup = MockSenderLookup::with_handle("owner", 42);
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(datetime!(2026-07-17 00:00:00 UTC)));
    let ids: Arc<dyn IdGenerator> = Arc::new(SeqIdGenerator::new(500));
    let http_sink = HttpDeliverySink::new(queue, http_sender_lookup, clock, ids);

    // RecipientTargetResolver's own LocalActorLookup: must resolve the local
    // recipient "alice" to an existing actor.
    let target_lookup = MockSenderLookup::with_handle("alice", 7);
    let target_resolver = crate::federation::outbound::RecipientTargetResolver::new(target_lookup);

    let service =
        crate::federation::outbound::DeliveryService::new(target_resolver, local_sink, http_sink);

    let req = crate::federation::outbound::DeliveryRequest {
        activity: json!({
            "id": "https://kawasemi.example/activities/convergence-1",
            "type": "Create",
        }),
        sender: handle("owner"),
        recipients: vec![
            crate::federation::outbound::Recipient::Local(handle("alice")),
            crate::federation::outbound::Recipient::Remote {
                inbox: "https://remote.example/inbox".to_string(),
                shared_inbox: None,
            },
        ],
    };

    service
        .deliver(req)
        .await
        .expect("a single deliver() call with mixed local+remote recipients must succeed");

    // Local path: the real InboxService's dispatcher was reached exactly
    // once, in-process, without ever invoking SignatureVerifier -- and no
    // queue job exists for it (checked below alongside the remote count).
    assert_eq!(
        handler.invocations.load(Ordering::SeqCst),
        1,
        "the local recipient's Activity must reach the real InboxService dispatcher"
    );
    assert_eq!(
        verifier_calls.load(Ordering::SeqCst),
        0,
        "the local path must never invoke SignatureVerifier (in-process, no HTTP)"
    );

    // Remote path: exactly one job was enqueued (never touching InboxService).
    let jobs = jobs_handle.lock().unwrap().clone();
    assert_eq!(
        jobs.len(),
        1,
        "the remote recipient must produce exactly one queue job, and the local recipient must \
         produce none (no queue job carries the local recipient's delivery)"
    );
    let job = &jobs[0];
    assert_eq!(job.target_inbox, "https://remote.example/inbox");

    // Convergence: the exact same canonical Activity this one deliver() call
    // produced reached both the real dispatcher (local, in-process) and the
    // real queue (remote), content-identical.
    let dispatched_activity =
        handler.last_activity.lock().unwrap().clone().expect(
            "the dispatcher must have captured the Activity handed to it via process_local",
        );
    assert_eq!(
        job.activity, dispatched_activity.raw,
        "the local dispatcher and the remote queue job must observe the identical canonical \
         Activity from the same single deliver() call (Requirement 10.5)"
    );
    assert_eq!(
        dispatched_activity.id,
        "https://kawasemi.example/activities/convergence-1"
    );
    assert_eq!(dispatched_activity.activity_type, "Create");
    assert_eq!(
        dispatched_activity
            .raw
            .get("@context")
            .and_then(|v| v.as_str()),
        Some("https://www.w3.org/ns/activitystreams"),
        "the canonical Activity both paths observed must carry the stamped @context"
    );
}
