//! Integration-style tests for `ActorRepository` (Requirements 1.1, 1.2,
//! 1.3, 2.2, 7.3, 8.1, 8.2), per task 2.2's observable completion condition:
//! "重複ハンドル挿入が重複エラーを返し、オーナー別取得が当該オーナーの
//! アクターのみを返す".
//!
//! Mirrors `src/actor/owner/tests.rs`'s established convention: reuses
//! `crate::test_harness::spawn_test_app` for an isolated, already-migrated
//! schema and a deterministic `RuntimeContext`, and creates a real owner row
//! via `crate::actor::owner::create_owner` first (an actor's `owner_id` is a
//! mandatory foreign key into `owners`, so every test needs one).
//!
//! `insert_actor` takes an already-open transaction (see repository.rs's
//! doc comment for why); these tests open one via `pool.begin()` and commit
//! it themselves, standing in for the future `ActorService` that will do so
//! in production.

use super::{find_by_handle, find_by_id, insert_actor, list_by_owner, update_state};
use crate::actor::model::{ActorState, ActorType, Handle, LocalActor};
use crate::actor::owner::create_owner;
use crate::domain::Id;
use crate::test_harness::spawn_test_app;

/// Builds a `LocalActor` value ready to hand to `insert_actor`, using the
/// harness's deterministic runtime for `id`/`created_at`/`updated_at`, and
/// the caller-supplied `owner_id`/`handle`.
fn sample_actor(owner_id: Id, id: Id, handle: &str, now: time::OffsetDateTime) -> LocalActor {
    LocalActor {
        id,
        owner_id,
        handle: Handle::new(handle).expect("test handle must be valid"),
        actor_type: ActorType::Person,
        display_name: "Test Actor".to_string(),
        summary: "a test actor".to_string(),
        state: ActorState::Active,
        created_at: now,
        updated_at: now,
    }
}

/// Requirements 1.1, 1.2, 2.2: inserting an actor persists a row associated
/// with an existing owner, retrievable unchanged by both handle and id.
#[tokio::test]
async fn insert_actor_persists_a_row_findable_by_handle_and_id() {
    let app = spawn_test_app().await;

    let owner_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    create_owner(&app.pool, owner_id, now)
        .await
        .expect("creating the owner must succeed");

    let actor_id = app.runtime.ids.next_id();
    let actor = sample_actor(owner_id, actor_id, "alice", now);

    let mut tx = app
        .pool
        .begin()
        .await
        .expect("opening a transaction must succeed");
    insert_actor(&mut tx, &actor)
        .await
        .expect("insert_actor must succeed for a fresh handle/id");
    tx.commit()
        .await
        .expect("committing the transaction must succeed");

    let by_handle = find_by_handle(&app.pool, &actor.handle)
        .await
        .expect("find_by_handle must succeed")
        .expect("the just-inserted actor must be found by handle");
    assert_eq!(by_handle, actor);

    let by_id = find_by_id(&app.pool, actor_id)
        .await
        .expect("find_by_id must succeed")
        .expect("the just-inserted actor must be found by id");
    assert_eq!(by_id, actor);

    app.cleanup().await;
}

/// Requirement 1.3: inserting a second actor with a handle that already
/// exists must be rejected with a caller-facing duplicate error, not a
/// generic 5xx, and must not disturb the original row.
#[tokio::test]
async fn insert_actor_rejects_duplicate_handle_with_a_client_error() {
    let app = spawn_test_app().await;

    let owner_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    create_owner(&app.pool, owner_id, now)
        .await
        .expect("creating the owner must succeed");

    let first_id = app.runtime.ids.next_id();
    let first_actor = sample_actor(owner_id, first_id, "bob", now);
    let mut tx = app
        .pool
        .begin()
        .await
        .expect("opening a transaction must succeed");
    insert_actor(&mut tx, &first_actor)
        .await
        .expect("first insert_actor must succeed");
    tx.commit()
        .await
        .expect("committing the first transaction must succeed");

    let second_id = app.runtime.ids.next_id();
    let second_actor = sample_actor(owner_id, second_id, "bob", now);
    let mut tx2 = app
        .pool
        .begin()
        .await
        .expect("opening a second transaction must succeed");
    let err = insert_actor(&mut tx2, &second_actor)
        .await
        .expect_err("inserting a duplicate handle must be rejected");
    assert_eq!(
        err.kind,
        crate::error::ErrorKind::Client,
        "a duplicate handle is a caller-triggerable condition, not a server error"
    );
    // The failed insert must not have left a stray row under `second_id`.
    let _ = tx2.rollback().await;

    let by_id = find_by_id(&app.pool, second_id)
        .await
        .expect("find_by_id must succeed");
    assert!(
        by_id.is_none(),
        "the rejected duplicate-handle insert must not have persisted a row"
    );

    // The original row must be untouched.
    let by_handle = find_by_handle(&app.pool, &first_actor.handle)
        .await
        .expect("find_by_handle must succeed")
        .expect("the original actor must still be found");
    assert_eq!(by_handle, first_actor);

    app.cleanup().await;
}

/// Requirement 8.1: `list_by_owner` returns only the actors belonging to
/// the queried owner, not actors belonging to a different owner.
#[tokio::test]
async fn list_by_owner_returns_only_that_owners_actors() {
    let app = spawn_test_app().await;

    let now = app.runtime.clock.now();
    let owner_a = app.runtime.ids.next_id();
    let owner_b = app.runtime.ids.next_id();
    create_owner(&app.pool, owner_a, now)
        .await
        .expect("creating owner A must succeed");
    create_owner(&app.pool, owner_b, now)
        .await
        .expect("creating owner B must succeed");

    let a1 = sample_actor(owner_a, app.runtime.ids.next_id(), "a_one", now);
    let a2 = sample_actor(owner_a, app.runtime.ids.next_id(), "a_two", now);
    let b1 = sample_actor(owner_b, app.runtime.ids.next_id(), "b_one", now);

    for actor in [&a1, &a2, &b1] {
        let mut tx = app
            .pool
            .begin()
            .await
            .expect("opening a transaction must succeed");
        insert_actor(&mut tx, actor)
            .await
            .expect("insert_actor must succeed");
        tx.commit().await.expect("committing must succeed");
    }

    let mut owner_a_actors = list_by_owner(&app.pool, owner_a)
        .await
        .expect("list_by_owner must succeed for owner A");
    owner_a_actors.sort_by_key(|a| a.id);
    let mut expected_a = vec![a1.clone(), a2.clone()];
    expected_a.sort_by_key(|a| a.id);
    assert_eq!(owner_a_actors, expected_a);
    assert!(
        owner_a_actors.iter().all(|a| a.owner_id == owner_a),
        "list_by_owner(owner_a) must not return actors belonging to a different owner"
    );

    let owner_b_actors = list_by_owner(&app.pool, owner_b)
        .await
        .expect("list_by_owner must succeed for owner B");
    assert_eq!(owner_b_actors, vec![b1]);

    app.cleanup().await;
}

/// `list_by_owner` returns an empty `Vec` (not an error) for an owner that
/// owns no actors.
#[tokio::test]
async fn list_by_owner_returns_empty_for_an_owner_with_no_actors() {
    let app = spawn_test_app().await;

    let owner_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    create_owner(&app.pool, owner_id, now)
        .await
        .expect("creating the owner must succeed");

    let actors = list_by_owner(&app.pool, owner_id)
        .await
        .expect("list_by_owner must succeed even with no actors");
    assert!(actors.is_empty());

    app.cleanup().await;
}

/// Requirement 7.3: `update_state` transitions a persisted actor's state
/// and `updated_at`, and reports success as `Ok(true)`.
#[tokio::test]
async fn update_state_transitions_state_and_updated_at_and_reports_true() {
    let app = spawn_test_app().await;

    let owner_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    create_owner(&app.pool, owner_id, now)
        .await
        .expect("creating the owner must succeed");

    let actor_id = app.runtime.ids.next_id();
    let actor = sample_actor(owner_id, actor_id, "carol", now);
    let mut tx = app
        .pool
        .begin()
        .await
        .expect("opening a transaction must succeed");
    insert_actor(&mut tx, &actor)
        .await
        .expect("insert_actor must succeed");
    tx.commit().await.expect("committing must succeed");

    let later = now + time::Duration::seconds(60);
    let updated = update_state(&app.pool, actor_id, ActorState::Deactivated, later)
        .await
        .expect("update_state must succeed for an existing actor");
    assert!(
        updated,
        "update_state must report true when a row was updated"
    );

    let found = find_by_id(&app.pool, actor_id)
        .await
        .expect("find_by_id must succeed")
        .expect("the actor must still be found after state update");
    assert_eq!(found.state, ActorState::Deactivated);
    assert_eq!(found.updated_at, later);

    app.cleanup().await;
}

/// `update_state` for an id nothing was ever created under returns
/// `Ok(false)`, not an error — mirrors `find_by_id`/`find_by_handle`'s
/// "no error for absence" contract at this data layer.
#[tokio::test]
async fn update_state_returns_false_for_an_unknown_id() {
    let app = spawn_test_app().await;

    let unknown_id = Id::from_i64(i64::MAX - 1);
    let now = app.runtime.clock.now();
    let updated = update_state(&app.pool, unknown_id, ActorState::Deactivated, now)
        .await
        .expect("update_state must succeed even when nothing matches");
    assert!(!updated);

    app.cleanup().await;
}

/// `find_by_handle` for a handle nothing was ever created under returns
/// `Ok(None)`, not an error.
#[tokio::test]
async fn find_by_handle_returns_none_for_an_unknown_handle() {
    let app = spawn_test_app().await;

    let unknown_handle = Handle::new("nobody_here").expect("valid handle");
    let found = find_by_handle(&app.pool, &unknown_handle)
        .await
        .expect("find_by_handle must succeed even when nothing matches");
    assert!(found.is_none());

    app.cleanup().await;
}
