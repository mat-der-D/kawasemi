//! Pure unit tests for `AppsEndpoint`'s helper functions (Requirements 1.1,
//! 1.5, task 5.1) — no database required. DB-backed, end-to-end handler
//! coverage (successful registration, validation rejection, credential
//! verification success/failure) lives in `tests/oauth_apps_it.rs` per
//! design.md's File Structure Plan, mirroring how `scope.rs`/`pkce.rs` keep
//! their own pure-function tests local while `OauthService`'s DB-dependent
//! behavior lives in `service/tests.rs`.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;

use super::*;
use crate::config::Secret;
use crate::domain::Id;
use crate::oauth::model::ScopeSet;

fn sample_app(with_secret: &str) -> OauthApp {
    OauthApp {
        id: Id::from_i64(7),
        client_id: "client-abc123".to_string(),
        client_secret: Secret::new(with_secret.to_string()),
        redirect_uris: vec!["https://client.example/callback".to_string()],
        scopes: ScopeSet::new(["read", "write"]),
        name: "Test Client".to_string(),
        created_at: time::OffsetDateTime::UNIX_EPOCH,
    }
}

fn basic_auth_header(client_id: &str, client_secret: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    let encoded = BASE64_STANDARD.encode(format!("{client_id}:{client_secret}"));
    headers.insert(
        header::AUTHORIZATION,
        format!("Basic {encoded}").parse().unwrap(),
    );
    headers
}

// ---- to_response ----

#[test]
fn to_response_includes_client_secret_when_requested() {
    let app = sample_app("plaintext-secret-value");
    let response = to_response(&app, true);
    assert_eq!(
        response.client_secret.as_deref(),
        Some("plaintext-secret-value")
    );
    assert_eq!(response.client_id, "client-abc123");
    assert_eq!(
        response.redirect_uris,
        vec!["https://client.example/callback".to_string()]
    );
    assert_eq!(
        response.scopes,
        vec!["read".to_string(), "write".to_string()]
    );
}

#[test]
fn to_response_omits_client_secret_when_not_requested() {
    let app = sample_app("plaintext-secret-value");
    let response = to_response(&app, false);
    assert!(response.client_secret.is_none());
}

#[test]
fn to_response_public_variant_serializes_without_a_client_secret_key() {
    let app = sample_app("plaintext-secret-value");
    let response = to_response(&app, false);
    let value = serde_json::to_value(&response).expect("AppResponse must serialize");
    assert!(
        value.get("client_secret").is_none(),
        "client_secret key must be entirely absent (not null) from a public response: {value:?}"
    );
}

#[test]
fn to_response_registration_variant_serializes_with_a_client_secret_key() {
    let app = sample_app("plaintext-secret-value");
    let response = to_response(&app, true);
    let value = serde_json::to_value(&response).expect("AppResponse must serialize");
    assert_eq!(
        value.get("client_secret").and_then(|v| v.as_str()),
        Some("plaintext-secret-value")
    );
}

#[test]
fn to_response_id_serializes_as_a_decimal_string_not_a_number() {
    let app = sample_app("s");
    let response = to_response(&app, false);
    let value = serde_json::to_value(&response).expect("AppResponse must serialize");
    assert_eq!(value.get("id").and_then(|v| v.as_str()), Some("7"));
}

// ---- extract_basic_credentials ----

#[test]
fn extract_basic_credentials_parses_a_valid_header() {
    let headers = basic_auth_header("my-client-id", "my-client-secret");
    let (client_id, client_secret) =
        extract_basic_credentials(&headers).expect("a well-formed Basic header must parse");
    assert_eq!(client_id, "my-client-id");
    assert_eq!(client_secret, "my-client-secret");
}

#[test]
fn extract_basic_credentials_splits_only_on_the_first_colon() {
    let headers = basic_auth_header("my-client-id", "secret:with:colons");
    let (client_id, client_secret) = extract_basic_credentials(&headers).expect("must parse");
    assert_eq!(client_id, "my-client-id");
    assert_eq!(client_secret, "secret:with:colons");
}

#[test]
fn extract_basic_credentials_rejects_a_missing_authorization_header() {
    let headers = HeaderMap::new();
    let err = extract_basic_credentials(&headers)
        .expect_err("a missing Authorization header must be rejected");
    assert_eq!(err.status, StatusCode::UNAUTHORIZED);
}

#[test]
fn extract_basic_credentials_rejects_a_non_basic_scheme() {
    let mut headers = HeaderMap::new();
    headers.insert(header::AUTHORIZATION, "Bearer some-token".parse().unwrap());
    let err = extract_basic_credentials(&headers).expect_err("a non-Basic scheme must be rejected");
    assert_eq!(err.status, StatusCode::UNAUTHORIZED);
}

#[test]
fn extract_basic_credentials_rejects_invalid_base64() {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        "Basic not-valid-base64!!!".parse().unwrap(),
    );
    let err = extract_basic_credentials(&headers).expect_err("invalid base64 must be rejected");
    assert_eq!(err.status, StatusCode::UNAUTHORIZED);
}

#[test]
fn extract_basic_credentials_rejects_a_payload_with_no_colon_separator() {
    let mut headers = HeaderMap::new();
    let encoded = BASE64_STANDARD.encode("no-colon-in-here");
    headers.insert(
        header::AUTHORIZATION,
        format!("Basic {encoded}").parse().unwrap(),
    );
    let err = extract_basic_credentials(&headers)
        .expect_err("a decoded payload with no ':' must be rejected");
    assert_eq!(err.status, StatusCode::UNAUTHORIZED);
}

#[test]
fn extract_basic_credentials_rejects_non_utf8_decoded_bytes() {
    let mut headers = HeaderMap::new();
    let encoded = BASE64_STANDARD.encode([0xFF, 0xFE, 0xFD]);
    headers.insert(
        header::AUTHORIZATION,
        format!("Basic {encoded}").parse().unwrap(),
    );
    let err = extract_basic_credentials(&headers)
        .expect_err("non-UTF-8 decoded bytes must be rejected, not panic");
    assert_eq!(err.status, StatusCode::UNAUTHORIZED);
}

#[test]
fn extract_basic_credentials_never_panics_on_a_variety_of_malformed_headers() {
    let malformed_values = [
        "",
        "Basic",
        "Basic ",
        "Basic ===",
        "basic bXk6c2VjcmV0", // lowercase scheme, not "Basic"
    ];
    for raw in malformed_values {
        let mut headers = HeaderMap::new();
        if let Ok(value) = raw.parse() {
            headers.insert(header::AUTHORIZATION, value);
        } else {
            continue;
        }
        let result = extract_basic_credentials(&headers);
        assert!(
            result.is_err(),
            "malformed header {raw:?} must be rejected, not accepted"
        );
    }
}
