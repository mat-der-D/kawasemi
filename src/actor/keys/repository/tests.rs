//! Integration-style tests for `ActorSigningKeyRepository` (Requirements
//! 4.1, 4.5, 5.2, 5.3, 5.4, 6.2), per task 2.3's observable completion
//! condition: "有効鍵を挿入後に取得でき、失効操作で status が retired に
//! 遷移し、一括ロードが全有効鍵を返す".
//!
//! Mirrors `src/actor/repository/tests.rs`'s established convention:
//! `spawn_test_app` for an isolated, already-migrated schema and a
//! deterministic `RuntimeContext`; a real owner (`create_owner`) and a real
//! actor (`insert_actor`, inside a self-opened-and-committed transaction)
//! are created first as fixtures, since `actor_signing_keys.actor_id` is a
//! mandatory foreign key into `local_actors`.
//!
//! `sealed_private_key` fixture values below are plain placeholder byte
//! strings, never real key material — this repository layer never
//! interprets those bytes as anything but opaque already-sealed ciphertext
//! (see repository.rs's doc comment), so a placeholder is exactly as valid
//! a fixture as real sealed bytes would be for exercising this module's own
//! contract.

use time::OffsetDateTime;

use super::{
    SigningKeyStatus, StoredSigningKey, find_active_public_key, insert_active_key,
    load_all_active, retire_active_key,
};
use crate::actor::model::{ActorState, ActorType, Handle, LocalActor};
use crate::actor::owner::create_owner;
use crate::actor::repository::insert_actor;
use crate::domain::Id;
use crate::test_harness::spawn_test_app;

/// Creates a real active actor fixture under an *already-existing* owner
/// (see [`create_owner_fixture`]), returning nothing (callers already have
/// `actor_id`).
///
/// Deliberately does not create the owner itself: several tests below
/// (e.g. `load_all_active_returns_every_active_key_across_actors`) attach
/// multiple actors to the *same* owner, and `create_owner` would reject a
/// second insert under the same `owner_id` (owners' primary key is unique) —
/// so owner creation is a separate, explicit, call-once-per-owner fixture
/// step ([`create_owner_fixture`]).
async fn insert_actor_fixture(
    pool: &sqlx::PgPool,
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

/// Creates a real owner fixture. A separate step from
/// [`insert_actor_fixture`] so a test can create one owner and attach
/// multiple actors to it (owners' primary key must not be inserted twice).
async fn create_owner_fixture(pool: &sqlx::PgPool, owner_id: Id, now: OffsetDateTime) {
    create_owner(pool, owner_id, now)
        .await
        .expect("creating the owner fixture must succeed");
}

/// Builds a `StoredSigningKey` value ready to hand to `insert_active_key`.
fn sample_key(id: Id, actor_id: Id, now: OffsetDateTime) -> StoredSigningKey {
    StoredSigningKey {
        id,
        actor_id,
        algorithm: "rsa-2048".to_string(),
        public_key_pem: "-----BEGIN PUBLIC KEY-----\ntest\n-----END PUBLIC KEY-----".to_string(),
        sealed_private_key: b"sealed-opaque-bytes".to_vec(),
        status: SigningKeyStatus::Active,
        created_at: now,
    }
}

/// Requirements 4.1, 4.5, 6.2: inserting an active key persists it, and
/// `find_active_public_key` returns its public material (never the sealed
/// private bytes, which `ActorPublicKey` has no field for at all).
#[tokio::test]
async fn insert_active_key_persists_and_is_findable_via_public_key_lookup() {
    let app = spawn_test_app().await;

    let owner_id = app.runtime.ids.next_id();
    let actor_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    create_owner_fixture(&app.pool, owner_id, now).await;
    insert_actor_fixture(&app.pool, owner_id, actor_id, "alice", now).await;

    let key_id = app.runtime.ids.next_id();
    let key = sample_key(key_id, actor_id, now);

    let mut tx = app
        .pool
        .begin()
        .await
        .expect("opening a transaction must succeed");
    insert_active_key(&mut tx, &key)
        .await
        .expect("insert_active_key must succeed for a fresh actor with no existing active key");
    tx.commit()
        .await
        .expect("committing the transaction must succeed");

    let public_key = find_active_public_key(&app.pool, actor_id)
        .await
        .expect("find_active_public_key must succeed")
        .expect("the just-inserted active key must be found");
    assert_eq!(public_key.actor_id, actor_id);
    assert_eq!(public_key.key_id, key_id);
    assert_eq!(public_key.public_key_pem, key.public_key_pem);

    app.cleanup().await;
}

/// `find_active_public_key` returns `Ok(None)` (not an error) for an actor
/// that has never had a key inserted.
#[tokio::test]
async fn find_active_public_key_returns_none_for_an_actor_with_no_keys() {
    let app = spawn_test_app().await;

    let owner_id = app.runtime.ids.next_id();
    let actor_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    create_owner_fixture(&app.pool, owner_id, now).await;
    insert_actor_fixture(&app.pool, owner_id, actor_id, "bob", now).await;

    let found = find_active_public_key(&app.pool, actor_id)
        .await
        .expect("find_active_public_key must succeed even with no keys");
    assert!(found.is_none());

    app.cleanup().await;
}

/// Requirements 5.2, 5.3, 5.4: retiring the active key transitions its
/// status to retired (it stops being returned as the active key, and
/// `load_all_active` no longer includes it), while `insert_active_key` for
/// a fresh replacement key succeeds afterward (the partial unique index no
/// longer blocks a new active key once the old one is retired).
#[tokio::test]
async fn retire_active_key_transitions_status_and_allows_a_new_active_key() {
    let app = spawn_test_app().await;

    let owner_id = app.runtime.ids.next_id();
    let actor_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    create_owner_fixture(&app.pool, owner_id, now).await;
    insert_actor_fixture(&app.pool, owner_id, actor_id, "carol", now).await;

    let old_key_id = app.runtime.ids.next_id();
    let old_key = sample_key(old_key_id, actor_id, now);
    let mut tx = app
        .pool
        .begin()
        .await
        .expect("opening a transaction must succeed");
    insert_active_key(&mut tx, &old_key)
        .await
        .expect("inserting the initial active key must succeed");
    tx.commit()
        .await
        .expect("committing must succeed");

    let later = now + time::Duration::seconds(60);
    let mut tx = app
        .pool
        .begin()
        .await
        .expect("opening a transaction must succeed");
    retire_active_key(&mut tx, actor_id, later)
        .await
        .expect("retire_active_key must succeed for an actor with an active key");
    tx.commit()
        .await
        .expect("committing the retirement must succeed");

    // The retired key must no longer be returned as the active public key.
    let after_retire = find_active_public_key(&app.pool, actor_id)
        .await
        .expect("find_active_public_key must succeed");
    assert!(
        after_retire.is_none(),
        "a retired key must not be returned by find_active_public_key"
    );

    // Requirement 5.3: with the old key retired, a fresh active key for the
    // same actor must be insertable (the partial unique index only blocks a
    // *second simultaneously-active* key).
    let new_key_id = app.runtime.ids.next_id();
    let new_key = sample_key(new_key_id, actor_id, later);
    let mut tx = app
        .pool
        .begin()
        .await
        .expect("opening a transaction must succeed");
    insert_active_key(&mut tx, &new_key)
        .await
        .expect("inserting a new active key after retirement must succeed");
    tx.commit()
        .await
        .expect("committing must succeed");

    let public_key = find_active_public_key(&app.pool, actor_id)
        .await
        .expect("find_active_public_key must succeed")
        .expect("the new active key must now be found");
    assert_eq!(public_key.key_id, new_key_id);

    // Requirement 5.4: the retired key is still distinguishable/retained —
    // `load_all_active` must return only the new active key, not the
    // retired one, proving retirement is tracked per-row rather than by
    // deletion.
    let active_keys = load_all_active(&app.pool)
        .await
        .expect("load_all_active must succeed");
    let ids: Vec<Id> = active_keys.iter().map(|k| k.id).collect();
    assert!(ids.contains(&new_key_id));
    assert!(
        !ids.contains(&old_key_id),
        "load_all_active must not include a retired key"
    );

    app.cleanup().await;
}

/// A second `insert_active_key` for an actor that already has an active key
/// (without retiring the first) must fail — the partial unique index
/// (`actor_signing_keys_active_unique`) rejects it — and must not disturb
/// the original active key.
#[tokio::test]
async fn insert_active_key_rejects_a_second_simultaneous_active_key_for_the_same_actor() {
    let app = spawn_test_app().await;

    let owner_id = app.runtime.ids.next_id();
    let actor_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    create_owner_fixture(&app.pool, owner_id, now).await;
    insert_actor_fixture(&app.pool, owner_id, actor_id, "dave", now).await;

    let first_key_id = app.runtime.ids.next_id();
    let first_key = sample_key(first_key_id, actor_id, now);
    let mut tx = app
        .pool
        .begin()
        .await
        .expect("opening a transaction must succeed");
    insert_active_key(&mut tx, &first_key)
        .await
        .expect("inserting the first active key must succeed");
    tx.commit()
        .await
        .expect("committing must succeed");

    let second_key_id = app.runtime.ids.next_id();
    let second_key = sample_key(second_key_id, actor_id, now);
    let mut tx = app
        .pool
        .begin()
        .await
        .expect("opening a second transaction must succeed");
    let err = insert_active_key(&mut tx, &second_key)
        .await
        .expect_err("a second simultaneous active key for the same actor must be rejected");
    assert_eq!(
        err.kind,
        crate::error::ErrorKind::Server,
        "the duplicate-active-key constraint violation is mapped to a generic server error, \
         not a caller-facing one -- see repository.rs's doc comment"
    );
    let _ = tx.rollback().await;

    // The original active key must be untouched.
    let public_key = find_active_public_key(&app.pool, actor_id)
        .await
        .expect("find_active_public_key must succeed")
        .expect("the original active key must still be found");
    assert_eq!(public_key.key_id, first_key_id);

    app.cleanup().await;
}

/// Retiring an actor that has no active key at all is a no-op success, not
/// an error (design.md's literal `Result<(), AppError>` contract, no
/// bool/count return).
#[tokio::test]
async fn retire_active_key_is_a_no_op_success_for_an_actor_with_no_active_key() {
    let app = spawn_test_app().await;

    let owner_id = app.runtime.ids.next_id();
    let actor_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    create_owner_fixture(&app.pool, owner_id, now).await;
    insert_actor_fixture(&app.pool, owner_id, actor_id, "erin", now).await;

    let mut tx = app
        .pool
        .begin()
        .await
        .expect("opening a transaction must succeed");
    retire_active_key(&mut tx, actor_id, now)
        .await
        .expect("retire_active_key must succeed even when there is no active key to retire");
    tx.commit()
        .await
        .expect("committing must succeed");

    app.cleanup().await;
}

/// Requirement 6.2 (startup bulk load): `load_all_active` returns every
/// currently-active key across multiple actors, including the sealed
/// private key bytes (needed by the future `KeyCache` warm-up), and none of
/// the actors' retired keys.
#[tokio::test]
async fn load_all_active_returns_every_active_key_across_actors() {
    let app = spawn_test_app().await;

    let owner_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();

    let actor_a = app.runtime.ids.next_id();
    let actor_b = app.runtime.ids.next_id();
    create_owner_fixture(&app.pool, owner_id, now).await;
    insert_actor_fixture(&app.pool, owner_id, actor_a, "frank", now).await;
    insert_actor_fixture(&app.pool, owner_id, actor_b, "grace", now).await;

    let key_a = sample_key(app.runtime.ids.next_id(), actor_a, now);
    let key_b = sample_key(app.runtime.ids.next_id(), actor_b, now);
    for key in [&key_a, &key_b] {
        let mut tx = app
            .pool
            .begin()
            .await
            .expect("opening a transaction must succeed");
        insert_active_key(&mut tx, key)
            .await
            .expect("insert_active_key must succeed");
        tx.commit().await.expect("committing must succeed");
    }

    let mut active_keys = load_all_active(&app.pool)
        .await
        .expect("load_all_active must succeed");
    active_keys.sort_by_key(|k| k.id);
    let mut expected = vec![key_a.clone(), key_b.clone()];
    expected.sort_by_key(|k| k.id);
    assert_eq!(active_keys, expected);
    assert!(
        active_keys
            .iter()
            .all(|k| k.status == SigningKeyStatus::Active),
        "load_all_active must only return active keys"
    );
    assert!(
        active_keys.iter().any(|k| !k.sealed_private_key.is_empty()),
        "load_all_active must include the sealed private key bytes, not strip them"
    );

    app.cleanup().await;
}

/// `load_all_active` returns an empty `Vec` (not an error) when no key has
/// ever been inserted at all.
#[tokio::test]
async fn load_all_active_returns_empty_when_no_key_exists() {
    let app = spawn_test_app().await;

    // No fixtures inserted at all in this isolated schema.
    let active_keys = load_all_active(&app.pool)
        .await
        .expect("load_all_active must succeed even with no keys");
    assert!(active_keys.is_empty());

    app.cleanup().await;
}
