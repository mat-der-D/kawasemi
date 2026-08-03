//! Integration-style, DB-backed tests for `IdempotencyStore` (Requirements
//! 5.1, 5.2), per task 2.3's observable completion condition: "同一冪等キー
//! の再送が記録済み status_id を返す（リポジトリ単体テストがグリーン）".
//!
//! Mirrors `poll_repository/tests.rs`'s established convention: reuses
//! `crate::test_harness::spawn_test_app`, and inserts a real target
//! `statuses` row via `status_repository::insert_status`
//! (`status_idempotency_keys.status_id` carries a real FK to `statuses(id)`,
//! so `bind` requires a genuine, already-persisted status row).

use crate::domain::{Id, Visibility};
use crate::statuses::model::Status;
use crate::statuses::status_repository::insert_status;
use crate::test_harness::{TestApp, spawn_test_app};

use super::{IdempotencyLookup, bind, check_or_reserve};

/// A small, deliberate duplicate of `status_repository/tests.rs`'s own
/// `sample_status` helper — same rationale as every sibling
/// `poll_repository`/`interaction_repository` test module's identical
/// helper (this task's Boundary forbids modifying `status_repository.rs`,
/// including loosening its private items' visibility, just to share a test
/// helper).
fn sample_status(app: &TestApp, actor_id: Id) -> Status {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    Status {
        id,
        actor_id,
        uri: format!("https://example.test/statuses/{}", id.as_i64()),
        url: Some(format!("https://example.test/@actor/{}", id.as_i64())),
        content: "hello, idempotently".to_string(),
        visibility: Visibility::Public,
        sensitive: false,
        spoiler_text: String::new(),
        in_reply_to_id: None,
        in_reply_to_account_id: None,
        reblog_of_id: None,
        poll_id: None,
        language: Some("en".to_string()),
        reblogs_count: 0,
        favourites_count: 0,
        replies_count: 0,
        local: true,
        created_at: now,
        edited_at: None,
    }
}

async fn insert_target_status(app: &TestApp, actor_id: Id) -> Status {
    let status = sample_status(app, actor_id);
    insert_status(&app.pool, &status)
        .await
        .expect("insert_status must succeed for a fresh id/uri");
    status
}

/// Requirement 5.1: a never-before-seen `(actor_id, key)` reports `Reserved`
/// (no existing binding), then `bind` records it.
#[tokio::test]
async fn check_or_reserve_reports_reserved_for_a_fresh_key() {
    let app = spawn_test_app().await;
    let actor_id = app.runtime.ids.next_id();

    let lookup = check_or_reserve(&app.pool, actor_id, "client-key-1")
        .await
        .expect("check_or_reserve must succeed");
    assert_eq!(lookup, IdempotencyLookup::Reserved);

    app.cleanup().await;
}

/// Requirement 5.2: after `bind`, a subsequent `check_or_reserve` for the
/// same `(actor_id, key)` resolves to the already-recorded `status_id`
/// instead of `Reserved` — the resend-returns-recorded-status contract this
/// task's own observable-completion text names directly.
#[tokio::test]
async fn check_or_reserve_resolves_to_the_bound_status_after_bind() {
    let app = spawn_test_app().await;
    let actor_id = app.runtime.ids.next_id();
    let status = insert_target_status(&app, actor_id).await;

    bind(
        &app.pool,
        actor_id,
        "client-key-2",
        status.id,
        app.runtime.clock.now(),
    )
    .await
    .expect("bind must succeed");

    let lookup = check_or_reserve(&app.pool, actor_id, "client-key-2")
        .await
        .expect("check_or_reserve must succeed");
    assert_eq!(lookup, IdempotencyLookup::Existing(status.id));

    app.cleanup().await;
}

/// A resend simulated end-to-end: `check_or_reserve` -> `Reserved` -> create
/// -> `bind` -> a second `check_or_reserve` for the identical key resolves
/// to the same `status_id` every time it is checked again (idempotent read,
/// not consumed by reading it).
#[tokio::test]
async fn repeated_check_or_reserve_after_bind_is_stable() {
    let app = spawn_test_app().await;
    let actor_id = app.runtime.ids.next_id();
    let status = insert_target_status(&app, actor_id).await;
    let key = "client-key-3";

    assert_eq!(
        check_or_reserve(&app.pool, actor_id, key).await.unwrap(),
        IdempotencyLookup::Reserved
    );

    bind(&app.pool, actor_id, key, status.id, app.runtime.clock.now())
        .await
        .expect("bind must succeed");

    for _ in 0..3 {
        assert_eq!(
            check_or_reserve(&app.pool, actor_id, key).await.unwrap(),
            IdempotencyLookup::Existing(status.id),
            "repeated resends must keep resolving to the same recorded status_id"
        );
    }

    app.cleanup().await;
}

/// The idempotency ledger is scoped per-actor: the same literal key string
/// used by two different actors does not collide — each actor's `bind`
/// establishes its own independent binding.
#[tokio::test]
async fn idempotency_key_is_scoped_per_actor() {
    let app = spawn_test_app().await;
    let actor_a = app.runtime.ids.next_id();
    let actor_b = app.runtime.ids.next_id();
    let status_a = insert_target_status(&app, actor_a).await;
    let status_b = insert_target_status(&app, actor_b).await;
    let shared_key = "shared-literal-key";

    bind(
        &app.pool,
        actor_a,
        shared_key,
        status_a.id,
        app.runtime.clock.now(),
    )
    .await
    .expect("actor_a's bind must succeed");
    bind(
        &app.pool,
        actor_b,
        shared_key,
        status_b.id,
        app.runtime.clock.now(),
    )
    .await
    .expect("actor_b's bind must succeed independently");

    assert_eq!(
        check_or_reserve(&app.pool, actor_a, shared_key)
            .await
            .unwrap(),
        IdempotencyLookup::Existing(status_a.id)
    );
    assert_eq!(
        check_or_reserve(&app.pool, actor_b, shared_key)
            .await
            .unwrap(),
        IdempotencyLookup::Existing(status_b.id)
    );

    app.cleanup().await;
}

/// A losing racer's `bind` for an already-bound `(actor_id, key)` is a
/// silent no-op (`Ok(())`), and the ledger keeps resolving to the *first*
/// bound `status_id`, never being overwritten by the later call — this
/// module's doc comment ("Race between two concurrent first-uses") documents
/// this as the mechanism `ON CONFLICT ... DO NOTHING` provides.
#[tokio::test]
async fn bind_does_not_overwrite_an_existing_binding() {
    let app = spawn_test_app().await;
    let actor_id = app.runtime.ids.next_id();
    let first_status = insert_target_status(&app, actor_id).await;
    let second_status = insert_target_status(&app, actor_id).await;
    let key = "client-key-race";

    bind(
        &app.pool,
        actor_id,
        key,
        first_status.id,
        app.runtime.clock.now(),
    )
    .await
    .expect("first bind must succeed");

    // A second bind for the same (actor_id, key), pointing at a *different*
    // status_id, must not error and must not overwrite the first binding.
    bind(
        &app.pool,
        actor_id,
        key,
        second_status.id,
        app.runtime.clock.now(),
    )
    .await
    .expect("a losing racer's bind must succeed silently, not error");

    assert_eq!(
        check_or_reserve(&app.pool, actor_id, key).await.unwrap(),
        IdempotencyLookup::Existing(first_status.id),
        "the ledger must keep resolving to the first-bound status_id"
    );

    app.cleanup().await;
}

/// Two distinct keys used by the same actor are independent bindings.
#[tokio::test]
async fn distinct_keys_for_the_same_actor_are_independent() {
    let app = spawn_test_app().await;
    let actor_id = app.runtime.ids.next_id();
    let status_a = insert_target_status(&app, actor_id).await;
    let status_b = insert_target_status(&app, actor_id).await;

    bind(
        &app.pool,
        actor_id,
        "key-a",
        status_a.id,
        app.runtime.clock.now(),
    )
    .await
    .expect("bind for key-a must succeed");
    bind(
        &app.pool,
        actor_id,
        "key-b",
        status_b.id,
        app.runtime.clock.now(),
    )
    .await
    .expect("bind for key-b must succeed");

    assert_eq!(
        check_or_reserve(&app.pool, actor_id, "key-a")
            .await
            .unwrap(),
        IdempotencyLookup::Existing(status_a.id)
    );
    assert_eq!(
        check_or_reserve(&app.pool, actor_id, "key-b")
            .await
            .unwrap(),
        IdempotencyLookup::Existing(status_b.id)
    );

    app.cleanup().await;
}
