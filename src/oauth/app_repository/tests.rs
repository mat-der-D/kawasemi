//! Integration tests for `OauthAppRepository` (Requirements 1.1, 1.4, 1.5,
//! 3.6), per task 3.1's observable completion condition: "登録したアプリが
//! 取得でき、正しい資格情報のみ検証を通過し、誤ったシークレットが拒否される".
//!
//! Mirrors `src/actor/repository/tests.rs`'s established convention: reuses
//! `crate::test_harness::spawn_test_app` for an isolated, already-migrated
//! schema and a deterministic `RuntimeContext` (`ids`/`rng` for
//! `register_app`).

use super::{
    NO_PLAINTEXT_SECRET_SENTINEL, NewApp, find_app_by_client_id, register_app,
    verify_app_credentials,
};
use crate::config::Secret;
use crate::oauth::hash::TokenHashKey;
use crate::oauth::model::ScopeSet;
use crate::test_harness::spawn_test_app;

/// A fixed, non-production token-hashing key for this test module only —
/// mirrors `test_harness::TEST_TOKEN_HASH_KEY`'s own "why fixed" reasoning,
/// but kept local (rather than importing the private harness constant) since
/// these tests only need *a* valid key, not the exact one the harness itself
/// boots `AppConfig` with (nothing here goes through `AppState`/`AppConfig`).
fn test_token_hash_key() -> TokenHashKey {
    Secret::new([0x11; 32])
}

fn other_token_hash_key() -> TokenHashKey {
    Secret::new([0x99; 32])
}

fn sample_new_app(name: &str) -> NewApp {
    NewApp {
        name: name.to_string(),
        redirect_uris: vec![
            "https://client.example/callback".to_string(),
            "https://client.example/callback2".to_string(),
        ],
        scopes: ScopeSet::new(["read", "write"]),
    }
}

/// Requirements 1.1, 1.4: registering an app persists it with its redirect
/// URIs and requested scopes, retrievable unchanged by `client_id`.
#[tokio::test]
async fn register_app_then_find_by_client_id_returns_the_registered_app() {
    let app = spawn_test_app().await;
    let key = test_token_hash_key();
    let now = app.runtime.clock.now();

    let registered = register_app(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        &key,
        now,
        sample_new_app("Test Client"),
    )
    .await
    .expect("register_app must succeed");

    assert_eq!(registered.name, "Test Client");
    assert_eq!(
        registered.redirect_uris,
        vec![
            "https://client.example/callback".to_string(),
            "https://client.example/callback2".to_string(),
        ]
    );
    assert!(!registered.client_id.is_empty());
    assert!(!registered.client_secret.expose_secret().is_empty());

    let found = find_app_by_client_id(&app.pool, &registered.client_id)
        .await
        .expect("find_app_by_client_id must succeed")
        .expect("the just-registered app must be found");

    assert_eq!(found.id, registered.id);
    assert_eq!(found.client_id, registered.client_id);
    assert_eq!(found.name, registered.name);
    assert_eq!(found.redirect_uris, registered.redirect_uris);
    assert_eq!(
        found.scopes.as_strs().collect::<Vec<_>>(),
        registered.scopes.as_strs().collect::<Vec<_>>()
    );

    app.cleanup().await;
}

/// Requirement 1.5: `find_app_by_client_id` never fabricates a plausible
/// plaintext secret — it must return the documented sentinel, never the
/// value `register_app` actually generated.
#[tokio::test]
async fn find_app_by_client_id_does_not_expose_a_real_client_secret() {
    let app = spawn_test_app().await;
    let key = test_token_hash_key();
    let now = app.runtime.clock.now();

    let registered = register_app(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        &key,
        now,
        sample_new_app("Sentinel Client"),
    )
    .await
    .expect("register_app must succeed");

    let found = find_app_by_client_id(&app.pool, &registered.client_id)
        .await
        .expect("find_app_by_client_id must succeed")
        .expect("the just-registered app must be found");

    assert_eq!(
        found.client_secret.expose_secret().as_str(),
        NO_PLAINTEXT_SECRET_SENTINEL
    );
    assert_ne!(
        found.client_secret.expose_secret(),
        registered.client_secret.expose_secret()
    );

    app.cleanup().await;
}

/// Requirement 1.5: the correct client_id/client_secret pair passes
/// verification, and the returned app matches what was registered.
#[tokio::test]
async fn verify_app_credentials_accepts_the_correct_secret() {
    let app = spawn_test_app().await;
    let key = test_token_hash_key();
    let now = app.runtime.clock.now();

    let registered = register_app(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        &key,
        now,
        sample_new_app("Verified Client"),
    )
    .await
    .expect("register_app must succeed");

    let verified = verify_app_credentials(
        &app.pool,
        &key,
        &registered.client_id,
        registered.client_secret.expose_secret(),
    )
    .await
    .expect("verify_app_credentials must succeed")
    .expect("the correct client_id/client_secret pair must verify");

    assert_eq!(verified.id, registered.id);
    assert_eq!(verified.client_id, registered.client_id);

    app.cleanup().await;
}

/// Requirement 1.5: a wrong secret for a real client_id must be rejected
/// (`None`, not an error).
#[tokio::test]
async fn verify_app_credentials_rejects_the_wrong_secret() {
    let app = spawn_test_app().await;
    let key = test_token_hash_key();
    let now = app.runtime.clock.now();

    let registered = register_app(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        &key,
        now,
        sample_new_app("Rejected Client"),
    )
    .await
    .expect("register_app must succeed");

    let verified = verify_app_credentials(
        &app.pool,
        &key,
        &registered.client_id,
        "definitely-the-wrong-secret",
    )
    .await
    .expect("verify_app_credentials must succeed even for a wrong secret");

    assert!(verified.is_none(), "a wrong client_secret must not verify");

    app.cleanup().await;
}

/// Requirement 1.5: verification must also fail if hashed under the wrong
/// `token_hash_key`, proving the hash is genuinely keyed (not a plain
/// unkeyed digest an attacker could recompute without the deployment's key).
#[tokio::test]
async fn verify_app_credentials_rejects_the_correct_secret_hashed_under_the_wrong_key() {
    let app = spawn_test_app().await;
    let registration_key = test_token_hash_key();
    let now = app.runtime.clock.now();

    let registered = register_app(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        &registration_key,
        now,
        sample_new_app("Wrong Key Client"),
    )
    .await
    .expect("register_app must succeed");

    let verified = verify_app_credentials(
        &app.pool,
        &other_token_hash_key(),
        &registered.client_id,
        registered.client_secret.expose_secret(),
    )
    .await
    .expect("verify_app_credentials must succeed");

    assert!(
        verified.is_none(),
        "the correct secret verified under the wrong token_hash_key must not pass"
    );

    app.cleanup().await;
}

/// An unknown `client_id` must not verify and must not be found.
#[tokio::test]
async fn unknown_client_id_is_neither_found_nor_verified() {
    let app = spawn_test_app().await;
    let key = test_token_hash_key();

    let found = find_app_by_client_id(&app.pool, "no-such-client-id")
        .await
        .expect("find_app_by_client_id must succeed");
    assert!(found.is_none());

    let verified = verify_app_credentials(&app.pool, &key, "no-such-client-id", "any-secret")
        .await
        .expect("verify_app_credentials must succeed");
    assert!(verified.is_none());

    app.cleanup().await;
}

/// Requirement 1.5 / 3.6: the persisted `client_secret_hash` column is never
/// the plaintext secret bytes, and is never even recoverable by comparing
/// against the plaintext directly — only `verify_keyed_hash` can confirm a
/// match.
#[tokio::test]
async fn persisted_client_secret_hash_column_never_holds_the_plaintext() {
    let app = spawn_test_app().await;
    let key = test_token_hash_key();
    let now = app.runtime.clock.now();

    let registered = register_app(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        &key,
        now,
        sample_new_app("Hash Column Client"),
    )
    .await
    .expect("register_app must succeed");

    let (stored_hash,): (Vec<u8>,) =
        sqlx::query_as("SELECT client_secret_hash FROM oauth_applications WHERE client_id = $1")
            .bind(&registered.client_id)
            .fetch_one(&app.pool)
            .await
            .expect("selecting the stored hash column must succeed");

    let plaintext_bytes = registered.client_secret.expose_secret().as_bytes();
    assert_ne!(stored_hash, plaintext_bytes);
    assert!(
        !stored_hash
            .windows(plaintext_bytes.len().min(stored_hash.len()))
            .any(|window| window == &plaintext_bytes[..window.len()]),
        "stored client_secret_hash column leaked the plaintext secret verbatim"
    );

    app.cleanup().await;
}

/// A registered app with multiple scopes round-trips them all, and a
/// second, differently-scoped app registered afterward does not bleed into
/// the first's row.
#[tokio::test]
async fn multiple_apps_are_independently_registered_and_retrievable() {
    let app = spawn_test_app().await;
    let key = test_token_hash_key();
    let now = app.runtime.clock.now();

    let first = register_app(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        &key,
        now,
        sample_new_app("First Client"),
    )
    .await
    .expect("registering the first app must succeed");

    let mut second_input = sample_new_app("Second Client");
    second_input.scopes = ScopeSet::new(["follow", "push"]);
    second_input.redirect_uris = vec!["https://other.example/cb".to_string()];
    let second = register_app(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        &key,
        now,
        second_input,
    )
    .await
    .expect("registering the second app must succeed");

    assert_ne!(first.id, second.id);
    assert_ne!(first.client_id, second.client_id);
    assert_ne!(
        first.client_secret.expose_secret(),
        second.client_secret.expose_secret()
    );

    let second_found = find_app_by_client_id(&app.pool, &second.client_id)
        .await
        .expect("find_app_by_client_id must succeed")
        .expect("the second app must be found");
    assert_eq!(
        second_found.redirect_uris,
        vec!["https://other.example/cb".to_string()]
    );
    assert_eq!(
        second_found.scopes.as_strs().collect::<Vec<_>>(),
        vec!["follow", "push"]
    );

    app.cleanup().await;
}
