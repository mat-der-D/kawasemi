use super::*;

// --- compute() is deterministic and content-sensitive ---

#[test]
fn compute_is_deterministic_for_the_same_body() {
    let body = b"hello activitypub";

    assert_eq!(Digest::compute(body), Digest::compute(body));
}

#[test]
fn compute_differs_for_different_bodies() {
    assert_ne!(Digest::compute(b"body a"), Digest::compute(b"body b"));
}

#[test]
fn compute_of_empty_body_is_deterministic() {
    assert_eq!(Digest::compute(b""), Digest::compute(b""));
}

// --- header_value() shape ---

#[test]
fn header_value_carries_the_sha_256_prefix() {
    let digest = Digest::compute(b"some body");

    assert!(digest.header_value().starts_with("SHA-256="));
}

#[test]
fn header_value_is_deterministic_for_the_same_body() {
    let a = Digest::compute(b"same body");
    let b = Digest::compute(b"same body");

    assert_eq!(a.header_value(), b.header_value());
}

// --- from_header_value() round-trips compute()/header_value() ---

#[test]
fn from_header_value_round_trips_a_computed_digest() {
    let original = Digest::compute(b"round trip me");
    let header = original.header_value();

    let parsed = Digest::from_header_value(&header).expect("valid header value must parse");

    assert_eq!(parsed, original);
}

#[test]
fn from_header_value_rejects_an_unsupported_algorithm_prefix() {
    let result = Digest::from_header_value("MD5=1B2M2Y8AsgTpgAmY7PhCfg==");

    assert!(result.is_err());
}

#[test]
fn from_header_value_rejects_invalid_base64_payload() {
    let result = Digest::from_header_value("SHA-256=not-valid-base64!!!");

    assert!(result.is_err());
}

// --- verify(): the task's core observable completion condition ---
// ("ダイジェスト不一致が検出される単体テストが通る")

#[test]
fn verify_succeeds_when_computed_digest_matches_expected() {
    let body = b"unmodified body";
    let computed = Digest::compute(body);
    let expected = Digest::compute(body);

    assert!(computed.verify(&expected).is_ok());
}

#[test]
fn verify_detects_a_mismatch_between_computed_and_expected_digest() {
    let computed = Digest::compute(b"the body that actually arrived");
    let expected = Digest::compute(b"a different body the signer claimed");

    let result = computed.verify(&expected);

    assert!(
        result.is_err(),
        "verify() must detect a digest mismatch between computed and expected digests"
    );
}

#[test]
fn verify_detects_tampering_via_a_full_send_then_receive_round_trip() {
    // Simulates the sender computing a digest over the original body and
    // asserting it in a `Digest` header (Requirement 1.3), and the receiver
    // computing its own digest over the body it actually received and
    // checking it against the header-asserted digest (Requirement 2.5).
    let original_body = b"{\"type\":\"Create\"}";
    let sender_header = Digest::compute(original_body).header_value();

    // Body arrives unmodified: verification must succeed.
    let expected = Digest::from_header_value(&sender_header).expect("header must parse");
    let received_unmodified = Digest::compute(original_body);
    assert!(received_unmodified.verify(&expected).is_ok());

    // Body arrives tampered with: verification must fail.
    let tampered_body = b"{\"type\":\"Delete\"}";
    let received_tampered = Digest::compute(tampered_body);
    assert!(received_tampered.verify(&expected).is_err());
}

#[test]
fn verify_is_order_independent_for_matching_digests() {
    let a = Digest::compute(b"symmetric body");
    let b = Digest::compute(b"symmetric body");

    assert!(a.verify(&b).is_ok());
    assert!(b.verify(&a).is_ok());
}
