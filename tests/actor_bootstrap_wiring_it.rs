//! Integration test proving task 6.1's observable completion condition
//! (`.kiro/specs/actor-model/tasks.md`, `6.1 bootstrap と AppState へ配線
//! する`): "起動後にアクター作成→供給→ローテーションが一連で機能し、署名鍵
//! 供給が DB 由来の最新鍵を返す" — after a real (bootstrap-style) startup,
//! actor creation -> key supply -> rotation works end to end, and the
//! signing-key supply boundary (`RuntimeContext.keys`) returns the
//! DB-sourced current key at every step, never a fixed placeholder
//! (Requirements 6.1-6.4).
//!
//! ## Why `spawn_test_app`, not a hand-rolled `ActorService`/`SigningKeyService`
//! `src/actor/service/tests.rs` and `src/actor/keys/service/tests.rs` each
//! build their *own* throwaway `ActorService`/`SigningKeyService` against a
//! private `KeyCache` via `spawn_test_app().pool`/`.runtime` — proving those
//! components work in isolation, but never exercising the actual
//! composition-root wiring this task adds (a `KeyCache` shared between the
//! services and the real `DbSigningKeyProvider` installed as
//! `RuntimeContext.keys`). This test instead drives the *one* shared
//! `ActorModule`/`RuntimeContext.keys` pair `spawn_test_app` now assembles
//! the same way `bootstrap()`'s production path does
//! (`crate::actor::build_actor_module`, `crate::actor::load_key_cache` —
//! see `src/test_harness.rs`'s own doc comments), so a passing run here is
//! evidence about the wiring itself, not just the already-tested individual
//! components.
//!
//! Uses `#[tokio::test]` + `spawn_test_app` (not a `bootstrap()`-driving
//! test): this task's wiring lives in `build_actor_wiring`/`build_state`,
//! which `spawn_test_app` recomposes from the same building blocks without
//! touching `telemetry::init_telemetry`'s global, install-once-per-process
//! subscriber — so, unlike `tests/bootstrap_lifecycle_it.rs`, this file is
//! free to declare more than one such test.

use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor};
use kawasemi::runtime::KeyRef;
use kawasemi::test_harness::spawn_test_app;

/// Requirements 6.1, 6.2, 6.4: after a real startup, creating an actor
/// (through the bootstrap-wired `ActorService`) provisions a signing key
/// that the real, DB-backed supply boundary (`RuntimeContext.keys`, a
/// `DbSigningKeyProvider`) immediately returns; rotating that key (through
/// the same bootstrap-wired `SigningKeyService`) changes what the supply
/// boundary returns to the newly rotated key, not the pre-rotation one.
#[tokio::test]
async fn actor_creation_key_supply_and_rotation_work_end_to_end_through_the_real_bootstrap_wiring()
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
            handle: Handle::new("wiring_test_actor").expect("valid handle"),
            actor_type: ActorType::Person,
            display_name: "Wiring Test Actor".to_string(),
            summary: "exercises task 6.1's bootstrap wiring".to_string(),
        })
        .await
        .expect("create_actor must succeed through the real bootstrap-wired ActorService");

    let key_ref = KeyRef(actor.id);

    // Supply immediately after creation: the real `RuntimeContext.keys`
    // must already return the key `create_actor` just provisioned. This is
    // what proves the create-actor -> key-cache-warm write path this task
    // wires together actually reaches the same supply boundary downstream
    // federation code will call (Requirement 6.2) -- not a separately
    // constructed, disconnected `KeyCache`.
    let first_key = app
        .runtime
        .keys
        .signing_key(key_ref)
        .expect("the real signing-key supply boundary must return the just-provisioned key");

    // Rotate through the same shared ActorModule.
    app.actor
        .signing_key_service()
        .rotate_key(actor.id)
        .await
        .expect("rotate_key must succeed for a freshly created actor");

    let second_key = app
        .runtime
        .keys
        .signing_key(key_ref)
        .expect("the real signing-key supply boundary must return the rotated key");

    assert_ne!(
        first_key.expose_pem_bytes(),
        second_key.expose_pem_bytes(),
        "the supply boundary must reflect the newly rotated key, not the pre-rotation one \
         (Requirement 6.4)"
    );

    app.cleanup().await;
}

/// Requirement 6.3: a `KeyRef` for an actor id that was never provisioned a
/// signing key must surface as a lookup failure through the real supply
/// boundary, not silently succeed or panic.
#[tokio::test]
async fn signing_key_supply_reports_not_found_for_an_actor_id_with_no_provisioned_key() {
    let app = spawn_test_app().await;

    let never_provisioned_actor_id = app.runtime.ids.next_id();

    let result = app
        .runtime
        .keys
        .signing_key(KeyRef(never_provisioned_actor_id));

    assert!(
        result.is_err(),
        "an actor id with no provisioned key must not resolve to a signing key"
    );

    app.cleanup().await;
}
