//! Integration test proving task 7.1's observable completion condition
//! (`.kiro/specs/actor-model/tasks.md`, `7.1 (P) アクターライフサイクルの統合
//! テスト`): "作成（active＋鍵1つ）、重複ハンドル拒否、無効化後の状態判別を
//! `spawn_test_app` 上で検証する" / "上記シナリオがグリーンで、無効化後に解決
//! で Deactivated が判別できる" (Requirements 1.1, 1.3, 7.2, 7.3, 7.4).
//!
//! Drives the already-bootstrap-wired `ActorService`/`ActorDirectory`
//! (`AppState::actor()`, task 6.1) through `spawn_test_app`, mirroring
//! `tests/actor_bootstrap_wiring_it.rs`'s established pattern for this
//! crate's actor-model integration tests: real Postgres, real composition
//! wiring, no hand-rolled `ActorService`/`ActorDirectory` built against a
//! private `KeyCache` (that style of test already exists per-component in
//! `src/actor/service/tests.rs` / `src/actor/directory/tests.rs` and is out
//! of this task's boundary).
//!
//! Only public `ActorService`/`ActorDirectory`/`keys::repository` APIs are
//! exercised (no reach into repository internals beyond the already-public
//! `keys::repository::load_all_active`, itself used the same way
//! `src/actor.rs::load_key_cache` uses it in production).

use axum::http::StatusCode;
use kawasemi::actor::keys::repository::load_all_active;
use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorState, ActorType, Handle, NewActor};
use kawasemi::test_harness::spawn_test_app;

/// Requirements 1.1, 7.2: creating an actor persists it in the `Active`
/// state, and provisions it exactly one active signing key (Requirement
/// 4.1's "鍵1つ", verified here as this task's "作成（active＋鍵1つ）"
/// acceptance bullet).
#[tokio::test]
async fn create_actor_persists_an_active_actor_with_exactly_one_active_signing_key() {
    let app = spawn_test_app().await;

    let owner_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    create_owner(&app.pool, owner_id, now)
        .await
        .expect("creating the owner fixture must succeed");

    let actor = app
        .actor
        .actor_service()
        .create_actor(NewActor {
            owner_id,
            handle: Handle::new("lifecycle_created").expect("valid handle"),
            actor_type: ActorType::Person,
            display_name: "Lifecycle Created Actor".to_string(),
            summary: "exercises task 7.1's create-actor scenario".to_string(),
        })
        .await
        .expect("create_actor must succeed for a valid owner and a fresh handle");

    assert_eq!(
        actor.state,
        ActorState::Active,
        "a freshly created actor must be initialized in the Active state (Requirement 7.2)"
    );

    // "鍵1つ": exactly one active signing key for this actor, not zero and
    // not more than one, persisted in `actor_signing_keys`.
    let active_keys_for_actor: Vec<_> = load_all_active(&app.pool)
        .await
        .expect("loading active signing keys must succeed")
        .into_iter()
        .filter(|key| key.actor_id == actor.id)
        .collect();
    assert_eq!(
        active_keys_for_actor.len(),
        1,
        "actor creation must provision exactly one active signing key (Requirement 4.1)"
    );

    // The directory's public-key supply path corroborates the same fact
    // through the protocol-layer-facing surface, not just the raw
    // repository listing.
    let public_key = app
        .actor
        .directory()
        .actor_public_key(actor.id)
        .await
        .expect("looking up the active public key must succeed")
        .expect("a freshly created actor must have an active public key");
    assert_eq!(public_key.actor_id, actor.id);
    assert_eq!(public_key.key_id, active_keys_for_actor[0].id);

    app.cleanup().await;
}

/// Requirement 1.3: creating a second actor with a handle that is already
/// in use is rejected with a duplicate-indicating (`409 Conflict`,
/// caller-facing) error, and does not silently succeed or overwrite the
/// existing actor.
#[tokio::test]
async fn create_actor_rejects_a_duplicate_handle_with_a_conflict_error() {
    let app = spawn_test_app().await;

    let owner_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    create_owner(&app.pool, owner_id, now)
        .await
        .expect("creating the owner fixture must succeed");

    let handle = Handle::new("lifecycle_duplicate").expect("valid handle");

    let first = app
        .actor
        .actor_service()
        .create_actor(NewActor {
            owner_id,
            handle: handle.clone(),
            actor_type: ActorType::Person,
            display_name: "First Actor".to_string(),
            summary: "the original holder of the handle".to_string(),
        })
        .await
        .expect("the first creation with a fresh handle must succeed");

    // A second owner requesting the exact same handle must still be
    // rejected: uniqueness is instance-wide (Requirement 1.2), not scoped
    // to a single owner.
    let second_owner_id = app.runtime.ids.next_id();
    create_owner(&app.pool, second_owner_id, now)
        .await
        .expect("creating the second owner fixture must succeed");

    let err = app
        .actor
        .actor_service()
        .create_actor(NewActor {
            owner_id: second_owner_id,
            handle: handle.clone(),
            actor_type: ActorType::Service,
            display_name: "Duplicate Handle Actor".to_string(),
            summary: "must be rejected".to_string(),
        })
        .await
        .expect_err("creating a second actor with a duplicate handle must be rejected");

    assert_eq!(
        err.status,
        StatusCode::CONFLICT,
        "a duplicate-handle rejection must surface as a caller-facing 409 Conflict (Requirement 1.3)"
    );
    assert_eq!(
        err.kind,
        kawasemi::error::ErrorKind::Client,
        "a duplicate-handle rejection must be caller-facing, not an internal server error"
    );

    // The original actor must be entirely unaffected by the rejected
    // duplicate attempt.
    let resolved = app
        .actor
        .directory()
        .resolve_actor_by_handle(&handle)
        .await
        .expect("resolving the still-unique handle must succeed")
        .expect("the original actor must still resolve by its handle");
    assert_eq!(resolved.id, first.id);
    assert_eq!(resolved.display_name, "First Actor");

    app.cleanup().await;
}

/// Requirements 7.3, 7.4: deactivating an actor transitions it to the
/// `Deactivated` state, and that state is distinguishable through a
/// downstream-facing resolution path (`ActorDirectory::resolve_actor_by_handle`)
/// afterward — not merely on the value `deactivate_actor` itself returns.
#[tokio::test]
async fn deactivating_an_actor_is_visible_as_deactivated_through_resolve_actor_by_handle() {
    let app = spawn_test_app().await;

    let owner_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    create_owner(&app.pool, owner_id, now)
        .await
        .expect("creating the owner fixture must succeed");

    let handle = Handle::new("lifecycle_deactivated").expect("valid handle");

    let actor = app
        .actor
        .actor_service()
        .create_actor(NewActor {
            owner_id,
            handle: handle.clone(),
            actor_type: ActorType::Person,
            display_name: "Soon Deactivated Actor".to_string(),
            summary: "exercises task 7.1's deactivation scenario".to_string(),
        })
        .await
        .expect("create_actor must succeed");
    assert_eq!(actor.state, ActorState::Active);

    // Before deactivation, resolution must still report Active.
    let resolved_before = app
        .actor
        .directory()
        .resolve_actor_by_handle(&handle)
        .await
        .expect("resolving before deactivation must succeed")
        .expect("the actor must resolve by handle before deactivation");
    assert_eq!(resolved_before.state, ActorState::Active);

    let deactivated = app
        .actor
        .actor_service()
        .deactivate_actor(actor.id)
        .await
        .expect("deactivating an existing actor must succeed");
    assert_eq!(
        deactivated.state,
        ActorState::Deactivated,
        "deactivate_actor's own return value must reflect the new state (Requirement 7.3)"
    );

    // Requirement 7.4: after deactivation, downstream resolution via handle
    // must distinguish the Deactivated state -- this is the task's own
    // "無効化後に解決で Deactivated が判別できる" completion condition.
    let resolved_after = app
        .actor
        .directory()
        .resolve_actor_by_handle(&handle)
        .await
        .expect("resolving after deactivation must succeed")
        .expect("a deactivated actor must still resolve by handle (it exists, just deactivated)");
    assert_eq!(
        resolved_after.state,
        ActorState::Deactivated,
        "resolution after deactivation must reveal the Deactivated state (Requirement 7.4)"
    );
    assert_eq!(resolved_after.id, actor.id);

    // Deactivating an actor that does not exist must be rejected, not
    // silently succeed (the natural counterpart to the "no such actor"
    // rejection design.md documents for this operation).
    let missing_actor_id = app.runtime.ids.next_id();
    let missing_err = app
        .actor
        .actor_service()
        .deactivate_actor(missing_actor_id)
        .await
        .expect_err("deactivating a non-existent actor must be rejected");
    assert_eq!(missing_err.status, StatusCode::NOT_FOUND);

    app.cleanup().await;
}
