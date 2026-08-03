//! Component tests for `DeliveryService` (Requirements 10.1, 10.2, 10.3,
//! 10.4, 10.5, 11.1), per task 4.2's observable completion condition: "同一
//! 配送依頼で local 経路と remote 経路が同一の正規 Activity を扱い、local は
//! キューを介さず受信意味論経路へ、remote はキューへ投入される統合テストが
//! 通る".
//!
//! Exercises `DeliveryService::deliver` against plain in-memory
//! [`DeliverySink`] test doubles ([`RecordingSink`]) rather than a real
//! `InboxService`/`DeliveryQueue` — `LocalDeliverySink`/`HttpDeliverySink`
//! themselves are unit-tested against their real dependencies in
//! `sink/tests.rs`; this file's job is proving `DeliveryService`'s own
//! contract (the common-part-once invariant and the branch-only-on-target
//! rule), which is fully observable at the `DeliverySink` trait boundary
//! without needing a live `InboxService`/Postgres-backed `DeliveryQueue`
//! (mirrors this spec's established precedent, e.g. `target/tests.rs`, for
//! pure in-memory component tests where a trait boundary already isolates
//! the behavior under test).

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use serde_json::json;

use super::*;
use crate::actor::{ActorState, ActorType, ResolvedActor};
use crate::domain::Id;
use crate::error::ErrorKind;

// --- Recording DeliverySink test double ---

struct RecordingSink {
    calls: Mutex<Vec<(DeliveryTarget, CanonicalActivity, Handle)>>,
    fail: bool,
}

impl RecordingSink {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            fail: false,
        }
    }

    fn failing() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            fail: true,
        }
    }

    fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
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
        if self.fail {
            return Err(AppError::client(
                StatusCode::BAD_REQUEST,
                "RecordingSink configured to fail",
            ));
        }
        Ok(())
    }
}

// --- Counting LocalActorLookup test double (mirrors target/tests.rs's
// MockLocalActorLookup, plus a call counter to prove recipient resolution
// runs once, not per resolved target). The counter lives behind a shared
// `Arc` handed back separately from the lookup itself, since the lookup is
// moved into `RecipientTargetResolver` (whose own `directory` field is
// private to `target.rs`) and this test module has no way to reach back
// into it otherwise. ---

struct CountingLocalActorLookup {
    known_handles: HashSet<String>,
    calls: Arc<AtomicUsize>,
}

impl CountingLocalActorLookup {
    /// Returns the lookup (to be moved into a `RecipientTargetResolver`) and
    /// a cloned handle to its call counter (for the test to assert against
    /// independently).
    fn with_handles(handles: &[&str]) -> (Self, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let lookup = Self {
            known_handles: handles.iter().map(|h| (*h).to_string()).collect(),
            calls: Arc::clone(&calls),
        };
        (lookup, calls)
    }
}

impl LocalActorLookup for CountingLocalActorLookup {
    async fn resolve_actor_by_handle(
        &self,
        handle: &Handle,
    ) -> Result<Option<ResolvedActor>, AppError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
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

fn handle(raw: &str) -> Handle {
    Handle::new(raw).expect("test handle must be valid")
}

fn sample_activity(id: &str) -> serde_json::Value {
    json!({ "id": id, "type": "Create" })
}

fn remote(inbox: &str) -> Recipient {
    Recipient::Remote {
        inbox: inbox.to_string(),
        shared_inbox: None,
    }
}

fn service(
    lookup: CountingLocalActorLookup,
    local_sink: RecordingSink,
    http_sink: RecordingSink,
) -> DeliveryService<CountingLocalActorLookup, RecordingSink, RecordingSink> {
    DeliveryService::new(RecipientTargetResolver::new(lookup), local_sink, http_sink)
}

// --- 1: a mixed local+remote request hands the identical canonical
// Activity to both sinks (Requirement 10.5), branching only on target
// (Requirements 10.3, 10.4). ---

#[tokio::test]
async fn deliver_hands_the_identical_canonical_activity_to_local_and_remote_sinks() {
    let (lookup, _lookup_calls) = CountingLocalActorLookup::with_handles(&["alice"]);
    let local_sink = RecordingSink::new();
    let http_sink = RecordingSink::new();
    let service = service(lookup, local_sink, http_sink);

    let req = DeliveryRequest {
        activity: sample_activity("https://kawasemi.example/activities/1"),
        sender: handle("owner"),
        recipients: vec![
            Recipient::Local(handle("alice")),
            remote("https://remote-a.example/inbox"),
            remote("https://remote-b.example/inbox"),
        ],
    };

    service.deliver(req).await.expect("deliver must succeed");

    let local_calls = service.local_sink.calls();
    let http_calls = service.http_sink.calls();

    assert_eq!(local_calls.len(), 1, "exactly one local target dispatched");
    assert_eq!(
        http_calls.len(),
        2,
        "exactly two distinct remote targets dispatched"
    );

    assert_eq!(
        local_calls[0].0,
        DeliveryTarget::Local {
            handle: handle("alice")
        }
    );
    assert_eq!(local_calls[0].2, handle("owner"));

    // Requirement 10.5: the exact same canonical Activity (content-equal,
    // and -- since both are clones of the single value `deliver()`
    // constructed once -- provably not two independently re-derived copies)
    // reaches both the local dispatch and every remote dispatch.
    let canonical = &local_calls[0].1;
    for (_, http_activity, http_sender) in &http_calls {
        assert_eq!(
            http_activity, canonical,
            "local and remote dispatches must observe the identical canonical Activity"
        );
        assert_eq!(http_sender, &handle("owner"));
    }
    assert_eq!(
        canonical.parsed().id,
        "https://kawasemi.example/activities/1"
    );
    assert_eq!(canonical.parsed().activity_type, "Create");
    assert_eq!(
        canonical
            .as_value()
            .get("@context")
            .and_then(|v| v.as_str()),
        Some("https://www.w3.org/ns/activitystreams"),
        "the canonical Activity must carry the stamped @context (Requirement 9.1, via the \
         common part's one-time serialize+validate step)"
    );
}

// --- 2: a malformed activity fails validation before any sink or recipient
// resolution runs (Requirements 10.1, 10.2). ---

#[tokio::test]
async fn deliver_rejects_a_malformed_activity_before_any_sink_or_resolution_runs() {
    let (lookup, lookup_calls) = CountingLocalActorLookup::with_handles(&["alice"]);
    let local_sink = RecordingSink::new();
    let http_sink = RecordingSink::new();
    let service = service(lookup, local_sink, http_sink);

    // Missing required `id`/`type` (Requirement 9.3) -- deliver() must fail
    // at the one-time canonical-generation step, strictly before recipient
    // resolution or any per-target dispatch.
    let req = DeliveryRequest {
        activity: json!({}),
        sender: handle("owner"),
        recipients: vec![
            Recipient::Local(handle("alice")),
            remote("https://remote-a.example/inbox"),
        ],
    };

    let err = service
        .deliver(req)
        .await
        .expect_err("a malformed activity must be rejected");

    assert_eq!(err.kind, ErrorKind::Client);
    assert_eq!(
        service.local_sink.call_count(),
        0,
        "no sink may run before the common part validates the Activity"
    );
    assert_eq!(service.http_sink.call_count(), 0);
    assert_eq!(
        lookup_calls.load(Ordering::SeqCst),
        0,
        "recipient resolution must not run either -- validation happens strictly first"
    );
}

// --- 3: recipient resolution runs exactly once per deliver() call, not once
// per resolved target (Requirements 10.1, 10.2). ---

#[tokio::test]
async fn deliver_resolves_recipients_exactly_once_regardless_of_target_count() {
    let (lookup, lookup_calls) = CountingLocalActorLookup::with_handles(&["alice"]);
    let local_sink = RecordingSink::new();
    let http_sink = RecordingSink::new();
    let service = service(lookup, local_sink, http_sink);

    let req = DeliveryRequest {
        activity: sample_activity("https://kawasemi.example/activities/2"),
        sender: handle("owner"),
        recipients: vec![
            Recipient::Local(handle("alice")),
            remote("https://remote-a.example/inbox"),
            remote("https://remote-b.example/inbox"),
            remote("https://remote-c.example/inbox"),
        ],
    };

    service.deliver(req).await.expect("deliver must succeed");

    assert_eq!(
        lookup_calls.load(Ordering::SeqCst),
        1,
        "the single local recipient must be looked up exactly once -- resolution is a single \
         pass over all recipients, not repeated once per resulting DeliveryTarget in the \
         dispatch loop (three remote targets did not inflate this count)"
    );
    assert_eq!(service.local_sink.call_count(), 1);
    assert_eq!(service.http_sink.call_count(), 3);
}

// --- 4: a local-only request never invokes the HTTP sink (Requirement 10.3). ---

#[tokio::test]
async fn local_only_recipients_never_invoke_the_http_sink() {
    let (lookup, _lookup_calls) = CountingLocalActorLookup::with_handles(&["alice", "bob"]);
    let local_sink = RecordingSink::new();
    let http_sink = RecordingSink::new();
    let service = service(lookup, local_sink, http_sink);

    let req = DeliveryRequest {
        activity: sample_activity("https://kawasemi.example/activities/3"),
        sender: handle("owner"),
        recipients: vec![
            Recipient::Local(handle("alice")),
            Recipient::Local(handle("bob")),
        ],
    };

    service.deliver(req).await.expect("deliver must succeed");

    assert_eq!(service.local_sink.call_count(), 2);
    assert_eq!(
        service.http_sink.call_count(),
        0,
        "a purely local delivery must never touch the HTTP/queue sink"
    );
}

// --- 5: a remote-only request never invokes the local sink (Requirement 10.4). ---

#[tokio::test]
async fn remote_only_recipients_never_invoke_the_local_sink() {
    let (lookup, _lookup_calls) = CountingLocalActorLookup::with_handles(&[]);
    let local_sink = RecordingSink::new();
    let http_sink = RecordingSink::new();
    let service = service(lookup, local_sink, http_sink);

    let req = DeliveryRequest {
        activity: sample_activity("https://kawasemi.example/activities/4"),
        sender: handle("owner"),
        recipients: vec![remote("https://remote-a.example/inbox")],
    };

    service.deliver(req).await.expect("deliver must succeed");

    assert_eq!(
        service.local_sink.call_count(),
        0,
        "a purely remote delivery must never touch the in-process local sink"
    );
    assert_eq!(service.http_sink.call_count(), 1);
}

// --- 6: a sink failure propagates instead of being swallowed. ---

#[tokio::test]
async fn a_sink_failure_propagates_from_deliver() {
    let (lookup, _lookup_calls) = CountingLocalActorLookup::with_handles(&["alice"]);
    let local_sink = RecordingSink::failing();
    let http_sink = RecordingSink::new();
    let service = service(lookup, local_sink, http_sink);

    let req = DeliveryRequest {
        activity: sample_activity("https://kawasemi.example/activities/5"),
        sender: handle("owner"),
        recipients: vec![Recipient::Local(handle("alice"))],
    };

    let err = service
        .deliver(req)
        .await
        .expect_err("a failing sink's error must propagate from deliver(), not be swallowed");
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
}
