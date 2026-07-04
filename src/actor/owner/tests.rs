//! Integration-style tests for `OwnerRepository` (Requirements 2.1, 2.4),
//! per task 2.1's observable completion condition: "オーナーを作成すると
//! 一意な識別子付きの行が永続化され、取得で同一オーナーが返る".
//!
//! Mirrors `src/db/tests.rs`'s/`src/migrate/tests.rs`'s established
//! convention of a sibling `tests.rs` module, but reuses
//! `crate::test_harness::spawn_test_app` (task 8.1, already available) for
//! an isolated, already-migrated schema and a deterministic
//! `RuntimeContext` rather than re-deriving schema isolation from scratch —
//! this is genuinely repository-level integration coverage (a real
//! Postgres round trip through the `owners` table), not a unit test, so
//! there is no reachability-skip preflight here: `spawn_test_app` itself
//! already panics with a clear message if the required local test database
//! is unavailable.

use super::{create_owner, find_owner};
use crate::domain::Id;
use crate::test_harness::spawn_test_app;

/// Requirements 2.1, 2.4: creating an owner persists a uniquely-identified
/// row (id minted via the injected `IdGenerator` boundary, `created_at` via
/// the injected `Clock` boundary), and retrieving it by that id returns the
/// same owner `create_owner` reported.
#[tokio::test]
async fn create_owner_persists_a_row_that_find_owner_returns_unchanged() {
    let app = spawn_test_app().await;

    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();

    let created = create_owner(&app.pool, id, now)
        .await
        .expect("create_owner must succeed for a fresh id");
    assert_eq!(created.id, id, "the stored owner must carry the id the caller minted");

    let found = find_owner(&app.pool, id)
        .await
        .expect("find_owner must succeed")
        .expect("the just-created owner must be found by its id");

    assert_eq!(
        found, created,
        "find_owner must return the same owner create_owner reported"
    );

    app.cleanup().await;
}

/// Two distinct `create_owner` calls (distinct ids from the injected
/// `IdGenerator`) must persist two distinct rows, each independently
/// retrievable — proving "一意な識別子付きの行" is actually per-owner, not a
/// single shared row.
#[tokio::test]
async fn create_owner_persists_distinct_rows_for_distinct_ids() {
    let app = spawn_test_app().await;

    let first_id = app.runtime.ids.next_id();
    let second_id = app.runtime.ids.next_id();
    assert_ne!(first_id, second_id, "IdGenerator must mint distinct ids");
    let now = app.runtime.clock.now();

    let first = create_owner(&app.pool, first_id, now)
        .await
        .expect("creating the first owner must succeed");
    let second = create_owner(&app.pool, second_id, now)
        .await
        .expect("creating the second owner must succeed");
    assert_ne!(first.id, second.id);

    let found_first = find_owner(&app.pool, first_id)
        .await
        .expect("find_owner must succeed")
        .expect("the first owner must be found");
    let found_second = find_owner(&app.pool, second_id)
        .await
        .expect("find_owner must succeed")
        .expect("the second owner must be found");

    assert_eq!(found_first, first);
    assert_eq!(found_second, second);
    assert_ne!(found_first, found_second);

    app.cleanup().await;
}

/// `find_owner` for an id nothing was ever created under returns `Ok(None)`,
/// not an error — "does this owner exist" is this operation's contract.
#[tokio::test]
async fn find_owner_returns_none_for_an_unknown_id() {
    let app = spawn_test_app().await;

    let unknown_id = Id::from_i64(i64::MAX - 1);
    let found = find_owner(&app.pool, unknown_id)
        .await
        .expect("find_owner must succeed even when nothing matches");
    assert!(found.is_none());

    app.cleanup().await;
}
