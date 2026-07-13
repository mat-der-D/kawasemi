//! Pure unit tests for `TokenEndpoint`'s helper functions (Requirements
//! 3.1, 3.4, task 5.3) — no database required. DB-backed, end-to-end
//! handler coverage (successful exchange, credential/code/redirect_uri/PKCE
//! rejection, revoke authentication and idempotency) lives in
//! `tests/oauth_token_it.rs` per design.md's File Structure Plan, mirroring
//! `apps_endpoint/tests.rs`'s established split between pure-function unit
//! tests here and DB-backed integration tests in `tests/`.

use super::*;
use crate::config::Secret;
use crate::domain::Id;
use crate::oauth::model::{AccessToken, ScopeSet};

fn sample_issued_token(scopes: &[&str], created_at_unix: i64) -> IssuedToken {
    IssuedToken {
        plaintext: Secret::new("plaintext-access-token-value".to_string()),
        token: AccessToken {
            id: Id::from_i64(1),
            token_hash: vec![0u8; 32],
            app_id: Id::from_i64(2),
            actor_id: Id::from_i64(3),
            scopes: ScopeSet::new(scopes.iter().copied()),
            created_at: time::OffsetDateTime::from_unix_timestamp(created_at_unix)
                .expect("a valid unix timestamp"),
            revoked: false,
        },
    }
}

// ---- validate_grant_type ----

#[test]
fn validate_grant_type_accepts_an_empty_grant_type() {
    validate_grant_type("").expect("an absent grant_type must be treated as implicit");
}

#[test]
fn validate_grant_type_accepts_the_supported_grant_type() {
    validate_grant_type("authorization_code").expect("the supported grant_type must be accepted");
}

#[test]
fn validate_grant_type_rejects_an_unsupported_grant_type() {
    let err = validate_grant_type("client_credentials")
        .expect_err("an unsupported grant_type must be rejected");
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
}

#[test]
fn validate_grant_type_rejects_a_misspelled_grant_type() {
    let err = validate_grant_type("authorization-code") // hyphen, not underscore
        .expect_err("a misspelled grant_type must be rejected, not fuzzy-matched");
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
}

// ---- to_token_response ----

#[test]
fn to_token_response_maps_plaintext_to_access_token() {
    let issued = sample_issued_token(&["read"], 1_700_000_000);
    let response = to_token_response(&issued);
    assert_eq!(response.access_token, "plaintext-access-token-value");
}

#[test]
fn to_token_response_token_type_is_always_bearer() {
    let issued = sample_issued_token(&["read"], 1_700_000_000);
    let response = to_token_response(&issued);
    assert_eq!(response.token_type, "Bearer");
}

#[test]
fn to_token_response_joins_multiple_scopes_space_separated() {
    let issued = sample_issued_token(&["read", "write:statuses"], 1_700_000_000);
    let response = to_token_response(&issued);
    // `model::ScopeSet` is stored as a `BTreeSet<String>`, so ordering is
    // lexicographic; "read" < "write:statuses".
    assert_eq!(response.scope, "read write:statuses");
}

#[test]
fn to_token_response_empty_scope_set_yields_empty_scope_string() {
    let issued = sample_issued_token(&[], 1_700_000_000);
    let response = to_token_response(&issued);
    assert_eq!(response.scope, "");
}

#[test]
fn to_token_response_created_at_is_a_unix_timestamp_in_seconds() {
    let issued = sample_issued_token(&["read"], 1_700_000_000);
    let response = to_token_response(&issued);
    assert_eq!(response.created_at, 1_700_000_000);
}

#[test]
fn to_token_response_serializes_with_the_mastodon_compatible_field_names() {
    let issued = sample_issued_token(&["read", "write"], 1_700_000_000);
    let response = to_token_response(&issued);
    let value = serde_json::to_value(&response).expect("TokenResponse must serialize");
    assert_eq!(
        value.get("access_token").and_then(|v| v.as_str()),
        Some("plaintext-access-token-value")
    );
    assert_eq!(
        value.get("token_type").and_then(|v| v.as_str()),
        Some("Bearer")
    );
    assert_eq!(
        value.get("scope").and_then(|v| v.as_str()),
        Some("read write")
    );
    assert_eq!(
        value.get("created_at").and_then(|v| v.as_i64()),
        Some(1_700_000_000)
    );
}

// ---- invalid_client_credentials ----

#[test]
fn invalid_client_credentials_is_a_401() {
    let err = invalid_client_credentials();
    assert_eq!(err.status, StatusCode::UNAUTHORIZED);
}
