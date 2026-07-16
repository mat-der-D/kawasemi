//! Integration test proving task 7.2's observable completion condition
//! (`.kiro/specs/actor-model/tasks.md`, `7.2 (P) 署名鍵の生成・ローテーション・
//! 供給の統合テスト`): "作成時鍵生成、ローテーション（active 高々1・旧鍵
//! retired）、供給が最新鍵を返す/未検出、決定的乱数での再現を検証する" /
//! "ローテーション後に供給が新鍵を返し、決定的構成で鍵が再現され、未登録参照
//! が未検出エラーになる" (Requirements 4.1, 4.3, 5.1, 5.2, 5.3, 5.5, 6.2, 6.3,
//! 6.4).
//!
//! Drives the already-bootstrap-wired `ActorService`/`SigningKeyService`/
//! `ActorDirectory` (`AppState::actor()`, task 6.1) and the real, DB-backed
//! `DbSigningKeyProvider` (`app.runtime.keys`) through `spawn_test_app`,
//! mirroring `tests/actor_bootstrap_wiring_it.rs`'s and
//! `tests/actor_lifecycle_it.rs`'s established pattern for this crate's
//! actor-model integration tests: real Postgres, real composition wiring, no
//! hand-rolled `SigningKeyService`/`KeyCache` built against private
//! internals (that style of test already exists per-component in
//! `src/actor/keys/service/tests.rs` and is out of this task's boundary).
//!
//! This file complements, rather than duplicates,
//! `tests/actor_bootstrap_wiring_it.rs`'s
//! `actor_creation_key_supply_and_rotation_work_end_to_end_through_the_real_bootstrap_wiring`
//! (which already shows the supply boundary's answer changes across a
//! rotation): this file additionally asserts the *retired* status of the
//! pre-rotation key and the "at most one active key" invariant (Requirements
//! 5.2, 5.3), rejection of rotation for a nonexistent actor (Requirement
//! 5.5), not-found supply for an unregistered actor id (Requirement 6.3),
//! and reproducibility of generated key material across two independently
//! constructed, identically-seeded `TestApp` instances (Requirement 4.3).
//!
//! Only public `ActorService`/`SigningKeyService`/`ActorDirectory`/
//! `DbSigningKeyProvider` APIs are exercised, plus a direct read-only SQL
//! query against `actor_signing_keys.status` (there is no public repository
//! function that surfaces a *retired* key's status — `find_active_public_key`
//! and `load_all_active` both deliberately only ever return active keys, per
//! their own doc comments in `src/actor/keys/repository.rs`), scoped to this
//! test's own isolated `spawn_test_app` schema.

use axum::http::StatusCode;
use kawasemi::actor::keys::repository::load_all_active;
use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor};
use kawasemi::error::ErrorKind;
use kawasemi::runtime::{KeyError, KeyRef};
use kawasemi::test_harness::spawn_test_app;

/// Reads the raw `actor_signing_keys.status` value for `key_id` directly,
/// scoped to `pool`'s isolated test schema. Used only to observe a
/// *retired* key's status, since no public repository function returns one
/// (see this file's module doc comment).
async fn signing_key_status(pool: &sqlx::PgPool, key_id: kawasemi::domain::Id) -> String {
    sqlx::query_scalar::<_, String>("SELECT status FROM actor_signing_keys WHERE id = $1")
        .bind(key_id.as_i64())
        .fetch_one(pool)
        .await
        .expect("querying the just-inserted/just-rotated key's status must succeed")
}

/// Requirements 5.1, 5.2, 5.3: rotating an actor's signing key retires the
/// previously active key (distinguishable as `retired`, not merely absent),
/// activates a newly generated key, and at every point at most one active
/// key exists for the actor (checked both via the active-key-only
/// repository listing and directly against the DB).
#[tokio::test]
async fn rotate_key_retires_the_previous_key_and_activates_exactly_one_new_key() {
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
            handle: Handle::new("signing_key_rotation_actor").expect("valid handle"),
            actor_type: ActorType::Person,
            display_name: "Rotation Test Actor".to_string(),
            summary: "exercises task 7.2's rotation scenario".to_string(),
        })
        .await
        .expect("create_actor must succeed for a valid owner and a fresh handle");

    // Capture the pre-rotation active key via the same protocol-facing
    // public-key supply path task 7.1 used, so this task's rotation
    // assertions are anchored to the real key id creation provisioned, not a
    // separately re-derived one.
    let pre_rotation_key = app
        .actor
        .directory()
        .actor_public_key(actor.id)
        .await
        .expect("looking up the active public key before rotation must succeed")
        .expect("a freshly created actor must have an active public key");

    app.actor
        .signing_key_service()
        .rotate_key(actor.id)
        .await
        .expect("rotate_key must succeed for a freshly created actor");

    let post_rotation_key = app
        .actor
        .directory()
        .actor_public_key(actor.id)
        .await
        .expect("looking up the active public key after rotation must succeed")
        .expect("the actor must still have an active public key after rotation");

    assert_ne!(
        pre_rotation_key.key_id, post_rotation_key.key_id,
        "rotation must activate a newly generated key, not reuse the pre-rotation key's id"
    );
    assert_ne!(
        pre_rotation_key.public_key_pem, post_rotation_key.public_key_pem,
        "rotation must generate genuinely new key material, not just re-tag the old row"
    );

    // Requirement 5.3: at most one active key per actor, even after
    // rotation -- checked via the active-key-only repository listing.
    let active_keys_for_actor: Vec<_> = load_all_active(&app.pool)
        .await
        .expect("loading active signing keys must succeed")
        .into_iter()
        .filter(|key| key.actor_id == actor.id)
        .collect();
    assert_eq!(
        active_keys_for_actor.len(),
        1,
        "at most one active key must exist for the actor after rotation (Requirement 5.3)"
    );
    assert_eq!(active_keys_for_actor[0].id, post_rotation_key.key_id);

    // Requirement 5.2: the previously active key is distinguishable as
    // `retired`, not merely absent from the active listing.
    assert_eq!(
        signing_key_status(&app.pool, pre_rotation_key.key_id).await,
        "retired",
        "the pre-rotation key must transition to retired, not stay active or vanish (Requirement 5.2)"
    );
    assert_eq!(
        signing_key_status(&app.pool, post_rotation_key.key_id).await,
        "active",
        "the newly rotated-in key must be active"
    );

    app.cleanup().await;
}

/// Requirement 5.5: rotating a signing key for an actor id that was never
/// created is rejected with a caller-facing `404 Not Found`, not silently
/// accepted or treated as an internal error.
#[tokio::test]
async fn rotate_key_rejects_a_nonexistent_actor_with_a_not_found_error() {
    let app = spawn_test_app().await;

    let nonexistent_actor_id = app.runtime.ids.next_id();

    let err = app
        .actor
        .signing_key_service()
        .rotate_key(nonexistent_actor_id)
        .await
        .expect_err("rotating a nonexistent actor's key must be rejected");

    assert_eq!(
        err.status,
        StatusCode::NOT_FOUND,
        "rotation of a nonexistent actor must surface as a caller-facing 404 Not Found (Requirement 5.5)"
    );
    assert_eq!(
        err.kind,
        ErrorKind::Client,
        "a nonexistent-actor rotation rejection must be caller-facing, not an internal server error"
    );

    app.cleanup().await;
}

/// Requirements 6.2, 6.3, 6.4: the real supply boundary
/// (`DbSigningKeyProvider`, `app.runtime.keys`) returns the actor's active
/// key immediately after creation, reflects the newly rotated key (not the
/// pre-rotation one) after rotation, and reports a not-found error for a
/// `KeyRef` that was never provisioned a key at all.
#[tokio::test]
async fn signing_key_supply_reflects_creation_and_rotation_and_reports_not_found_for_unregistered_refs()
 {
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
            handle: Handle::new("signing_key_supply_actor").expect("valid handle"),
            actor_type: ActorType::Person,
            display_name: "Supply Test Actor".to_string(),
            summary: "exercises task 7.2's supply-boundary scenario".to_string(),
        })
        .await
        .expect("create_actor must succeed");

    let key_ref = KeyRef(actor.id);

    let key_after_creation = app
        .runtime
        .keys
        .signing_key(key_ref)
        .expect("the supply boundary must return the just-provisioned key (Requirement 6.2)");

    app.actor
        .signing_key_service()
        .rotate_key(actor.id)
        .await
        .expect("rotate_key must succeed");

    let key_after_rotation = app
        .runtime
        .keys
        .signing_key(key_ref)
        .expect("the supply boundary must return the rotated key");

    assert_ne!(
        key_after_creation.expose_pem_bytes(),
        key_after_rotation.expose_pem_bytes(),
        "the supply boundary must reflect the newly rotated key, not the pre-rotation one \
         (Requirement 6.4)"
    );

    // Requirement 6.3: an actor id that was never provisioned a key at all
    // (not even before rotation) must surface as a lookup failure, not
    // panic or silently resolve to some other actor's key.
    let never_provisioned_actor_id = app.runtime.ids.next_id();
    let unregistered_ref = KeyRef(never_provisioned_actor_id);
    let not_found_err = app
        .runtime
        .keys
        .signing_key(unregistered_ref)
        .expect_err("an unregistered actor id must not resolve to any signing key");
    assert_eq!(
        not_found_err,
        KeyError::NotFound(unregistered_ref),
        "the not-found error must identify the exact unregistered KeyRef"
    );

    app.cleanup().await;
}

/// Requirement 4.3: "決定的な乱数境界を用いて再現可能な署名鍵を生成できる" --
/// under the deterministic test configuration `spawn_test_app` always builds
/// (a fixed seed, per `src/test_harness.rs`'s own doc comment), performing
/// the identical sequence of id/owner/actor-creation calls against two
/// independently constructed `TestApp` instances reproduces identical
/// signing key material, not merely similarly-shaped output.
#[tokio::test]
async fn key_generation_is_reproducible_across_independently_constructed_deterministic_test_apps() {
    let app_a = spawn_test_app().await;
    let app_b = spawn_test_app().await;

    // Sanity check the premise itself: two freshly constructed TestApps
    // must hand out the same deterministic id sequence and fixed clock
    // value before this test's own actions can diverge them.
    let owner_id_a = app_a.runtime.ids.next_id();
    let owner_id_b = app_b.runtime.ids.next_id();
    assert_eq!(
        owner_id_a, owner_id_b,
        "two independently constructed TestApps must hand out the same deterministic id sequence"
    );
    let now_a = app_a.runtime.clock.now();
    let now_b = app_b.runtime.clock.now();
    assert_eq!(
        now_a, now_b,
        "two independently constructed TestApps must report the same deterministic fixed time"
    );

    create_owner(&app_a.pool, owner_id_a, now_a)
        .await
        .expect("creating the owner fixture in app_a must succeed");
    create_owner(&app_b.pool, owner_id_b, now_b)
        .await
        .expect("creating the owner fixture in app_b must succeed");

    let handle = Handle::new("determinism_test_actor").expect("valid handle");
    let new_actor = |owner_id: kawasemi::domain::Id, handle: Handle| NewActor {
        owner_id,
        handle,
        actor_type: ActorType::Person,
        display_name: "Determinism Test Actor".to_string(),
        summary: "exercises task 7.2's determinism scenario".to_string(),
    };

    let actor_a = app_a
        .actor
        .actor_service()
        .create_actor(new_actor(owner_id_a, handle.clone()))
        .await
        .expect("create_actor must succeed in app_a");
    let actor_b = app_b
        .actor
        .actor_service()
        .create_actor(new_actor(owner_id_b, handle.clone()))
        .await
        .expect("create_actor must succeed in app_b");

    assert_eq!(
        actor_a.id, actor_b.id,
        "identical call sequences against identically-seeded TestApps must produce the same actor id"
    );

    // The heart of Requirement 4.3: the generated *private* signing key
    // material itself, not just its identifier, must be reproducible.
    let key_a = app_a
        .runtime
        .keys
        .signing_key(KeyRef(actor_a.id))
        .expect("app_a's supply boundary must return the just-provisioned key");
    let key_b = app_b
        .runtime
        .keys
        .signing_key(KeyRef(actor_b.id))
        .expect("app_b's supply boundary must return the just-provisioned key");

    assert_eq!(
        key_a.expose_pem_bytes(),
        key_b.expose_pem_bytes(),
        "deterministic rng injection must reproduce identical signing key material across \
         independently constructed TestApp instances (Requirement 4.3)"
    );

    // Corroborate via the protocol-facing public-key supply path too, not
    // just the raw supply-boundary private key material.
    let public_key_a = app_a
        .actor
        .directory()
        .actor_public_key(actor_a.id)
        .await
        .expect("looking up app_a's active public key must succeed")
        .expect("app_a's actor must have an active public key");
    let public_key_b = app_b
        .actor
        .directory()
        .actor_public_key(actor_b.id)
        .await
        .expect("looking up app_b's active public key must succeed")
        .expect("app_b's actor must have an active public key");
    assert_eq!(
        public_key_a.public_key_pem, public_key_b.public_key_pem,
        "the reproduced key pair's public component must also match"
    );

    app_a.cleanup().await;
    app_b.cleanup().await;
}
