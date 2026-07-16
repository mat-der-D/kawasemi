//! Integration-style tests for `ReceivedActivityStore`/
//! `DbReceivedActivityStore` (Requirement 7.4), per task 3.1's observable
//! completion condition: "同一 Activity id の二度目が既知と判定され、保持
//! 日数を超えた行がプルーニング実行後に削除される統合テストが通る".
//!
//! Mirrors `src/federation/signatures/key_resolver/tests.rs`'s established
//! convention: `spawn_test_app` for an isolated, already-migrated schema (so
//! this exercises the real `received_activities` table, not a stand-in),
//! and a fixed, per-`DbReceivedActivityStore` `FixedClock` (`FixedClock`
//! itself cannot be advanced mid-instance) so "time has passed" between
//! calls is simulated by building a second store sharing the same pool but
//! a later `FixedClock`, never by depending on wall-clock time.

use std::sync::Arc;

use time::Duration;
use time::macros::datetime;

use super::*;
use crate::runtime::FixedClock;
use crate::test_harness::spawn_test_app;

const ACTIVITY_ID: &str = "https://remote.example/activities/1";
const OTHER_ACTIVITY_ID: &str = "https://remote.example/activities/2";

fn fixed_clock_at(offset_seconds: i64) -> Arc<dyn Clock> {
    let base = datetime!(2026-07-16 00:00:00 UTC);
    Arc::new(FixedClock::new(base + Duration::seconds(offset_seconds)))
}

// --- 1 & 2: first record_if_new is new (true), second for the same id is known (false) ---

#[tokio::test]
async fn record_if_new_is_true_first_time_and_false_on_the_same_id_again() {
    let app = spawn_test_app().await;
    let store = DbReceivedActivityStore::new(
        app.pool.clone(),
        fixed_clock_at(0),
        DEFAULT_RECEIVED_ACTIVITY_RETENTION,
    );

    let first = store
        .record_if_new(ACTIVITY_ID)
        .await
        .expect("recording a brand-new activity id must succeed");
    assert!(first, "the first sighting of an activity id must be new");

    let second = store
        .record_if_new(ACTIVITY_ID)
        .await
        .expect("recording the same activity id again must still succeed (no error)");
    assert!(
        !second,
        "a second delivery of the same activity id must be reported as already known, \
         so the caller does not dispatch business-logic twice (Requirement 7.4)"
    );

    // A different activity id is unaffected: still reported as new.
    let unrelated = store
        .record_if_new(OTHER_ACTIVITY_ID)
        .await
        .expect("recording a different activity id must succeed");
    assert!(
        unrelated,
        "a different activity id must be treated as new regardless of an unrelated id's history"
    );

    app.cleanup().await;
}

#[tokio::test]
async fn record_if_new_persists_a_row_in_received_activities() {
    let app = spawn_test_app().await;
    let store = DbReceivedActivityStore::new(
        app.pool.clone(),
        fixed_clock_at(0),
        DEFAULT_RECEIVED_ACTIVITY_RETENTION,
    );

    store
        .record_if_new(ACTIVITY_ID)
        .await
        .expect("recording must succeed");

    let row: (String,) =
        sqlx::query_as("SELECT activity_id FROM received_activities WHERE activity_id = $1")
            .bind(ACTIVITY_ID)
            .fetch_one(&app.pool)
            .await
            .expect("the recorded activity id must be persisted in received_activities");
    assert_eq!(row.0, ACTIVITY_ID);

    app.cleanup().await;
}

// --- 3: pruning deletes rows older than retention, keeps rows within retention ---

#[tokio::test]
async fn prune_expired_deletes_rows_older_than_retention_and_keeps_rows_within_it() {
    let app = spawn_test_app().await;
    let retention = Duration::days(14);

    // Recorded "now" (t = 0).
    let store_at_t0 = DbReceivedActivityStore::new(app.pool.clone(), fixed_clock_at(0), retention);
    store_at_t0
        .record_if_new(ACTIVITY_ID)
        .await
        .expect("recording the old activity id must succeed");

    // Recorded just before the retention boundary, from a clock offset by
    // (retention - 1 hour) relative to the pruning store below, so this row
    // must survive pruning ("boundary: within retention is not pruned").
    let within_retention_offset = retention.whole_seconds() - Duration::hours(1).whole_seconds();
    let store_within_retention = DbReceivedActivityStore::new(
        app.pool.clone(),
        fixed_clock_at(within_retention_offset),
        retention,
    );
    store_within_retention
        .record_if_new(OTHER_ACTIVITY_ID)
        .await
        .expect("recording the recent activity id must succeed");

    // Prune from a clock strictly after `t0 + retention`, so ACTIVITY_ID
    // (recorded at t0) is now older than retention, while OTHER_ACTIVITY_ID
    // (recorded at `retention - 1h`) is still within retention relative to
    // this pruning clock's own "now".
    let prune_offset = retention.whole_seconds() + Duration::hours(1).whole_seconds();
    let pruning_store =
        DbReceivedActivityStore::new(app.pool.clone(), fixed_clock_at(prune_offset), retention);

    let deleted = pruning_store
        .prune_expired()
        .await
        .expect("pruning must succeed");
    assert_eq!(
        deleted, 1,
        "exactly the one row older than the retention window must be deleted"
    );

    let old_still_present: Option<(String,)> =
        sqlx::query_as("SELECT activity_id FROM received_activities WHERE activity_id = $1")
            .bind(ACTIVITY_ID)
            .fetch_optional(&app.pool)
            .await
            .expect("querying received_activities must succeed");
    assert!(
        old_still_present.is_none(),
        "a row older than the retention window must be gone after pruning"
    );

    let recent_still_present: Option<(String,)> =
        sqlx::query_as("SELECT activity_id FROM received_activities WHERE activity_id = $1")
            .bind(OTHER_ACTIVITY_ID)
            .fetch_optional(&app.pool)
            .await
            .expect("querying received_activities must succeed");
    assert!(
        recent_still_present.is_some(),
        "a row within the retention window must NOT be pruned"
    );

    app.cleanup().await;
}

#[test]
fn default_received_activity_retention_is_fourteen_days() {
    assert_eq!(DEFAULT_RECEIVED_ACTIVITY_RETENTION, Duration::days(14));
}
