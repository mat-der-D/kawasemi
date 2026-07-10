//! Integration-style tests for `ActorDirectory` (Requirements 2.5, 3.1, 3.2,
//! 3.3, 8.1, 8.2, 8.3, 8.4), per task 5.2's observable completion condition:
//! "オーナー別一覧が当該オーナー分のみ返し、ハンドル解決・公開鍵供給の戻り値
//! に owner 情報が含まれない".
//!
//! Mirrors `src/actor/repository/tests.rs`'s and
//! `src/actor/keys/repository/tests.rs`'s established convention:
//! `spawn_test_app` for an isolated, already-migrated schema and a
//! deterministic `RuntimeContext`; real owner/actor/key fixtures created via
//! the already-implemented, already-tested repository functions from
//! sibling modules (never mocks).
//!
//! `ActorSummary`/`ResolvedActor`/`ActorPublicKey` are structurally
//! owner-free already (compile-time-proven by `src/actor/model.rs`'s own
//! exhaustive-destructuring tests), so these tests do not re-prove that
//! type-level fact. What they do prove, at the value level, is this
//! component's actual behavior: `list_actors_for_owner` only returns the
//! queried owner's actors (Requirements 2.5, 8.1), `resolve_actor_by_handle`
//! correctly projects a found actor and reports "not found" for an unknown
//! handle (Requirements 3.1, 3.2, 8.2), and `actor_public_key` correctly
//! projects an actor's active key material and reports "not found" when
//! there is none (Requirements 3.1, 8.3).

use time::OffsetDateTime;

use super::ActorDirectory;
use crate::actor::keys::repository::{SigningKeyStatus, StoredSigningKey, insert_active_key};
use crate::actor::model::{ActorState, ActorType, Handle, LocalActor};
use crate::actor::owner::create_owner;
use crate::actor::repository::insert_actor;
use crate::domain::Id;
use crate::test_harness::spawn_test_app;

/// Creates a real owner fixture.
async fn create_owner_fixture(pool: &sqlx::PgPool, owner_id: Id, now: OffsetDateTime) {
    create_owner(pool, owner_id, now)
        .await
        .expect("creating the owner fixture must succeed");
}

/// Creates a real active actor fixture under an already-existing owner.
async fn insert_actor_fixture(
    pool: &sqlx::PgPool,
    owner_id: Id,
    actor_id: Id,
    handle: &str,
    state: ActorState,
    now: OffsetDateTime,
) -> LocalActor {
    let actor = LocalActor {
        id: actor_id,
        owner_id,
        handle: Handle::new(handle).expect("test handle must be valid"),
        actor_type: ActorType::Person,
        display_name: "Test Actor".to_string(),
        summary: "a test actor".to_string(),
        state,
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
    actor
}

/// Creates a real active signing-key fixture for `actor_id`.
async fn insert_active_key_fixture(
    pool: &sqlx::PgPool,
    key_id: Id,
    actor_id: Id,
    now: OffsetDateTime,
) -> StoredSigningKey {
    let key = StoredSigningKey {
        id: key_id,
        actor_id,
        algorithm: "rsa-2048".to_string(),
        public_key_pem: "-----BEGIN PUBLIC KEY-----\ntest\n-----END PUBLIC KEY-----".to_string(),
        sealed_private_key: b"sealed-opaque-bytes".to_vec(),
        status: SigningKeyStatus::Active,
        created_at: now,
    };
    let mut tx = pool
        .begin()
        .await
        .expect("opening a transaction for the key fixture must succeed");
    insert_active_key(&mut tx, &key)
        .await
        .expect("inserting the active key fixture must succeed");
    tx.commit()
        .await
        .expect("committing the key fixture transaction must succeed");
    key
}

/// Requirements 2.5, 8.1: `list_actors_for_owner` returns only the queried
/// owner's actors (not a different owner's), projected as `ActorSummary`.
#[tokio::test]
async fn list_actors_for_owner_returns_only_that_owners_actors() {
    let app = spawn_test_app().await;
    let directory = ActorDirectory::new(app.pool.clone());

    let now = app.runtime.clock.now();
    let owner_a = app.runtime.ids.next_id();
    let owner_b = app.runtime.ids.next_id();
    create_owner_fixture(&app.pool, owner_a, now).await;
    create_owner_fixture(&app.pool, owner_b, now).await;

    let a1 = insert_actor_fixture(
        &app.pool,
        owner_a,
        app.runtime.ids.next_id(),
        "a_one",
        ActorState::Active,
        now,
    )
    .await;
    let a2 = insert_actor_fixture(
        &app.pool,
        owner_a,
        app.runtime.ids.next_id(),
        "a_two",
        ActorState::Active,
        now,
    )
    .await;
    let b1 = insert_actor_fixture(
        &app.pool,
        owner_b,
        app.runtime.ids.next_id(),
        "b_one",
        ActorState::Active,
        now,
    )
    .await;

    let mut owner_a_summaries = directory
        .list_actors_for_owner(owner_a)
        .await
        .expect("list_actors_for_owner must succeed for owner A");
    owner_a_summaries.sort_by_key(|s| s.id);

    assert_eq!(owner_a_summaries.len(), 2, "must return exactly owner A's two actors");
    let ids: Vec<Id> = owner_a_summaries.iter().map(|s| s.id).collect();
    assert!(ids.contains(&a1.id));
    assert!(ids.contains(&a2.id));
    assert!(
        !ids.contains(&b1.id),
        "list_actors_for_owner(owner_a) must not return owner B's actor"
    );

    for summary in &owner_a_summaries {
        let expected = if summary.id == a1.id { &a1 } else { &a2 };
        assert_eq!(summary.handle, expected.handle);
        assert_eq!(summary.actor_type, expected.actor_type);
        assert_eq!(summary.display_name, expected.display_name);
        assert_eq!(summary.state, expected.state);
    }

    let owner_b_summaries = directory
        .list_actors_for_owner(owner_b)
        .await
        .expect("list_actors_for_owner must succeed for owner B");
    assert_eq!(owner_b_summaries.len(), 1);
    assert_eq!(owner_b_summaries[0].id, b1.id);

    app.cleanup().await;
}

/// `list_actors_for_owner` returns an empty `Vec` (not an error) for an
/// owner that owns no actors.
#[tokio::test]
async fn list_actors_for_owner_returns_empty_for_an_owner_with_no_actors() {
    let app = spawn_test_app().await;
    let directory = ActorDirectory::new(app.pool.clone());

    let owner_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    create_owner_fixture(&app.pool, owner_id, now).await;

    let summaries = directory
        .list_actors_for_owner(owner_id)
        .await
        .expect("list_actors_for_owner must succeed even with no actors");
    assert!(summaries.is_empty());

    app.cleanup().await;
}

/// Requirements 3.1, 3.2, 8.2: `resolve_actor_by_handle` returns the
/// matching actor's data projected into the owner-free `ResolvedActor` shape.
#[tokio::test]
async fn resolve_actor_by_handle_returns_resolved_actor_for_a_known_handle() {
    let app = spawn_test_app().await;
    let directory = ActorDirectory::new(app.pool.clone());

    let owner_id = app.runtime.ids.next_id();
    let actor_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    create_owner_fixture(&app.pool, owner_id, now).await;
    let actor = insert_actor_fixture(
        &app.pool,
        owner_id,
        actor_id,
        "alice",
        ActorState::Active,
        now,
    )
    .await;

    let resolved = directory
        .resolve_actor_by_handle(&actor.handle)
        .await
        .expect("resolve_actor_by_handle must succeed")
        .expect("the just-inserted actor must be resolvable by handle");

    assert_eq!(resolved.id, actor.id);
    assert_eq!(resolved.handle, actor.handle);
    assert_eq!(resolved.actor_type, actor.actor_type);
    assert_eq!(resolved.display_name, actor.display_name);
    assert_eq!(resolved.summary, actor.summary);
    assert_eq!(resolved.state, actor.state);

    app.cleanup().await;
}

/// Requirement 8.2: `resolve_actor_by_handle` returns `Ok(None)` (not an
/// error) for a handle nothing was ever created under.
#[tokio::test]
async fn resolve_actor_by_handle_returns_none_for_an_unknown_handle() {
    let app = spawn_test_app().await;
    let directory = ActorDirectory::new(app.pool.clone());

    let unknown_handle = Handle::new("nobody_here").expect("valid handle");
    let resolved = directory
        .resolve_actor_by_handle(&unknown_handle)
        .await
        .expect("resolve_actor_by_handle must succeed even when nothing matches");
    assert!(resolved.is_none());

    app.cleanup().await;
}

/// Requirement 7.4-adjacent: a deactivated actor is still resolvable by
/// handle, with its state distinguishable through the owner-free
/// `ResolvedActor` (downstream resolution must be able to tell active from
/// deactivated, even though owner information stays hidden).
#[tokio::test]
async fn resolve_actor_by_handle_reports_deactivated_state() {
    let app = spawn_test_app().await;
    let directory = ActorDirectory::new(app.pool.clone());

    let owner_id = app.runtime.ids.next_id();
    let actor_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    create_owner_fixture(&app.pool, owner_id, now).await;
    let actor = insert_actor_fixture(
        &app.pool,
        owner_id,
        actor_id,
        "deactivated_alice",
        ActorState::Deactivated,
        now,
    )
    .await;

    let resolved = directory
        .resolve_actor_by_handle(&actor.handle)
        .await
        .expect("resolve_actor_by_handle must succeed")
        .expect("a deactivated actor must still be resolvable by handle");
    assert_eq!(resolved.state, ActorState::Deactivated);

    app.cleanup().await;
}

/// Requirements 3.1, 8.3: `actor_public_key` returns the actor's active
/// signing key's public material, projected into the owner-free
/// `ActorPublicKey` shape.
#[tokio::test]
async fn actor_public_key_returns_active_public_key_when_present() {
    let app = spawn_test_app().await;
    let directory = ActorDirectory::new(app.pool.clone());

    let owner_id = app.runtime.ids.next_id();
    let actor_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    create_owner_fixture(&app.pool, owner_id, now).await;
    insert_actor_fixture(&app.pool, owner_id, actor_id, "keyed_bob", ActorState::Active, now).await;

    let key_id = app.runtime.ids.next_id();
    let key = insert_active_key_fixture(&app.pool, key_id, actor_id, now).await;

    let public_key = directory
        .actor_public_key(actor_id)
        .await
        .expect("actor_public_key must succeed")
        .expect("the just-inserted active key must be found");

    assert_eq!(public_key.actor_id, actor_id);
    assert_eq!(public_key.key_id, key.id);
    assert_eq!(public_key.public_key_pem, key.public_key_pem);

    app.cleanup().await;
}

/// Requirement 8.3: `actor_public_key` returns `Ok(None)` (not an error) for
/// an actor that has no active signing key.
#[tokio::test]
async fn actor_public_key_returns_none_for_an_actor_with_no_active_key() {
    let app = spawn_test_app().await;
    let directory = ActorDirectory::new(app.pool.clone());

    let owner_id = app.runtime.ids.next_id();
    let actor_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    create_owner_fixture(&app.pool, owner_id, now).await;
    insert_actor_fixture(&app.pool, owner_id, actor_id, "keyless_carol", ActorState::Active, now)
        .await;

    let public_key = directory
        .actor_public_key(actor_id)
        .await
        .expect("actor_public_key must succeed even with no active key");
    assert!(public_key.is_none());

    app.cleanup().await;
}
