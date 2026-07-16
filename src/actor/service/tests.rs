//! Integration-style tests for `ActorService` (Requirements 1.1, 1.3, 1.5,
//! 1.6, 2.2, 2.3, 7.2, 7.3, 7.5), per task 5.1's observable completion
//! condition: "アクター作成で active アクターと有効鍵1つが永続化され、
//! オーナー不在/重複ハンドル/形式不正が各エラーを返し、無効化で状態が
//! 遷移する".
//!
//! Mirrors `src/actor/keys/service/tests.rs`'s established convention:
//! `spawn_test_app` for an isolated, already-migrated schema and a
//! deterministic `RuntimeContext`; a fresh `ChaCha20Poly1305KeyCipher` under
//! a fixed test KEK backs the `SigningKeyService` this `ActorService` is
//! built on top of.
//!
//! Requirement 1.6 (handle-format rejection) is exercised by
//! `src/actor/model.rs`'s own `Handle::new` tests, not re-tested here — see
//! `service.rs`'s doc comment ("`NewActor.handle` is a pre-validated
//! `Handle`") for why: `NewActor.handle` is already a validated `Handle` by
//! the time any of these tests can construct one, so there is no
//! service-level "reject a malformed raw string" behavior left to observe.

use std::sync::Arc;

use sqlx::PgPool;

use super::{ActorService, NewActor};
use crate::actor::keys::cache::KeyCache;
use crate::actor::keys::cipher::ChaCha20Poly1305KeyCipher;
use crate::actor::keys::repository::find_active_public_key;
use crate::actor::keys::service::SigningKeyService;
use crate::actor::model::{ActorState, ActorType, Handle};
use crate::actor::owner::create_owner;
use crate::config::Secret;
use crate::domain::Id;
use crate::error::ErrorKind;
use crate::runtime::RuntimeContext;
use crate::test_harness::spawn_test_app;

/// Builds an `ActorService` sharing `pool`/`runtime` with a fresh
/// `SigningKeyService` (its own `ChaCha20Poly1305KeyCipher` under a fixed
/// test KEK and a fresh `KeyCache`), mirroring
/// `src/actor/keys/service/tests.rs`'s `service_under_test` convention.
fn service_under_test(pool: PgPool, runtime: RuntimeContext) -> ActorService {
    let cipher = Arc::new(ChaCha20Poly1305KeyCipher::new(Secret::new([7u8; 32])));
    let signing_key_service = Arc::new(SigningKeyService::new(
        pool.clone(),
        runtime.clone(),
        cipher,
        KeyCache::new(),
    ));
    ActorService::new(pool, runtime, signing_key_service)
}

fn sample_new_actor(owner_id: Id, handle: &str) -> NewActor {
    NewActor {
        owner_id,
        handle: Handle::new(handle).expect("test handle must be valid"),
        actor_type: ActorType::Person,
        display_name: "Test Actor".to_string(),
        summary: "a test actor".to_string(),
    }
}

/// Requirements 1.1, 1.5, 2.2, 4.1, 7.2, 7.5: `create_actor` persists an
/// `Active`-initialized actor (id/timestamps from the injected runtime) and
/// exactly one active signing key for it.
#[tokio::test]
async fn create_actor_persists_an_active_actor_and_exactly_one_active_key() {
    let app = spawn_test_app().await;
    let service = service_under_test(app.pool.clone(), app.runtime.clone());

    let owner_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    create_owner(&app.pool, owner_id, now)
        .await
        .expect("creating the owner must succeed");

    let created = service
        .create_actor(sample_new_actor(owner_id, "alice"))
        .await
        .expect("create_actor must succeed for a valid owner and fresh handle");

    assert_eq!(created.owner_id, owner_id);
    assert_eq!(created.handle.as_str(), "alice");
    assert_eq!(created.state, ActorState::Active);
    assert_eq!(created.created_at, created.updated_at);

    // Persisted row matches what create_actor returned.
    let by_id = crate::actor::repository::find_by_id(&app.pool, created.id)
        .await
        .expect("find_by_id must succeed")
        .expect("the just-created actor must be persisted");
    assert_eq!(by_id, created);

    // Exactly one active signing key exists for the new actor.
    let public_key = find_active_public_key(&app.pool, created.id)
        .await
        .expect("find_active_public_key must succeed")
        .expect("create_actor must have provisioned an active signing key");
    assert_eq!(public_key.actor_id, created.id);
    assert!(
        public_key
            .public_key_pem
            .starts_with("-----BEGIN PUBLIC KEY-----")
    );

    app.cleanup().await;
}

/// Requirement 2.3: creating an actor against a nonexistent owner is
/// rejected with a caller-facing error, and persists neither an actor row
/// nor a signing key.
#[tokio::test]
async fn create_actor_rejects_a_nonexistent_owner() {
    let app = spawn_test_app().await;
    let service = service_under_test(app.pool.clone(), app.runtime.clone());

    let nonexistent_owner = app.runtime.ids.next_id();

    let err = service
        .create_actor(sample_new_actor(nonexistent_owner, "orphan"))
        .await
        .expect_err("create_actor must reject a nonexistent owner");
    assert_eq!(err.kind, ErrorKind::Client);
    assert!(err.status.is_client_error());

    let found = crate::actor::repository::find_by_handle(
        &app.pool,
        &Handle::new("orphan").expect("valid handle"),
    )
    .await
    .expect("find_by_handle must succeed");
    assert!(
        found.is_none(),
        "no actor row must be persisted when owner resolution fails"
    );

    app.cleanup().await;
}

/// Requirement 1.3: creating a second actor with a handle that already
/// exists is rejected with a caller-facing duplicate error, and the
/// original actor/its key are left untouched.
#[tokio::test]
async fn create_actor_rejects_a_duplicate_handle() {
    let app = spawn_test_app().await;
    let service = service_under_test(app.pool.clone(), app.runtime.clone());

    let owner_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    create_owner(&app.pool, owner_id, now)
        .await
        .expect("creating the owner must succeed");

    let first = service
        .create_actor(sample_new_actor(owner_id, "bob"))
        .await
        .expect("first create_actor must succeed");

    let err = service
        .create_actor(sample_new_actor(owner_id, "bob"))
        .await
        .expect_err("create_actor must reject a duplicate handle");
    assert_eq!(err.kind, ErrorKind::Client);
    assert_eq!(err.status, axum::http::StatusCode::CONFLICT);

    // The original actor and its key are untouched.
    let by_handle = crate::actor::repository::find_by_handle(
        &app.pool,
        &Handle::new("bob").expect("valid handle"),
    )
    .await
    .expect("find_by_handle must succeed")
    .expect("the original actor must still be found");
    assert_eq!(by_handle, first);

    let public_key = find_active_public_key(&app.pool, first.id)
        .await
        .expect("find_active_public_key must succeed")
        .expect("the original actor's key must still be active");
    assert_eq!(public_key.actor_id, first.id);

    app.cleanup().await;
}

/// Requirement 7.3, 7.5: `deactivate_actor` transitions a persisted actor's
/// state to `Deactivated`, updates `updated_at`, and returns the actor
/// reflecting the new state.
#[tokio::test]
async fn deactivate_actor_transitions_state_and_returns_the_updated_actor() {
    let app = spawn_test_app().await;
    let service = service_under_test(app.pool.clone(), app.runtime.clone());

    let owner_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    create_owner(&app.pool, owner_id, now)
        .await
        .expect("creating the owner must succeed");

    let created = service
        .create_actor(sample_new_actor(owner_id, "carol"))
        .await
        .expect("create_actor must succeed");
    assert_eq!(created.state, ActorState::Active);

    let deactivated = service
        .deactivate_actor(created.id)
        .await
        .expect("deactivate_actor must succeed for an existing actor");

    assert_eq!(deactivated.id, created.id);
    assert_eq!(deactivated.state, ActorState::Deactivated);
    assert!(
        deactivated.updated_at >= created.updated_at,
        "updated_at must not move backwards on deactivation"
    );

    let by_id = crate::actor::repository::find_by_id(&app.pool, created.id)
        .await
        .expect("find_by_id must succeed")
        .expect("the actor must still be found after deactivation");
    assert_eq!(by_id, deactivated);

    app.cleanup().await;
}

/// `deactivate_actor` for an id nothing was ever created under is rejected
/// with a caller-facing error, not a generic 5xx.
#[tokio::test]
async fn deactivate_actor_rejects_a_nonexistent_actor() {
    let app = spawn_test_app().await;
    let service = service_under_test(app.pool.clone(), app.runtime.clone());

    let unknown_id = Id::from_i64(i64::MAX - 1);

    let err = service
        .deactivate_actor(unknown_id)
        .await
        .expect_err("deactivate_actor must reject a nonexistent actor");
    assert_eq!(err.kind, ErrorKind::Client);
    assert!(err.status.is_client_error());

    app.cleanup().await;
}
