//! Integration-style tests for `RequestSigner` (Requirements 1.1, 1.2, 1.3,
//! 1.5), per task 2.2's observable completion condition: "署名付きリクエスト
//! が生成され、有効鍵が無いアクターでは署名がエラーになる単体テストが通る".
//!
//! Mirrors `src/actor/directory/tests.rs`'s and
//! `src/federation/signatures/key_resolver/tests.rs`'s established
//! convention: `spawn_test_app` for a real, already-migrated schema and a
//! deterministic `RuntimeContext`, with real actor/owner/key fixtures
//! created through this crate's own already-implemented, already-tested
//! service/repository functions (`ActorService::create_actor` for the
//! "has a valid key" path, direct `repository::insert_actor` for the "actor
//! exists but has no key" path — mirroring `directory/tests.rs`'s own
//! `insert_actor_fixture` helper) rather than mocks. `app.runtime.keys` is
//! the real, DB-backed `DbSigningKeyProvider` (see `test_harness.rs`'s own
//! doc comment), so these tests exercise the exact same `SigningKeyProvider`
//! wiring the running application would use, not a fixed stand-in.
//!
//! Beyond asserting the expected headers are present, one test
//! (`sign_request_produces_a_signature_verifiable_with_the_actors_own_public_key`)
//! independently reconstructs the signing input from the signed request and
//! cryptographically verifies the produced signature against the actor's
//! real RSA public key — proving genuine RSA-SHA256 signing happened, not
//! merely that a plausible-looking header string was inserted.

use axum::http::Method;
use rsa::RsaPublicKey;
use rsa::pkcs8::DecodePublicKey;

use super::*;
use crate::actor::model::{ActorState, ActorType, LocalActor};
use crate::actor::owner::create_owner;
use crate::actor::repository::insert_actor;
use crate::actor::{NewActor, ResolvedActor};
use crate::domain::Id;
use crate::error::ErrorKind;
use crate::federation::signatures::suite::{DraftCavageSuite, Rfc9421Suite, SignatureSuite};
use crate::test_harness::{TestApp, spawn_test_app};

/// Builds a `RequestSigner` wired against `app`'s own real
/// `ActorDirectory`/`SigningKeyProvider`/`Clock`, and an `ActorUrls` bound to
/// `app`'s configured test-harness domain — the same construction a future
/// bootstrap-wiring task is expected to perform.
fn signer_for(app: &TestApp) -> RequestSigner {
    RequestSigner::new(
        app.actor.directory().clone(),
        app.runtime.keys.clone(),
        ActorUrls::new(app.state.config().server.domain.clone()),
        app.runtime.clock.clone(),
    )
}

/// Creates a real owner + a real local actor (via `ActorService::create_actor`,
/// which also provisions a real, currently valid RSA-2048 signing key
/// through the real `SigningKeyService` -> `KeyCache` -> `DbSigningKeyProvider`
/// path) under `handle`. Returns the persisted actor.
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
            display_name: "Signer Test Actor".to_string(),
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

/// Creates a real local actor row directly (bypassing `ActorService`, so no
/// signing key is ever provisioned for it) — mirrors
/// `src/actor/directory/tests.rs`'s own `insert_actor_fixture` helper.
/// Exercises the "actor exists, but has no valid signing key" branch of
/// Requirement 1.5, distinct from "no such actor at all".
async fn create_keyless_actor(app: &TestApp, handle: &str) -> Id {
    let owner_id = app.runtime.ids.next_id();
    let actor_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    create_owner(&app.pool, owner_id, now)
        .await
        .expect("creating the owner fixture must succeed");

    let actor = LocalActor {
        id: actor_id,
        owner_id,
        handle: Handle::new(handle).expect("test handle must be valid"),
        actor_type: ActorType::Person,
        display_name: "Keyless Test Actor".to_string(),
        summary: String::new(),
        state: ActorState::Active,
        created_at: now,
        updated_at: now,
    };
    let mut tx = app
        .pool
        .begin()
        .await
        .expect("opening a transaction for the keyless actor fixture must succeed");
    insert_actor(&mut tx, &actor)
        .await
        .expect("inserting the keyless actor fixture must succeed");
    tx.commit()
        .await
        .expect("committing the keyless actor fixture transaction must succeed");

    actor_id
}

// --- Requirements 1.1, 1.2, 1.4: draft-cavage signing produces a real Signature header ---

#[tokio::test]
async fn sign_request_draft_cavage_sets_signature_header_with_matching_key_id() {
    let app = spawn_test_app().await;
    let actor = create_signable_actor(&app, "cavage_alice").await;
    let signer = signer_for(&app);

    let mut req = OutboundRequest::new(Method::POST, "https://remote.example/inbox");
    signer
        .sign_request(&actor.handle, SignatureFormat::DraftCavage, &mut req)
        .await
        .expect("signing with a valid key must succeed");

    let signature_header = req
        .headers
        .get("signature")
        .expect("draft-cavage signing must set a Signature header")
        .to_str()
        .expect("Signature header must be valid ASCII/UTF-8");

    let expected_key_id =
        ActorUrls::new(app.state.config().server.domain.clone()).key_id(&actor.handle);
    assert!(
        signature_header.contains(&format!("keyId=\"{expected_key_id}\"")),
        "Signature header must carry the actor's keyId: {signature_header}"
    );
    assert!(
        signature_header.contains("algorithm=\"rsa-sha256\""),
        "Signature header must advertise rsa-sha256: {signature_header}"
    );
    assert!(
        req.headers.contains_key("host"),
        "signing must set a Host header"
    );
    assert!(
        req.headers.contains_key("date"),
        "signing must set a Date header"
    );
    // draft-cavage's distinguishing header shape: no Signature-Input header.
    assert!(
        !req.headers.contains_key("signature-input"),
        "draft-cavage signing must not set an RFC 9421 Signature-Input header"
    );

    app.cleanup().await;
}

// --- Requirements 1.1, 1.2, 1.4: RFC 9421 signing produces Signature-Input + Signature ---

#[tokio::test]
async fn sign_request_rfc9421_sets_signature_input_and_signature_headers() {
    let app = spawn_test_app().await;
    let actor = create_signable_actor(&app, "rfc9421_bob").await;
    let signer = signer_for(&app);

    let mut req = OutboundRequest::new(Method::POST, "https://remote.example/inbox");
    signer
        .sign_request(&actor.handle, SignatureFormat::Rfc9421, &mut req)
        .await
        .expect("signing with a valid key must succeed");

    let signature_input = req
        .headers
        .get("signature-input")
        .expect("RFC 9421 signing must set a Signature-Input header")
        .to_str()
        .expect("Signature-Input header must be valid ASCII/UTF-8");
    let signature = req
        .headers
        .get("signature")
        .expect("RFC 9421 signing must set a Signature header")
        .to_str()
        .expect("Signature header must be valid ASCII/UTF-8");

    let expected_key_id =
        ActorUrls::new(app.state.config().server.domain.clone()).key_id(&actor.handle);
    assert!(
        signature_input.contains(&format!("keyid=\"{expected_key_id}\"")),
        "Signature-Input header must carry the actor's keyid: {signature_input}"
    );
    assert!(
        signature_input.contains("alg=\"rsa-v1_5-sha256\""),
        "Signature-Input header must advertise rsa-v1_5-sha256: {signature_input}"
    );
    // RFC 9421's distinguishing header shape: `sig1=:<base64>:`, not
    // draft-cavage's `keyId="...",algorithm="...",...` comma-param syntax.
    assert!(
        signature.starts_with("sig1=:") && signature.ends_with(':'),
        "RFC 9421 Signature header must use the sig1=:<base64>: shape: {signature}"
    );

    app.cleanup().await;
}

// --- Requirement 1.3: body present -> Digest header set and covered ---

#[tokio::test]
async fn sign_request_with_body_sets_a_digest_header_matching_the_body() {
    let app = spawn_test_app().await;
    let actor = create_signable_actor(&app, "digest_carol").await;
    let signer = signer_for(&app);

    let body = br#"{"type":"Create","id":"https://example.com/activities/1"}"#.to_vec();
    let mut req =
        OutboundRequest::new(Method::POST, "https://remote.example/inbox").with_body(body.clone());
    signer
        .sign_request(&actor.handle, SignatureFormat::DraftCavage, &mut req)
        .await
        .expect("signing a request with a body must succeed");

    let digest_header = req
        .headers
        .get("digest")
        .expect("a request with a body must get a Digest header")
        .to_str()
        .expect("Digest header must be valid ASCII/UTF-8");
    assert_eq!(digest_header, BodyDigest::compute(&body).header_value());

    let signature_header = req
        .headers
        .get("signature")
        .expect("Signature header must be present")
        .to_str()
        .expect("valid ASCII/UTF-8");
    assert!(
        signature_header.contains("digest"),
        "the Digest header must be covered by the signature's headers param: {signature_header}"
    );

    app.cleanup().await;
}

// --- Requirement 1.3 (converse): no body -> no Digest header ---

#[tokio::test]
async fn sign_request_without_body_sets_no_digest_header() {
    let app = spawn_test_app().await;
    let actor = create_signable_actor(&app, "nodigest_dave").await;
    let signer = signer_for(&app);

    let mut req = OutboundRequest::new(Method::GET, "https://remote.example/users/dave");
    signer
        .sign_request(&actor.handle, SignatureFormat::DraftCavage, &mut req)
        .await
        .expect("signing a bodyless request must succeed");

    assert!(
        !req.headers.contains_key("digest"),
        "a bodyless request must not get a Digest header"
    );

    app.cleanup().await;
}

// --- Requirement 1.5: no such local actor -> error, req untouched ---

#[tokio::test]
async fn sign_request_for_an_unknown_actor_fails_and_does_not_mutate_req() {
    let app = spawn_test_app().await;
    let signer = signer_for(&app);
    let unknown_handle = Handle::new("nobody_registered_here").expect("valid handle");

    let mut req = OutboundRequest::new(Method::POST, "https://remote.example/inbox");
    let err = signer
        .sign_request(&unknown_handle, SignatureFormat::DraftCavage, &mut req)
        .await
        .expect_err("signing as an unknown actor must fail");

    assert_eq!(err.kind, ErrorKind::Client);
    assert!(
        req.headers.is_empty(),
        "a failed signing attempt must not mutate req's headers: {:?}",
        req.headers
    );
    assert!(req.body.is_none());

    app.cleanup().await;
}

// --- Requirement 1.5: actor exists but has no valid signing key -> error, req untouched ---

#[tokio::test]
async fn sign_request_for_an_actor_with_no_valid_key_fails_and_does_not_mutate_req() {
    let app = spawn_test_app().await;
    create_keyless_actor(&app, "keyless_erin").await;
    let signer = signer_for(&app);
    let handle = Handle::new("keyless_erin").expect("valid handle");

    let mut req = OutboundRequest::new(Method::POST, "https://remote.example/inbox");
    let err = signer
        .sign_request(&handle, SignatureFormat::DraftCavage, &mut req)
        .await
        .expect_err("signing as a keyless actor must fail");

    assert_eq!(err.kind, ErrorKind::Client);
    assert!(
        req.headers.is_empty(),
        "a failed signing attempt must not mutate req's headers: {:?}",
        req.headers
    );

    app.cleanup().await;
}

// --- Genuine cryptographic correctness: the produced signature actually verifies ---

#[tokio::test]
async fn sign_request_produces_a_signature_verifiable_with_the_actors_own_public_key() {
    let app = spawn_test_app().await;
    let actor = create_signable_actor(&app, "verify_frank").await;
    let signer = signer_for(&app);

    let body = br#"{"type":"Follow"}"#.to_vec();
    let mut req =
        OutboundRequest::new(Method::POST, "https://remote.example/inbox").with_body(body);
    signer
        .sign_request(&actor.handle, SignatureFormat::DraftCavage, &mut req)
        .await
        .expect("signing must succeed");

    let public_key_record = app
        .actor
        .directory()
        .actor_public_key(actor.id)
        .await
        .expect("looking up the actor's public key must succeed")
        .expect("the actor must have an active public key");
    let public_key = RsaPublicKey::from_public_key_pem(&public_key_record.public_key_pem)
        .expect("the stored public key PEM must parse");

    // Independently reconstruct the exact signing input `sign_request` used:
    // `build_signing_input` is a pure function of method/url/key_id/headers,
    // and every header it reads (Host/Date/Digest) is already present on
    // `req.headers` post-signing.
    let expected_key_id =
        ActorUrls::new(app.state.config().server.domain.clone()).key_id(&actor.handle);
    let signable = SignableRequest {
        method: req.method.clone(),
        url: req.url.clone(),
        key_id: expected_key_id.clone(),
        headers: req.headers.clone(),
    };
    let suite = DraftCavageSuite::new();
    let signing_input = suite.build_signing_input(&signable);
    let parsed = suite
        .parse(&req.headers)
        .expect("parsing the just-produced Signature header must succeed");
    assert_eq!(parsed.key_id, expected_key_id);

    let hashed = Sha256::digest(signing_input.signing_string.as_bytes());
    public_key
        .verify(
            sha256_pkcs1v15_padding(),
            hashed.as_slice(),
            &parsed.signature,
        )
        .expect("the produced signature must verify against the actor's own public key");

    // Tampering with the signed content must make verification fail --
    // proves this is a real signature over the actual signing string, not
    // an unconditionally-accepted stub.
    let tampered_hashed = Sha256::digest(b"not the real signing string");
    assert!(
        public_key
            .verify(
                sha256_pkcs1v15_padding(),
                tampered_hashed.as_slice(),
                &parsed.signature
            )
            .is_err(),
        "a signature over different content must not verify"
    );

    app.cleanup().await;
}

// --- Rfc9421 suite also verifies genuinely ---

#[tokio::test]
async fn sign_request_rfc9421_produces_a_signature_verifiable_with_the_actors_own_public_key() {
    let app = spawn_test_app().await;
    let actor = create_signable_actor(&app, "verify_grace").await;
    let signer = signer_for(&app);

    let mut req = OutboundRequest::new(Method::POST, "https://remote.example/inbox");
    signer
        .sign_request(&actor.handle, SignatureFormat::Rfc9421, &mut req)
        .await
        .expect("signing must succeed");

    let public_key_record = app
        .actor
        .directory()
        .actor_public_key(actor.id)
        .await
        .expect("looking up the actor's public key must succeed")
        .expect("the actor must have an active public key");
    let public_key = RsaPublicKey::from_public_key_pem(&public_key_record.public_key_pem)
        .expect("the stored public key PEM must parse");

    let expected_key_id =
        ActorUrls::new(app.state.config().server.domain.clone()).key_id(&actor.handle);
    let signable = SignableRequest {
        method: req.method.clone(),
        url: req.url.clone(),
        key_id: expected_key_id.clone(),
        headers: req.headers.clone(),
    };
    let suite = Rfc9421Suite::new();
    let signing_input = suite.build_signing_input(&signable);
    let parsed = suite
        .parse(&req.headers)
        .expect("parsing the just-produced Signature/Signature-Input headers must succeed");

    let hashed = Sha256::digest(signing_input.signing_string.as_bytes());
    public_key
        .verify(
            sha256_pkcs1v15_padding(),
            hashed.as_slice(),
            &parsed.signature,
        )
        .expect("the produced RFC 9421 signature must verify against the actor's own public key");

    app.cleanup().await;
}

// --- http_date / host_from_url: pure unit tests, no DB/network involved ---

#[test]
fn http_date_matches_rfc_9110s_own_worked_example_shape() {
    use time::macros::datetime;

    // RFC 9110 §5.6.7 gives this exact IMF-fixdate example.
    let when = datetime!(1994-11-06 08:49:37 UTC);
    assert_eq!(http_date(when), "Sun, 06 Nov 1994 08:49:37 GMT");
}

#[test]
fn host_from_url_extracts_the_authority_without_scheme_or_path() {
    assert_eq!(
        host_from_url("https://example.com/inbox?x=1"),
        "example.com"
    );
    assert_eq!(host_from_url("https://example.com"), "example.com");
    assert_eq!(
        host_from_url("https://example.com:8443/x"),
        "example.com:8443"
    );
}
