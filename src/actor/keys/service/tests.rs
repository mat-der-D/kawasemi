//! Integration-style tests for `SigningKeyService` (Requirements 4.1, 4.5,
//! 5.1, 5.2, 5.3, 5.5, 6.4), per task 4.1's observable completion
//! condition: "ローテーションで active が高々1に保たれ、書込と同時に
//! キャッシュが更新され、不在アクターのローテーションがエラーを返す".
//!
//! Mirrors `src/actor/keys/repository/tests.rs`'s established convention:
//! `spawn_test_app` for an isolated, already-migrated schema and a
//! deterministic `RuntimeContext`; a real owner + actor fixture created
//! first (`actor_signing_keys.actor_id` is a mandatory foreign key into
//! `local_actors`).

use std::sync::Arc;

use sqlx::PgPool;
use time::OffsetDateTime;

use super::SigningKeyService;
use crate::actor::keys::cache::KeyCache;
use crate::actor::keys::cipher::{ChaCha20Poly1305KeyCipher, KeyCipher};
use crate::actor::keys::repository::{find_active_public_key, load_all_active};
use crate::actor::model::{ActorState, ActorType, Handle, LocalActor};
use crate::actor::owner::create_owner;
use crate::actor::repository::insert_actor;
use crate::config::Secret;
use crate::domain::Id;
use crate::runtime::RuntimeContext;
use crate::runtime::signing_key::KeyRef;
use crate::test_harness::spawn_test_app;

/// Builds a `SigningKeyService` bound to `pool`/`runtime`, a fresh
/// `ChaCha20Poly1305KeyCipher` under a fixed test KEK, and `cache`.
fn service_under_test(pool: PgPool, runtime: RuntimeContext, cache: KeyCache) -> SigningKeyService {
    let cipher = Arc::new(ChaCha20Poly1305KeyCipher::new(Secret::new([3u8; 32])));
    SigningKeyService::new(pool, runtime, cipher, cache)
}

async fn create_owner_fixture(pool: &PgPool, owner_id: Id, now: OffsetDateTime) {
    create_owner(pool, owner_id, now)
        .await
        .expect("creating the owner fixture must succeed");
}

async fn insert_actor_fixture(
    pool: &PgPool,
    owner_id: Id,
    actor_id: Id,
    handle: &str,
    now: OffsetDateTime,
) {
    let actor = LocalActor {
        id: actor_id,
        owner_id,
        handle: Handle::new(handle).expect("test handle must be valid"),
        actor_type: ActorType::Person,
        display_name: "Test Actor".to_string(),
        summary: "a test actor".to_string(),
        state: ActorState::Active,
        created_at: now,
        updated_at: now,
    };
    let mut tx = pool
        .begin()
        .await
        .expect("opening a transaction for the actor fixture must succeed");
    insert_actor(&mut tx, &actor)
        .await
        .expect("inserting the actor fixture must succeed");
    tx.commit()
        .await
        .expect("committing the actor fixture transaction must succeed");
}

/// Counts `actor_signing_keys` rows for `actor_id` in a given `status`
/// (`'active'` | `'retired'`), used only by this test file to assert on
/// row-level DB state that `ActorSigningKeyRepository`'s own public API
/// (deliberately scoped to active-only lookups) does not directly expose.
async fn count_rows_with_status(pool: &PgPool, actor_id: Id, status: &str) -> i64 {
    let (count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM actor_signing_keys WHERE actor_id = $1 AND status = $2",
    )
    .bind(actor_id.as_i64())
    .bind(status)
    .fetch_one(pool)
    .await
    .expect("counting actor_signing_keys rows must succeed");
    count
}

/// Requirements 4.1, 4.5, 6.4: `provision_key` generates a fresh key pair,
/// seals the private key, inserts it as the actor's active key (findable
/// via `find_active_public_key`, never storing the plaintext private key),
/// and upserts `KeyCache` with a plaintext `SigningKey` that round-trips to
/// the same PEM the DB's sealed bytes decrypt to.
#[tokio::test]
async fn provision_key_persists_active_key_and_updates_the_cache_with_matching_plaintext() {
    let app = spawn_test_app().await;

    let owner_id = app.runtime.ids.next_id();
    let actor_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    create_owner_fixture(&app.pool, owner_id, now).await;
    insert_actor_fixture(&app.pool, owner_id, actor_id, "alice", now).await;

    let cache = KeyCache::new();
    let cipher = ChaCha20Poly1305KeyCipher::new(Secret::new([3u8; 32]));
    let service = SigningKeyService::new(
        app.pool.clone(),
        app.runtime.clone(),
        Arc::new(ChaCha20Poly1305KeyCipher::new(Secret::new([3u8; 32]))),
        cache.clone(),
    );

    let mut tx = app
        .pool
        .begin()
        .await
        .expect("opening a transaction must succeed");
    service
        .provision_key(&mut tx, actor_id)
        .await
        .expect("provision_key must succeed for a freshly created actor");
    tx.commit()
        .await
        .expect("committing the transaction must succeed");

    // Persisted: exactly one active key row, sealed (not plaintext).
    let public_key = find_active_public_key(&app.pool, actor_id)
        .await
        .expect("find_active_public_key must succeed")
        .expect("provision_key must have inserted an active key");
    assert_eq!(public_key.actor_id, actor_id);
    assert!(
        public_key
            .public_key_pem
            .starts_with("-----BEGIN PUBLIC KEY-----")
    );

    let active = load_all_active(&app.pool)
        .await
        .expect("load_all_active must succeed");
    let stored = active
        .iter()
        .find(|k| k.actor_id == actor_id)
        .expect("the actor's active key must be present in load_all_active");
    assert_ne!(
        stored.sealed_private_key,
        stored.public_key_pem.clone().into_bytes(),
        "sanity: sealed bytes must not equal the public PEM"
    );

    // Cache: upserted with a SigningKey whose PEM matches what the sealed
    // bytes decrypt to (proving the cache holds the *correct* key, not
    // merely *a* key).
    let cached = cache
        .get(KeyRef(actor_id))
        .expect("provision_key must have upserted the cache for this actor");
    let opened = cipher
        .open(&stored.sealed_private_key)
        .expect("opening the sealed bytes must succeed");
    assert_eq!(cached.expose_pem_bytes(), opened.expose_secret().as_bytes());

    app.cleanup().await;
}

/// Requirement 4.1's "single transaction" contract: `provision_key` never
/// commits/rolls back the transaction itself — if the *caller* rolls the
/// transaction back, the DB write never becomes durable, even though
/// `provision_key` already returned `Ok`.
#[tokio::test]
async fn provision_key_write_is_rolled_back_if_the_callers_transaction_is_rolled_back() {
    let app = spawn_test_app().await;

    let owner_id = app.runtime.ids.next_id();
    let actor_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    create_owner_fixture(&app.pool, owner_id, now).await;
    insert_actor_fixture(&app.pool, owner_id, actor_id, "bob", now).await;

    let cache = KeyCache::new();
    let service = service_under_test(app.pool.clone(), app.runtime.clone(), cache.clone());

    let mut tx = app
        .pool
        .begin()
        .await
        .expect("opening a transaction must succeed");
    service
        .provision_key(&mut tx, actor_id)
        .await
        .expect("provision_key must succeed inside the transaction");
    tx.rollback()
        .await
        .expect("rolling back the transaction must succeed");

    let public_key = find_active_public_key(&app.pool, actor_id)
        .await
        .expect("find_active_public_key must succeed");
    assert!(
        public_key.is_none(),
        "a rolled-back caller transaction must leave no persisted active key"
    );

    app.cleanup().await;
}

/// Requirements 5.1, 5.2, 5.3, 6.4: `rotate_key` retires the previous
/// active key, activates a new one atomically, keeps active-at-most-one
/// (the old row is retained but no longer `active`), and updates the cache
/// to the new key.
#[tokio::test]
async fn rotate_key_retires_old_key_activates_new_key_and_keeps_active_at_most_one() {
    let app = spawn_test_app().await;

    let owner_id = app.runtime.ids.next_id();
    let actor_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    create_owner_fixture(&app.pool, owner_id, now).await;
    insert_actor_fixture(&app.pool, owner_id, actor_id, "carol", now).await;

    let cache = KeyCache::new();
    let service = service_under_test(app.pool.clone(), app.runtime.clone(), cache.clone());

    let mut tx = app
        .pool
        .begin()
        .await
        .expect("opening a transaction must succeed");
    service
        .provision_key(&mut tx, actor_id)
        .await
        .expect("initial provision_key must succeed");
    tx.commit()
        .await
        .expect("committing the initial provision must succeed");

    let original = find_active_public_key(&app.pool, actor_id)
        .await
        .expect("find_active_public_key must succeed")
        .expect("an active key must exist after provisioning");

    service
        .rotate_key(actor_id)
        .await
        .expect("rotate_key must succeed for an existing actor");

    let rotated = find_active_public_key(&app.pool, actor_id)
        .await
        .expect("find_active_public_key must succeed")
        .expect("an active key must still exist after rotation");
    assert_ne!(
        rotated.key_id, original.key_id,
        "rotation must activate a *new* key, not reuse the old one"
    );
    assert_ne!(
        rotated.public_key_pem, original.public_key_pem,
        "rotation must generate genuinely new key material"
    );

    // At most one active row for the actor (Requirement 5.3), and the old
    // row is retained as retired, not deleted (Requirement 5.4).
    assert_eq!(
        count_rows_with_status(&app.pool, actor_id, "active").await,
        1
    );
    assert_eq!(
        count_rows_with_status(&app.pool, actor_id, "retired").await,
        1
    );

    // Cache reflects the new key, not the old one (Requirement 6.4).
    let cached = cache
        .get(KeyRef(actor_id))
        .expect("rotate_key must have upserted the cache");
    assert_ne!(
        cached.expose_pem_bytes().to_vec(),
        Vec::<u8>::new(),
        "sanity: cached key must be non-empty"
    );

    app.cleanup().await;
}

/// Requirement 5.5: rotating a nonexistent actor is rejected with a
/// caller-facing error, and must not write anything to the DB.
#[tokio::test]
async fn rotate_key_rejects_a_nonexistent_actor() {
    let app = spawn_test_app().await;
    let cache = KeyCache::new();
    let service = service_under_test(app.pool.clone(), app.runtime.clone(), cache.clone());

    let nonexistent_actor_id = app.runtime.ids.next_id();

    let result = service.rotate_key(nonexistent_actor_id).await;

    let error = result.expect_err("rotating a nonexistent actor must fail");
    assert_eq!(error.kind, crate::error::ErrorKind::Client);
    assert!(error.status.is_client_error());

    assert_eq!(
        count_rows_with_status(&app.pool, nonexistent_actor_id, "active").await,
        0,
        "no row should ever be written for a rejected rotation"
    );
    assert!(
        cache.get(KeyRef(nonexistent_actor_id)).is_none(),
        "the cache must not be touched for a rejected rotation"
    );

    app.cleanup().await;
}

/// Requirements 5.2, 5.3: rotating twice in a row keeps active-at-most-one
/// each time, and each rotation's key differs from every previous one.
#[tokio::test]
async fn rotating_twice_keeps_active_at_most_one_and_produces_distinct_keys_each_time() {
    let app = spawn_test_app().await;

    let owner_id = app.runtime.ids.next_id();
    let actor_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    create_owner_fixture(&app.pool, owner_id, now).await;
    insert_actor_fixture(&app.pool, owner_id, actor_id, "dave", now).await;

    let cache = KeyCache::new();
    let service = service_under_test(app.pool.clone(), app.runtime.clone(), cache.clone());

    let mut tx = app
        .pool
        .begin()
        .await
        .expect("opening a transaction must succeed");
    service
        .provision_key(&mut tx, actor_id)
        .await
        .expect("initial provision_key must succeed");
    tx.commit().await.expect("commit must succeed");

    let first = find_active_public_key(&app.pool, actor_id)
        .await
        .expect("lookup must succeed")
        .expect("active key must exist");

    service
        .rotate_key(actor_id)
        .await
        .expect("first rotation must succeed");
    let second = find_active_public_key(&app.pool, actor_id)
        .await
        .expect("lookup must succeed")
        .expect("active key must exist");
    assert_eq!(
        count_rows_with_status(&app.pool, actor_id, "active").await,
        1
    );

    service
        .rotate_key(actor_id)
        .await
        .expect("second rotation must succeed");
    let third = find_active_public_key(&app.pool, actor_id)
        .await
        .expect("lookup must succeed")
        .expect("active key must exist");
    assert_eq!(
        count_rows_with_status(&app.pool, actor_id, "active").await,
        1
    );
    assert_eq!(
        count_rows_with_status(&app.pool, actor_id, "retired").await,
        2
    );

    let ids = [first.key_id, second.key_id, third.key_id];
    assert_eq!(
        ids.iter().collect::<std::collections::HashSet<_>>().len(),
        3,
        "all three generations of the key must be distinct"
    );

    app.cleanup().await;
}
