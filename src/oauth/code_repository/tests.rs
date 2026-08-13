//! Integration tests for `AuthorizationCodeRepository` (Requirements 2.5,
//! 3.1, 3.2), per task 3.2's observable completion condition: "消費済みまた
//! は期限切れコードは交換に使えないことを統合テストで確認できる".
//!
//! Mirrors `src/oauth/app_repository/tests.rs`'s established convention:
//! reuses `crate::test_harness::db_fixture::spawn_test_db` for an isolated,
//! already-migrated schema and a deterministic `RuntimeContext`, and
//! registers a real `oauth_applications` row via
//! `crate::oauth::app_repository::register_app` first (an authorization
//! code's `app_id` is a mandatory foreign key into `oauth_applications`, so
//! every test needs one) — mirroring how `src/actor/repository/tests.rs`
//! creates an owner via `create_owner` first for its own FK dependency.

use super::{consume_code, insert_code};
use crate::config::Secret;
use crate::domain::Id;
use crate::oauth::app_repository::{self, NewApp};
use crate::oauth::hash::TokenHashKey;
use crate::oauth::model::{AuthorizationCode, PkceChallenge, ScopeSet};
use crate::test_harness::db_fixture::spawn_test_db;
use time::{Duration, OffsetDateTime};

/// A fixed, non-production token-hashing key for this test module only —
/// mirrors `app_repository/tests.rs::test_token_hash_key`'s own reasoning.
fn test_token_hash_key() -> TokenHashKey {
    Secret::new([0x33; 32])
}

/// Registers a real `oauth_applications` row and returns its `Id`, so tests
/// can satisfy `oauth_authorization_codes.app_id`'s FK constraint.
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
            name: "Code Repository Test Client".to_string(),
            redirect_uris: vec!["https://client.example/callback".to_string()],
            scopes: ScopeSet::new(["read", "write"]),
        },
    )
    .await
    .expect("register_app must succeed");
    registered.id
}

/// Builds an `AuthorizationCode` ready to hand to `insert_code`, expiring
/// `ttl` from `now` and carrying `pkce` (if any).
fn sample_code(
    app_id: Id,
    actor_id: Id,
    raw_code: &str,
    now: OffsetDateTime,
    ttl: Duration,
    pkce: Option<PkceChallenge>,
) -> AuthorizationCode {
    AuthorizationCode {
        code: Secret::new(raw_code.to_string()),
        app_id,
        actor_id,
        scopes: ScopeSet::new(["read", "write"]),
        redirect_uri: "https://client.example/callback".to_string(),
        pkce,
        expires_at: now + ttl,
        consumed: false,
    }
}

/// Requirements 2.5, 3.1, 3.2: a freshly inserted, unexpired code can be
/// consumed exactly once, and the returned data matches what was inserted
/// (selected actor, approved scopes, redirect URI).
#[tokio::test]
async fn insert_code_then_consume_it_once_succeeds_and_returns_the_codes_data() {
    let db = spawn_test_db().await;
    let key = test_token_hash_key();
    let app_id = register_test_app(&db.pool, &db.runtime).await;
    let actor_id = db.runtime.ids.next_id();
    let now = db.runtime.clock.now();

    let code = sample_code(
        app_id,
        actor_id,
        "raw-authorization-code-value",
        now,
        Duration::minutes(10),
        None,
    );
    insert_code(&db.pool, &key, &code)
        .await
        .expect("insert_code must succeed");

    let consumed = consume_code(&db.pool, &key, "raw-authorization-code-value", now)
        .await
        .expect("consume_code must succeed")
        .expect("an unconsumed, unexpired code must be consumable");

    assert_eq!(consumed.app_id, app_id);
    assert_eq!(consumed.actor_id, actor_id);
    assert_eq!(
        consumed.scopes.as_strs().collect::<Vec<_>>(),
        vec!["read", "write"]
    );
    assert_eq!(consumed.redirect_uri, "https://client.example/callback");
    assert!(consumed.pkce.is_none());
    assert!(consumed.consumed);

    db.cleanup().await;
}

/// Requirement 2.6: a code carrying a PKCE challenge round-trips it exactly.
#[tokio::test]
async fn a_code_with_a_pkce_challenge_round_trips_the_challenge_value() {
    let db = spawn_test_db().await;
    let key = test_token_hash_key();
    let app_id = register_test_app(&db.pool, &db.runtime).await;
    let actor_id = db.runtime.ids.next_id();
    let now = db.runtime.clock.now();

    let code = sample_code(
        app_id,
        actor_id,
        "raw-code-with-pkce",
        now,
        Duration::minutes(10),
        Some(PkceChallenge::new("s256-challenge-value")),
    );
    insert_code(&db.pool, &key, &code)
        .await
        .expect("insert_code must succeed");

    let consumed = consume_code(&db.pool, &key, "raw-code-with-pkce", now)
        .await
        .expect("consume_code must succeed")
        .expect("the code must be consumable");

    let pkce = consumed.pkce.expect("the PKCE challenge must round-trip");
    assert_eq!(pkce.as_str(), "s256-challenge-value");

    db.cleanup().await;
}

/// Requirements 2.5, 3.1, 3.2 (the core acceptance criterion): a code that
/// has already been consumed once cannot be consumed again — the second
/// `consume_code` call must return `None`, proving single-use consumption
/// actually prevents a double-spend rather than merely recording a flag
/// nobody checks.
#[tokio::test]
async fn consuming_an_already_consumed_code_a_second_time_returns_none() {
    let db = spawn_test_db().await;
    let key = test_token_hash_key();
    let app_id = register_test_app(&db.pool, &db.runtime).await;
    let actor_id = db.runtime.ids.next_id();
    let now = db.runtime.clock.now();

    let code = sample_code(
        app_id,
        actor_id,
        "raw-code-double-spend-attempt",
        now,
        Duration::minutes(10),
        None,
    );
    insert_code(&db.pool, &key, &code)
        .await
        .expect("insert_code must succeed");

    let first = consume_code(&db.pool, &key, "raw-code-double-spend-attempt", now)
        .await
        .expect("consume_code must succeed");
    assert!(first.is_some(), "the first consumption must succeed");

    let second = consume_code(&db.pool, &key, "raw-code-double-spend-attempt", now)
        .await
        .expect("consume_code must succeed (as a call), even though it rejects the code");
    assert!(
        second.is_none(),
        "a second consumption of an already-consumed code must be rejected"
    );

    db.cleanup().await;
}

/// Requirements 2.5, 3.2: an expired (but never-consumed) code cannot be
/// consumed.
#[tokio::test]
async fn consuming_an_expired_code_returns_none() {
    let db = spawn_test_db().await;
    let key = test_token_hash_key();
    let app_id = register_test_app(&db.pool, &db.runtime).await;
    let actor_id = db.runtime.ids.next_id();
    let now = db.runtime.clock.now();

    // Expired one second before "now".
    let code = sample_code(
        app_id,
        actor_id,
        "raw-code-already-expired",
        now,
        Duration::seconds(-1),
        None,
    );
    insert_code(&db.pool, &key, &code)
        .await
        .expect("insert_code must succeed");

    let consumed = consume_code(&db.pool, &key, "raw-code-already-expired", now)
        .await
        .expect("consume_code must succeed (as a call)");
    assert!(
        consumed.is_none(),
        "an expired code must not be consumable, even though it was never consumed"
    );

    db.cleanup().await;
}

/// A code presented with the wrong raw value (no matching `code_hash`) must
/// not be consumable.
#[tokio::test]
async fn consuming_an_unknown_code_returns_none() {
    let db = spawn_test_db().await;
    let key = test_token_hash_key();
    let now = db.runtime.clock.now();

    let consumed = consume_code(&db.pool, &key, "no-such-code-was-ever-inserted", now)
        .await
        .expect("consume_code must succeed (as a call)");
    assert!(consumed.is_none());

    db.cleanup().await;
}

/// Requirement 3.6 (mirrored for authorization codes): the persisted
/// `code_hash` column never holds the plaintext code bytes, and the
/// plaintext never appears verbatim inside the stored digest.
#[tokio::test]
async fn persisted_code_hash_column_never_holds_the_plaintext() {
    let db = spawn_test_db().await;
    let key = test_token_hash_key();
    let app_id = register_test_app(&db.pool, &db.runtime).await;
    let actor_id = db.runtime.ids.next_id();
    let now = db.runtime.clock.now();

    let plaintext = "raw-authorization-code-for-plaintext-check";
    let code = sample_code(
        app_id,
        actor_id,
        plaintext,
        now,
        Duration::minutes(10),
        None,
    );
    insert_code(&db.pool, &key, &code)
        .await
        .expect("insert_code must succeed");

    let (stored_hash,): (Vec<u8>,) = sqlx::query_as(
        "SELECT code_hash FROM oauth_authorization_codes WHERE app_id = $1 AND actor_id = $2",
    )
    .bind(app_id.as_i64())
    .bind(actor_id.as_i64())
    .fetch_one(&db.pool)
    .await
    .expect("selecting the stored code_hash column must succeed");

    let plaintext_bytes = plaintext.as_bytes();
    assert_ne!(stored_hash, plaintext_bytes);
    assert!(
        !stored_hash
            .windows(plaintext_bytes.len().min(stored_hash.len()))
            .any(|window| window == &plaintext_bytes[..window.len()]),
        "stored code_hash column leaked the plaintext code verbatim"
    );

    db.cleanup().await;
}

/// Requirement 3.2 (atomicity, the core acceptance criterion): when two
/// concurrent callers race to consume the *same* code at the same instant,
/// exactly one must succeed and the other must observe `None` — never both
/// succeeding (a double-spend) and never both failing.
#[tokio::test]
async fn concurrent_consumption_of_the_same_code_lets_exactly_one_caller_win() {
    let db = spawn_test_db().await;
    let key = test_token_hash_key();
    let app_id = register_test_app(&db.pool, &db.runtime).await;
    let actor_id = db.runtime.ids.next_id();
    let now = db.runtime.clock.now();

    let code = sample_code(
        app_id,
        actor_id,
        "raw-code-concurrent-race",
        now,
        Duration::minutes(10),
        None,
    );
    insert_code(&db.pool, &key, &code)
        .await
        .expect("insert_code must succeed");

    let pool_a = db.pool.clone();
    let pool_b = db.pool.clone();
    let key_a = key.clone();
    let key_b = key.clone();

    let (result_a, result_b) = tokio::join!(
        tokio::spawn(async move {
            consume_code(&pool_a, &key_a, "raw-code-concurrent-race", now).await
        }),
        tokio::spawn(async move {
            consume_code(&pool_b, &key_b, "raw-code-concurrent-race", now).await
        }),
    );

    let winner_count = [
        result_a
            .expect("task a must not panic")
            .expect("consume_code call a must succeed"),
        result_b
            .expect("task b must not panic")
            .expect("consume_code call b must succeed"),
    ]
    .into_iter()
    .filter(Option::is_some)
    .count();

    assert_eq!(
        winner_count, 1,
        "exactly one concurrent consumption attempt must win, never zero or both"
    );

    db.cleanup().await;
}
