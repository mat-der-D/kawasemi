//! Integration-style tests for `SignatureNegotiator` (Requirements 3.1, 3.2,
//! 3.3), per task 2.4's observable completion condition: "未知 host で片形式
//! →拒否→他形式再送が起き、成功後は記録形式が優先される単体/統合テストが通
//! る".
//!
//! Mirrors `src/federation/signatures/signer/tests.rs`'s and
//! `src/federation/signatures/key_resolver/tests.rs`'s established
//! convention: `spawn_test_app` for a real, already-migrated schema (so
//! these tests exercise the real `instance_signature_capabilities` table,
//! not a stand-in), a real actor/signing-key fixture created through
//! `ActorService::create_actor` (so signing genuinely succeeds), and
//! `MockFederationHttpClient` (task 1.4) so the "which format was sent, in
//! what order, how many times" assertions are fully deterministic without
//! any real network call.

use std::sync::Arc;

use axum::http::{HeaderMap, Method, StatusCode};
use time::OffsetDateTime;

use super::*;
use crate::actor::model::ActorType;
use crate::actor::owner::create_owner;
use crate::actor::{NewActor, ResolvedActor};
use crate::error::ErrorKind;
use crate::federation::signatures::http_client::MockFederationHttpClient;
use crate::federation::urls::ActorUrls;
use crate::test_harness::{TestApp, spawn_test_app};

/// Creates a real owner + a real local actor (via `ActorService::create_actor`,
/// which provisions a real, currently valid RSA-2048 signing key) under
/// `handle` — mirrors `signer/tests.rs`'s own `create_signable_actor`.
async fn create_signable_actor(app: &TestApp, handle: &str) -> ResolvedActor {
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
            display_name: "Negotiator Test Actor".to_string(),
            summary: String::new(),
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

/// Builds a `SignatureNegotiator` wired against `app`'s own real
/// `ActorDirectory`/`SigningKeyProvider`/`Clock`-backed `RequestSigner`, `app`'s
/// real pool (the `instance_signature_capabilities` capability store), and
/// `mock` as the send boundary — mirrors `signer/tests.rs`'s `signer_for`.
fn negotiator_for(
    app: &TestApp,
    mock: Arc<MockFederationHttpClient>,
) -> SignatureNegotiator<MockFederationHttpClient> {
    let signer = RequestSigner::new(
        app.actor.directory().clone(),
        app.runtime.keys.clone(),
        ActorUrls::new(app.state.config().server.domain.clone()),
        app.runtime.clock.clone(),
    );
    SignatureNegotiator::new(app.pool.clone(), mock, signer, app.runtime.clock.clone())
}

/// A minimal canned `HttpResponse` with `status` and no body — every test
/// here only cares about the status code driving negotiation decisions.
fn response(status: StatusCode) -> HttpResponse {
    HttpResponse {
        status,
        headers: HeaderMap::new(),
        body: Vec::new(),
    }
}

/// Reads the currently recorded `format` for `host` directly off
/// `instance_signature_capabilities`, bypassing `SignatureNegotiator`
/// entirely — an independent assertion channel so these tests are not just
/// checking the module's own read path against its own write path.
async fn recorded_format_row(pool: &sqlx::PgPool, host: &str) -> Option<String> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT format FROM instance_signature_capabilities WHERE host = $1")
            .bind(host)
            .fetch_optional(pool)
            .await
            .expect("reading instance_signature_capabilities directly must succeed");
    row.map(|(format,)| format)
}

/// Inserts a capability row for `host` directly, bypassing
/// `SignatureNegotiator` entirely — used to set up the "host already has a
/// recorded format" precondition independently of the module under test's
/// own write path.
async fn record_capability(
    pool: &sqlx::PgPool,
    host: &str,
    format: &str,
    updated_at: OffsetDateTime,
) {
    sqlx::query(
        "INSERT INTO instance_signature_capabilities (host, format, updated_at) \
         VALUES ($1, $2, $3)",
    )
    .bind(host)
    .bind(format)
    .bind(updated_at)
    .execute(pool)
    .await
    .expect("inserting the capability fixture directly must succeed");
}

/// A fresh unsigned `OutboundRequest` for a delivery attempt to `host`'s
/// inbox.
fn delivery_request(host: &str) -> OutboundRequest {
    OutboundRequest::new(Method::POST, format!("https://{host}/inbox"))
        .with_body(br#"{"type":"Create"}"#.to_vec())
}

// --- Requirement 3.1: unknown host, 401 on the default format -> retry with the other format ---

#[tokio::test]
async fn unknown_host_retries_with_other_format_after_401_and_records_success_format() {
    let app = spawn_test_app().await;
    let actor = create_signable_actor(&app, "double_knock_alice").await;
    let mock = Arc::new(MockFederationHttpClient::new());
    mock.queue_send_response(response(StatusCode::UNAUTHORIZED));
    mock.queue_send_response(response(StatusCode::OK));
    let negotiator = negotiator_for(&app, mock.clone());

    let result = negotiator
        .negotiate_and_send(
            &actor.handle,
            "remote.example",
            delivery_request("remote.example"),
        )
        .await
        .expect("negotiate_and_send must succeed once the retry gets a 2xx");

    assert_eq!(result.status, StatusCode::OK);

    let sent = mock.sent_requests();
    assert_eq!(sent.len(), 2, "exactly one retry, not a loop");

    // First attempt: draft-cavage (the default, Requirement 3.1's "既定") --
    // its distinguishing shape is a bare Signature header, no Signature-Input.
    assert!(
        sent[0].headers.contains_key("signature"),
        "first attempt must be signed"
    );
    assert!(
        !sent[0].headers.contains_key("signature-input"),
        "first attempt must use draft-cavage (no Signature-Input header): {:?}",
        sent[0].headers
    );

    // Second attempt: RFC 9421 -- distinguishing shape is Signature-Input present.
    assert!(
        sent[1].headers.contains_key("signature-input"),
        "second attempt must use RFC 9421 (Signature-Input header present): {:?}",
        sent[1].headers
    );
    assert!(sent[1].headers.contains_key("signature"));

    // The two attempts carry genuinely different signature material, not
    // merely "called twice" with identical headers.
    assert_ne!(
        sent[0].headers.get("signature"),
        sent[1].headers.get("signature"),
        "the two attempts must carry different signature values (different formats)"
    );

    assert_eq!(
        recorded_format_row(&app.pool, "remote.example")
            .await
            .as_deref(),
        Some("rfc9421"),
        "the format that actually succeeded (rfc9421) must be recorded"
    );

    app.cleanup().await;
}

// --- Requirement 3.3: a host with a recorded format uses it first, no probing ---

#[tokio::test]
async fn host_with_recorded_format_uses_it_first_and_sends_only_once_on_success() {
    let app = spawn_test_app().await;
    let actor = create_signable_actor(&app, "recorded_bob").await;
    record_capability(
        &app.pool,
        "known.example",
        "rfc9421",
        app.runtime.clock.now(),
    )
    .await;
    let mock = Arc::new(MockFederationHttpClient::new());
    mock.queue_send_response(response(StatusCode::OK));
    let negotiator = negotiator_for(&app, mock.clone());

    let result = negotiator
        .negotiate_and_send(
            &actor.handle,
            "known.example",
            delivery_request("known.example"),
        )
        .await
        .expect("negotiate_and_send must succeed on the first, recorded-format attempt");

    assert_eq!(result.status, StatusCode::OK);

    let sent = mock.sent_requests();
    assert_eq!(
        sent.len(),
        1,
        "a recorded format that succeeds immediately must not trigger any retry"
    );
    assert!(
        sent[0].headers.contains_key("signature-input"),
        "the recorded rfc9421 format must be used on the very first attempt: {:?}",
        sent[0].headers
    );

    app.cleanup().await;
}

// --- Requirement 3.1 (converse): a 403 (blocked) rejection is not signature-related ---

#[tokio::test]
async fn blocked_403_response_does_not_retry_or_record_capability() {
    let app = spawn_test_app().await;
    let actor = create_signable_actor(&app, "blocked_carol").await;
    let mock = Arc::new(MockFederationHttpClient::new());
    mock.queue_send_response(response(StatusCode::FORBIDDEN));
    let negotiator = negotiator_for(&app, mock.clone());

    let result = negotiator
        .negotiate_and_send(
            &actor.handle,
            "blocked.example",
            delivery_request("blocked.example"),
        )
        .await
        .expect("a 403 response is not an Err -- it is propagated as Ok");

    assert_eq!(result.status, StatusCode::FORBIDDEN);
    assert_eq!(
        mock.sent_requests().len(),
        1,
        "a 403 (blocked) rejection must not trigger a format retry"
    );
    assert!(
        recorded_format_row(&app.pool, "blocked.example")
            .await
            .is_none(),
        "no capability must be recorded for a rejected delivery"
    );

    app.cleanup().await;
}

// --- Requirement 3.1 (converse): a general (non-401, non-403) failure is not signature-related ---

#[tokio::test]
async fn general_failure_500_response_does_not_retry() {
    let app = spawn_test_app().await;
    let actor = create_signable_actor(&app, "general_failure_dana").await;
    let mock = Arc::new(MockFederationHttpClient::new());
    mock.queue_send_response(response(StatusCode::INTERNAL_SERVER_ERROR));
    let negotiator = negotiator_for(&app, mock.clone());

    let result = negotiator
        .negotiate_and_send(
            &actor.handle,
            "flaky.example",
            delivery_request("flaky.example"),
        )
        .await
        .expect("a 500 response is not an Err -- it is propagated as Ok");

    assert_eq!(result.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        mock.sent_requests().len(),
        1,
        "a general (non-401) failure must not trigger a format retry"
    );
    assert!(
        recorded_format_row(&app.pool, "flaky.example")
            .await
            .is_none()
    );

    app.cleanup().await;
}

// --- Requirement 3.1 (converse): a transport-level Err is propagated, never retried ---

#[tokio::test]
async fn transport_level_failure_is_propagated_without_retry() {
    let app = spawn_test_app().await;
    let actor = create_signable_actor(&app, "transport_erin").await;
    let mock = Arc::new(MockFederationHttpClient::new());
    mock.queue_send_error(StatusCode::BAD_GATEWAY, "connection reset");
    let negotiator = negotiator_for(&app, mock.clone());

    let err = negotiator
        .negotiate_and_send(
            &actor.handle,
            "unreachable.example",
            delivery_request("unreachable.example"),
        )
        .await
        .expect_err("a transport-level Err must propagate as Err, not be swallowed");

    assert_eq!(err.kind, ErrorKind::Server);
    assert_eq!(
        mock.sent_requests().len(),
        1,
        "a transport-level failure must not trigger a format retry"
    );
    assert!(
        recorded_format_row(&app.pool, "unreachable.example")
            .await
            .is_none()
    );

    app.cleanup().await;
}

// --- Requirement 3.2: a successful retry overwrites a previously recorded, now-stale format ---

#[tokio::test]
async fn successful_retry_overwrites_a_previously_recorded_different_format() {
    let app = spawn_test_app().await;
    let actor = create_signable_actor(&app, "overwrite_frank").await;
    record_capability(
        &app.pool,
        "switching.example",
        "draft_cavage",
        app.runtime.clock.now(),
    )
    .await;
    let mock = Arc::new(MockFederationHttpClient::new());
    // The previously-recorded draft-cavage format is used first, but this
    // host has since switched to only accepting rfc9421.
    mock.queue_send_response(response(StatusCode::UNAUTHORIZED));
    mock.queue_send_response(response(StatusCode::OK));
    let negotiator = negotiator_for(&app, mock.clone());

    let result = negotiator
        .negotiate_and_send(
            &actor.handle,
            "switching.example",
            delivery_request("switching.example"),
        )
        .await
        .expect("negotiate_and_send must succeed once the retry gets a 2xx");

    assert_eq!(result.status, StatusCode::OK);

    let sent = mock.sent_requests();
    assert_eq!(sent.len(), 2);
    assert!(
        !sent[0].headers.contains_key("signature-input"),
        "first attempt must use the previously recorded draft-cavage format"
    );
    assert!(
        sent[1].headers.contains_key("signature-input"),
        "retry must use rfc9421"
    );

    assert_eq!(
        recorded_format_row(&app.pool, "switching.example")
            .await
            .as_deref(),
        Some("rfc9421"),
        "the newly successful format must overwrite the stale recorded one"
    );

    app.cleanup().await;
}

// --- format_to_db / format_from_db: pure unit tests, no DB/network involved ---

#[test]
fn format_to_db_and_back_round_trips_both_variants() {
    assert_eq!(format_to_db(SignatureFormat::DraftCavage), "draft_cavage");
    assert_eq!(format_to_db(SignatureFormat::Rfc9421), "rfc9421");
    assert_eq!(
        format_from_db("draft_cavage"),
        Some(SignatureFormat::DraftCavage)
    );
    assert_eq!(format_from_db("rfc9421"), Some(SignatureFormat::Rfc9421));
    assert_eq!(format_from_db("something_else"), None);
}

#[test]
fn other_format_is_the_opposite_variant() {
    assert_eq!(
        other_format(SignatureFormat::DraftCavage),
        SignatureFormat::Rfc9421
    );
    assert_eq!(
        other_format(SignatureFormat::Rfc9421),
        SignatureFormat::DraftCavage
    );
}
