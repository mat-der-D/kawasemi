//! Integration tests for `OauthService` (Requirements 1.1, 1.2, 1.3, 2.1,
//! 2.3, 2.5, 2.6, 3.1, 3.2, 3.3, 3.4), per task 4.2's observable completion
//! condition: "不正なクライアント/URI/スコープ/コードが拒否され、正常系でア
//! クターとスコープを保持したコードとトークンが発行されることを統合テストで
//! 確認できる".
//!
//! Mirrors `src/oauth/app_repository/tests.rs`'s/`code_repository/tests.rs`'s
//! established convention: reuses `crate::test_harness::spawn_test_app` for
//! an isolated, already-migrated schema and a deterministic `RuntimeContext`.

use super::{AuthorizeApproval, NewApp, OauthService, TokenRequest};
use crate::config::Secret;
use crate::domain::Id;
use crate::oauth::hash::TokenHashKey;
use crate::oauth::pkce::PkceChallenge as RealPkceChallenge;
use crate::runtime::RuntimeContext;
use crate::runtime::clock::FixedClock;
use crate::test_harness::spawn_test_app;
use std::sync::Arc;

/// A fixed, non-production token-hashing key for this test module only —
/// mirrors `app_repository/tests.rs::test_token_hash_key`'s own reasoning.
fn test_token_hash_key() -> TokenHashKey {
    Secret::new([0x55; 32])
}

fn sample_new_app(name: &str) -> NewApp {
    NewApp {
        name: name.to_string(),
        redirect_uris: vec!["https://client.example/callback".to_string()],
        scopes: "read write".to_string(),
    }
}

/// Registers a sample app through the service itself and returns it (its
/// `client_id`/`client_secret` are needed by most authorization/token
/// tests).
async fn register_sample_app(service: &OauthService, name: &str) -> crate::oauth::OauthApp {
    service
        .register_app(sample_new_app(name))
        .await
        .expect("register_app must succeed for a well-formed request")
}

// ---- register_app ----

/// Requirements 1.1, 1.4: registering an app returns client credentials and
/// the registered redirect URI.
#[tokio::test]
async fn register_app_returns_client_credentials_and_registered_redirect_uri() {
    let app = spawn_test_app().await;
    let service = OauthService::new(app.pool.clone(), app.runtime.clone(), test_token_hash_key());

    let registered = register_sample_app(&service, "Test Client").await;

    assert!(!registered.client_id.is_empty());
    assert!(!registered.client_secret.expose_secret().is_empty());
    assert_eq!(
        registered.redirect_uris,
        vec!["https://client.example/callback".to_string()]
    );

    app.cleanup().await;
}

/// Requirement 1.2: an empty name is rejected.
#[tokio::test]
async fn register_app_rejects_an_empty_name() {
    let app = spawn_test_app().await;
    let service = OauthService::new(app.pool.clone(), app.runtime.clone(), test_token_hash_key());

    let err = service
        .register_app(NewApp {
            name: "   ".to_string(),
            redirect_uris: vec!["https://client.example/callback".to_string()],
            scopes: "read".to_string(),
        })
        .await
        .expect_err("an empty/whitespace-only name must be rejected");
    assert_eq!(err.status, axum::http::StatusCode::UNPROCESSABLE_ENTITY);

    app.cleanup().await;
}

/// Requirement 1.2: no redirect URIs at all is rejected.
#[tokio::test]
async fn register_app_rejects_no_redirect_uris() {
    let app = spawn_test_app().await;
    let service = OauthService::new(app.pool.clone(), app.runtime.clone(), test_token_hash_key());

    let err = service
        .register_app(NewApp {
            name: "Test Client".to_string(),
            redirect_uris: vec![],
            scopes: "read".to_string(),
        })
        .await
        .expect_err("no redirect_uris must be rejected");
    assert_eq!(err.status, axum::http::StatusCode::UNPROCESSABLE_ENTITY);

    app.cleanup().await;
}

/// Requirement 1.2: a malformed redirect URI (no scheme) is rejected.
#[tokio::test]
async fn register_app_rejects_a_malformed_redirect_uri() {
    let app = spawn_test_app().await;
    let service = OauthService::new(app.pool.clone(), app.runtime.clone(), test_token_hash_key());

    let err = service
        .register_app(NewApp {
            name: "Test Client".to_string(),
            redirect_uris: vec!["not-a-valid-uri".to_string()],
            scopes: "read".to_string(),
        })
        .await
        .expect_err("a redirect_uri with no scheme must be rejected");
    assert_eq!(err.status, axum::http::StatusCode::UNPROCESSABLE_ENTITY);

    app.cleanup().await;
}

/// Requirement 1.3: an unknown scope token is rejected.
#[tokio::test]
async fn register_app_rejects_an_unknown_scope() {
    let app = spawn_test_app().await;
    let service = OauthService::new(app.pool.clone(), app.runtime.clone(), test_token_hash_key());

    let err = service
        .register_app(NewApp {
            name: "Test Client".to_string(),
            redirect_uris: vec!["https://client.example/callback".to_string()],
            scopes: "read bogus_scope".to_string(),
        })
        .await
        .expect_err("an unknown scope token must be rejected");
    assert_eq!(err.status, axum::http::StatusCode::UNPROCESSABLE_ENTITY);

    app.cleanup().await;
}

// ---- issue_authorization_code ----

/// Requirements 2.1, 2.3, 2.6: a valid approval issues a code bound to the
/// selected actor and approved scopes.
#[tokio::test]
async fn issue_authorization_code_binds_actor_and_approved_scopes() {
    let app = spawn_test_app().await;
    let service = OauthService::new(app.pool.clone(), app.runtime.clone(), test_token_hash_key());
    let registered = register_sample_app(&service, "Test Client").await;
    let actor_id = app.runtime.ids.next_id();

    let issued = service
        .issue_authorization_code(AuthorizeApproval {
            client_id: registered.client_id.clone(),
            redirect_uri: "https://client.example/callback".to_string(),
            scopes: "read".to_string(),
            actor_id,
            code_challenge: None,
        })
        .await
        .expect("issuing a code for a valid approval must succeed");

    assert!(!issued.plaintext.expose_secret().is_empty());
    assert_eq!(issued.code.actor_id, actor_id);
    assert_eq!(
        issued.code.scopes.as_strs().collect::<Vec<_>>(),
        vec!["read"]
    );
    assert!(issued.code.pkce.is_none());
    assert!(!issued.code.consumed);

    app.cleanup().await;
}

/// Requirement 2.1: an unknown `client_id` is rejected.
#[tokio::test]
async fn issue_authorization_code_rejects_an_unknown_client_id() {
    let app = spawn_test_app().await;
    let service = OauthService::new(app.pool.clone(), app.runtime.clone(), test_token_hash_key());

    let err = service
        .issue_authorization_code(AuthorizeApproval {
            client_id: "no-such-client".to_string(),
            redirect_uri: "https://client.example/callback".to_string(),
            scopes: "read".to_string(),
            actor_id: Id::from_i64(1),
            code_challenge: None,
        })
        .await
        .expect_err("an unknown client_id must be rejected");
    assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);

    app.cleanup().await;
}

/// Requirement 2.1: a `redirect_uri` not matching the registered redirect
/// URI is rejected.
#[tokio::test]
async fn issue_authorization_code_rejects_a_mismatched_redirect_uri() {
    let app = spawn_test_app().await;
    let service = OauthService::new(app.pool.clone(), app.runtime.clone(), test_token_hash_key());
    let registered = register_sample_app(&service, "Test Client").await;

    let err = service
        .issue_authorization_code(AuthorizeApproval {
            client_id: registered.client_id,
            redirect_uri: "https://attacker.example/callback".to_string(),
            scopes: "read".to_string(),
            actor_id: Id::from_i64(1),
            code_challenge: None,
        })
        .await
        .expect_err("a mismatched redirect_uri must be rejected");
    assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);

    app.cleanup().await;
}

/// Requirement 1.3 (reused by authorization): an unknown approved scope is
/// rejected.
#[tokio::test]
async fn issue_authorization_code_rejects_an_unknown_scope() {
    let app = spawn_test_app().await;
    let service = OauthService::new(app.pool.clone(), app.runtime.clone(), test_token_hash_key());
    let registered = register_sample_app(&service, "Test Client").await;

    let err = service
        .issue_authorization_code(AuthorizeApproval {
            client_id: registered.client_id,
            redirect_uri: "https://client.example/callback".to_string(),
            scopes: "bogus_scope".to_string(),
            actor_id: Id::from_i64(1),
            code_challenge: None,
        })
        .await
        .expect_err("an unknown approved scope must be rejected");
    assert_eq!(err.status, axum::http::StatusCode::UNPROCESSABLE_ENTITY);

    app.cleanup().await;
}

/// Requirement 4.5's shared inclusion judgment, applied as a narrowing
/// check: approved scopes exceeding the app's own registered scopes are
/// rejected.
#[tokio::test]
async fn issue_authorization_code_rejects_approved_scopes_exceeding_registered_scopes() {
    let app = spawn_test_app().await;
    let service = OauthService::new(app.pool.clone(), app.runtime.clone(), test_token_hash_key());
    // Registered with only `read`.
    let registered = service
        .register_app(NewApp {
            name: "Read Only Client".to_string(),
            redirect_uris: vec!["https://client.example/callback".to_string()],
            scopes: "read".to_string(),
        })
        .await
        .expect("register_app must succeed");

    let err = service
        .issue_authorization_code(AuthorizeApproval {
            client_id: registered.client_id,
            redirect_uri: "https://client.example/callback".to_string(),
            scopes: "write".to_string(),
            actor_id: Id::from_i64(1),
            code_challenge: None,
        })
        .await
        .expect_err("approving a scope beyond the app's registered scopes must be rejected");
    assert_eq!(err.status, axum::http::StatusCode::UNPROCESSABLE_ENTITY);

    app.cleanup().await;
}

/// Requirement 2.6: a PKCE challenge is bound to the issued code and
/// persists through storage.
#[tokio::test]
async fn issue_authorization_code_binds_pkce_challenge_when_present() {
    let app = spawn_test_app().await;
    let service = OauthService::new(app.pool.clone(), app.runtime.clone(), test_token_hash_key());
    let registered = register_sample_app(&service, "Test Client").await;
    let verifier = "a-high-entropy-code-verifier-1234567890";
    let challenge = RealPkceChallenge::from_verifier_s256(verifier);

    let issued = service
        .issue_authorization_code(AuthorizeApproval {
            client_id: registered.client_id,
            redirect_uri: "https://client.example/callback".to_string(),
            scopes: "read".to_string(),
            actor_id: Id::from_i64(1),
            code_challenge: Some(challenge.challenge.clone()),
        })
        .await
        .expect("issuing a code with a PKCE challenge must succeed");

    assert_eq!(
        issued.code.pkce.expect("pkce must be bound").as_str(),
        challenge.challenge
    );

    app.cleanup().await;
}

// ---- exchange_token ----

/// Requirements 3.1, 3.5: a valid code + matching client credentials +
/// matching redirect_uri exchanges for a token bound to the code's actor
/// and scopes.
#[tokio::test]
async fn exchange_token_issues_a_token_bound_to_the_codes_actor_and_scopes() {
    let app = spawn_test_app().await;
    let key = test_token_hash_key();
    let service = OauthService::new(app.pool.clone(), app.runtime.clone(), key.clone());
    let registered = register_sample_app(&service, "Test Client").await;
    let actor_id = app.runtime.ids.next_id();

    let issued_code = service
        .issue_authorization_code(AuthorizeApproval {
            client_id: registered.client_id.clone(),
            redirect_uri: "https://client.example/callback".to_string(),
            scopes: "read write".to_string(),
            actor_id,
            code_challenge: None,
        })
        .await
        .expect("issuing a code must succeed");

    let issued_token = service
        .exchange_token(TokenRequest {
            code: issued_code.plaintext.expose_secret().clone(),
            client_id: registered.client_id.clone(),
            client_secret: registered.client_secret.expose_secret().clone(),
            redirect_uri: "https://client.example/callback".to_string(),
            code_verifier: None,
        })
        .await
        .expect("exchanging a valid code must succeed");

    assert_eq!(issued_token.token.actor_id, actor_id);
    assert_eq!(
        issued_token.token.scopes.as_strs().collect::<Vec<_>>(),
        vec!["read", "write"]
    );
    assert!(!issued_token.plaintext.expose_secret().is_empty());
    assert!(!issued_token.token.revoked);

    app.cleanup().await;
}

/// Requirement 2.5: a code can only be exchanged once; the second attempt
/// is rejected.
#[tokio::test]
async fn exchange_token_rejects_a_code_that_was_already_redeemed() {
    let app = spawn_test_app().await;
    let key = test_token_hash_key();
    let service = OauthService::new(app.pool.clone(), app.runtime.clone(), key.clone());
    let registered = register_sample_app(&service, "Test Client").await;

    let issued_code = service
        .issue_authorization_code(AuthorizeApproval {
            client_id: registered.client_id.clone(),
            redirect_uri: "https://client.example/callback".to_string(),
            scopes: "read".to_string(),
            actor_id: Id::from_i64(1),
            code_challenge: None,
        })
        .await
        .expect("issuing a code must succeed");

    let token_request = || TokenRequest {
        code: issued_code.plaintext.expose_secret().clone(),
        client_id: registered.client_id.clone(),
        client_secret: registered.client_secret.expose_secret().clone(),
        redirect_uri: "https://client.example/callback".to_string(),
        code_verifier: None,
    };

    service
        .exchange_token(token_request())
        .await
        .expect("the first exchange must succeed");

    let err = service
        .exchange_token(token_request())
        .await
        .expect_err("redeeming the same code twice must be rejected");
    assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);

    app.cleanup().await;
}

/// Requirement 3.2: wrong client credentials are rejected and do not issue
/// a token.
#[tokio::test]
async fn exchange_token_rejects_wrong_client_credentials() {
    let app = spawn_test_app().await;
    let key = test_token_hash_key();
    let service = OauthService::new(app.pool.clone(), app.runtime.clone(), key.clone());
    let registered = register_sample_app(&service, "Test Client").await;

    let issued_code = service
        .issue_authorization_code(AuthorizeApproval {
            client_id: registered.client_id.clone(),
            redirect_uri: "https://client.example/callback".to_string(),
            scopes: "read".to_string(),
            actor_id: Id::from_i64(1),
            code_challenge: None,
        })
        .await
        .expect("issuing a code must succeed");

    let err = service
        .exchange_token(TokenRequest {
            code: issued_code.plaintext.expose_secret().clone(),
            client_id: registered.client_id.clone(),
            client_secret: "wrong-secret".to_string(),
            redirect_uri: "https://client.example/callback".to_string(),
            code_verifier: None,
        })
        .await
        .expect_err("wrong client_secret must be rejected");
    assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);

    app.cleanup().await;
}

/// Requirement 3.2: a mismatched `redirect_uri` at exchange time is
/// rejected, even with correct credentials and an otherwise-valid code.
#[tokio::test]
async fn exchange_token_rejects_a_mismatched_redirect_uri() {
    let app = spawn_test_app().await;
    let key = test_token_hash_key();
    let service = OauthService::new(app.pool.clone(), app.runtime.clone(), key.clone());
    let registered = register_sample_app(&service, "Test Client").await;

    let issued_code = service
        .issue_authorization_code(AuthorizeApproval {
            client_id: registered.client_id.clone(),
            redirect_uri: "https://client.example/callback".to_string(),
            scopes: "read".to_string(),
            actor_id: Id::from_i64(1),
            code_challenge: None,
        })
        .await
        .expect("issuing a code must succeed");

    let err = service
        .exchange_token(TokenRequest {
            code: issued_code.plaintext.expose_secret().clone(),
            client_id: registered.client_id.clone(),
            client_secret: registered.client_secret.expose_secret().clone(),
            redirect_uri: "https://different.example/callback".to_string(),
            code_verifier: None,
        })
        .await
        .expect_err("a mismatched redirect_uri at exchange time must be rejected");
    assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);

    app.cleanup().await;
}

/// Requirement 3.3: a code issued with a PKCE challenge, exchanged with the
/// matching verifier, succeeds.
#[tokio::test]
async fn exchange_token_succeeds_with_a_matching_pkce_verifier() {
    let app = spawn_test_app().await;
    let key = test_token_hash_key();
    let service = OauthService::new(app.pool.clone(), app.runtime.clone(), key.clone());
    let registered = register_sample_app(&service, "Test Client").await;
    let verifier = "a-high-entropy-code-verifier-abcdefghijkl";
    let challenge = RealPkceChallenge::from_verifier_s256(verifier);

    let issued_code = service
        .issue_authorization_code(AuthorizeApproval {
            client_id: registered.client_id.clone(),
            redirect_uri: "https://client.example/callback".to_string(),
            scopes: "read".to_string(),
            actor_id: Id::from_i64(1),
            code_challenge: Some(challenge.challenge.clone()),
        })
        .await
        .expect("issuing a code with pkce must succeed");

    let issued_token = service
        .exchange_token(TokenRequest {
            code: issued_code.plaintext.expose_secret().clone(),
            client_id: registered.client_id.clone(),
            client_secret: registered.client_secret.expose_secret().clone(),
            redirect_uri: "https://client.example/callback".to_string(),
            code_verifier: Some(verifier.to_string()),
        })
        .await
        .expect("exchanging with the matching verifier must succeed");

    assert!(!issued_token.plaintext.expose_secret().is_empty());

    app.cleanup().await;
}

/// Requirement 3.3: a code issued with a PKCE challenge, exchanged with a
/// non-matching verifier, is rejected and issues no token.
#[tokio::test]
async fn exchange_token_rejects_a_mismatched_pkce_verifier() {
    let app = spawn_test_app().await;
    let key = test_token_hash_key();
    let service = OauthService::new(app.pool.clone(), app.runtime.clone(), key.clone());
    let registered = register_sample_app(&service, "Test Client").await;
    let real_verifier = "the-real-verifier-the-client-actually-used";
    let challenge = RealPkceChallenge::from_verifier_s256(real_verifier);

    let issued_code = service
        .issue_authorization_code(AuthorizeApproval {
            client_id: registered.client_id.clone(),
            redirect_uri: "https://client.example/callback".to_string(),
            scopes: "read".to_string(),
            actor_id: Id::from_i64(1),
            code_challenge: Some(challenge.challenge.clone()),
        })
        .await
        .expect("issuing a code with pkce must succeed");

    let err = service
        .exchange_token(TokenRequest {
            code: issued_code.plaintext.expose_secret().clone(),
            client_id: registered.client_id.clone(),
            client_secret: registered.client_secret.expose_secret().clone(),
            redirect_uri: "https://client.example/callback".to_string(),
            code_verifier: Some("an-attacker-or-buggy-client-supplied-verifier".to_string()),
        })
        .await
        .expect_err("a mismatched code_verifier must be rejected");
    assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);

    app.cleanup().await;
}

/// Requirement 2.6's `Where`-conditionality: a code issued *with* a PKCE
/// challenge, but exchanged *without* a verifier, is rejected rather than
/// silently accepted.
#[tokio::test]
async fn exchange_token_rejects_a_missing_verifier_when_the_code_required_pkce() {
    let app = spawn_test_app().await;
    let key = test_token_hash_key();
    let service = OauthService::new(app.pool.clone(), app.runtime.clone(), key.clone());
    let registered = register_sample_app(&service, "Test Client").await;
    let verifier = "a-high-entropy-code-verifier-zzzzzzzzzzzz";
    let challenge = RealPkceChallenge::from_verifier_s256(verifier);

    let issued_code = service
        .issue_authorization_code(AuthorizeApproval {
            client_id: registered.client_id.clone(),
            redirect_uri: "https://client.example/callback".to_string(),
            scopes: "read".to_string(),
            actor_id: Id::from_i64(1),
            code_challenge: Some(challenge.challenge.clone()),
        })
        .await
        .expect("issuing a code with pkce must succeed");

    let err = service
        .exchange_token(TokenRequest {
            code: issued_code.plaintext.expose_secret().clone(),
            client_id: registered.client_id.clone(),
            client_secret: registered.client_secret.expose_secret().clone(),
            redirect_uri: "https://client.example/callback".to_string(),
            code_verifier: None,
        })
        .await
        .expect_err("a missing verifier for a pkce-bound code must be rejected");
    assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);

    app.cleanup().await;
}

/// A code_verifier presented for a code that was issued *without* PKCE is
/// rejected rather than silently ignored.
#[tokio::test]
async fn exchange_token_rejects_an_unexpected_verifier_when_the_code_had_no_pkce() {
    let app = spawn_test_app().await;
    let key = test_token_hash_key();
    let service = OauthService::new(app.pool.clone(), app.runtime.clone(), key.clone());
    let registered = register_sample_app(&service, "Test Client").await;

    let issued_code = service
        .issue_authorization_code(AuthorizeApproval {
            client_id: registered.client_id.clone(),
            redirect_uri: "https://client.example/callback".to_string(),
            scopes: "read".to_string(),
            actor_id: Id::from_i64(1),
            code_challenge: None,
        })
        .await
        .expect("issuing a code without pkce must succeed");

    let err = service
        .exchange_token(TokenRequest {
            code: issued_code.plaintext.expose_secret().clone(),
            client_id: registered.client_id.clone(),
            client_secret: registered.client_secret.expose_secret().clone(),
            redirect_uri: "https://client.example/callback".to_string(),
            code_verifier: Some("unexpected-verifier".to_string()),
        })
        .await
        .expect_err("an unexpected verifier must be rejected");
    assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);

    app.cleanup().await;
}

/// Requirements 2.5, 3.2: an expired code is rejected even though it was
/// never consumed. Simulated by issuing the code under one service instance
/// bound to an earlier fixed clock, then exchanging it under a second
/// service instance (same pool/key) bound to a later fixed clock, past the
/// code's TTL.
#[tokio::test]
async fn exchange_token_rejects_an_expired_code() {
    let app = spawn_test_app().await;
    let key = test_token_hash_key();

    let issuance_time = app.runtime.clock.now();
    let issuance_runtime = RuntimeContext {
        clock: Arc::new(FixedClock::new(issuance_time)),
        ids: app.runtime.ids.clone(),
        rng: app.runtime.rng.clone(),
        keys: app.runtime.keys.clone(),
    };
    let issuing_service = OauthService::new(app.pool.clone(), issuance_runtime, key.clone());
    let registered = register_sample_app(&issuing_service, "Test Client").await;

    let issued_code = issuing_service
        .issue_authorization_code(AuthorizeApproval {
            client_id: registered.client_id.clone(),
            redirect_uri: "https://client.example/callback".to_string(),
            scopes: "read".to_string(),
            actor_id: Id::from_i64(1),
            code_challenge: None,
        })
        .await
        .expect("issuing a code must succeed");

    let later_time = issuance_time + time::Duration::minutes(11);
    let later_runtime = RuntimeContext {
        clock: Arc::new(FixedClock::new(later_time)),
        ids: app.runtime.ids.clone(),
        rng: app.runtime.rng.clone(),
        keys: app.runtime.keys.clone(),
    };
    let exchanging_service = OauthService::new(app.pool.clone(), later_runtime, key.clone());

    let err = exchanging_service
        .exchange_token(TokenRequest {
            code: issued_code.plaintext.expose_secret().clone(),
            client_id: registered.client_id,
            client_secret: registered.client_secret.expose_secret().clone(),
            redirect_uri: "https://client.example/callback".to_string(),
            code_verifier: None,
        })
        .await
        .expect_err("an expired code must be rejected");
    assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);

    app.cleanup().await;
}

// ---- revoke_token ----

/// Requirement 3.4: revoking a token invalidates it for subsequent
/// resolution.
#[tokio::test]
async fn revoke_token_invalidates_the_token_for_subsequent_resolution() {
    let app = spawn_test_app().await;
    let key = test_token_hash_key();
    let service = OauthService::new(app.pool.clone(), app.runtime.clone(), key.clone());
    let registered = register_sample_app(&service, "Test Client").await;

    let issued_code = service
        .issue_authorization_code(AuthorizeApproval {
            client_id: registered.client_id.clone(),
            redirect_uri: "https://client.example/callback".to_string(),
            scopes: "read".to_string(),
            actor_id: Id::from_i64(1),
            code_challenge: None,
        })
        .await
        .expect("issuing a code must succeed");

    let issued_token = service
        .exchange_token(TokenRequest {
            code: issued_code.plaintext.expose_secret().clone(),
            client_id: registered.client_id.clone(),
            client_secret: registered.client_secret.expose_secret().clone(),
            redirect_uri: "https://client.example/callback".to_string(),
            code_verifier: None,
        })
        .await
        .expect("exchanging a valid code must succeed");

    service
        .revoke_token(issued_token.plaintext.expose_secret())
        .await
        .expect("revoking an active token must succeed");

    let resolved = crate::oauth::token_repository::resolve_token(
        &app.pool,
        &key,
        issued_token.plaintext.expose_secret(),
    )
    .await
    .expect("resolve_token must not error");
    assert!(resolved.is_none(), "a revoked token must no longer resolve");

    app.cleanup().await;
}

/// Requirement 3.4 (RFC 7009 alignment): revoking an unknown/already-revoked
/// token is not an error.
#[tokio::test]
async fn revoke_token_is_idempotent_and_does_not_error_on_an_unknown_token() {
    let app = spawn_test_app().await;
    let service = OauthService::new(app.pool.clone(), app.runtime.clone(), test_token_hash_key());

    service
        .revoke_token("a-token-value-that-was-never-issued")
        .await
        .expect("revoking an unknown token must not error");

    app.cleanup().await;
}
