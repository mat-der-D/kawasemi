//! Integration tests for `TokenEndpoint` (Requirements 3.1, 3.2, 3.4; task
//! 5.3, design.md's File Structure Plan).
//!
//! Naming: design.md's File Structure Plan names a single combined
//! `tests/oauth_flow_it.rs` for the full end-to-end authorize+token flow
//! (owner auth -> actor selection -> code -> token -> revoke), which reads
//! more naturally as the later cross-cutting integration task (9.1) that
//! exercises `AuthorizeEndpoint` and `TokenEndpoint` together over the fully
//! wired router (task 7.1's job). This task's own boundary
//! (`Boundary: TokenEndpoint`) covers exactly the token/revoke endpoints in
//! isolation, so this file is named `oauth_token_it.rs` — mirroring task
//! 5.2's own identical naming choice (`tests/oauth_authorize_it.rs`, scoped
//! to `AuthorizeEndpoint` alone rather than the combined flow file).
//!
//! `TokenEndpoint` is not mounted on any router yet (that wiring is task
//! 7.1, out of this task's boundary — see `src/oauth/token_endpoint.rs`'s
//! own doc comment). These tests therefore call
//! `kawasemi::oauth::token_endpoint::exchange_token`/`revoke_token` directly
//! as ordinary async functions, constructing the axum extractor values
//! (`State`, `Form`) by hand, against a real Postgres instance via
//! `spawn_test_app` (mirroring `tests/oauth_apps_it.rs`'s established
//! convention for a not-yet-wired OAuth component).

use std::sync::Arc;

use axum::Json;
use axum::extract::{Form, State};
use axum::http::StatusCode;

use kawasemi::config::Secret;
use kawasemi::oauth::hash::TokenHashKey;
use kawasemi::oauth::pkce::PkceChallenge as RealPkceChallenge;
use kawasemi::oauth::service::{AuthorizeApproval, NewApp, OauthService};
use kawasemi::oauth::token_endpoint::{
    self, RevokeRequest, RevokeResponse, TokenEndpointState, TokenExchangeRequest,
};
use kawasemi::oauth::token_repository;
use kawasemi::test_harness::spawn_test_app;

/// A fixed, non-production token-hashing key for this test module only —
/// mirrors `oauth_apps_it.rs::test_token_hash_key`'s own reasoning.
fn test_token_hash_key() -> TokenHashKey {
    Secret::new([0x99; 32])
}

fn sample_new_app() -> NewApp {
    NewApp {
        name: "Test Client".to_string(),
        redirect_uris: vec!["https://client.example/callback".to_string()],
        scopes: "read write".to_string(),
    }
}

/// Everything a test needs: a `TokenEndpointState` plus the underlying
/// `OauthService` (for setting up apps/codes directly, mirroring
/// `service/tests.rs`'s own established pattern for not-yet-wired OAuth
/// components).
struct Fixture {
    state: TokenEndpointState,
    service: Arc<OauthService>,
}

fn build_fixture(pool: sqlx::PgPool, runtime: kawasemi::runtime::RuntimeContext) -> Fixture {
    let key = test_token_hash_key();
    let service = Arc::new(OauthService::new(pool.clone(), runtime, key.clone()));
    let state = TokenEndpointState {
        service: service.clone(),
        pool,
        token_hash_key: key,
    };
    Fixture { state, service }
}

fn empty_token_request() -> TokenExchangeRequest {
    TokenExchangeRequest {
        grant_type: String::new(),
        code: String::new(),
        client_id: String::new(),
        client_secret: String::new(),
        redirect_uri: String::new(),
        code_verifier: None,
    }
}

// ---- exchange_token: success ----

/// Requirements 3.1, 3.5: a valid code + matching client credentials +
/// matching redirect_uri returns a Mastodon-compatible token JSON body
/// (`access_token`/`token_type`/`scope`/`created_at`).
#[tokio::test]
async fn exchange_token_returns_a_mastodon_compatible_token_response_on_success() {
    let app = spawn_test_app().await;
    let fx = build_fixture(app.pool.clone(), app.runtime.clone());
    let registered = fx
        .service
        .register_app(sample_new_app())
        .await
        .expect("register_app must succeed");
    let actor_id = app.runtime.ids.next_id();
    let issued_code = fx
        .service
        .issue_authorization_code(AuthorizeApproval {
            client_id: registered.client_id.clone(),
            redirect_uri: "https://client.example/callback".to_string(),
            scopes: "read write".to_string(),
            actor_id,
            code_challenge: None,
        })
        .await
        .expect("issuing a code must succeed");

    let Json(response) = token_endpoint::exchange_token(
        State(fx.state.clone()),
        Form(TokenExchangeRequest {
            grant_type: "authorization_code".to_string(),
            code: issued_code.plaintext.expose_secret().clone(),
            client_id: registered.client_id.clone(),
            client_secret: registered.client_secret.expose_secret().clone(),
            redirect_uri: "https://client.example/callback".to_string(),
            code_verifier: None,
        }),
    )
    .await
    .expect("exchanging a valid code must succeed");

    assert!(!response.access_token.is_empty());
    assert_eq!(response.token_type, "Bearer");
    assert_eq!(response.scope, "read write");
    assert!(response.created_at > 0);

    // The issued token must actually resolve as an active bearer token.
    let resolved = token_repository::resolve_token(
        &app.pool,
        &fx.state.token_hash_key,
        &response.access_token,
    )
    .await
    .expect("resolve_token must not error");
    assert!(
        resolved.is_some(),
        "the freshly issued token must resolve as active"
    );
    assert_eq!(resolved.unwrap().actor_id, actor_id);

    app.cleanup().await;
}

/// Requirement 3.1: an absent `grant_type` is treated as implicit
/// `authorization_code` and still succeeds.
#[tokio::test]
async fn exchange_token_treats_an_absent_grant_type_as_implicit_authorization_code() {
    let app = spawn_test_app().await;
    let fx = build_fixture(app.pool.clone(), app.runtime.clone());
    let registered = fx
        .service
        .register_app(sample_new_app())
        .await
        .expect("register_app must succeed");
    let issued_code = fx
        .service
        .issue_authorization_code(AuthorizeApproval {
            client_id: registered.client_id.clone(),
            redirect_uri: "https://client.example/callback".to_string(),
            scopes: "read".to_string(),
            actor_id: app.runtime.ids.next_id(),
            code_challenge: None,
        })
        .await
        .expect("issuing a code must succeed");

    let result = token_endpoint::exchange_token(
        State(fx.state.clone()),
        Form(TokenExchangeRequest {
            grant_type: String::new(),
            code: issued_code.plaintext.expose_secret().clone(),
            client_id: registered.client_id.clone(),
            client_secret: registered.client_secret.expose_secret().clone(),
            redirect_uri: "https://client.example/callback".to_string(),
            code_verifier: None,
        }),
    )
    .await;

    assert!(result.is_ok(), "an absent grant_type must not be rejected");

    app.cleanup().await;
}

// ---- exchange_token: rejections ----

/// Requirement 3.1 (endpoint-layer dispatch): an explicit, unsupported
/// `grant_type` is rejected with a 400, without ever touching
/// `OauthService`.
#[tokio::test]
async fn exchange_token_rejects_an_unsupported_grant_type_with_400() {
    let app = spawn_test_app().await;
    let fx = build_fixture(app.pool.clone(), app.runtime.clone());

    let err = token_endpoint::exchange_token(
        State(fx.state.clone()),
        Form(TokenExchangeRequest {
            grant_type: "client_credentials".to_string(),
            ..empty_token_request()
        }),
    )
    .await
    .expect_err("an unsupported grant_type must be rejected");
    assert_eq!(err.status, StatusCode::BAD_REQUEST);

    app.cleanup().await;
}

/// Requirement 3.2: a code that was never issued is rejected; no token is
/// issued.
#[tokio::test]
async fn exchange_token_rejects_an_invalid_code() {
    let app = spawn_test_app().await;
    let fx = build_fixture(app.pool.clone(), app.runtime.clone());
    let registered = fx
        .service
        .register_app(sample_new_app())
        .await
        .expect("register_app must succeed");

    let err = token_endpoint::exchange_token(
        State(fx.state.clone()),
        Form(TokenExchangeRequest {
            grant_type: "authorization_code".to_string(),
            code: "a-code-that-was-never-issued".to_string(),
            client_id: registered.client_id.clone(),
            client_secret: registered.client_secret.expose_secret().clone(),
            redirect_uri: "https://client.example/callback".to_string(),
            code_verifier: None,
        }),
    )
    .await
    .expect_err("an invalid code must be rejected");
    assert_eq!(err.status, StatusCode::BAD_REQUEST);

    app.cleanup().await;
}

/// Requirement 2.5 (reused by exchange): a code already redeemed once
/// cannot be redeemed again; the second exchange is rejected.
#[tokio::test]
async fn exchange_token_rejects_an_already_consumed_code() {
    let app = spawn_test_app().await;
    let fx = build_fixture(app.pool.clone(), app.runtime.clone());
    let registered = fx
        .service
        .register_app(sample_new_app())
        .await
        .expect("register_app must succeed");
    let issued_code = fx
        .service
        .issue_authorization_code(AuthorizeApproval {
            client_id: registered.client_id.clone(),
            redirect_uri: "https://client.example/callback".to_string(),
            scopes: "read".to_string(),
            actor_id: app.runtime.ids.next_id(),
            code_challenge: None,
        })
        .await
        .expect("issuing a code must succeed");

    let first_request = || TokenExchangeRequest {
        grant_type: "authorization_code".to_string(),
        code: issued_code.plaintext.expose_secret().clone(),
        client_id: registered.client_id.clone(),
        client_secret: registered.client_secret.expose_secret().clone(),
        redirect_uri: "https://client.example/callback".to_string(),
        code_verifier: None,
    };

    let _ = token_endpoint::exchange_token(State(fx.state.clone()), Form(first_request()))
        .await
        .expect("the first exchange of a fresh code must succeed");

    let err = token_endpoint::exchange_token(State(fx.state.clone()), Form(first_request()))
        .await
        .expect_err("redeeming the same code twice must be rejected");
    assert_eq!(err.status, StatusCode::BAD_REQUEST);

    app.cleanup().await;
}

/// Requirement 3.2: invalid client credentials are rejected; no token is
/// issued. See `src/oauth/token_endpoint.rs`'s doc comment ("Exchange-
/// failure status codes...") for why this asserts `400`, matching
/// `OauthService::exchange_token`'s own already-reviewed behavior, rather
/// than `401`.
#[tokio::test]
async fn exchange_token_rejects_invalid_client_credentials() {
    let app = spawn_test_app().await;
    let fx = build_fixture(app.pool.clone(), app.runtime.clone());
    let registered = fx
        .service
        .register_app(sample_new_app())
        .await
        .expect("register_app must succeed");
    let issued_code = fx
        .service
        .issue_authorization_code(AuthorizeApproval {
            client_id: registered.client_id.clone(),
            redirect_uri: "https://client.example/callback".to_string(),
            scopes: "read".to_string(),
            actor_id: app.runtime.ids.next_id(),
            code_challenge: None,
        })
        .await
        .expect("issuing a code must succeed");

    let err = token_endpoint::exchange_token(
        State(fx.state.clone()),
        Form(TokenExchangeRequest {
            grant_type: "authorization_code".to_string(),
            code: issued_code.plaintext.expose_secret().clone(),
            client_id: registered.client_id.clone(),
            client_secret: "definitely-the-wrong-secret".to_string(),
            redirect_uri: "https://client.example/callback".to_string(),
            code_verifier: None,
        }),
    )
    .await
    .expect_err("wrong client credentials must be rejected");
    assert_eq!(err.status, StatusCode::BAD_REQUEST);

    // The code must not have been silently consumed by a doomed attempt with
    // the *correct* credentials afterward failing for a different reason —
    // verify a subsequent exchange with correct credentials still works,
    // i.e. the wrong-credentials attempt never touched the code at all.
    let issued = token_endpoint::exchange_token(
        State(fx.state.clone()),
        Form(TokenExchangeRequest {
            grant_type: "authorization_code".to_string(),
            code: issued_code.plaintext.expose_secret().clone(),
            client_id: registered.client_id.clone(),
            client_secret: registered.client_secret.expose_secret().clone(),
            redirect_uri: "https://client.example/callback".to_string(),
            code_verifier: None,
        }),
    )
    .await
    .expect("a subsequent exchange with correct credentials must still succeed");
    assert!(!issued.access_token.is_empty());

    app.cleanup().await;
}

/// Requirement 3.2: a mismatched `redirect_uri` at exchange time is
/// rejected.
#[tokio::test]
async fn exchange_token_rejects_a_mismatched_redirect_uri() {
    let app = spawn_test_app().await;
    let fx = build_fixture(app.pool.clone(), app.runtime.clone());
    let registered = fx
        .service
        .register_app(sample_new_app())
        .await
        .expect("register_app must succeed");
    let issued_code = fx
        .service
        .issue_authorization_code(AuthorizeApproval {
            client_id: registered.client_id.clone(),
            redirect_uri: "https://client.example/callback".to_string(),
            scopes: "read".to_string(),
            actor_id: app.runtime.ids.next_id(),
            code_challenge: None,
        })
        .await
        .expect("issuing a code must succeed");

    let err = token_endpoint::exchange_token(
        State(fx.state.clone()),
        Form(TokenExchangeRequest {
            grant_type: "authorization_code".to_string(),
            code: issued_code.plaintext.expose_secret().clone(),
            client_id: registered.client_id.clone(),
            client_secret: registered.client_secret.expose_secret().clone(),
            redirect_uri: "https://attacker.example/callback".to_string(),
            code_verifier: None,
        }),
    )
    .await
    .expect_err("a mismatched redirect_uri must be rejected");
    assert_eq!(err.status, StatusCode::BAD_REQUEST);

    app.cleanup().await;
}

/// Requirement 3.3: a code issued with a PKCE challenge, exchanged with a
/// non-matching verifier, is rejected; no token is issued.
#[tokio::test]
async fn exchange_token_rejects_a_mismatched_pkce_verifier() {
    let app = spawn_test_app().await;
    let fx = build_fixture(app.pool.clone(), app.runtime.clone());
    let registered = fx
        .service
        .register_app(sample_new_app())
        .await
        .expect("register_app must succeed");
    let challenge = RealPkceChallenge::from_verifier_s256("the-correct-verifier-1234567890");
    let issued_code = fx
        .service
        .issue_authorization_code(AuthorizeApproval {
            client_id: registered.client_id.clone(),
            redirect_uri: "https://client.example/callback".to_string(),
            scopes: "read".to_string(),
            actor_id: app.runtime.ids.next_id(),
            code_challenge: Some(challenge.challenge.clone()),
        })
        .await
        .expect("issuing a code with a PKCE challenge must succeed");

    let err = token_endpoint::exchange_token(
        State(fx.state.clone()),
        Form(TokenExchangeRequest {
            grant_type: "authorization_code".to_string(),
            code: issued_code.plaintext.expose_secret().clone(),
            client_id: registered.client_id.clone(),
            client_secret: registered.client_secret.expose_secret().clone(),
            redirect_uri: "https://client.example/callback".to_string(),
            code_verifier: Some("the-wrong-verifier-0987654321".to_string()),
        }),
    )
    .await
    .expect_err("a mismatched PKCE verifier must be rejected");
    assert_eq!(err.status, StatusCode::BAD_REQUEST);

    app.cleanup().await;
}

/// Requirement 3.3: a code issued *with* a PKCE challenge and the matching
/// verifier succeeds end to end through the endpoint.
#[tokio::test]
async fn exchange_token_succeeds_with_a_matching_pkce_verifier() {
    let app = spawn_test_app().await;
    let fx = build_fixture(app.pool.clone(), app.runtime.clone());
    let registered = fx
        .service
        .register_app(sample_new_app())
        .await
        .expect("register_app must succeed");
    let verifier = "the-correct-verifier-1234567890";
    let challenge = RealPkceChallenge::from_verifier_s256(verifier);
    let issued_code = fx
        .service
        .issue_authorization_code(AuthorizeApproval {
            client_id: registered.client_id.clone(),
            redirect_uri: "https://client.example/callback".to_string(),
            scopes: "read".to_string(),
            actor_id: app.runtime.ids.next_id(),
            code_challenge: Some(challenge.challenge.clone()),
        })
        .await
        .expect("issuing a code with a PKCE challenge must succeed");

    let Json(response) = token_endpoint::exchange_token(
        State(fx.state.clone()),
        Form(TokenExchangeRequest {
            grant_type: "authorization_code".to_string(),
            code: issued_code.plaintext.expose_secret().clone(),
            client_id: registered.client_id.clone(),
            client_secret: registered.client_secret.expose_secret().clone(),
            redirect_uri: "https://client.example/callback".to_string(),
            code_verifier: Some(verifier.to_string()),
        }),
    )
    .await
    .expect("a matching PKCE verifier must succeed");
    assert!(!response.access_token.is_empty());

    app.cleanup().await;
}

// ---- revoke_token ----

/// Requirement 3.4: revoking a valid token with valid client credentials
/// succeeds, and the token subsequently no longer resolves as active.
#[tokio::test]
async fn revoke_token_with_valid_credentials_invalidates_the_token() {
    let app = spawn_test_app().await;
    let fx = build_fixture(app.pool.clone(), app.runtime.clone());
    let registered = fx
        .service
        .register_app(sample_new_app())
        .await
        .expect("register_app must succeed");
    let issued_code = fx
        .service
        .issue_authorization_code(AuthorizeApproval {
            client_id: registered.client_id.clone(),
            redirect_uri: "https://client.example/callback".to_string(),
            scopes: "read".to_string(),
            actor_id: app.runtime.ids.next_id(),
            code_challenge: None,
        })
        .await
        .expect("issuing a code must succeed");
    let issued_token = fx
        .service
        .exchange_token(kawasemi::oauth::service::TokenRequest {
            code: issued_code.plaintext.expose_secret().clone(),
            client_id: registered.client_id.clone(),
            client_secret: registered.client_secret.expose_secret().clone(),
            redirect_uri: "https://client.example/callback".to_string(),
            code_verifier: None,
        })
        .await
        .expect("exchanging the code must succeed");

    let Json(body) = token_endpoint::revoke_token(
        State(fx.state.clone()),
        Form(RevokeRequest {
            token: issued_token.plaintext.expose_secret().clone(),
            client_id: registered.client_id.clone(),
            client_secret: registered.client_secret.expose_secret().clone(),
        }),
    )
    .await
    .expect("revoking with valid credentials and a valid token must succeed");
    assert_eq!(body, RevokeResponse::default());

    let resolved = token_repository::resolve_token(
        &app.pool,
        &fx.state.token_hash_key,
        issued_token.plaintext.expose_secret(),
    )
    .await
    .expect("resolve_token must not error");
    assert!(
        resolved.is_none(),
        "a revoked token must no longer resolve as active"
    );

    app.cleanup().await;
}

/// Requirement 3.4 / RFC 6749 section 2.4.1 (client authentication):
/// invalid client credentials are rejected with a 401, and — critically —
/// the target token is NOT revoked as a side effect of the failed
/// authentication attempt.
#[tokio::test]
async fn revoke_token_rejects_invalid_client_credentials_without_revoking_the_token() {
    let app = spawn_test_app().await;
    let fx = build_fixture(app.pool.clone(), app.runtime.clone());
    let registered = fx
        .service
        .register_app(sample_new_app())
        .await
        .expect("register_app must succeed");
    let issued_code = fx
        .service
        .issue_authorization_code(AuthorizeApproval {
            client_id: registered.client_id.clone(),
            redirect_uri: "https://client.example/callback".to_string(),
            scopes: "read".to_string(),
            actor_id: app.runtime.ids.next_id(),
            code_challenge: None,
        })
        .await
        .expect("issuing a code must succeed");
    let issued_token = fx
        .service
        .exchange_token(kawasemi::oauth::service::TokenRequest {
            code: issued_code.plaintext.expose_secret().clone(),
            client_id: registered.client_id.clone(),
            client_secret: registered.client_secret.expose_secret().clone(),
            redirect_uri: "https://client.example/callback".to_string(),
            code_verifier: None,
        })
        .await
        .expect("exchanging the code must succeed");

    let err = token_endpoint::revoke_token(
        State(fx.state.clone()),
        Form(RevokeRequest {
            token: issued_token.plaintext.expose_secret().clone(),
            client_id: registered.client_id.clone(),
            client_secret: "definitely-the-wrong-secret".to_string(),
        }),
    )
    .await
    .expect_err("invalid client credentials must be rejected");
    assert_eq!(err.status, StatusCode::UNAUTHORIZED);

    let resolved = token_repository::resolve_token(
        &app.pool,
        &fx.state.token_hash_key,
        issued_token.plaintext.expose_secret(),
    )
    .await
    .expect("resolve_token must not error");
    assert!(
        resolved.is_some(),
        "a failed client-credential check must not revoke the token as a side effect"
    );

    app.cleanup().await;
}

/// Requirement 3.4: an unknown `client_id` at the revoke endpoint is
/// rejected with a 401, same as a wrong secret.
#[tokio::test]
async fn revoke_token_rejects_an_unknown_client_id_with_401() {
    let app = spawn_test_app().await;
    let fx = build_fixture(app.pool.clone(), app.runtime.clone());

    let err = token_endpoint::revoke_token(
        State(fx.state.clone()),
        Form(RevokeRequest {
            token: "irrelevant-token-value".to_string(),
            client_id: "no-such-client-was-ever-registered".to_string(),
            client_secret: "any-secret".to_string(),
        }),
    )
    .await
    .expect_err("an unknown client_id must be rejected");
    assert_eq!(err.status, StatusCode::UNAUTHORIZED);

    app.cleanup().await;
}

/// Requirement 3.4, RFC 7009 section 2.2 alignment: revoking a token that
/// has already been revoked is still idempotently successful, as long as
/// client credentials are valid.
#[tokio::test]
async fn revoke_token_is_idempotent_for_an_already_revoked_token() {
    let app = spawn_test_app().await;
    let fx = build_fixture(app.pool.clone(), app.runtime.clone());
    let registered = fx
        .service
        .register_app(sample_new_app())
        .await
        .expect("register_app must succeed");
    let issued_code = fx
        .service
        .issue_authorization_code(AuthorizeApproval {
            client_id: registered.client_id.clone(),
            redirect_uri: "https://client.example/callback".to_string(),
            scopes: "read".to_string(),
            actor_id: app.runtime.ids.next_id(),
            code_challenge: None,
        })
        .await
        .expect("issuing a code must succeed");
    let issued_token = fx
        .service
        .exchange_token(kawasemi::oauth::service::TokenRequest {
            code: issued_code.plaintext.expose_secret().clone(),
            client_id: registered.client_id.clone(),
            client_secret: registered.client_secret.expose_secret().clone(),
            redirect_uri: "https://client.example/callback".to_string(),
            code_verifier: None,
        })
        .await
        .expect("exchanging the code must succeed");

    let revoke_request = || RevokeRequest {
        token: issued_token.plaintext.expose_secret().clone(),
        client_id: registered.client_id.clone(),
        client_secret: registered.client_secret.expose_secret().clone(),
    };

    let _ = token_endpoint::revoke_token(State(fx.state.clone()), Form(revoke_request()))
        .await
        .expect("the first revocation must succeed");

    let _ = token_endpoint::revoke_token(State(fx.state.clone()), Form(revoke_request()))
        .await
        .expect("revoking an already-revoked token must still succeed idempotently");

    app.cleanup().await;
}

/// Requirement 3.4, RFC 7009 section 2.2 alignment: revoking a token value
/// that was never issued is still idempotently successful, as long as
/// client credentials are valid.
#[tokio::test]
async fn revoke_token_is_idempotent_for_a_nonexistent_token() {
    let app = spawn_test_app().await;
    let fx = build_fixture(app.pool.clone(), app.runtime.clone());
    let registered = fx
        .service
        .register_app(sample_new_app())
        .await
        .expect("register_app must succeed");

    let _ = token_endpoint::revoke_token(
        State(fx.state.clone()),
        Form(RevokeRequest {
            token: "a-token-value-that-was-never-issued".to_string(),
            client_id: registered.client_id.clone(),
            client_secret: registered.client_secret.expose_secret().clone(),
        }),
    )
    .await
    .expect("revoking a nonexistent token with valid credentials must not error");

    app.cleanup().await;
}
