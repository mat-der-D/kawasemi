//! Integration tests for `AppsEndpoint` (Requirements 1.1, 1.2, 1.5; task
//! 5.1, design.md's File Structure Plan: `tests/oauth_apps_it.rs`).
//!
//! `AppsEndpoint` is not mounted on any router yet (that wiring is task
//! 7.1, out of this task's boundary — see `src/oauth/apps_endpoint.rs`'s own
//! doc comment). These tests therefore call
//! `kawasemi::oauth::apps_endpoint::register_app`/`verify_credentials`
//! directly as ordinary async functions, constructing the axum extractor
//! values (`State`, `Json`, `HeaderMap`) by hand, against a real Postgres
//! instance via `spawn_test_app` (mirroring `src/oauth/service/tests.rs`'s
//! own established convention for a not-yet-wired OAuth component).

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;

use kawasemi::config::Secret;
use kawasemi::oauth::apps_endpoint::{self, AppsEndpointState, RegisterAppRequest};
use kawasemi::oauth::hash::TokenHashKey;
use kawasemi::oauth::service::OauthService;
use kawasemi::test_harness::spawn_test_app;

/// A fixed, non-production token-hashing key for this test module only —
/// mirrors `service/tests.rs::test_token_hash_key`'s own reasoning.
fn test_token_hash_key() -> TokenHashKey {
    Secret::new([0x77; 32])
}

fn sample_register_request() -> RegisterAppRequest {
    RegisterAppRequest {
        client_name: "Test Client".to_string(),
        redirect_uris: vec!["https://client.example/callback".to_string()],
        scopes: "read write".to_string(),
    }
}

fn basic_auth_header(client_id: &str, client_secret: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    let encoded = BASE64_STANDARD.encode(format!("{client_id}:{client_secret}"));
    headers.insert(
        header::AUTHORIZATION,
        format!("Basic {encoded}")
            .parse()
            .expect("a base64-encoded Basic auth value is a valid header value"),
    );
    headers
}

// ---- register_app ----

/// Requirements 1.1, 1.4: a well-formed registration request returns 200
/// with client credentials (a non-empty `client_id`/`client_secret`) and
/// the registered app's redirect URIs/scopes included in the response.
#[tokio::test]
async fn register_app_returns_client_credentials_in_the_response() {
    let app = spawn_test_app().await;
    let key = test_token_hash_key();
    let service = Arc::new(OauthService::new(
        app.pool.clone(),
        app.runtime.clone(),
        key.clone(),
    ));
    let state = AppsEndpointState {
        service,
        pool: app.pool.clone(),
        token_hash_key: key,
    };

    let Json(response) = apps_endpoint::register_app(State(state), Json(sample_register_request()))
        .await
        .expect("registering a well-formed app must succeed");

    assert!(!response.client_id.is_empty());
    assert_eq!(
        response.client_secret.as_deref().map(str::is_empty),
        Some(false),
        "registration response must include a non-empty client_secret"
    );
    assert_eq!(response.name, "Test Client");
    assert_eq!(
        response.redirect_uris,
        vec!["https://client.example/callback".to_string()]
    );
    assert_eq!(
        response.scopes,
        vec!["read".to_string(), "write".to_string()]
    );

    app.cleanup().await;
}

/// Requirement 1.2: a missing `client_name` (empty string, since the JSON
/// body omits the field and `RegisterAppRequest` defaults it) is rejected
/// with a 422 (Unprocessable Entity), a compatible validation-shaped error.
#[tokio::test]
async fn register_app_rejects_a_missing_client_name_with_422() {
    let app = spawn_test_app().await;
    let key = test_token_hash_key();
    let service = Arc::new(OauthService::new(
        app.pool.clone(),
        app.runtime.clone(),
        key.clone(),
    ));
    let state = AppsEndpointState {
        service,
        pool: app.pool.clone(),
        token_hash_key: key,
    };

    let err = apps_endpoint::register_app(
        State(state),
        Json(RegisterAppRequest {
            client_name: String::new(),
            ..sample_register_request()
        }),
    )
    .await
    .expect_err("a missing client_name must be rejected");
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);

    app.cleanup().await;
}

/// Requirement 1.2: a missing `redirect_uris` (empty vec, from an omitted
/// JSON field) is rejected with a 422.
#[tokio::test]
async fn register_app_rejects_missing_redirect_uris_with_422() {
    let app = spawn_test_app().await;
    let key = test_token_hash_key();
    let service = Arc::new(OauthService::new(
        app.pool.clone(),
        app.runtime.clone(),
        key.clone(),
    ));
    let state = AppsEndpointState {
        service,
        pool: app.pool.clone(),
        token_hash_key: key,
    };

    let err = apps_endpoint::register_app(
        State(state),
        Json(RegisterAppRequest {
            redirect_uris: vec![],
            ..sample_register_request()
        }),
    )
    .await
    .expect_err("missing redirect_uris must be rejected");
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);

    app.cleanup().await;
}

/// Requirement 1.2: a malformed redirect URI (no `scheme://` form) is
/// rejected with a 422.
#[tokio::test]
async fn register_app_rejects_a_malformed_redirect_uri_with_422() {
    let app = spawn_test_app().await;
    let key = test_token_hash_key();
    let service = Arc::new(OauthService::new(
        app.pool.clone(),
        app.runtime.clone(),
        key.clone(),
    ));
    let state = AppsEndpointState {
        service,
        pool: app.pool.clone(),
        token_hash_key: key,
    };

    let err = apps_endpoint::register_app(
        State(state),
        Json(RegisterAppRequest {
            redirect_uris: vec!["not-a-valid-uri".to_string()],
            ..sample_register_request()
        }),
    )
    .await
    .expect_err("a malformed redirect_uri must be rejected");
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);

    app.cleanup().await;
}

/// Requirement 1.3 (shared with registration): an unknown scope token is
/// rejected with a 422.
#[tokio::test]
async fn register_app_rejects_an_unknown_scope_with_422() {
    let app = spawn_test_app().await;
    let key = test_token_hash_key();
    let service = Arc::new(OauthService::new(
        app.pool.clone(),
        app.runtime.clone(),
        key.clone(),
    ));
    let state = AppsEndpointState {
        service,
        pool: app.pool.clone(),
        token_hash_key: key,
    };

    let err = apps_endpoint::register_app(
        State(state),
        Json(RegisterAppRequest {
            scopes: "read totally_bogus_scope".to_string(),
            ..sample_register_request()
        }),
    )
    .await
    .expect_err("an unknown scope token must be rejected");
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);

    app.cleanup().await;
}

// ---- verify_credentials ----

/// Requirement 1.5: presenting the correct client credentials (as issued by
/// a prior registration) via HTTP Basic returns the application's public
/// info, without the client_secret.
#[tokio::test]
async fn verify_credentials_returns_public_info_without_the_secret() {
    let app = spawn_test_app().await;
    let key = test_token_hash_key();
    let service = Arc::new(OauthService::new(
        app.pool.clone(),
        app.runtime.clone(),
        key.clone(),
    ));
    let state = AppsEndpointState {
        service: service.clone(),
        pool: app.pool.clone(),
        token_hash_key: key,
    };

    let Json(registered) =
        apps_endpoint::register_app(State(state.clone()), Json(sample_register_request()))
            .await
            .expect("registering the app under test must succeed");
    let client_secret = registered
        .client_secret
        .clone()
        .expect("registration response must carry the plaintext secret");

    let headers = basic_auth_header(&registered.client_id, &client_secret);
    let Json(verified) = apps_endpoint::verify_credentials(State(state), headers)
        .await
        .expect("correct client credentials must verify");

    assert_eq!(verified.client_id, registered.client_id);
    assert_eq!(verified.name, registered.name);
    assert_eq!(verified.redirect_uris, registered.redirect_uris);
    assert_eq!(verified.scopes, registered.scopes);
    assert!(
        verified.client_secret.is_none(),
        "verify_credentials must never echo the client_secret back"
    );

    app.cleanup().await;
}

/// Requirement 1.5: a wrong client_secret for a real, registered client_id
/// is rejected with a 401 (authentication error), not the app's info.
#[tokio::test]
async fn verify_credentials_rejects_a_wrong_client_secret_with_401() {
    let app = spawn_test_app().await;
    let key = test_token_hash_key();
    let service = Arc::new(OauthService::new(
        app.pool.clone(),
        app.runtime.clone(),
        key.clone(),
    ));
    let state = AppsEndpointState {
        service: service.clone(),
        pool: app.pool.clone(),
        token_hash_key: key,
    };

    let Json(registered) =
        apps_endpoint::register_app(State(state.clone()), Json(sample_register_request()))
            .await
            .expect("registering the app under test must succeed");

    let headers = basic_auth_header(&registered.client_id, "definitely-the-wrong-secret");
    let err = apps_endpoint::verify_credentials(State(state), headers)
        .await
        .expect_err("a wrong client_secret must be rejected");
    assert_eq!(err.status, StatusCode::UNAUTHORIZED);

    app.cleanup().await;
}

/// Requirement 1.5: an unknown client_id is rejected with a 401.
#[tokio::test]
async fn verify_credentials_rejects_an_unknown_client_id_with_401() {
    let app = spawn_test_app().await;
    let key = test_token_hash_key();
    let service = Arc::new(OauthService::new(
        app.pool.clone(),
        app.runtime.clone(),
        key.clone(),
    ));
    let state = AppsEndpointState {
        service,
        pool: app.pool.clone(),
        token_hash_key: key,
    };

    let headers = basic_auth_header("no-such-client-id-was-ever-registered", "any-secret");
    let err = apps_endpoint::verify_credentials(State(state), headers)
        .await
        .expect_err("an unknown client_id must be rejected");
    assert_eq!(err.status, StatusCode::UNAUTHORIZED);

    app.cleanup().await;
}

/// Requirement 1.5: a request presenting no credentials at all (no
/// Authorization header) is rejected with a 401, the same as invalid
/// credentials — never a different status that would let a caller
/// distinguish "absent" from "wrong".
#[tokio::test]
async fn verify_credentials_rejects_a_missing_authorization_header_with_401() {
    let app = spawn_test_app().await;
    let key = test_token_hash_key();
    let service = Arc::new(OauthService::new(
        app.pool.clone(),
        app.runtime.clone(),
        key.clone(),
    ));
    let state = AppsEndpointState {
        service,
        pool: app.pool.clone(),
        token_hash_key: key,
    };

    let err = apps_endpoint::verify_credentials(State(state), HeaderMap::new())
        .await
        .expect_err("a missing Authorization header must be rejected");
    assert_eq!(err.status, StatusCode::UNAUTHORIZED);

    app.cleanup().await;
}
