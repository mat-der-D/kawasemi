//! Unit/component tests for `InboxService` (Requirements 6.4, 7.1, 7.2, 7.3,
//! 7.4, 9.3, 12.1, 12.2), per task 4.1's observable completion condition:
//! "検証失敗が認証失敗・ブロック署名者が拒否・必須欠落が不正・重複が再処理
//! なしになり、リモート経路とローカル経路が同一のブロック判定・重複排除・
//! ディスパッチ処理に合流する統合テストが通る".
//!
//! Pure in-memory logic against hand-written mocks for
//! `SignatureVerifier`/`BlockPolicy`/`ReceivedActivityStore` (all three are
//! non-`dyn`-safe generic traits in this crate, so `InboxService` is generic
//! over them — see `service.rs`'s own doc comment) plus the real,
//! non-generic `InboundActivityDispatcher` with a counting stub handler, and
//! a real `ActorUrls`. No DB, no HTTP — plain `#[tokio::test]` unit tests,
//! matching this spec's established precedent for component-scoped tests
//! (e.g. `dispatcher/tests.rs`, `block_policy/tests.rs`).

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::http::Method;
use serde_json::json;

use super::*;
use crate::federation::inbound::dispatcher::{HandleOutcome, InboundActivityHandler};
use crate::federation::signatures::IncomingRequest;

const REMOTE_SIGNER_ACTOR_URI: &str = "https://remote.example/actors/alice";
const REMOTE_SIGNER_KEY_ID: &str = "https://remote.example/actors/alice#main-key";
const INBOX_URL: &str = "https://kawasemi.example/inbox";

fn remote_signer() -> VerifiedSigner {
    VerifiedSigner {
        key_id: REMOTE_SIGNER_KEY_ID.to_string(),
        actor_uri: REMOTE_SIGNER_ACTOR_URI.to_string(),
    }
}

fn activity_body(id: &str, activity_type: &str) -> Vec<u8> {
    json!({ "id": id, "type": activity_type })
        .to_string()
        .into_bytes()
}

fn activity(id: &str, activity_type: &str) -> ParsedActivity {
    ParsedActivity {
        id: id.to_string(),
        activity_type: activity_type.to_string(),
        raw: json!({ "id": id, "type": activity_type }),
    }
}

fn incoming_request(body: Option<Vec<u8>>) -> IncomingRequest {
    let req = IncomingRequest::new(Method::POST, INBOX_URL);
    match body {
        Some(body) => req.with_body(body),
        None => req,
    }
}

// --- Mock SignatureVerifier ---

enum VerifierBehavior {
    Succeed(VerifiedSigner),
    Fail,
}

struct MockVerifier {
    behavior: VerifierBehavior,
    calls: AtomicUsize,
}

impl MockVerifier {
    fn succeeding(signer: VerifiedSigner) -> Self {
        Self {
            behavior: VerifierBehavior::Succeed(signer),
            calls: AtomicUsize::new(0),
        }
    }

    fn failing() -> Self {
        Self {
            behavior: VerifierBehavior::Fail,
            calls: AtomicUsize::new(0),
        }
    }

    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl SignatureVerifier for MockVerifier {
    async fn verify_request(&self, _req: &IncomingRequest) -> Result<VerifiedSigner, AppError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match &self.behavior {
            VerifierBehavior::Succeed(signer) => Ok(signer.clone()),
            VerifierBehavior::Fail => Err(AppError::client(
                StatusCode::UNAUTHORIZED,
                "mock signature verification failure",
            )),
        }
    }
}

// --- Mock BlockPolicy ---

struct MockBlockPolicy {
    blocked_actor_uris: Vec<String>,
    calls: AtomicUsize,
    last_context: Mutex<Option<LocalRecipientContext>>,
}

impl MockBlockPolicy {
    fn blocking(actor_uris: &[&str]) -> Self {
        Self {
            blocked_actor_uris: actor_uris.iter().map(|s| s.to_string()).collect(),
            calls: AtomicUsize::new(0),
            last_context: Mutex::new(None),
        }
    }

    fn never_blocking() -> Self {
        Self::blocking(&[])
    }

    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn last_context(&self) -> Option<LocalRecipientContext> {
        self.last_context.lock().unwrap().clone()
    }
}

impl BlockPolicy for MockBlockPolicy {
    async fn is_blocked(
        &self,
        actor_uri: &str,
        local_recipient: LocalRecipientContext,
    ) -> Result<bool, AppError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.last_context.lock().unwrap() = Some(local_recipient);
        Ok(self
            .blocked_actor_uris
            .iter()
            .any(|blocked| blocked == actor_uri))
    }
}

// --- Mock ReceivedActivityStore ---

struct MockDedupStore {
    seen: Mutex<HashSet<String>>,
    calls: AtomicUsize,
}

impl MockDedupStore {
    fn new() -> Self {
        Self {
            seen: Mutex::new(HashSet::new()),
            calls: AtomicUsize::new(0),
        }
    }

    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl ReceivedActivityStore for MockDedupStore {
    async fn record_if_new(&self, activity_id: &str) -> Result<bool, AppError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.seen.lock().unwrap().insert(activity_id.to_string()))
    }

    async fn prune_expired(&self) -> Result<u64, AppError> {
        Ok(0)
    }
}

// --- Counting stub InboundActivityHandler (mirrors dispatcher/tests.rs's own StubHandler) ---

struct CountingHandler {
    types: Vec<&'static str>,
    invocations: std::sync::Arc<AtomicUsize>,
}

impl InboundActivityHandler for CountingHandler {
    fn activity_types(&self) -> &[&str] {
        &self.types
    }

    fn handle<'a>(
        &'a self,
        _activity: &'a ParsedActivity,
        _ctx: &'a InboundContext,
    ) -> Pin<Box<dyn Future<Output = Result<HandleOutcome, AppError>> + Send + 'a>> {
        Box::pin(async move {
            self.invocations.fetch_add(1, Ordering::SeqCst);
            Ok(HandleOutcome::Handled)
        })
    }
}

/// Builds a dispatcher with a single "Follow"-registered handler, plus the
/// shared invocation counter so tests can assert whether dispatch actually
/// ran.
fn dispatcher_with_counting_handler() -> (InboundActivityDispatcher, std::sync::Arc<AtomicUsize>) {
    let invocations = std::sync::Arc::new(AtomicUsize::new(0));
    let mut dispatcher = InboundActivityDispatcher::new();
    dispatcher.register(std::sync::Arc::new(CountingHandler {
        types: vec!["Follow"],
        invocations: std::sync::Arc::clone(&invocations),
    }));
    (dispatcher, invocations)
}

fn actor_urls() -> ActorUrls {
    ActorUrls::new("kawasemi.example")
}

// --- 1: process_inbound rejects an unverifiable signature before anything else runs ---

#[tokio::test]
async fn process_inbound_rejects_unauthorized_signature_before_later_stages() {
    let verifier = MockVerifier::failing();
    let block_policy = MockBlockPolicy::never_blocking();
    let dedup = MockDedupStore::new();
    let (dispatcher, invocations) = dispatcher_with_counting_handler();
    let service = InboxService::new(verifier, block_policy, dedup, dispatcher, actor_urls());

    let req = incoming_request(Some(activity_body(
        "https://remote.example/activities/1",
        "Follow",
    )));
    let result = service
        .process_inbound(req, LocalRecipientContext::SharedInbox)
        .await;

    let err = result.expect_err("an unverifiable signature must be rejected");
    assert_eq!(
        err.status,
        StatusCode::UNAUTHORIZED,
        "signature verification failure must surface as an authentication failure (Requirement 7.2)"
    );
    assert_eq!(
        service.block_policy.call_count(),
        0,
        "block judgment must never run before signature verification succeeds"
    );
    assert_eq!(
        service.dedup.call_count(),
        0,
        "dedup must never run before signature verification succeeds"
    );
    assert_eq!(
        invocations.load(Ordering::SeqCst),
        0,
        "dispatch must never run before signature verification succeeds"
    );
}

// --- 2: process_inbound rejects a body missing required properties before block/dedup/dispatch ---

#[tokio::test]
async fn process_inbound_rejects_missing_required_properties_before_later_stages() {
    let verifier = MockVerifier::succeeding(remote_signer());
    let block_policy = MockBlockPolicy::never_blocking();
    let dedup = MockDedupStore::new();
    let (dispatcher, invocations) = dispatcher_with_counting_handler();
    let service = InboxService::new(verifier, block_policy, dedup, dispatcher, actor_urls());

    // Missing "id" (Requirement 9.3).
    let malformed_body = json!({ "type": "Follow" }).to_string().into_bytes();
    let req = incoming_request(Some(malformed_body));
    let result = service
        .process_inbound(req, LocalRecipientContext::SharedInbox)
        .await;

    let err = result.expect_err("a body missing a required property must be rejected");
    assert_eq!(
        err.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "missing required properties must be rejected as malformed (Requirement 9.3)"
    );
    assert_eq!(
        service.block_policy.call_count(),
        0,
        "block judgment must never run on a malformed document"
    );
    assert_eq!(
        service.dedup.call_count(),
        0,
        "dedup must never run on a malformed document"
    );
    assert_eq!(invocations.load(Ordering::SeqCst), 0);
}

// --- 3: process_inbound rejects a missing body the same way as missing required properties ---

#[tokio::test]
async fn process_inbound_rejects_a_missing_body() {
    let verifier = MockVerifier::succeeding(remote_signer());
    let block_policy = MockBlockPolicy::never_blocking();
    let dedup = MockDedupStore::new();
    let (dispatcher, invocations) = dispatcher_with_counting_handler();
    let service = InboxService::new(verifier, block_policy, dedup, dispatcher, actor_urls());

    let req = incoming_request(None);
    let result = service
        .process_inbound(req, LocalRecipientContext::SharedInbox)
        .await;

    let err = result.expect_err("a bodyless inbox POST must be rejected");
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(service.block_policy.call_count(), 0);
    assert_eq!(invocations.load(Ordering::SeqCst), 0);
}

// --- 4: process_inbound rejects a blocked signer before dedup/dispatch ---

#[tokio::test]
async fn process_inbound_rejects_a_blocked_signer_before_dedup_and_dispatch() {
    let verifier = MockVerifier::succeeding(remote_signer());
    let block_policy = MockBlockPolicy::blocking(&[REMOTE_SIGNER_ACTOR_URI]);
    let dedup = MockDedupStore::new();
    let (dispatcher, invocations) = dispatcher_with_counting_handler();
    let service = InboxService::new(verifier, block_policy, dedup, dispatcher, actor_urls());

    let req = incoming_request(Some(activity_body(
        "https://remote.example/activities/1",
        "Follow",
    )));
    let destination = LocalRecipientContext::Actor {
        actor_uri: "https://kawasemi.example/users/owner".to_string(),
    };
    let result = service.process_inbound(req, destination.clone()).await;

    let err = result.expect_err("a blocked signer must be rejected");
    assert_eq!(
        err.status,
        StatusCode::FORBIDDEN,
        "a blocked signer must be rejected (Requirement 12.2)"
    );
    assert_eq!(
        service.block_policy.last_context(),
        Some(destination),
        "process_inbound must forward the caller-supplied destination context to BlockPolicy \
         unchanged"
    );
    assert_eq!(
        service.dedup.call_count(),
        0,
        "dedup must never run once the signer is judged blocked"
    );
    assert_eq!(invocations.load(Ordering::SeqCst), 0);
}

// --- 5: process_inbound accepts and dispatches a fresh, allowed Activity ---

#[tokio::test]
async fn process_inbound_accepts_and_dispatches_a_fresh_activity() {
    let verifier = MockVerifier::succeeding(remote_signer());
    let block_policy = MockBlockPolicy::never_blocking();
    let dedup = MockDedupStore::new();
    let (dispatcher, invocations) = dispatcher_with_counting_handler();
    let service = InboxService::new(verifier, block_policy, dedup, dispatcher, actor_urls());

    let req = incoming_request(Some(activity_body(
        "https://remote.example/activities/1",
        "Follow",
    )));
    let outcome = service
        .process_inbound(req, LocalRecipientContext::SharedInbox)
        .await
        .expect("a fresh, allowed Activity must be accepted");

    assert_eq!(outcome, InboxOutcome::Accepted);
    assert_eq!(
        invocations.load(Ordering::SeqCst),
        1,
        "a fresh Activity must be dispatched exactly once (Requirement 7.3)"
    );
}

// --- 6: process_inbound treats a repeat Activity id as a duplicate, never re-dispatching ---

#[tokio::test]
async fn process_inbound_treats_a_repeat_activity_id_as_duplicate_without_reprocessing() {
    let verifier = MockVerifier::succeeding(remote_signer());
    let block_policy = MockBlockPolicy::never_blocking();
    let dedup = MockDedupStore::new();
    let (dispatcher, invocations) = dispatcher_with_counting_handler();
    let service = InboxService::new(verifier, block_policy, dedup, dispatcher, actor_urls());

    let body = || activity_body("https://remote.example/activities/1", "Follow");

    let first = service
        .process_inbound(
            incoming_request(Some(body())),
            LocalRecipientContext::SharedInbox,
        )
        .await
        .expect("the first delivery of this Activity id must succeed");
    assert_eq!(first, InboxOutcome::Accepted);
    assert_eq!(invocations.load(Ordering::SeqCst), 1);

    let second = service
        .process_inbound(
            incoming_request(Some(body())),
            LocalRecipientContext::SharedInbox,
        )
        .await
        .expect("a repeated delivery of the same Activity id must still succeed (no error)");
    assert_eq!(
        second,
        InboxOutcome::Duplicate,
        "a repeated Activity id must be reported as a duplicate (Requirement 7.4)"
    );
    assert_eq!(
        invocations.load(Ordering::SeqCst),
        1,
        "business-logic dispatch must not run a second time for a duplicate Activity"
    );
    assert_eq!(
        service.block_policy.call_count(),
        2,
        "block judgment still runs on every delivery, including duplicates -- it is not skipped \
         just because dedup would later short-circuit"
    );
    assert_eq!(service.dedup.call_count(), 2);
}

// --- 7: block judgment happens before dedup, even when both conditions would otherwise apply ---

#[tokio::test]
async fn block_judgment_takes_priority_over_a_known_duplicate_id() {
    const ACTIVITY_ID: &str = "https://remote.example/activities/1";

    // Seed the dedup store so this Activity id is already known, as if a
    // prior delivery had already recorded (and dispatched) it -- if dedup
    // ran before block judgment, this call would resolve to `Duplicate`.
    let dedup = MockDedupStore::new();
    dedup
        .record_if_new(ACTIVITY_ID)
        .await
        .expect("seeding the dedup store must succeed");

    let verifier = MockVerifier::succeeding(remote_signer());
    let block_policy = MockBlockPolicy::blocking(&[REMOTE_SIGNER_ACTOR_URI]);
    let (dispatcher, invocations) = dispatcher_with_counting_handler();
    let service = InboxService::new(verifier, block_policy, dedup, dispatcher, actor_urls());

    let result = service
        .process_inbound(
            incoming_request(Some(activity_body(ACTIVITY_ID, "Follow"))),
            LocalRecipientContext::SharedInbox,
        )
        .await;

    let err = result.expect_err(
        "when both a blocked signer and an already-known Activity id apply, the block \
         rejection must win -- block judgment runs strictly before dedup",
    );
    assert_eq!(err.status, StatusCode::FORBIDDEN);
    assert_eq!(
        service.dedup.call_count(),
        1,
        "dedup was only consulted once, during the seeding call above -- the actual \
         process_inbound call under test must never reach it once the signer is judged blocked"
    );
    assert_eq!(invocations.load(Ordering::SeqCst), 0);
}

// --- 8: process_local never invokes signature verification ---

#[tokio::test]
async fn process_local_never_invokes_signature_verification() {
    let local_signer = VerifiedSigner {
        key_id: "https://kawasemi.example/users/owner#main-key".to_string(),
        actor_uri: "https://kawasemi.example/users/owner".to_string(),
    };
    let verifier = MockVerifier::succeeding(local_signer.clone());
    let block_policy = MockBlockPolicy::never_blocking();
    let dedup = MockDedupStore::new();
    let (dispatcher, invocations) = dispatcher_with_counting_handler();
    let service = InboxService::new(verifier, block_policy, dedup, dispatcher, actor_urls());

    let recipient = crate::actor::Handle::new("bob").expect("valid handle");
    let outcome = service
        .process_local(
            activity("https://kawasemi.example/activities/1", "Follow"),
            local_signer,
            recipient,
        )
        .await
        .expect("a fresh, allowed local Activity must be accepted");

    assert_eq!(outcome, InboxOutcome::Accepted);
    assert_eq!(
        service.verifier.call_count(),
        0,
        "process_local must never call SignatureVerifier -- it is the same semantic path minus \
         signature verification"
    );
    assert_eq!(invocations.load(Ordering::SeqCst), 1);
}

// --- 9: process_local always judges block using LocalRecipientContext::Actor, built from ActorUrls ---

#[tokio::test]
async fn process_local_always_uses_actor_context_built_from_recipient_handle() {
    let local_signer = VerifiedSigner {
        key_id: "https://kawasemi.example/users/owner#main-key".to_string(),
        actor_uri: "https://kawasemi.example/users/owner".to_string(),
    };
    let verifier = MockVerifier::succeeding(local_signer.clone());
    let block_policy = MockBlockPolicy::never_blocking();
    let dedup = MockDedupStore::new();
    let (dispatcher, _invocations) = dispatcher_with_counting_handler();
    let urls = actor_urls();
    let recipient = crate::actor::Handle::new("bob").expect("valid handle");
    let expected_actor_uri = urls.actor_url(&recipient);
    let service = InboxService::new(verifier, block_policy, dedup, dispatcher, urls);

    service
        .process_local(
            activity("https://kawasemi.example/activities/1", "Follow"),
            local_signer,
            recipient,
        )
        .await
        .expect("must be accepted");

    assert_eq!(
        service.block_policy.last_context(),
        Some(LocalRecipientContext::Actor {
            actor_uri: expected_actor_uri
        }),
        "process_local must always judge with LocalRecipientContext::Actor built from the \
         recipient Handle via ActorUrls, never SharedInbox (in-process delivery always has a \
         resolved destination local actor)"
    );
}

// --- 10: process_local rejects a blocked signer identically to process_inbound ---

#[tokio::test]
async fn process_local_rejects_a_blocked_signer() {
    let local_signer = VerifiedSigner {
        key_id: "https://remote.example/actors/mallory#main-key".to_string(),
        actor_uri: "https://remote.example/actors/mallory".to_string(),
    };
    let recipient = crate::actor::Handle::new("bob").expect("valid handle");
    let urls = actor_urls();
    // This local delivery's destination actor_uri is the *recipient's* own
    // actor URL, not the signer's -- BlockPolicy blocks based on the
    // signer's actor_uri, which is unrelated to the recipient here, so
    // block instead on the signer's actor_uri directly (mirrors how a real
    // BlockPolicy would judge "is this sender blocked by this recipient").
    let verifier = MockVerifier::succeeding(local_signer.clone());
    let block_policy = MockBlockPolicy::blocking(&["https://remote.example/actors/mallory"]);
    let dedup = MockDedupStore::new();
    let (dispatcher, invocations) = dispatcher_with_counting_handler();
    let service = InboxService::new(verifier, block_policy, dedup, dispatcher, urls);

    let result = service
        .process_local(
            activity("https://kawasemi.example/activities/1", "Follow"),
            local_signer,
            recipient,
        )
        .await;

    let err = result.expect_err("a blocked signer's locally-delivered Activity must be rejected");
    assert_eq!(err.status, StatusCode::FORBIDDEN);
    assert_eq!(invocations.load(Ordering::SeqCst), 0);
}

// --- 11: process_inbound (post-verification) and process_local converge on identical dedup state ---

#[tokio::test]
async fn process_inbound_and_process_local_converge_on_the_same_dedup_and_dispatch_state() {
    let remote_verifier = MockVerifier::succeeding(remote_signer());
    let block_policy = MockBlockPolicy::never_blocking();
    let dedup = MockDedupStore::new();
    let (dispatcher, invocations) = dispatcher_with_counting_handler();
    let service = InboxService::new(
        remote_verifier,
        block_policy,
        dedup,
        dispatcher,
        actor_urls(),
    );

    const SHARED_ACTIVITY_ID: &str = "https://remote.example/activities/shared-1";

    // First seen via the *remote* path (process_inbound).
    let remote_outcome = service
        .process_inbound(
            incoming_request(Some(activity_body(SHARED_ACTIVITY_ID, "Follow"))),
            LocalRecipientContext::SharedInbox,
        )
        .await
        .expect("first remote delivery must be accepted");
    assert_eq!(remote_outcome, InboxOutcome::Accepted);
    assert_eq!(invocations.load(Ordering::SeqCst), 1);

    // The very same Activity id, now arriving via the *local* in-process
    // path (process_local) -- e.g. a hypothetical DeliveryService handing
    // the same logical Activity to a local recipient after it was already
    // recorded via the remote path. Because `ReceivedActivityStore` is
    // shared across both entry points on this one `InboxService`, this must
    // be reported as a duplicate and must NOT dispatch again -- proving
    // both entry points converge on the exact same dedup/dispatch state,
    // not two independent copies of the pipeline.
    let local_signer = VerifiedSigner {
        key_id: "https://kawasemi.example/users/owner#main-key".to_string(),
        actor_uri: "https://kawasemi.example/users/owner".to_string(),
    };
    let recipient = crate::actor::Handle::new("bob").expect("valid handle");
    let local_outcome = service
        .process_local(
            activity(SHARED_ACTIVITY_ID, "Follow"),
            local_signer,
            recipient,
        )
        .await
        .expect("the local path must still succeed (no error), just without reprocessing");

    assert_eq!(
        local_outcome,
        InboxOutcome::Duplicate,
        "the same Activity id already recorded via process_inbound must be reported as a \
         duplicate when it later arrives via process_local -- both entry points share the same \
         ReceivedActivityStore state because they converge on the same process_verified method"
    );
    assert_eq!(
        invocations.load(Ordering::SeqCst),
        1,
        "dispatch must still have run exactly once total across both entry points -- \
         process_local must not re-dispatch an Activity id process_inbound already processed"
    );
}
