//! Pure unit tests for `AuthorizeEndpoint`'s helper functions (Requirements
//! 2.1-2.4, task 5.2) — no database required. DB-backed, end-to-end handler
//! coverage (GET login/consent rendering, POST login/consent/deny/CSRF-
//! mismatch/actor-ownership behavior) lives in `tests/oauth_authorize_it.rs`,
//! mirroring `apps_endpoint/tests.rs`'s own established split (pure helpers
//! here, DB-backed handler behavior in the `tests/*_it.rs` integration
//! file).

use time::{Duration, OffsetDateTime};

use super::*;

fn key(byte: u8) -> TokenHashKey {
    crate::config::Secret::new([byte; 32])
}

fn sample_session() -> OwnerSession {
    OwnerSession {
        owner_id: Id::from_i64(42),
        expires_at: OffsetDateTime::UNIX_EPOCH + Duration::seconds(1_700_000_000),
    }
}

// ---- hex_encode / hex_decode ----

#[test]
fn hex_round_trips_arbitrary_bytes() {
    let bytes = vec![0x00, 0x01, 0xAB, 0xCD, 0xEF, 0xFF];
    let encoded = hex_encode(&bytes);
    assert_eq!(hex_decode(&encoded), Some(bytes));
}

#[test]
fn hex_decode_rejects_odd_length_input() {
    assert_eq!(hex_decode("abc"), None);
}

#[test]
fn hex_decode_rejects_non_hex_characters() {
    assert_eq!(hex_decode("zz"), None);
}

#[test]
fn hex_decode_never_panics_on_multibyte_utf8() {
    assert_eq!(hex_decode("🙂🙂"), None);
}

// ---- CSRF token generation/verification ----

#[test]
fn generated_csrf_token_verifies_against_the_same_session() {
    let k = key(1);
    let session = sample_session();
    let token = generate_csrf_token(&session, &k);
    assert!(verify_csrf_token(&token, &session, &k));
}

#[test]
fn csrf_token_is_rejected_for_a_different_session() {
    let k = key(1);
    let session = sample_session();
    let token = generate_csrf_token(&session, &k);

    let mut other_session = sample_session();
    other_session.owner_id = Id::from_i64(99);
    assert!(!verify_csrf_token(&token, &other_session, &k));
}

#[test]
fn csrf_token_is_rejected_under_a_different_key() {
    let session = sample_session();
    let token = generate_csrf_token(&session, &key(1));
    assert!(!verify_csrf_token(&token, &session, &key(2)));
}

#[test]
fn csrf_token_is_rejected_when_tampered() {
    let k = key(1);
    let session = sample_session();
    let mut token = generate_csrf_token(&session, &k);
    // Flip the first hex character so the decoded bytes differ.
    let first = token.chars().next().unwrap();
    let replacement = if first == '0' { '1' } else { '0' };
    token.replace_range(0..1, &replacement.to_string());
    assert!(!verify_csrf_token(&token, &session, &k));
}

#[test]
fn csrf_token_never_panics_on_malformed_presented_values() {
    let k = key(1);
    let session = sample_session();
    for malformed in ["", "not-hex-at-all", "deadbee", "dead🙂ef"] {
        assert!(!verify_csrf_token(malformed, &session, &k));
    }
}

/// A session cookie's own signed value must never double as a valid CSRF
/// token, even though both are HMACed under the same `token_hash_key` — see
/// this module's doc comment ("CSRF token...") for the domain-separation
/// rationale.
#[test]
fn a_session_cookies_own_signature_is_not_a_valid_csrf_token() {
    let k = key(1);
    let session = sample_session();
    let cookie_value = encode_session_cookie(&session, &k);
    let (_, cookie_mac_hex) = cookie_value.rsplit_once('.').unwrap();
    assert!(!verify_csrf_token(cookie_mac_hex, &session, &k));
}

// ---- query_separator ----

#[test]
fn query_separator_is_question_mark_when_no_query_string_present() {
    assert_eq!(query_separator("https://client.example/callback"), '?');
}

#[test]
fn query_separator_is_ampersand_when_a_query_string_is_already_present() {
    assert_eq!(
        query_separator("https://client.example/callback?foo=bar"),
        '&'
    );
}

// ---- extract_owner_session_cookie / resolve_owner_session ----

#[test]
fn extract_owner_session_cookie_finds_the_named_cookie_among_several() {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::COOKIE,
        format!("unrelated=1; {OWNER_SESSION_COOKIE_NAME}=abc123; other=2")
            .parse()
            .unwrap(),
    );
    assert_eq!(
        extract_owner_session_cookie(&headers),
        Some("abc123".to_string())
    );
}

#[test]
fn extract_owner_session_cookie_returns_none_when_absent() {
    let headers = HeaderMap::new();
    assert_eq!(extract_owner_session_cookie(&headers), None);
}

#[test]
fn resolve_owner_session_round_trips_a_freshly_encoded_session() {
    let k = key(7);
    let session = sample_session();
    let cookie_value = encode_session_cookie(&session, &k);
    let mut headers = HeaderMap::new();
    headers.insert(
        header::COOKIE,
        format!("{OWNER_SESSION_COOKIE_NAME}={cookie_value}")
            .parse()
            .unwrap(),
    );

    let resolved = resolve_owner_session(&headers, &k, session.expires_at - Duration::seconds(1))
        .expect("a freshly encoded, not-yet-expired session must resolve");
    assert_eq!(resolved, session);
}

#[test]
fn resolve_owner_session_returns_none_for_a_missing_cookie() {
    let headers = HeaderMap::new();
    assert_eq!(
        resolve_owner_session(&headers, &key(7), OffsetDateTime::UNIX_EPOCH),
        None
    );
}

#[test]
fn resolve_owner_session_returns_none_for_an_expired_session() {
    let k = key(7);
    let session = sample_session();
    let cookie_value = encode_session_cookie(&session, &k);
    let mut headers = HeaderMap::new();
    headers.insert(
        header::COOKIE,
        format!("{OWNER_SESSION_COOKIE_NAME}={cookie_value}")
            .parse()
            .unwrap(),
    );

    let after_expiry = session.expires_at + Duration::seconds(1);
    assert_eq!(resolve_owner_session(&headers, &k, after_expiry), None);
}

// ---- build_session_set_cookie_header ----

#[test]
fn set_cookie_header_carries_httponly_and_samesite_lax() {
    let header = build_session_set_cookie_header("cookie-value", false);
    assert!(header.starts_with(&format!("{OWNER_SESSION_COOKIE_NAME}=cookie-value;")));
    assert!(header.contains("HttpOnly"));
    assert!(header.contains("SameSite=Lax"));
    assert!(header.contains("Path=/oauth/authorize"));
    assert!(!header.contains("Secure"));
}

#[test]
fn set_cookie_header_carries_secure_when_requested() {
    let header = build_session_set_cookie_header("cookie-value", true);
    assert!(header.contains("; Secure"));
}

// ---- bad_request ----

#[test]
fn bad_request_builds_a_400_client_error_with_the_given_message() {
    let err = bad_request("something is wrong");
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert_eq!(err.kind, crate::error::ErrorKind::Client);
    assert_eq!(err.public_message, "something is wrong");
}
