//! Unit tests for `InboundActivityDispatcher` (Requirements 7.3, 7.5, 7.6),
//! per task 3.2's observable completion condition: "スタブハンドラを登録す
//! ると対応種別がそれへ委譲される" plus design.md's multimap/fan-out/
//! exactly-one-or-zero contract.
//!
//! Pure in-memory logic — no DB, no HTTP; plain `#[tokio::test]` unit tests.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;

use super::*;
use crate::federation::signatures::VerifiedSigner;

fn activity(activity_type: &str) -> ParsedActivity {
    ParsedActivity {
        id: format!("https://remote.example/activities/{activity_type}"),
        activity_type: activity_type.to_string(),
        raw: json!({ "type": activity_type }),
    }
}

fn ctx() -> InboundContext {
    InboundContext {
        signer: VerifiedSigner {
            key_id: "https://remote.example/actors/alice#main-key".to_string(),
            actor_uri: "https://remote.example/actors/alice".to_string(),
        },
    }
}

/// A stub handler that counts invocations and always returns a fixed
/// `HandleOutcome` — used to observe whether `dispatch` actually invoked it.
struct StubHandler {
    types: Vec<&'static str>,
    outcome: HandleOutcome,
    invocations: Arc<AtomicUsize>,
}

impl StubHandler {
    fn new(types: Vec<&'static str>, outcome: HandleOutcome) -> (Arc<Self>, Arc<AtomicUsize>) {
        let invocations = Arc::new(AtomicUsize::new(0));
        (
            Arc::new(Self {
                types,
                outcome,
                invocations: Arc::clone(&invocations),
            }),
            invocations,
        )
    }
}

impl InboundActivityHandler for StubHandler {
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
            Ok(self.outcome)
        })
    }
}

// --- 1: unregistered type is a safe no-op ---

#[tokio::test]
async fn dispatch_of_unregistered_type_does_not_error() {
    let dispatcher = InboundActivityDispatcher::new();
    let activity = activity("Follow");

    let result = dispatcher.dispatch(&activity, &ctx()).await;

    assert!(
        result.is_ok(),
        "an outer type with no registered handler must be a safe no-op (receive-only), \
         not an error: {result:?}"
    );
}

// --- 2: single handler dispatch ---

#[tokio::test]
async fn dispatch_invokes_the_single_registered_handler_for_its_type() {
    let mut dispatcher = InboundActivityDispatcher::new();
    let (handler, invocations) = StubHandler::new(vec!["Follow"], HandleOutcome::Handled);
    dispatcher.register(handler);

    let result = dispatcher.dispatch(&activity("Follow"), &ctx()).await;

    assert!(result.is_ok());
    assert_eq!(
        invocations.load(Ordering::SeqCst),
        1,
        "the registered handler for \"Follow\" must actually be invoked exactly once"
    );
}

// --- 3: multimap fan-out, not overwrite ---

#[tokio::test]
async fn registering_two_handlers_for_the_same_type_fans_out_to_both() {
    let mut dispatcher = InboundActivityDispatcher::new();
    // Mirrors design.md's own example: statuses-core and social-graph both
    // register for "Undo" (Requirement 7.6).
    let (statuses_core_handler, statuses_core_invocations) =
        StubHandler::new(vec!["Undo"], HandleOutcome::Ignored);
    let (social_graph_handler, social_graph_invocations) =
        StubHandler::new(vec!["Undo"], HandleOutcome::Handled);
    dispatcher.register(statuses_core_handler);
    dispatcher.register(social_graph_handler);

    let result = dispatcher.dispatch(&activity("Undo"), &ctx()).await;

    assert!(result.is_ok());
    assert_eq!(
        statuses_core_invocations.load(Ordering::SeqCst),
        1,
        "registering a second handler for an already-registered type must not silently \
         overwrite the first (multimap, not one-handler-per-type)"
    );
    assert_eq!(
        social_graph_invocations.load(Ordering::SeqCst),
        1,
        "both handlers registered for the same outer type must be fanned out to"
    );
}

// --- 4: exactly-one-or-zero semantics ---

#[tokio::test]
async fn dispatch_succeeds_when_only_one_of_two_handlers_reports_handled() {
    let mut dispatcher = InboundActivityDispatcher::new();
    let (ignoring_handler, ignoring_invocations) =
        StubHandler::new(vec!["Undo"], HandleOutcome::Ignored);
    let (handling_handler, handling_invocations) =
        StubHandler::new(vec!["Undo"], HandleOutcome::Handled);
    dispatcher.register(ignoring_handler);
    dispatcher.register(handling_handler);

    let result = dispatcher.dispatch(&activity("Undo"), &ctx()).await;

    assert!(
        result.is_ok(),
        "exactly one Handled among fanned-out handlers must still succeed: {result:?}"
    );
    assert_eq!(ignoring_invocations.load(Ordering::SeqCst), 1);
    assert_eq!(handling_invocations.load(Ordering::SeqCst), 1);
}

// --- both handlers report Handled: still Ok(()), just a warning (not asserted here) ---

#[tokio::test]
async fn dispatch_still_succeeds_when_two_handlers_both_report_handled() {
    let mut dispatcher = InboundActivityDispatcher::new();
    let (first, first_invocations) = StubHandler::new(vec!["Undo"], HandleOutcome::Handled);
    let (second, second_invocations) = StubHandler::new(vec!["Undo"], HandleOutcome::Handled);
    dispatcher.register(first);
    dispatcher.register(second);

    let result = dispatcher.dispatch(&activity("Undo"), &ctx()).await;

    assert!(
        result.is_ok(),
        "double-Handled is a logged warning, not a fatal error: {result:?}"
    );
    assert_eq!(first_invocations.load(Ordering::SeqCst), 1);
    assert_eq!(second_invocations.load(Ordering::SeqCst), 1);
}
