//! Integration-style tests for `SignatureVerifier`/`HttpSignatureVerifier`
//! (Requirements 2.1, 2.2, 2.5, 2.6, 7.1), per task 2.3's observable
//! completion condition: "正当署名で署名者 URI を返し、改ざん・欠落・鍵取得
//! 失敗が検証失敗になり、両形式で検証が成立する統合テストが通る".
//!
//! No DB/network/`spawn_test_app` is needed here: [`HttpSignatureVerifier`]
//! is generic over `R: PublicKeyResolver` (never `Arc<dyn PublicKeyResolver>`
//! — see this module's own doc comment), so a deterministic, in-memory
//! [`MockPublicKeyResolver`] (mirroring `http_client.rs`'s
//! `MockFederationHttpClient` FIFO-outcome-queue convention) stands in for
//! `DbFederationPublicKeyResolver` entirely. Request fixtures are built
//! directly against [`SignatureSuite`]/real RSA keys (generated via the
//! same deterministic [`generate_keypair`]/[`SeededRng`] convention
//! `src/actor/keys/material.rs`'s own tests use) rather than through
//! `RequestSigner`, so these tests exercise the verifier independently of
//! that other component's own correctness.

use std::collections::VecDeque;
use std::sync::Mutex;

use axum::http::header::{DATE, HOST};
use axum::http::{HeaderName, HeaderValue, Method, StatusCode};
use rsa::RsaPrivateKey;
use rsa::pkcs8::DecodePrivateKey;
use time::macros::datetime;

use super::*;
use crate::actor::keys::material::generate_keypair;
use crate::federation::signatures::key_resolver::RemotePublicKey;
use crate::runtime::{FixedClock, SeededRng};

const TEST_KEY_ID: &str = "https://remote.example/users/alice#main-key";
const TEST_ACTOR_URI: &str = "https://remote.example/users/alice";
const REQUEST_URL: &str = "https://kawasemi.example/inbox";
const REQUEST_HOST: &str = "kawasemi.example";

/// Generates a deterministic real RSA-2048 key pair for test fixtures
/// (mirrors `src/actor/keys/material.rs`'s own `SeededRng`-based test
/// convention) and parses the private half back into an `RsaPrivateKey`
/// usable for hand-signing a test request.
fn test_keypair(seed: u64) -> (RsaPrivateKey, String) {
    let generated =
        generate_keypair(&SeededRng::new(seed)).expect("test key generation must succeed");
    let private_key = RsaPrivateKey::from_pkcs8_pem(generated.private_key_pem.expose_secret())
        .expect("generated private key PEM must parse");
    (private_key, generated.public_key_pem)
}

fn suite_for_test(format: SignatureFormat) -> Box<dyn SignatureSuite> {
    match format {
        SignatureFormat::DraftCavage => Box::new(DraftCavageSuite::new()),
        SignatureFormat::Rfc9421 => Box::new(Rfc9421Suite::new()),
    }
}

fn format_http_date(when: OffsetDateTime) -> String {
    when.to_offset(time::UtcOffset::UTC)
        .format(HTTP_DATE_FORMAT)
        .expect("HTTP-date formatting must not fail")
}

fn fixed_clock(when: OffsetDateTime) -> Arc<dyn Clock> {
    Arc::new(FixedClock::new(when))
}

/// Hand-builds a genuinely signed [`IncomingRequest`] for `key_id`/
/// `private_key`, covering `Host` + (if `date_value` is `Some`) `Date` +
/// (if `body` is `Some`) `Digest` — exercising the exact same
/// [`SignatureSuite`] surface a real `RequestSigner` would, independently
/// of that component. `date_value: None` omits the `Date` header entirely
/// (both from the signed headers and the covered-component set), for the
/// "Date required unconditionally" test below.
fn build_signed_request(
    format: SignatureFormat,
    key_id: &str,
    private_key: &RsaPrivateKey,
    date_value: Option<&str>,
    body: Option<Vec<u8>>,
) -> IncomingRequest {
    let suite = suite_for_test(format);

    let mut headers = RequestHeaders::new();
    headers.insert(
        HOST,
        HeaderValue::from_str(REQUEST_HOST).expect("valid host header value"),
    );
    if let Some(date_value) = date_value {
        headers.insert(
            DATE,
            HeaderValue::from_str(date_value).expect("valid date header value"),
        );
    }
    if let Some(body) = &body {
        headers.insert(
            HeaderName::from_static("digest"),
            HeaderValue::from_str(&BodyDigest::compute(body).header_value())
                .expect("valid digest header value"),
        );
    }

    let signable = SignableRequest {
        method: Method::POST,
        url: REQUEST_URL.to_string(),
        key_id: key_id.to_string(),
        headers: headers.clone(),
    };
    let signing_input = suite.build_signing_input(&signable);
    let hashed = Sha256::digest(signing_input.signing_string.as_bytes());
    let signature = private_key
        .sign(sha256_pkcs1v15_padding(), hashed.as_slice())
        .expect("test signing must succeed");

    for (name, value) in suite.assemble_headers(key_id, &signature, &signing_input) {
        headers.insert(
            HeaderName::from_bytes(name.as_bytes()).expect("valid header name"),
            HeaderValue::from_str(&value).expect("valid header value"),
        );
    }

    IncomingRequest {
        method: Method::POST,
        url: REQUEST_URL.to_string(),
        headers,
        body,
    }
}

/// Deterministic, in-memory [`PublicKeyResolver`] test double (mirrors
/// `http_client.rs`'s `MockFederationHttpClient` FIFO-outcome-queue
/// convention): each `resolve_public_key` call pops the next queued
/// outcome and records `(key_id, force)`, so tests can assert exactly how
/// many times (and with which `force` value) the resolver was called --
/// the load-bearing proof that the invalidate-and-retry path is real, not
/// merely present in the code.
#[derive(Default)]
struct MockPublicKeyResolver {
    state: Mutex<MockResolverState>,
}

#[derive(Default)]
struct MockResolverState {
    outcomes: VecDeque<Result<RemotePublicKey, String>>,
    calls: Vec<(String, bool)>,
}

impl MockPublicKeyResolver {
    fn new() -> Self {
        Self::default()
    }

    fn queue_ok(&self, key: RemotePublicKey) {
        self.state
            .lock()
            .expect("MockPublicKeyResolver mutex must not be poisoned")
            .outcomes
            .push_back(Ok(key));
    }

    fn queue_err(&self) {
        self.state
            .lock()
            .expect("MockPublicKeyResolver mutex must not be poisoned")
            .outcomes
            .push_back(Err("mock resolve failure".to_string()));
    }

    fn calls(&self) -> Vec<(String, bool)> {
        self.state
            .lock()
            .expect("MockPublicKeyResolver mutex must not be poisoned")
            .calls
            .clone()
    }
}

impl PublicKeyResolver for MockPublicKeyResolver {
    async fn resolve_public_key(
        &self,
        key_id: &str,
        force: bool,
    ) -> Result<RemotePublicKey, AppError> {
        let mut state = self
            .state
            .lock()
            .expect("MockPublicKeyResolver mutex must not be poisoned");
        state.calls.push((key_id.to_string(), force));
        match state.outcomes.pop_front() {
            Some(Ok(key)) => Ok(key),
            Some(Err(message)) => Err(AppError::server(StatusCode::BAD_GATEWAY, message)),
            None => Err(AppError::server(
                StatusCode::BAD_GATEWAY,
                "MockPublicKeyResolver: no queued outcome",
            )),
        }
    }
}

fn remote_key(key_id: &str, pem: String) -> RemotePublicKey {
    RemotePublicKey {
        key_id: key_id.to_string(),
        actor_uri: TEST_ACTOR_URI.to_string(),
        public_key_pem: pem,
    }
}

// --- Requirements 2.1, 2.2: a genuinely valid signature verifies, both formats ---

#[tokio::test]
async fn valid_draft_cavage_signature_verifies_and_returns_the_signer() {
    let (private_key, public_pem) = test_keypair(1);
    let now = datetime!(2026-07-16 12:00:00 UTC);
    let req = build_signed_request(
        SignatureFormat::DraftCavage,
        TEST_KEY_ID,
        &private_key,
        Some(&format_http_date(now)),
        None,
    );

    let resolver = Arc::new(MockPublicKeyResolver::new());
    resolver.queue_ok(remote_key(TEST_KEY_ID, public_pem));
    let verifier = HttpSignatureVerifier::new(
        resolver.clone(),
        fixed_clock(now),
        DEFAULT_SIGNATURE_MAX_AGE,
    );

    let verified = verifier
        .verify_request(&req)
        .await
        .expect("a genuinely valid draft-cavage signature must verify");

    assert_eq!(verified.key_id, TEST_KEY_ID);
    assert_eq!(verified.actor_uri, TEST_ACTOR_URI);
    assert_eq!(
        resolver.calls(),
        vec![(TEST_KEY_ID.to_string(), false)],
        "a valid signature must resolve the key exactly once, without forcing"
    );
}

#[tokio::test]
async fn valid_rfc9421_signature_verifies_and_returns_the_signer() {
    let (private_key, public_pem) = test_keypair(2);
    let now = datetime!(2026-07-16 12:00:00 UTC);
    let req = build_signed_request(
        SignatureFormat::Rfc9421,
        TEST_KEY_ID,
        &private_key,
        Some(&format_http_date(now)),
        Some(br#"{"type":"Follow"}"#.to_vec()),
    );

    let resolver = Arc::new(MockPublicKeyResolver::new());
    resolver.queue_ok(remote_key(TEST_KEY_ID, public_pem));
    let verifier = HttpSignatureVerifier::new(
        resolver.clone(),
        fixed_clock(now),
        DEFAULT_SIGNATURE_MAX_AGE,
    );

    let verified = verifier
        .verify_request(&req)
        .await
        .expect("a genuinely valid RFC 9421 signature (with body) must verify");

    assert_eq!(verified.key_id, TEST_KEY_ID);
    assert_eq!(verified.actor_uri, TEST_ACTOR_URI);
}

// --- Requirement 2.6: tampered signature bytes are rejected (both resolve attempts exhausted) ---

#[tokio::test]
async fn tampered_signature_bytes_are_rejected_after_the_retry_is_exhausted() {
    let (private_key, public_pem) = test_keypair(3);
    let now = datetime!(2026-07-16 12:00:00 UTC);
    let mut req = build_signed_request(
        SignatureFormat::DraftCavage,
        TEST_KEY_ID,
        &private_key,
        Some(&format_http_date(now)),
        None,
    );
    // Corrupt the base64 signature payload embedded in the Signature
    // header's `signature="..."` param, leaving everything else (keyId,
    // algorithm, headers list) intact.
    let original = req
        .headers
        .get("signature")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let tampered = original.replacen("signature=\"", "signature=\"AAAA", 1);
    req.headers.insert(
        HeaderName::from_static("signature"),
        HeaderValue::from_str(&tampered).unwrap(),
    );

    let resolver = Arc::new(MockPublicKeyResolver::new());
    resolver.queue_ok(remote_key(TEST_KEY_ID, public_pem.clone()));
    resolver.queue_ok(remote_key(TEST_KEY_ID, public_pem));
    let verifier = HttpSignatureVerifier::new(
        resolver.clone(),
        fixed_clock(now),
        DEFAULT_SIGNATURE_MAX_AGE,
    );

    let result = verifier.verify_request(&req).await;

    assert!(result.is_err(), "a tampered signature must not verify");
    assert_eq!(
        resolver.calls(),
        vec![
            (TEST_KEY_ID.to_string(), false),
            (TEST_KEY_ID.to_string(), true)
        ],
        "a crypto verify failure must trigger exactly one invalidate-and-retry resolve"
    );
}

// --- Requirement 2.5: tampered body (Digest mismatch) is rejected before ever resolving a key ---

#[tokio::test]
async fn tampered_body_fails_digest_verification_without_resolving_a_key() {
    let (private_key, _public_pem) = test_keypair(4);
    let now = datetime!(2026-07-16 12:00:00 UTC);
    let mut req = build_signed_request(
        SignatureFormat::DraftCavage,
        TEST_KEY_ID,
        &private_key,
        Some(&format_http_date(now)),
        Some(br#"{"type":"Follow"}"#.to_vec()),
    );
    req.body = Some(br#"{"type":"Block"}"#.to_vec());

    let resolver = Arc::new(MockPublicKeyResolver::new());
    let verifier = HttpSignatureVerifier::new(
        resolver.clone(),
        fixed_clock(now),
        DEFAULT_SIGNATURE_MAX_AGE,
    );

    let result = verifier.verify_request(&req).await;

    assert!(result.is_err(), "a body not matching the Digest must fail");
    assert!(
        resolver.calls().is_empty(),
        "digest verification must fail before any key resolution is attempted"
    );
}

// --- Requirement 2.6: no detectable signature at all is rejected ---

#[tokio::test]
async fn missing_signature_header_is_rejected() {
    let now = datetime!(2026-07-16 12:00:00 UTC);
    let mut req = IncomingRequest::new(Method::POST, REQUEST_URL);
    req.headers.insert(
        HOST,
        HeaderValue::from_str(REQUEST_HOST).expect("valid host header value"),
    );
    req.headers.insert(
        DATE,
        HeaderValue::from_str(&format_http_date(now)).expect("valid date header value"),
    );

    let resolver = Arc::new(MockPublicKeyResolver::new());
    let verifier = HttpSignatureVerifier::new(
        resolver.clone(),
        fixed_clock(now),
        DEFAULT_SIGNATURE_MAX_AGE,
    );

    let result = verifier.verify_request(&req).await;

    assert!(
        result.is_err(),
        "a request with no signature must be rejected"
    );
    assert!(resolver.calls().is_empty());
}

// --- Requirement 2.5: body present but Digest header missing is rejected ---

#[tokio::test]
async fn body_present_with_no_digest_header_is_rejected() {
    let (private_key, _public_pem) = test_keypair(5);
    let now = datetime!(2026-07-16 12:00:00 UTC);
    let mut req = build_signed_request(
        SignatureFormat::DraftCavage,
        TEST_KEY_ID,
        &private_key,
        Some(&format_http_date(now)),
        Some(br#"{"type":"Follow"}"#.to_vec()),
    );
    req.headers.remove("digest");

    let resolver = Arc::new(MockPublicKeyResolver::new());
    let verifier = HttpSignatureVerifier::new(
        resolver.clone(),
        fixed_clock(now),
        DEFAULT_SIGNATURE_MAX_AGE,
    );

    let result = verifier.verify_request(&req).await;

    assert!(result.is_err());
    assert!(resolver.calls().is_empty());
}

// --- Requirement 2.6: key-fetch failure is rejected, with no retry ---

#[tokio::test]
async fn key_fetch_failure_is_rejected_without_a_retry() {
    let (private_key, _public_pem) = test_keypair(6);
    let now = datetime!(2026-07-16 12:00:00 UTC);
    let req = build_signed_request(
        SignatureFormat::DraftCavage,
        TEST_KEY_ID,
        &private_key,
        Some(&format_http_date(now)),
        None,
    );

    let resolver = Arc::new(MockPublicKeyResolver::new());
    resolver.queue_err();
    let verifier = HttpSignatureVerifier::new(
        resolver.clone(),
        fixed_clock(now),
        DEFAULT_SIGNATURE_MAX_AGE,
    );

    let result = verifier.verify_request(&req).await;

    assert!(
        result.is_err(),
        "a key-fetch failure must be a verification failure"
    );
    assert_eq!(
        resolver.calls(),
        vec![(TEST_KEY_ID.to_string(), false)],
        "a resolve failure on the initial (non-forced) attempt must not be retried"
    );
}

// --- Requirement 2.6 + design.md's invalidate-and-retry: genuine key rotation succeeds on retry ---

#[tokio::test]
async fn key_rotation_signature_made_with_a_new_key_succeeds_after_cache_invalidation_retry() {
    let (_stale_private_key, stale_public_pem) = test_keypair(7);
    let (new_private_key, new_public_pem) = test_keypair(8);
    let now = datetime!(2026-07-16 12:00:00 UTC);

    // The request is actually signed with the NEW key -- the cache
    // (queued as the first, non-forced resolve outcome) still only knows
    // about the stale key, exactly like an un-invalidated cache after a
    // remote key rotation.
    let req = build_signed_request(
        SignatureFormat::DraftCavage,
        TEST_KEY_ID,
        &new_private_key,
        Some(&format_http_date(now)),
        None,
    );
    let resolver = Arc::new(MockPublicKeyResolver::new());
    resolver.queue_ok(remote_key(TEST_KEY_ID, stale_public_pem));
    resolver.queue_ok(remote_key(TEST_KEY_ID, new_public_pem));
    let verifier = HttpSignatureVerifier::new(
        resolver.clone(),
        fixed_clock(now),
        DEFAULT_SIGNATURE_MAX_AGE,
    );

    let verified = verifier.verify_request(&req).await.expect(
        "verification must succeed once the cache is invalidated and the new key is fetched",
    );

    assert_eq!(verified.key_id, TEST_KEY_ID);
    assert_eq!(verified.actor_uri, TEST_ACTOR_URI);
    assert_eq!(
        resolver.calls(),
        vec![
            (TEST_KEY_ID.to_string(), false),
            (TEST_KEY_ID.to_string(), true)
        ],
        "key rotation must be observed as exactly two resolver calls: the initial cache-preferring \
         attempt (which fails against the stale key) and the forced invalidate-and-refetch retry \
         (which succeeds against the freshly fetched key)"
    );
}

// --- Requirement 2.6: wrong signing key end-to-end (not a rotation case) still fails after retry ---

#[tokio::test]
async fn signature_made_with_an_unrelated_key_fails_even_after_retry() {
    let (signing_key, _signing_pem) = test_keypair(9);
    let (_wrong_key, wrong_pem) = test_keypair(10);
    let now = datetime!(2026-07-16 12:00:00 UTC);
    let req = build_signed_request(
        SignatureFormat::DraftCavage,
        TEST_KEY_ID,
        &signing_key,
        Some(&format_http_date(now)),
        None,
    );

    let resolver = Arc::new(MockPublicKeyResolver::new());
    resolver.queue_ok(remote_key(TEST_KEY_ID, wrong_pem.clone()));
    resolver.queue_ok(remote_key(TEST_KEY_ID, wrong_pem));
    let verifier = HttpSignatureVerifier::new(
        resolver.clone(),
        fixed_clock(now),
        DEFAULT_SIGNATURE_MAX_AGE,
    );

    let result = verifier.verify_request(&req).await;

    assert!(result.is_err());
    assert_eq!(resolver.calls().len(), 2);
}

// --- Requirement 2.6 (staleness/"期限切れ"): a signature older than the max age is rejected ---

#[tokio::test]
async fn a_signature_older_than_the_max_age_is_rejected() {
    let (private_key, _public_pem) = test_keypair(11);
    let signed_at = datetime!(2026-07-16 08:00:00 UTC);
    let now = signed_at + Duration::hours(3); // well past DEFAULT_SIGNATURE_MAX_AGE (1 hour)
    let req = build_signed_request(
        SignatureFormat::DraftCavage,
        TEST_KEY_ID,
        &private_key,
        Some(&format_http_date(signed_at)),
        None,
    );

    let resolver = Arc::new(MockPublicKeyResolver::new());
    let verifier = HttpSignatureVerifier::new(
        resolver.clone(),
        fixed_clock(now),
        DEFAULT_SIGNATURE_MAX_AGE,
    );

    let result = verifier.verify_request(&req).await;

    assert!(result.is_err(), "an expired signature must be rejected");
    assert!(resolver.calls().is_empty());
}

// --- Requirement 2.6: a Date header still within the max age succeeds (converse of the above) ---

#[tokio::test]
async fn a_signature_within_the_max_age_is_accepted() {
    let (private_key, public_pem) = test_keypair(12);
    let signed_at = datetime!(2026-07-16 08:00:00 UTC);
    let now = signed_at + Duration::minutes(30); // within DEFAULT_SIGNATURE_MAX_AGE (1 hour)
    let req = build_signed_request(
        SignatureFormat::DraftCavage,
        TEST_KEY_ID,
        &private_key,
        Some(&format_http_date(signed_at)),
        None,
    );

    let resolver = Arc::new(MockPublicKeyResolver::new());
    resolver.queue_ok(remote_key(TEST_KEY_ID, public_pem));
    let verifier = HttpSignatureVerifier::new(
        resolver.clone(),
        fixed_clock(now),
        DEFAULT_SIGNATURE_MAX_AGE,
    );

    verifier
        .verify_request(&req)
        .await
        .expect("a signature within the staleness window must verify");
}

// --- Date required unconditionally, even if the (self-consistent) covered-component set omits it ---

#[tokio::test]
async fn a_request_with_no_date_header_at_all_is_rejected() {
    let (private_key, _public_pem) = test_keypair(13);
    let now = datetime!(2026-07-16 12:00:00 UTC);
    let req = build_signed_request(
        SignatureFormat::DraftCavage,
        TEST_KEY_ID,
        &private_key,
        None, // no Date header signed or present at all
        None,
    );
    assert!(
        !req.headers.contains_key("date"),
        "test fixture sanity check: no Date header should be present"
    );

    let resolver = Arc::new(MockPublicKeyResolver::new());
    let verifier = HttpSignatureVerifier::new(
        resolver.clone(),
        fixed_clock(now),
        DEFAULT_SIGNATURE_MAX_AGE,
    );

    let result = verifier.verify_request(&req).await;

    assert!(
        result.is_err(),
        "a request with no Date header at all must be rejected, even though its own (self-consistent) \
         covered-component declaration never claimed to cover `date`"
    );
    assert!(resolver.calls().is_empty());
}

// --- A malformed (but present) Date header is rejected ---

#[tokio::test]
async fn a_malformed_date_header_is_rejected() {
    let (private_key, _public_pem) = test_keypair(14);
    let now = datetime!(2026-07-16 12:00:00 UTC);
    let mut req = build_signed_request(
        SignatureFormat::DraftCavage,
        TEST_KEY_ID,
        &private_key,
        Some(&format_http_date(now)),
        None,
    );
    req.headers.insert(
        DATE,
        HeaderValue::from_str("not-a-valid-http-date").expect("valid header value syntax"),
    );

    let resolver = Arc::new(MockPublicKeyResolver::new());
    let verifier = HttpSignatureVerifier::new(
        resolver.clone(),
        fixed_clock(now),
        DEFAULT_SIGNATURE_MAX_AGE,
    );

    let result = verifier.verify_request(&req).await;

    assert!(result.is_err());
    assert!(resolver.calls().is_empty());
}

// --- tasks.md's 1.5 Implementation Note: declared vs. actual covered-components mismatch ---

#[tokio::test]
async fn declared_covered_components_mismatching_the_actual_set_is_rejected() {
    let (private_key, _public_pem) = test_keypair(15);
    let now = datetime!(2026-07-16 12:00:00 UTC);
    let mut req = build_signed_request(
        SignatureFormat::DraftCavage,
        TEST_KEY_ID,
        &private_key,
        Some(&format_http_date(now)),
        None,
    );

    let original = req
        .headers
        .get("signature")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        original.contains("headers=\"(request-target) host date\""),
        "test fixture sanity check: expected the untampered covered-components list: {original}"
    );
    // Declare a *reduced* covered-component set than what was actually
    // signed and what the request's actual headers would recompute --
    // this verifier must not silently accept the partial declaration.
    let tampered = original.replacen(
        "headers=\"(request-target) host date\"",
        "headers=\"(request-target) host\"",
        1,
    );
    req.headers.insert(
        HeaderName::from_static("signature"),
        HeaderValue::from_str(&tampered).unwrap(),
    );

    let resolver = Arc::new(MockPublicKeyResolver::new());
    let verifier = HttpSignatureVerifier::new(
        resolver.clone(),
        fixed_clock(now),
        DEFAULT_SIGNATURE_MAX_AGE,
    );

    let result = verifier.verify_request(&req).await;

    assert!(
        result.is_err(),
        "a covered-components declaration that does not match what would actually be covered must \
         be rejected"
    );
    assert!(
        resolver.calls().is_empty(),
        "a covered-components mismatch must be caught before ever resolving a key"
    );
}

// --- DEFAULT_SIGNATURE_MAX_AGE has the documented value ---

#[test]
fn default_signature_max_age_is_one_hour() {
    assert_eq!(DEFAULT_SIGNATURE_MAX_AGE, Duration::hours(1));
}
