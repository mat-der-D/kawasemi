//! Tests for `OwnerGate` (Requirement 2.2, task 4.1).
//!
//! Split into two groups, mirroring this task brief's guidance:
//! - Pure unit tests (no DB) for the constant-time credential comparison
//!   and the cookie signing/verification primitives — both are plain
//!   functions over in-memory values.
//! - `spawn_test_app`-backed integration tests for [`authenticate_owner`]
//!   itself, since it needs a real [`ActorDirectory`] (backed by a real
//!   Postgres pool) to resolve `sole_owner()`.

use time::{Duration, OffsetDateTime};

use super::*;
use crate::actor::ActorDirectory;
use crate::actor::owner::create_owner;
use crate::config::Secret;
use crate::error::ErrorKind;
use crate::oauth::hash::TokenHashKey;
use crate::test_harness::spawn_test_app;

fn credential(password: &str) -> OwnerCredential {
    OwnerCredential {
        password: Secret::new(password.to_string()),
    }
}

fn login(password: &str) -> OwnerLogin {
    OwnerLogin {
        password: Secret::new(password.to_string()),
    }
}

fn key(byte: u8) -> TokenHashKey {
    Secret::new([byte; 32])
}

fn sample_session() -> OwnerSession {
    OwnerSession {
        owner_id: Id::from_i64(42),
        // Whole-second precision so encode/decode round-trips are exact
        // (see `encode_session_cookie`'s doc comment's precision note).
        expires_at: OffsetDateTime::UNIX_EPOCH + Duration::seconds(1_700_000_000),
    }
}

// --- passwords_match: constant-time credential comparison (pure, no DB) ---

#[test]
fn passwords_match_accepts_the_correct_password() {
    let cfg = credential("correct-horse-battery-staple");
    let presented = login("correct-horse-battery-staple");
    assert!(passwords_match(&cfg, &presented));
}

#[test]
fn passwords_match_rejects_the_wrong_password() {
    let cfg = credential("correct-horse-battery-staple");
    let presented = login("wrong-password-entirely");
    assert!(!passwords_match(&cfg, &presented));
}

#[test]
fn passwords_match_rejects_a_password_that_differs_only_in_length() {
    let cfg = credential("correct-horse-battery-staple");
    let presented = login("correct-horse-battery-staple-but-longer");
    assert!(!passwords_match(&cfg, &presented));
}

#[test]
fn passwords_match_rejects_empty_presented_password() {
    let cfg = credential("correct-horse-battery-staple");
    let presented = login("");
    assert!(!passwords_match(&cfg, &presented));
}

// --- encode_session_cookie / decode_session_cookie (pure, no DB) ---

#[test]
fn encoded_cookie_decodes_back_to_the_same_session() {
    let k = key(7);
    let session = sample_session();
    let cookie_value = encode_session_cookie(&session, &k);

    let decoded =
        decode_session_cookie(&cookie_value, &k, session.expires_at - Duration::seconds(1))
            .expect("a freshly encoded, not-yet-expired cookie must decode");
    assert_eq!(decoded, session);
}

#[test]
fn decode_session_cookie_rejects_a_tampered_payload() {
    let k = key(7);
    let session = sample_session();
    let cookie_value = encode_session_cookie(&session, &k);

    // Flip the owner_id in the payload half without recomputing the MAC.
    let (payload, mac_hex) = cookie_value.rsplit_once('.').unwrap();
    let tampered_payload = payload.replacen("42", "99", 1);
    let tampered = format!("{tampered_payload}.{mac_hex}");

    let err = decode_session_cookie(&tampered, &k, session.expires_at - Duration::seconds(1))
        .expect_err("a tampered payload must be rejected");
    assert_eq!(err.status, axum::http::StatusCode::UNAUTHORIZED);
}

#[test]
fn decode_session_cookie_rejects_a_cookie_signed_under_a_different_key() {
    let session = sample_session();
    let cookie_value = encode_session_cookie(&session, &key(7));

    let err = decode_session_cookie(
        &cookie_value,
        &key(9),
        session.expires_at - Duration::seconds(1),
    )
    .expect_err("a cookie signed under a different key must be rejected");
    assert_eq!(err.status, axum::http::StatusCode::UNAUTHORIZED);
}

#[test]
fn decode_session_cookie_rejects_an_expired_session() {
    let k = key(7);
    let session = sample_session();
    let cookie_value = encode_session_cookie(&session, &k);

    let after_expiry = session.expires_at + Duration::seconds(1);
    let err = decode_session_cookie(&cookie_value, &k, after_expiry)
        .expect_err("an expired session must be rejected even with a valid signature");
    assert_eq!(err.status, axum::http::StatusCode::UNAUTHORIZED);
}

#[test]
fn decode_session_cookie_rejects_a_session_exactly_at_its_expiry_instant() {
    let k = key(7);
    let session = sample_session();
    let cookie_value = encode_session_cookie(&session, &k);

    let err = decode_session_cookie(&cookie_value, &k, session.expires_at)
        .expect_err("expires_at is not itself a still-valid instant");
    assert_eq!(err.status, axum::http::StatusCode::UNAUTHORIZED);
}

#[test]
fn decode_session_cookie_never_panics_on_malformed_input() {
    let k = key(7);
    let now = OffsetDateTime::UNIX_EPOCH;
    for malformed in [
        "",
        "no-dot-separator",
        "42:1700000000.not-hex-at-all",
        "42:1700000000.deadbee",  // odd-length hex
        "42:1700000000.dead🙂ef", // multi-byte UTF-8 mixed into the hex half
        "🙂:1700000000.deadbeef", // multi-byte UTF-8 mixed into the payload half
        "notanumber:notanumber.deadbeef",
        ".",
        "...",
    ] {
        let result = decode_session_cookie(malformed, &k, now);
        assert!(
            result.is_err(),
            "malformed cookie value {malformed:?} must be rejected, not accepted"
        );
    }
}

// --- authenticate_owner (integration: needs a real ActorDirectory/pool) ---

#[tokio::test]
async fn authenticate_owner_succeeds_and_resolves_the_sole_owner() {
    let app = spawn_test_app().await;
    let directory = ActorDirectory::new(app.pool.clone());

    let now = app.runtime.clock.now();
    let owner_id = app.runtime.ids.next_id();
    create_owner(&app.pool, owner_id, now)
        .await
        .expect("creating the owner fixture must succeed");

    let cfg = credential("the-real-owner-passphrase");
    let presented = login("the-real-owner-passphrase");

    let session = authenticate_owner(&cfg, &presented, &directory, now)
        .await
        .expect("correct credentials against a bootstrapped instance must authenticate");
    assert_eq!(session.owner_id, owner_id);
    assert_eq!(session.expires_at, now + OWNER_SESSION_TTL);

    app.cleanup().await;
}

#[tokio::test]
async fn authenticate_owner_rejects_the_wrong_password_without_querying_the_directory() {
    let app = spawn_test_app().await;
    let directory = ActorDirectory::new(app.pool.clone());
    // Deliberately no owner fixture created: if this rejection path ever
    // regressed into calling `directory.sole_owner()` before checking the
    // password, it would surface as the *sole_owner* 5xx error below
    // instead of the expected 401 — making that regression visible here.
    let now = app.runtime.clock.now();

    let cfg = credential("the-real-owner-passphrase");
    let presented = login("a-completely-wrong-passphrase");

    let err = authenticate_owner(&cfg, &presented, &directory, now)
        .await
        .expect_err("wrong credentials must be rejected");
    assert_eq!(err.kind, ErrorKind::Client);
    assert_eq!(err.status, axum::http::StatusCode::UNAUTHORIZED);

    app.cleanup().await;
}

#[tokio::test]
async fn authenticate_owner_propagates_sole_owner_invariant_violations_as_server_errors() {
    let app = spawn_test_app().await;
    let directory = ActorDirectory::new(app.pool.clone());
    // No owner fixture created: a fresh schema has zero `owners` rows.
    let now = app.runtime.clock.now();

    let cfg = credential("the-real-owner-passphrase");
    let presented = login("the-real-owner-passphrase");

    let err = authenticate_owner(&cfg, &presented, &directory, now)
        .await
        .expect_err("a correct credential against an un-bootstrapped instance must still fail");
    assert_eq!(
        err.kind,
        ErrorKind::Server,
        "an un-bootstrapped instance is a system problem, not an authentication failure"
    );
    assert_eq!(err.status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);

    app.cleanup().await;
}

/// End-to-end within this task's boundary: correct credentials yield a
/// session, that session encodes into a signed cookie value, and the cookie
/// decodes back to the same session — directly covering `tasks.md`'s stated
/// completion condition ("正しい資格情報でセッションが得られて署名付き
/// Cookie が発行され...ることを単体テストで確認できる").
#[tokio::test]
async fn correct_credentials_yield_a_session_that_encodes_into_a_verifiable_signed_cookie() {
    let app = spawn_test_app().await;
    let directory = ActorDirectory::new(app.pool.clone());

    let now = app.runtime.clock.now();
    let owner_id = app.runtime.ids.next_id();
    create_owner(&app.pool, owner_id, now)
        .await
        .expect("creating the owner fixture must succeed");

    let cfg = credential("the-real-owner-passphrase");
    let presented = login("the-real-owner-passphrase");
    let session = authenticate_owner(&cfg, &presented, &directory, now)
        .await
        .expect("correct credentials must authenticate");

    let signing_key = key(3);
    let cookie_value = encode_session_cookie(&session, &signing_key);
    let decoded = decode_session_cookie(&cookie_value, &signing_key, now)
        .expect("the just-issued cookie must decode while still valid");
    assert_eq!(decoded, session);
    assert_eq!(decoded.owner_id, owner_id);

    app.cleanup().await;
}
