//! Integration tests for federation-core's signature send/receive pipeline
//! (`.kiro/specs/federation-core/tasks.md`, task 6.1, `_Boundary:
//! signatures_it_`; Requirements 1.1, 2.1, 2.2, 2.6, 2.7, 3.1, 3.2, 3.3).
//!
//! Every underlying signature component (`RequestSigner`, `SignatureSuite`,
//! `HttpSignatureVerifier`, `DbFederationPublicKeyResolver`,
//! `SignatureNegotiator`) already has dedicated unit/integration-style
//! coverage under `src/federation/signatures/*/tests.rs` (each exercising
//! its own component against a hand-built fixture or a deterministic mock
//! resolver). This file's job is different: prove the *assembled* pipeline
//! end to end, wired the same way task 5.4 wires it into the real, live
//! `spawn_test_app()` instance --
//!
//! - A genuinely local actor (`app.actor.actor_service().create_actor`,
//!   which provisions a real RSA-2048 signing key through the real
//!   `SigningKeyProvider`/`ActorDirectory` boundaries -- Requirement 1.1)
//!   signs an outbound request via [`RequestSigner`], and that exact,
//!   over-the-wire signed request is independently verified by a real
//!   [`HttpSignatureVerifier`]`<`[`DbFederationPublicKeyResolver`]`<`[`MockFederationHttpClient`]`>>`
//!   backed by `app`'s own real, migrated Postgres pool (Requirement 2.1),
//!   for both signature formats (Requirement 2.2).
//! - Tampering with the signed bytes, and omitting a signature entirely,
//!   are both rejected (Requirement 2.6).
//! - [`SignatureNegotiator`] double-knocks against an unknown host through
//!   [`MockFederationHttpClient`], records the format that actually
//!   succeeded, and a later send to the same host uses that recorded format
//!   first (Requirements 3.1, 3.2, 3.3) -- and each of the two double-knock
//!   attempts is itself re-verified by the real verifier, proving the
//!   negotiator's output is not merely "shaped like" a valid signature of
//!   the right format but a genuinely verifiable one.
//! - The public-key resolver's cache is reused across repeated
//!   verifications without a second network fetch, and a crypto-verify
//!   failure against a stale cached key triggers exactly one
//!   invalidate-and-refetch retry that then succeeds (Requirement 2.6's
//!   "公開鍵が取得できない" and key-rotation-tolerance path).
//!
//! Every network boundary this file touches is
//! [`MockFederationHttpClient`] (Requirement 2.7: "ネットワーク取得を...
//! テストでモック実装へ差し替えられるようにする") -- no real HTTP call is
//! ever made, so every scenario here is fully deterministic.

use std::sync::Arc;

use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use serde_json::json;

use kawasemi::actor::keys::material::generate_keypair;
use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor, ResolvedActor};
use kawasemi::federation::signatures::{
    DEFAULT_PUBLIC_KEY_CACHE_TTL, DEFAULT_SIGNATURE_MAX_AGE, SignatureFormat,
};
use kawasemi::federation::urls::ActorUrls;
use kawasemi::federation::{
    DbFederationPublicKeyResolver, HttpResponse, HttpSignatureVerifier, IncomingRequest,
    MockFederationHttpClient, OutboundRequest, PublicKeyResolver, RequestSigner,
    SignatureNegotiator, SignatureVerifier,
};
use kawasemi::runtime::SeededRng;
use kawasemi::test_harness::{TestApp, spawn_test_app};

// ==========================================================================
// Fixtures (mirrors `tests/federation_bootstrap_it.rs`'s own
// `insert_actor_fixture`/`test_keypair` conventions -- each integration
// test file is its own compiled crate, so these are independently
// duplicated rather than shared).
// ==========================================================================

/// Creates a real owner + a real local actor via `ActorService::create_actor`
/// (which provisions a real, currently valid RSA-2048 signing key through
/// the real `SigningKeyProvider`), then resolves it back through
/// `ActorDirectory` -- the exact path `RequestSigner` itself uses to turn a
/// `Handle` into signing-key material (Requirement 1.1).
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
            display_name: format!("Signatures IT {handle_str}"),
            summary: "an actor used by the signature send/receive integration test".to_string(),
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
/// `ActorDirectory`/`SigningKeyProvider`/`Clock` -- the same construction
/// task 5.4's bootstrap wiring and `src/federation/signatures/negotiation/tests.rs`'s
/// own `negotiator_for` helper use.
fn signer_for(app: &TestApp) -> RequestSigner {
    RequestSigner::new(
        app.actor.directory().clone(),
        app.runtime.keys.clone(),
        ActorUrls::new(test_domain(app)),
        app.runtime.clock.clone(),
    )
}

/// Builds a `DbFederationPublicKeyResolver` against `app`'s own real,
/// migrated pool (the real `remote_public_keys` cache table) and `mock` as
/// the fetch-on-miss/stale/force network boundary (Requirement 2.7).
fn resolver_for(
    app: &TestApp,
    mock: Arc<MockFederationHttpClient>,
) -> DbFederationPublicKeyResolver<MockFederationHttpClient> {
    DbFederationPublicKeyResolver::new(
        app.pool.clone(),
        mock,
        app.runtime.clock.clone(),
        DEFAULT_PUBLIC_KEY_CACHE_TTL,
    )
}

/// Builds an `HttpSignatureVerifier` against `resolver` and `app`'s own
/// (fixed, deterministic) clock, using the documented default staleness
/// window.
fn verifier_for<R: PublicKeyResolver>(app: &TestApp, resolver: Arc<R>) -> HttpSignatureVerifier<R> {
    HttpSignatureVerifier::new(
        resolver,
        app.runtime.clock.clone(),
        DEFAULT_SIGNATURE_MAX_AGE,
    )
}

/// Converts a signed `OutboundRequest` (the send side's own shape) into the
/// `IncomingRequest` the receive-side verifier expects -- both share an
/// identical `method`/`url`/`headers`/`body` shape by design (see
/// `src/federation/signatures/verifier.rs`'s doc comment, "`IncomingRequest`:
/// not defined anywhere else in this spec": "Mirrors `OutboundRequest`'s
/// shape for the receiving side"), so this is a pure field-for-field copy --
/// exactly what a real network hop would do.
fn to_incoming(req: &OutboundRequest) -> IncomingRequest {
    IncomingRequest {
        method: req.method.clone(),
        url: req.url.clone(),
        headers: req.headers.clone(),
        body: req.body.clone(),
    }
}

/// A minimal, `200 OK` actor document body carrying exactly the fields
/// `DbFederationPublicKeyResolver::parse_public_key_document` reads
/// (`publicKey.publicKeyPem`, `publicKey.owner`) -- the shape a real remote
/// (or, here, mocked) actor GET response for `key_id` would return.
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
        headers: HeaderMap::new(),
        body,
    }
}

/// A minimal `200 OK` `HttpResponse` with no body -- used by the
/// `SignatureNegotiator` scenarios, which only care about `status` driving
/// the double-knock decision.
fn plain_status_response(status: StatusCode) -> HttpResponse {
    HttpResponse {
        status,
        headers: HeaderMap::new(),
        body: Vec::new(),
    }
}

/// Generates an unrelated, deterministic RSA-2048 public key PEM (via the
/// same `generate_keypair`/`SeededRng` convention `src/actor/keys/material.rs`'s
/// own tests and `tests/federation_bootstrap_it.rs` use) -- used as a
/// deliberately *wrong* cached public key to simulate a stale cache entry
/// left behind by a since-rotated key.
fn unrelated_public_key_pem(seed: u64) -> String {
    generate_keypair(&SeededRng::new(seed))
        .expect("test key generation must succeed")
        .public_key_pem
}

/// Reads the current cached `remote_public_keys.public_key_pem` for
/// `key_id`, if any -- an independent assertion channel, bypassing the
/// resolver under test.
async fn seed_cached_public_key(
    app: &TestApp,
    key_id: &str,
    actor_uri: &str,
    public_key_pem: &str,
    fetched_at: time::OffsetDateTime,
) {
    sqlx::query(
        "INSERT INTO remote_public_keys (key_id, actor_uri, public_key_pem, fetched_at) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(key_id)
    .bind(actor_uri)
    .bind(public_key_pem)
    .bind(fetched_at)
    .execute(&app.pool)
    .await
    .expect("seeding a cached remote public key must succeed");
}

async fn recorded_capability_format(app: &TestApp, host: &str) -> Option<String> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT format FROM instance_signature_capabilities WHERE host = $1")
            .bind(host)
            .fetch_optional(&app.pool)
            .await
            .expect("reading instance_signature_capabilities must succeed");
    row.map(|(format,)| format)
}

/// The real, currently-valid public key PEM for `actor`, read through the
/// same `ActorDirectory::actor_public_key` boundary `RequestSigner`'s
/// counterpart on the verifying side would ultimately need to have been
/// told about (here: fed to the mock fetch response directly, standing in
/// for "the remote actually serves this document").
async fn actor_public_key_pem(app: &TestApp, actor: &ResolvedActor) -> String {
    app.actor
        .directory()
        .actor_public_key(actor.id)
        .await
        .expect("actor_public_key query must succeed")
        .expect("a freshly created actor must have an active signing key")
        .public_key_pem
}

fn signable_delivery_request(url: &str) -> OutboundRequest {
    OutboundRequest::new(Method::POST, url)
        .with_body(br#"{"type":"Create","id":"https://sender.example/activities/1"}"#.to_vec())
}

// ==========================================================================
// (1) Requirements 1.1, 2.1, 2.2, 2.7: a real local actor's signature,
// produced by `RequestSigner`, verifies end to end through a real
// `HttpSignatureVerifier`/`DbFederationPublicKeyResolver`, for both formats.
// ==========================================================================

async fn assert_round_trip_verifies(format: SignatureFormat, handle_suffix: &str) {
    let app = spawn_test_app().await;
    let urls = ActorUrls::new(test_domain(&app));
    let actor = insert_actor_fixture(&app, &format!("rt_{handle_suffix}")).await;
    let actor_uri = urls.actor_url(&actor.handle);
    let key_id = urls.key_id(&actor.handle);
    let public_key_pem = actor_public_key_pem(&app, &actor).await;

    let mut req = signable_delivery_request("https://remote.example/inbox");
    signer_for(&app)
        .sign_request(&actor.handle, format, &mut req)
        .await
        .expect("signing with a freshly provisioned real signing key must succeed");

    let mock = Arc::new(MockFederationHttpClient::new());
    mock.queue_fetch_response(actor_document_response(
        &actor_uri,
        &key_id,
        &public_key_pem,
    ));
    let resolver = Arc::new(resolver_for(&app, mock.clone()));
    let verifier = verifier_for(&app, resolver);

    let verified = verifier
        .verify_request(&to_incoming(&req))
        .await
        .expect("a genuinely signed request from a real local actor must verify");

    assert_eq!(verified.key_id, key_id);
    assert_eq!(verified.actor_uri, actor_uri);
    assert_eq!(
        mock.fetched_urls().len(),
        1,
        "a first-time verification must fetch the signer's public key exactly once"
    );
    assert_eq!(mock.fetched_urls()[0].0, key_id);

    app.cleanup().await;
}

#[tokio::test]
async fn draft_cavage_signed_request_from_a_real_actor_verifies_end_to_end() {
    assert_round_trip_verifies(SignatureFormat::DraftCavage, "cavage").await;
}

#[tokio::test]
async fn rfc9421_signed_request_from_a_real_actor_verifies_end_to_end() {
    assert_round_trip_verifies(SignatureFormat::Rfc9421, "rfc9421").await;
}

// ==========================================================================
// (2) Requirement 2.6: a tampered signature is rejected (after the
// documented invalidate-and-refetch retry is exhausted).
// ==========================================================================

#[tokio::test]
async fn tampered_signature_is_rejected_after_the_verifier_exhausts_its_retry() {
    let app = spawn_test_app().await;
    let urls = ActorUrls::new(test_domain(&app));
    let actor = insert_actor_fixture(&app, "tamper_bob").await;
    let actor_uri = urls.actor_url(&actor.handle);
    let key_id = urls.key_id(&actor.handle);
    let public_key_pem = actor_public_key_pem(&app, &actor).await;

    let mut req = signable_delivery_request("https://remote.example/inbox");
    signer_for(&app)
        .sign_request(&actor.handle, SignatureFormat::DraftCavage, &mut req)
        .await
        .expect("signing must succeed before it can be tampered with");

    // Corrupt the base64 signature payload embedded in the Signature
    // header's `signature="..."` param, leaving keyId/algorithm/headers
    // intact -- mirrors `src/federation/signatures/verifier/tests.rs`'s own
    // `tampered_signature_bytes_are_rejected_after_the_retry_is_exhausted`
    // tampering technique, applied here to a `RequestSigner`-produced
    // signature instead of a hand-built one.
    let original = req
        .headers
        .get("signature")
        .expect("a signed request must carry a Signature header")
        .to_str()
        .expect("test header values are ASCII")
        .to_string();
    let tampered = original.replacen("signature=\"", "signature=\"AAAA", 1);
    assert_ne!(
        tampered, original,
        "the tamper must actually change the header value"
    );
    req.headers.insert(
        HeaderName::from_static("signature"),
        HeaderValue::from_str(&tampered).expect("valid header value"),
    );

    let mock = Arc::new(MockFederationHttpClient::new());
    // The verifier's documented "verification-failure retry" (see
    // `verifier.rs`'s doc comment) resolves once with `force: false`, and
    // -- since the crypto check fails -- once more with `force: true`. Both
    // calls hit the network here (nothing was cached yet), so two fetch
    // outcomes are queued, both carrying the *real* (untampered) key: the
    // tamper is in the signature bytes, not the key material, so a correct
    // key still cannot make a genuinely tampered signature verify.
    mock.queue_fetch_response(actor_document_response(
        &actor_uri,
        &key_id,
        &public_key_pem,
    ));
    mock.queue_fetch_response(actor_document_response(
        &actor_uri,
        &key_id,
        &public_key_pem,
    ));
    let resolver = Arc::new(resolver_for(&app, mock.clone()));
    let verifier = verifier_for(&app, resolver);

    let err = verifier
        .verify_request(&to_incoming(&req))
        .await
        .expect_err("a tampered signature must be rejected");
    assert_eq!(err.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        mock.fetched_urls().len(),
        2,
        "a crypto failure must trigger exactly one invalidate-and-refetch retry before giving up"
    );

    app.cleanup().await;
}

// ==========================================================================
// (3) Requirement 2.6: a request carrying no signature at all is rejected,
// without ever attempting a public-key fetch.
// ==========================================================================

#[tokio::test]
async fn missing_signature_is_rejected_without_any_public_key_fetch() {
    let app = spawn_test_app().await;
    let req = IncomingRequest::new(Method::POST, "https://remote.example/inbox")
        .with_body(br#"{"type":"Create"}"#.to_vec());

    let mock = Arc::new(MockFederationHttpClient::new());
    let resolver = Arc::new(resolver_for(&app, mock.clone()));
    let verifier = verifier_for(&app, resolver);

    let err = verifier
        .verify_request(&req)
        .await
        .expect_err("a request with no Signature/Signature-Input header must be rejected");
    assert_eq!(err.status, StatusCode::UNAUTHORIZED);
    assert!(
        mock.fetched_urls().is_empty(),
        "a missing signature must fail format detection before any public-key fetch is attempted"
    );

    app.cleanup().await;
}

// ==========================================================================
// (4) Requirements 3.1, 3.2, 3.3: double-knock resend after a
// signature-related (401) rejection, format recording, and the recorded
// format being used first afterward -- plus proving each double-knock
// attempt is itself a genuinely verifiable signature, not merely
// correctly-shaped.
// ==========================================================================

#[tokio::test]
async fn double_knock_resend_succeeds_records_the_format_and_both_attempts_are_genuinely_verifiable()
 {
    let app = spawn_test_app().await;
    let urls = ActorUrls::new(test_domain(&app));
    let actor = insert_actor_fixture(&app, "knock_carol").await;
    let actor_uri = urls.actor_url(&actor.handle);
    let key_id = urls.key_id(&actor.handle);
    let public_key_pem = actor_public_key_pem(&app, &actor).await;
    let host = "knock.example";

    let mock = Arc::new(MockFederationHttpClient::new());
    mock.queue_send_response(plain_status_response(StatusCode::UNAUTHORIZED));
    mock.queue_send_response(plain_status_response(StatusCode::OK));
    let negotiator = SignatureNegotiator::new(
        app.pool.clone(),
        mock.clone(),
        signer_for(&app),
        app.runtime.clock.clone(),
    );

    let result = negotiator
        .negotiate_and_send(
            &actor.handle,
            host,
            signable_delivery_request(&format!("https://{host}/inbox")),
        )
        .await
        .expect("negotiate_and_send must succeed once the resend gets a 2xx");
    assert_eq!(result.status, StatusCode::OK);

    let sent = mock.sent_requests();
    assert_eq!(
        sent.len(),
        2,
        "exactly one resend after a 401 (signature-related) rejection"
    );
    assert!(
        !sent[0].headers.contains_key("signature-input"),
        "the first attempt must use the default draft-cavage format: {:?}",
        sent[0].headers
    );
    assert!(
        sent[1].headers.contains_key("signature-input"),
        "the resend must use the other format, RFC 9421: {:?}",
        sent[1].headers
    );

    assert_eq!(
        recorded_capability_format(&app, host).await.as_deref(),
        Some("rfc9421"),
        "the format that actually succeeded (rfc9421) must be recorded (Requirement 3.2)"
    );

    // Both double-knock attempts, though produced from two different
    // format guesses, are each genuinely valid, independently verifiable
    // signatures -- this is the load-bearing proof that the negotiator's
    // output round-trips through real receive-side verification, not just
    // "carries the right header shape" (which `negotiation/tests.rs`'s own
    // unit tests already check, without ever verifying the bytes).
    let verify_mock = Arc::new(MockFederationHttpClient::new());
    verify_mock.queue_fetch_response(actor_document_response(
        &actor_uri,
        &key_id,
        &public_key_pem,
    ));
    verify_mock.queue_fetch_response(actor_document_response(
        &actor_uri,
        &key_id,
        &public_key_pem,
    ));
    let verify_resolver = Arc::new(resolver_for(&app, verify_mock.clone()));
    let verifier = verifier_for(&app, verify_resolver);
    for attempt in &sent {
        let verified = verifier
            .verify_request(&to_incoming(attempt))
            .await
            .expect("each double-knock attempt must itself be a genuinely valid signature");
        assert_eq!(verified.actor_uri, actor_uri);
        assert_eq!(verified.key_id, key_id);
    }

    // Requirement 3.3: a subsequent send to the same host uses the recorded
    // format first, with no probing.
    let mock2 = Arc::new(MockFederationHttpClient::new());
    mock2.queue_send_response(plain_status_response(StatusCode::OK));
    let negotiator2 = SignatureNegotiator::new(
        app.pool.clone(),
        mock2.clone(),
        signer_for(&app),
        app.runtime.clock.clone(),
    );
    let result2 = negotiator2
        .negotiate_and_send(
            &actor.handle,
            host,
            signable_delivery_request(&format!("https://{host}/inbox")),
        )
        .await
        .expect("using the recorded format must succeed on the very first attempt");
    assert_eq!(result2.status, StatusCode::OK);
    let sent2 = mock2.sent_requests();
    assert_eq!(
        sent2.len(),
        1,
        "a host with a recorded format must not be probed again"
    );
    assert!(
        sent2[0].headers.contains_key("signature-input"),
        "the recorded rfc9421 format must be used first: {:?}",
        sent2[0].headers
    );

    app.cleanup().await;
}

// ==========================================================================
// (5) Requirement 2.7 (mockable, deterministic boundary): a cached public
// key is reused across repeated verifications without a second network
// fetch.
// ==========================================================================

#[tokio::test]
async fn cached_public_key_is_reused_across_verifications_without_a_second_fetch() {
    let app = spawn_test_app().await;
    let urls = ActorUrls::new(test_domain(&app));
    let actor = insert_actor_fixture(&app, "cache_dana").await;
    let actor_uri = urls.actor_url(&actor.handle);
    let key_id = urls.key_id(&actor.handle);
    let public_key_pem = actor_public_key_pem(&app, &actor).await;

    let mock = Arc::new(MockFederationHttpClient::new());
    // Only ONE fetch outcome is ever queued: if the resolver fetched twice,
    // the second call would find an empty queue and error loudly (see
    // `MockFederationHttpClient::fetch`'s "no queued fetch() outcome"
    // fallback), which would fail this test -- the absence of that failure
    // is itself the proof of cache reuse.
    mock.queue_fetch_response(actor_document_response(
        &actor_uri,
        &key_id,
        &public_key_pem,
    ));
    let resolver = Arc::new(resolver_for(&app, mock.clone()));
    let verifier = verifier_for(&app, resolver);

    for _ in 0..2 {
        let mut req = signable_delivery_request("https://remote.example/inbox");
        signer_for(&app)
            .sign_request(&actor.handle, SignatureFormat::DraftCavage, &mut req)
            .await
            .expect("signing must succeed");
        let verified = verifier
            .verify_request(&to_incoming(&req))
            .await
            .expect("a valid signature verified against a cached key must succeed");
        assert_eq!(verified.actor_uri, actor_uri);
    }

    assert_eq!(
        mock.fetched_urls().len(),
        1,
        "the second verification must be served entirely from the TTL-valid cache"
    );

    app.cleanup().await;
}

// ==========================================================================
// (6) Requirement 2.6 (key-rotation tolerance): a crypto-verify failure
// against a stale cached key triggers exactly one forced refetch, which
// then succeeds against the actor's real, current key.
// ==========================================================================

#[tokio::test]
async fn stale_cached_key_triggers_exactly_one_forced_refetch_that_then_succeeds() {
    let app = spawn_test_app().await;
    let urls = ActorUrls::new(test_domain(&app));
    let actor = insert_actor_fixture(&app, "rotate_erin").await;
    let actor_uri = urls.actor_url(&actor.handle);
    let key_id = urls.key_id(&actor.handle);
    let real_public_key_pem = actor_public_key_pem(&app, &actor).await;

    // Seed the cache with a WRONG public key for this actor's keyId --
    // simulating a key rotation the cache has not yet observed. The seeded
    // row is fresh (`fetched_at = now`), so the resolver's `force: false`
    // lookup will use it directly rather than treating it as stale by TTL.
    let wrong_public_key_pem = unrelated_public_key_pem(999);
    seed_cached_public_key(
        &app,
        &key_id,
        &actor_uri,
        &wrong_public_key_pem,
        app.runtime.clock.now(),
    )
    .await;

    let mut req = signable_delivery_request("https://remote.example/inbox");
    signer_for(&app)
        .sign_request(&actor.handle, SignatureFormat::DraftCavage, &mut req)
        .await
        .expect("signing with the actor's real, current key must succeed");

    let mock = Arc::new(MockFederationHttpClient::new());
    // The cache hit means the first (`force: false`) resolve never touches
    // the network -- only the `force: true` refetch does, and only once.
    mock.queue_fetch_response(actor_document_response(
        &actor_uri,
        &key_id,
        &real_public_key_pem,
    ));
    let resolver = Arc::new(resolver_for(&app, mock.clone()));
    let verifier = verifier_for(&app, resolver);

    let verified = verifier.verify_request(&to_incoming(&req)).await.expect(
        "verification must succeed once the automatic force-refetch retrieves the \
             actor's rotated (real, current) key",
    );
    assert_eq!(verified.actor_uri, actor_uri);
    assert_eq!(
        mock.fetched_urls().len(),
        1,
        "the stale cache hit must not itself cause a network call; only the forced retry does"
    );

    app.cleanup().await;
}
