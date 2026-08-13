//! Integration tests for `AccessTokenRepository` (Requirements 3.1, 3.4, 3.6,
//! 5.1, 5.2), per task 3.3's observable completion condition: "トークン平文を
//! 永続化せず、失効済みトークンが解決で無効扱いになることを統合テストで確認
//! できる".
//!
//! Mirrors `src/oauth/code_repository/tests.rs`'s established convention:
//! reuses `crate::test_harness::db_fixture::spawn_test_db` for an isolated,
//! already-migrated schema and a deterministic `RuntimeContext`, and
//! registers a real `oauth_applications` row via
//! `crate::oauth::app_repository::register_app` first (an access token's
//! `app_id` is a mandatory foreign key into `oauth_applications`, so every
//! test needs one).

use super::{NewAccessToken, issue_token, resolve_token, revoke_token};
use crate::config::Secret;
use crate::domain::Id;
use crate::oauth::app_repository::{self, NewApp};
use crate::oauth::hash::TokenHashKey;
use crate::oauth::model::ScopeSet;
use crate::test_harness::db_fixture::spawn_test_db;

/// A fixed, non-production token-hashing key for this test module only —
/// mirrors `app_repository/tests.rs::test_token_hash_key`'s own reasoning.
fn test_token_hash_key() -> TokenHashKey {
    Secret::new([0x55; 32])
}

fn other_token_hash_key() -> TokenHashKey {
    Secret::new([0x66; 32])
}

/// Registers a real `oauth_applications` row and returns its `Id`, so tests
/// can satisfy `oauth_access_tokens.app_id`'s FK constraint.
async fn register_test_app(pool: &sqlx::PgPool, runtime: &crate::runtime::RuntimeContext) -> Id {
    let key = test_token_hash_key();
    let now = runtime.clock.now();
    let registered = app_repository::register_app(
        pool,
        runtime.ids.as_ref(),
        runtime.rng.as_ref(),
        &key,
        now,
        NewApp {
            name: "Token Repository Test Client".to_string(),
            redirect_uris: vec!["https://client.example/callback".to_string()],
            scopes: ScopeSet::new(["read", "write"]),
        },
    )
    .await
    .expect("register_app must succeed");
    registered.id
}

/// Requirements 3.1, 3.5, 5.1: a freshly issued token resolves back to the
/// actor/app/scopes it was bound to at issuance.
#[tokio::test]
async fn issue_token_then_resolve_returns_the_bound_actor_app_and_scopes() {
    let db = spawn_test_db().await;
    let key = test_token_hash_key();
    let app_id = register_test_app(&db.pool, &db.runtime).await;
    let actor_id = db.runtime.ids.next_id();
    let now = db.runtime.clock.now();

    let issued = issue_token(
        &db.pool,
        db.runtime.ids.as_ref(),
        db.runtime.rng.as_ref(),
        &key,
        now,
        NewAccessToken {
            app_id,
            actor_id,
            scopes: ScopeSet::new(["read", "write"]),
        },
    )
    .await
    .expect("issue_token must succeed");

    assert!(!issued.plaintext.expose_secret().is_empty());
    assert_eq!(issued.token.app_id, app_id);
    assert_eq!(issued.token.actor_id, actor_id);
    assert!(!issued.token.revoked);

    let resolved = resolve_token(&db.pool, &key, issued.plaintext.expose_secret())
        .await
        .expect("resolve_token must succeed")
        .expect("a freshly issued token must resolve");

    assert_eq!(resolved.id, issued.token.id);
    assert_eq!(resolved.app_id, app_id);
    assert_eq!(resolved.actor_id, actor_id);
    assert_eq!(
        resolved.scopes.as_strs().collect::<Vec<_>>(),
        vec!["read", "write"]
    );
    assert!(!resolved.revoked);

    db.cleanup().await;
}

/// A garbage/unrelated token value (no matching `token_hash`) must not
/// resolve.
#[tokio::test]
async fn resolving_a_wrong_or_garbage_token_returns_none() {
    let db = spawn_test_db().await;
    let key = test_token_hash_key();

    let resolved = resolve_token(&db.pool, &key, "no-such-token-was-ever-issued")
        .await
        .expect("resolve_token must succeed");
    assert!(resolved.is_none());

    db.cleanup().await;
}

/// Requirement 5.1: resolving must also fail if hashed under the wrong
/// `token_hash_key`, proving the hash is genuinely keyed.
#[tokio::test]
async fn resolving_the_correct_token_hashed_under_the_wrong_key_returns_none() {
    let db = spawn_test_db().await;
    let registration_key = test_token_hash_key();
    let app_id = register_test_app(&db.pool, &db.runtime).await;
    let actor_id = db.runtime.ids.next_id();
    let now = db.runtime.clock.now();

    let issued = issue_token(
        &db.pool,
        db.runtime.ids.as_ref(),
        db.runtime.rng.as_ref(),
        &registration_key,
        now,
        NewAccessToken {
            app_id,
            actor_id,
            scopes: ScopeSet::new(["read"]),
        },
    )
    .await
    .expect("issue_token must succeed");

    let resolved = resolve_token(
        &db.pool,
        &other_token_hash_key(),
        issued.plaintext.expose_secret(),
    )
    .await
    .expect("resolve_token must succeed");
    assert!(
        resolved.is_none(),
        "the correct token hashed under the wrong token_hash_key must not resolve"
    );

    db.cleanup().await;
}

/// Requirements 3.4, 5.1, 5.2 (the core acceptance criterion): a revoked
/// token must resolve to `None`, even though its row still exists (revoked,
/// not deleted).
#[tokio::test]
async fn resolving_a_revoked_token_returns_none_even_though_the_row_still_exists() {
    let db = spawn_test_db().await;
    let key = test_token_hash_key();
    let app_id = register_test_app(&db.pool, &db.runtime).await;
    let actor_id = db.runtime.ids.next_id();
    let now = db.runtime.clock.now();

    let issued = issue_token(
        &db.pool,
        db.runtime.ids.as_ref(),
        db.runtime.rng.as_ref(),
        &key,
        now,
        NewAccessToken {
            app_id,
            actor_id,
            scopes: ScopeSet::new(["read"]),
        },
    )
    .await
    .expect("issue_token must succeed");

    // Sanity: resolves fine before revocation.
    assert!(
        resolve_token(&db.pool, &key, issued.plaintext.expose_secret())
            .await
            .expect("resolve_token must succeed")
            .is_some()
    );

    let revoked = revoke_token(&db.pool, &key, issued.plaintext.expose_secret())
        .await
        .expect("revoke_token must succeed");
    assert!(revoked, "revoking an active token must report true");

    let resolved_after_revoke = resolve_token(&db.pool, &key, issued.plaintext.expose_secret())
        .await
        .expect("resolve_token must succeed");
    assert!(
        resolved_after_revoke.is_none(),
        "a revoked token must resolve to None"
    );

    let (row_revoked,): (bool,) =
        sqlx::query_as("SELECT revoked FROM oauth_access_tokens WHERE id = $1")
            .bind(issued.token.id.as_i64())
            .fetch_one(&db.pool)
            .await
            .expect("the token row must still exist after revocation");
    assert!(
        row_revoked,
        "the row must still exist with revoked = TRUE, not be deleted"
    );

    db.cleanup().await;
}

/// Revoking a token value that was never issued must be reported as `false`,
/// not an error.
#[tokio::test]
async fn revoking_an_unknown_token_returns_false() {
    let db = spawn_test_db().await;
    let key = test_token_hash_key();

    let revoked = revoke_token(&db.pool, &key, "no-such-token-was-ever-issued")
        .await
        .expect("revoke_token must succeed (as a call)");
    assert!(!revoked);

    db.cleanup().await;
}

/// Revoking an already-revoked token a second time must return `false` (this
/// module's documented judgment call), not an error and not `true` again.
#[tokio::test]
async fn revoking_an_already_revoked_token_a_second_time_returns_false() {
    let db = spawn_test_db().await;
    let key = test_token_hash_key();
    let app_id = register_test_app(&db.pool, &db.runtime).await;
    let actor_id = db.runtime.ids.next_id();
    let now = db.runtime.clock.now();

    let issued = issue_token(
        &db.pool,
        db.runtime.ids.as_ref(),
        db.runtime.rng.as_ref(),
        &key,
        now,
        NewAccessToken {
            app_id,
            actor_id,
            scopes: ScopeSet::new(["read"]),
        },
    )
    .await
    .expect("issue_token must succeed");

    let first = revoke_token(&db.pool, &key, issued.plaintext.expose_secret())
        .await
        .expect("first revoke_token call must succeed");
    assert!(first, "the first revocation must succeed");

    let second = revoke_token(&db.pool, &key, issued.plaintext.expose_secret())
        .await
        .expect("second revoke_token call must succeed (as a call)");
    assert!(
        !second,
        "revoking an already-revoked token again must return false"
    );

    db.cleanup().await;
}

/// Requirement 3.6 (the core acceptance criterion): the persisted
/// `token_hash` column never holds the plaintext token bytes, and the
/// plaintext never appears verbatim inside the stored digest.
#[tokio::test]
async fn persisted_token_hash_column_never_holds_the_plaintext() {
    let db = spawn_test_db().await;
    let key = test_token_hash_key();
    let app_id = register_test_app(&db.pool, &db.runtime).await;
    let actor_id = db.runtime.ids.next_id();
    let now = db.runtime.clock.now();

    let issued = issue_token(
        &db.pool,
        db.runtime.ids.as_ref(),
        db.runtime.rng.as_ref(),
        &key,
        now,
        NewAccessToken {
            app_id,
            actor_id,
            scopes: ScopeSet::new(["read"]),
        },
    )
    .await
    .expect("issue_token must succeed");

    let (stored_hash,): (Vec<u8>,) =
        sqlx::query_as("SELECT token_hash FROM oauth_access_tokens WHERE id = $1")
            .bind(issued.token.id.as_i64())
            .fetch_one(&db.pool)
            .await
            .expect("selecting the stored token_hash column must succeed");

    let plaintext_bytes = issued.plaintext.expose_secret().as_bytes();
    assert_ne!(stored_hash, plaintext_bytes);
    assert!(
        !stored_hash
            .windows(plaintext_bytes.len().min(stored_hash.len()))
            .any(|window| window == &plaintext_bytes[..window.len()]),
        "stored token_hash column leaked the plaintext token verbatim"
    );

    db.cleanup().await;
}

/// Two tokens issued for different actors are independently issued and
/// resolved — resolving one never returns the other's data.
#[tokio::test]
async fn multiple_tokens_are_independently_issued_and_resolved() {
    let db = spawn_test_db().await;
    let key = test_token_hash_key();
    let app_id = register_test_app(&db.pool, &db.runtime).await;
    let actor_a = db.runtime.ids.next_id();
    let actor_b = db.runtime.ids.next_id();
    let now = db.runtime.clock.now();

    let issued_a = issue_token(
        &db.pool,
        db.runtime.ids.as_ref(),
        db.runtime.rng.as_ref(),
        &key,
        now,
        NewAccessToken {
            app_id,
            actor_id: actor_a,
            scopes: ScopeSet::new(["read"]),
        },
    )
    .await
    .expect("issuing the first token must succeed");

    let issued_b = issue_token(
        &db.pool,
        db.runtime.ids.as_ref(),
        db.runtime.rng.as_ref(),
        &key,
        now,
        NewAccessToken {
            app_id,
            actor_id: actor_b,
            scopes: ScopeSet::new(["follow", "push"]),
        },
    )
    .await
    .expect("issuing the second token must succeed");

    assert_ne!(issued_a.token.id, issued_b.token.id);
    assert_ne!(
        issued_a.plaintext.expose_secret(),
        issued_b.plaintext.expose_secret()
    );

    let resolved_b = resolve_token(&db.pool, &key, issued_b.plaintext.expose_secret())
        .await
        .expect("resolve_token must succeed")
        .expect("the second token must resolve");
    assert_eq!(resolved_b.actor_id, actor_b);
    assert_eq!(
        resolved_b.scopes.as_strs().collect::<Vec<_>>(),
        vec!["follow", "push"]
    );

    // Revoking the first token must not affect the second.
    let revoked_a = revoke_token(&db.pool, &key, issued_a.plaintext.expose_secret())
        .await
        .expect("revoke_token must succeed");
    assert!(revoked_a);

    let resolved_b_after_a_revoked =
        resolve_token(&db.pool, &key, issued_b.plaintext.expose_secret())
            .await
            .expect("resolve_token must succeed");
    assert!(
        resolved_b_after_a_revoked.is_some(),
        "revoking token a must not affect token b"
    );

    db.cleanup().await;
}

/// Requirement 3.4 (atomicity, mirroring `consume_code`'s analogous
/// concurrency guarantee): when two concurrent callers race to revoke the
/// *same* token at the same instant, exactly one must observe `true` and the
/// other `false` — never both `true` and never both `false`.
#[tokio::test]
async fn concurrent_revocation_of_the_same_token_lets_exactly_one_caller_win() {
    let db = spawn_test_db().await;
    let key = test_token_hash_key();
    let app_id = register_test_app(&db.pool, &db.runtime).await;
    let actor_id = db.runtime.ids.next_id();
    let now = db.runtime.clock.now();

    let issued = issue_token(
        &db.pool,
        db.runtime.ids.as_ref(),
        db.runtime.rng.as_ref(),
        &key,
        now,
        NewAccessToken {
            app_id,
            actor_id,
            scopes: ScopeSet::new(["read"]),
        },
    )
    .await
    .expect("issue_token must succeed");

    let pool_a = db.pool.clone();
    let pool_b = db.pool.clone();
    let key_a = key.clone();
    let key_b = key.clone();
    let raw_a = issued.plaintext.expose_secret().to_string();
    let raw_b = issued.plaintext.expose_secret().to_string();

    let (result_a, result_b) = tokio::join!(
        tokio::spawn(async move { revoke_token(&pool_a, &key_a, &raw_a).await }),
        tokio::spawn(async move { revoke_token(&pool_b, &key_b, &raw_b).await }),
    );

    let winner_count = [
        result_a
            .expect("task a must not panic")
            .expect("revoke_token call a must succeed"),
        result_b
            .expect("task b must not panic")
            .expect("revoke_token call b must succeed"),
    ]
    .into_iter()
    .filter(|revoked| *revoked)
    .count();

    assert_eq!(
        winner_count, 1,
        "exactly one concurrent revocation attempt must win, never zero or both"
    );

    db.cleanup().await;
}
