//! Integration test proving task 7.3's observable completion condition
//! (`.kiro/specs/actor-model/tasks.md`, `7.3 (P) オーナー↔アクター境界の統合
//! テスト`): "管理層一覧がオーナー別に正しく、プロトコル層参照（ハンドル解決・
//! 公開鍵供給）に owner 情報が一切現れないこと、秘密鍵がログ/参照型/DB 平文に
//! 現れないことを検証する" / "プロトコル経路の戻り値・ログに owner と平文秘密
//! 鍵が含まれず、管理層一覧のみが owner 別対応を返す" (Requirements 2.5, 3.1,
//! 3.2, 3.3, 4.4, 4.5, 8.1, 8.4).
//!
//! Drives the already-bootstrap-wired `ActorService`/`ActorDirectory`
//! (`AppState::actor()`, task 6.1) through `spawn_test_app`, mirroring
//! `tests/actor_lifecycle_it.rs`'s and `tests/signing_key_it.rs`'s
//! established pattern for this crate's actor-model integration tests: real
//! Postgres, real composition wiring, no hand-rolled services built against
//! private internals.
//!
//! Three scenarios, matching design.md's "Integration Tests" / "Security"
//! Testing Strategy bullets:
//! - `list_actors_for_owner` (the one management-layer, owner-scoped
//!   operation, Requirement 3.3) returns exactly the actors owned by the
//!   queried owner, never another owner's actors.
//! - `resolve_actor_by_handle` / `actor_public_key` (protocol-layer) return
//!   types that structurally carry no owner field at all -- proven the same
//!   way `src/actor/model.rs`'s own unit tests do, via exhaustive
//!   destructuring (no `..`) that would fail to compile if an owner field
//!   were ever added -- and, as a belt-and-suspenders behavioral check,
//!   actors created under two different owners are resolved via these paths
//!   with no owner-comparable field to even distinguish them by.
//! - Private key material is never plaintext in any protocol-facing
//!   reference type (`ResolvedActor`, `ActorPublicKey`, `ActorSummary` --
//!   structurally, via the same exhaustive-destructure technique) nor in the
//!   DB's persisted `sealed_private_key` bytes, which are compared directly
//!   against the actor's real plaintext PEM (obtained via the public
//!   `SigningKeyProvider` supply boundary, `app.runtime.keys`) and asserted
//!   to differ, with the plaintext PEM not appearing as a substring of the
//!   sealed bytes. Log-plaintext-absence is not independently re-verified
//!   here: this codebase has no log-capture test harness (only `tracing`'s
//!   standard subscriber wiring), and Requirement 4.4/4.5's protection is
//!   already structurally enforced by `Secret`/masked-`Debug` wrappers
//!   (`src/config/secret.rs`, `src/runtime/signing_key.rs`) -- this file
//!   additionally asserts that a `SigningKey`'s `Debug` output does not leak
//!   the plaintext PEM, as the closest in-process proxy for "never printed",
//!   without inventing a new logging-integration harness out of this task's
//!   boundary.
//!
//! Only public `ActorService`/`ActorDirectory`/`SigningKeyProvider`/
//! `keys::repository::load_all_active` APIs are exercised (the latter is
//! already used the same way by `tests/actor_lifecycle_it.rs`/
//! `tests/signing_key_it.rs` and by `src/actor.rs::load_key_cache` in
//! production; it is the one public repository function that surfaces the
//! raw `sealed_private_key` bytes this task's DB-plaintext check needs, so
//! no scoped raw SQL was necessary here).

use kawasemi::actor::keys::repository::load_all_active;
use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorPublicKey, ActorSummary, ActorType, Handle, NewActor, ResolvedActor};
use kawasemi::runtime::KeyRef;
use kawasemi::test_harness::spawn_test_app;

/// Requirements 2.5, 3.3, 8.1: `list_actors_for_owner` returns exactly the
/// actors belonging to the queried owner -- never another owner's actors --
/// even when two owners each hold multiple actors and one owner's actor
/// count differs from the other's.
#[tokio::test]
async fn list_actors_for_owner_returns_only_that_owners_actors() {
    let app = spawn_test_app().await;

    let owner_a = app.runtime.ids.next_id();
    let owner_b = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    create_owner(&app.pool, owner_a, now)
        .await
        .expect("creating owner A fixture must succeed");
    create_owner(&app.pool, owner_b, now)
        .await
        .expect("creating owner B fixture must succeed");

    let make_actor = |owner_id, handle: &str, display_name: &str| NewActor {
        owner_id,
        handle: Handle::new(handle).expect("valid handle"),
        actor_type: ActorType::Person,
        display_name: display_name.to_string(),
        summary: "exercises task 7.3's owner-scoped listing scenario".to_string(),
    };

    // Owner A holds two actors, owner B holds one -- an asymmetric split so
    // a naive "return everything" or "return the first N rows" bug would be
    // caught by the count assertions below, not just by set membership.
    let actor_a1 = app
        .actor
        .actor_service()
        .create_actor(make_actor(
            owner_a,
            "boundary_owner_a_one",
            "Owner A Actor One",
        ))
        .await
        .expect("creating owner A's first actor must succeed");
    let actor_a2 = app
        .actor
        .actor_service()
        .create_actor(make_actor(
            owner_a,
            "boundary_owner_a_two",
            "Owner A Actor Two",
        ))
        .await
        .expect("creating owner A's second actor must succeed");
    let actor_b1 = app
        .actor
        .actor_service()
        .create_actor(make_actor(
            owner_b,
            "boundary_owner_b_one",
            "Owner B Actor One",
        ))
        .await
        .expect("creating owner B's actor must succeed");

    let owner_a_listing = app
        .actor
        .directory()
        .list_actors_for_owner(owner_a)
        .await
        .expect("listing owner A's actors must succeed");
    let mut owner_a_ids: Vec<_> = owner_a_listing.iter().map(|summary| summary.id).collect();
    owner_a_ids.sort_by_key(|id| id.as_i64());
    let mut expected_owner_a_ids = vec![actor_a1.id, actor_a2.id];
    expected_owner_a_ids.sort_by_key(|id| id.as_i64());
    assert_eq!(
        owner_a_ids, expected_owner_a_ids,
        "owner A's listing must contain exactly owner A's two actors, not owner B's (Requirement 8.1)"
    );
    assert!(
        !owner_a_ids.contains(&actor_b1.id),
        "owner A's listing must never include owner B's actor"
    );

    let owner_b_listing = app
        .actor
        .directory()
        .list_actors_for_owner(owner_b)
        .await
        .expect("listing owner B's actors must succeed");
    let owner_b_ids: Vec<_> = owner_b_listing.iter().map(|summary| summary.id).collect();
    assert_eq!(
        owner_b_ids,
        vec![actor_b1.id],
        "owner B's listing must contain exactly owner B's single actor, not owner A's (Requirement 8.1)"
    );

    // A third owner that holds no actors at all must get an empty listing,
    // not an error and not someone else's actors.
    let owner_c = app.runtime.ids.next_id();
    create_owner(&app.pool, owner_c, now)
        .await
        .expect("creating owner C fixture must succeed");
    let owner_c_listing = app
        .actor
        .directory()
        .list_actors_for_owner(owner_c)
        .await
        .expect("listing an actor-less owner must succeed, not error");
    assert!(
        owner_c_listing.is_empty(),
        "an owner with no actors must get an empty listing (Requirement 3.3's owner-scoped-only guarantee)"
    );

    app.cleanup().await;
}

/// Requirements 3.1, 3.2, 8.2, 8.4: `resolve_actor_by_handle` and
/// `actor_public_key` return types structurally carry no owner field at all
/// (proven via exhaustive destructuring, the same technique
/// `src/actor/model.rs`'s own unit tests use -- this integration test
/// additionally exercises it against real rows fetched over the wire, not
/// just hand-built values), and actors created under two distinct owners
/// resolved via these protocol-layer paths have no owner-comparable field
/// to distinguish them by.
#[tokio::test]
async fn protocol_layer_resolution_exposes_no_owner_information() {
    let app = spawn_test_app().await;

    let owner_a = app.runtime.ids.next_id();
    let owner_b = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    create_owner(&app.pool, owner_a, now)
        .await
        .expect("creating owner A fixture must succeed");
    create_owner(&app.pool, owner_b, now)
        .await
        .expect("creating owner B fixture must succeed");

    let handle_a = Handle::new("boundary_protocol_owner_a").expect("valid handle");
    let handle_b = Handle::new("boundary_protocol_owner_b").expect("valid handle");

    let actor_a = app
        .actor
        .actor_service()
        .create_actor(NewActor {
            owner_id: owner_a,
            handle: handle_a.clone(),
            actor_type: ActorType::Person,
            display_name: "Protocol Owner A Actor".to_string(),
            summary: "exercises task 7.3's protocol-layer non-exposure scenario".to_string(),
        })
        .await
        .expect("creating owner A's actor must succeed");
    let actor_b = app
        .actor
        .actor_service()
        .create_actor(NewActor {
            owner_id: owner_b,
            handle: handle_b.clone(),
            actor_type: ActorType::Service,
            display_name: "Protocol Owner B Actor".to_string(),
            summary: "exercises task 7.3's protocol-layer non-exposure scenario".to_string(),
        })
        .await
        .expect("creating owner B's actor must succeed");

    let resolved_a = app
        .actor
        .directory()
        .resolve_actor_by_handle(&handle_a)
        .await
        .expect("resolving owner A's actor by handle must succeed")
        .expect("owner A's actor must resolve");
    let resolved_b = app
        .actor
        .directory()
        .resolve_actor_by_handle(&handle_b)
        .await
        .expect("resolving owner B's actor by handle must succeed")
        .expect("owner B's actor must resolve");

    // Exhaustive destructuring (no `..`): this is a compile-time proof that
    // `ResolvedActor` has exactly these fields. If an `owner_id` (or any
    // other) field were ever added to `ResolvedActor`, this would fail to
    // compile (E0027) rather than silently continue to pass.
    let ResolvedActor {
        id: resolved_a_id,
        handle: resolved_a_handle,
        actor_type: resolved_a_type,
        display_name: resolved_a_display_name,
        summary: resolved_a_summary,
        state: resolved_a_state,
    } = resolved_a;
    let ResolvedActor {
        id: resolved_b_id,
        handle: resolved_b_handle,
        actor_type: resolved_b_type,
        display_name: resolved_b_display_name,
        summary: resolved_b_summary,
        state: resolved_b_state,
    } = resolved_b;
    assert_eq!(resolved_a_id, actor_a.id);
    assert_eq!(resolved_b_id, actor_b.id);
    assert_eq!(resolved_a_handle, handle_a);
    assert_eq!(resolved_b_handle, handle_b);
    assert_eq!(resolved_a_type, ActorType::Person);
    assert_eq!(resolved_b_type, ActorType::Service);
    assert_eq!(resolved_a_display_name, "Protocol Owner A Actor");
    assert_eq!(resolved_b_display_name, "Protocol Owner B Actor");
    assert_eq!(resolved_a_summary, actor_a.summary);
    assert_eq!(resolved_b_summary, actor_b.summary);
    assert_eq!(resolved_a_state, actor_a.state);
    assert_eq!(resolved_b_state, actor_b.state);
    // The two owners' resolved actors are distinguishable only by their own
    // actor-scoped fields (id/handle/etc.) -- there is no owner-comparable
    // field on `ResolvedActor` to check at all, which is the point: nothing
    // above compared an owner identifier, because none exists on this type.

    let public_key_a = app
        .actor
        .directory()
        .actor_public_key(actor_a.id)
        .await
        .expect("looking up owner A's actor's public key must succeed")
        .expect("owner A's actor must have an active public key");
    let public_key_b = app
        .actor
        .directory()
        .actor_public_key(actor_b.id)
        .await
        .expect("looking up owner B's actor's public key must succeed")
        .expect("owner B's actor must have an active public key");

    // Same exhaustive-destructure proof for `ActorPublicKey`.
    let ActorPublicKey {
        actor_id: public_key_a_actor_id,
        key_id: _,
        public_key_pem: public_key_a_pem,
    } = public_key_a;
    let ActorPublicKey {
        actor_id: public_key_b_actor_id,
        key_id: _,
        public_key_pem: public_key_b_pem,
    } = public_key_b;
    assert_eq!(public_key_a_actor_id, actor_a.id);
    assert_eq!(public_key_b_actor_id, actor_b.id);
    assert_ne!(
        public_key_a_pem, public_key_b_pem,
        "two independently generated actors must have distinct public keys"
    );

    // `ActorSummary` (the management-layer type) is also structurally
    // owner-field-free, even though it is only ever reachable via an
    // owner-scoped query (Requirement 3.3's "対応を取得する操作を...限定" is
    // enforced by which *method* takes an `owner_id`, not by the returned
    // type itself carrying one).
    let owner_a_summaries = app
        .actor
        .directory()
        .list_actors_for_owner(owner_a)
        .await
        .expect("listing owner A's actors must succeed");
    let summary_a = owner_a_summaries
        .into_iter()
        .find(|summary| summary.id == actor_a.id)
        .expect("owner A's actor must appear in owner A's own listing");
    let ActorSummary {
        id: summary_a_id,
        handle: summary_a_handle,
        actor_type: summary_a_type,
        display_name: summary_a_display_name,
        state: summary_a_state,
    } = summary_a;
    assert_eq!(summary_a_id, actor_a.id);
    assert_eq!(summary_a_handle, handle_a);
    assert_eq!(summary_a_type, ActorType::Person);
    assert_eq!(summary_a_display_name, "Protocol Owner A Actor");
    assert_eq!(summary_a_state, actor_a.state);

    app.cleanup().await;
}

/// Requirements 4.4, 4.5: private key material never appears in plaintext
/// in (a) the protocol-facing reference types (`ResolvedActor`,
/// `ActorPublicKey`, `ActorSummary`), (b) the DB's persisted
/// `sealed_private_key` bytes, or a `SigningKey`'s `Debug` output, as the
/// closest in-process proxy for "never printed to logs/diagnostics" that
/// this codebase's existing test infrastructure can exercise (no
/// log-capture harness exists to independently re-verify the logging path
/// itself; that protection is structural, via `Secret`'s masked `Debug`/
/// `Display`, see `src/config/secret.rs`).
#[tokio::test]
async fn private_key_material_never_appears_in_plaintext_outside_the_signing_key_supply_boundary() {
    let app = spawn_test_app().await;

    let owner_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    create_owner(&app.pool, owner_id, now)
        .await
        .expect("creating the owner fixture must succeed");

    let handle = Handle::new("boundary_secret_material_actor").expect("valid handle");
    let actor = app
        .actor
        .actor_service()
        .create_actor(NewActor {
            owner_id,
            handle: handle.clone(),
            actor_type: ActorType::Person,
            display_name: "Secret Material Test Actor".to_string(),
            summary: "exercises task 7.3's secret-material scenario".to_string(),
        })
        .await
        .expect("create_actor must succeed");

    // The one legitimate path to plaintext private key material: the
    // signing-key supply boundary (`SigningKeyProvider`).
    let signing_key = app
        .runtime
        .keys
        .signing_key(KeyRef(actor.id))
        .expect("the supply boundary must return the just-provisioned key");
    let plaintext_pem = signing_key.expose_pem_bytes().to_vec();
    assert!(
        plaintext_pem.starts_with(b"-----BEGIN"),
        "sanity check: the exposed key material must actually be a PEM block, \
         otherwise the comparisons below would be vacuous; got {} bytes starting {:?}",
        plaintext_pem.len(),
        String::from_utf8_lossy(&plaintext_pem[..plaintext_pem.len().min(16)])
    );

    // (c, closest in-process proxy) Debug-formatting the `SigningKey` itself
    // must never print the plaintext PEM (Requirement 4.4).
    let debug_output = format!("{signing_key:?}");
    assert!(
        !debug_output.contains("-----BEGIN"),
        "SigningKey's Debug output must not leak plaintext PEM material: {debug_output}"
    );

    // (b) The DB-persisted `sealed_private_key` bytes for this actor's
    // active key must not be the plaintext PEM, and must not contain it as
    // a substring -- a mechanical proof that the stored bytes are actually
    // sealed ciphertext, not merely a same-named field holding plaintext.
    let stored_active_keys: Vec<_> = load_all_active(&app.pool)
        .await
        .expect("loading active signing keys must succeed")
        .into_iter()
        .filter(|key| key.actor_id == actor.id)
        .collect();
    assert_eq!(
        stored_active_keys.len(),
        1,
        "the freshly created actor must have exactly one active signing key row"
    );
    let sealed_private_key = &stored_active_keys[0].sealed_private_key;

    assert_ne!(
        sealed_private_key, &plaintext_pem,
        "the DB-persisted sealed_private_key bytes must not equal the plaintext PEM verbatim \
         (Requirement 4.5)"
    );
    assert!(
        !contains_subslice(sealed_private_key, &plaintext_pem),
        "the DB-persisted sealed_private_key bytes must not contain the plaintext PEM as a \
         substring (Requirement 4.5)"
    );
    // Corroborate with the PEM's most distinctive marker line alone, in
    // case a hypothetical (buggy) sealing scheme happened to reorder or
    // partially pass through bytes such that the full-PEM substring check
    // above could pass despite still leaking the marker.
    assert!(
        !contains_subslice(sealed_private_key, b"-----BEGIN PRIVATE KEY-----")
            && !contains_subslice(sealed_private_key, b"-----BEGIN RSA PRIVATE KEY-----"),
        "the DB-persisted sealed_private_key bytes must not contain a PEM header marker \
         (Requirement 4.5)"
    );

    // (a) The protocol-facing reference types structurally have no
    // private-key field at all -- exhaustive destructuring proves this at
    // compile time for `ActorPublicKey` (already covered field-by-field in
    // `protocol_layer_resolution_exposes_no_owner_information` and
    // `src/actor/model.rs`'s own unit tests); here, additionally confirm
    // that neither `ActorPublicKey`'s nor `ResolvedActor`'s `Debug` output
    // (their only remaining possible leak surface, since neither has a
    // private-key-typed field to destructure in the first place) contains
    // the plaintext PEM, as a behavioral corroboration of that structural
    // fact against this test's real generated key material.
    let public_key = app
        .actor
        .directory()
        .actor_public_key(actor.id)
        .await
        .expect("looking up the active public key must succeed")
        .expect("the actor must have an active public key");
    let resolved = app
        .actor
        .directory()
        .resolve_actor_by_handle(&handle)
        .await
        .expect("resolving by handle must succeed")
        .expect("the actor must resolve by handle");
    let owner_listing = app
        .actor
        .directory()
        .list_actors_for_owner(owner_id)
        .await
        .expect("listing the owner's actors must succeed");

    let public_key_debug = format!("{public_key:?}");
    let resolved_debug = format!("{resolved:?}");
    let owner_listing_debug = format!("{owner_listing:?}");
    assert!(
        !contains_subslice(public_key_debug.as_bytes(), &plaintext_pem),
        "ActorPublicKey's Debug output must not contain the plaintext private key PEM"
    );
    assert!(
        !contains_subslice(resolved_debug.as_bytes(), &plaintext_pem),
        "ResolvedActor's Debug output must not contain the plaintext private key PEM"
    );
    assert!(
        !contains_subslice(owner_listing_debug.as_bytes(), &plaintext_pem),
        "ActorSummary's Debug output must not contain the plaintext private key PEM"
    );

    app.cleanup().await;
}

/// Returns whether `needle` occurs as a contiguous subslice of `haystack`.
/// A small local helper since `[u8]` has no built-in `contains` for
/// subslices (only single-element `contains`).
fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}
