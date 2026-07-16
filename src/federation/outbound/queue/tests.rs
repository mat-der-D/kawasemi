//! Integration-style tests for `DeliveryQueue`/`DbDeliveryQueue`
//! (Requirements 11.1, 11.2, 11.3, 11.5), per task 3.3's observable
//! completion condition: "enqueue 後に claim_due で取得でき、reschedule で
//! 次回試行時刻が後ろ倒しされ、上限到達で failed に遷移する統合テストが
//! 通る".
//!
//! Mirrors `src/federation/inbound/dedup/tests.rs`'s established
//! convention: `spawn_test_app` for an isolated, already-migrated schema
//! (so this exercises the real `delivery_jobs` table, not a stand-in), and
//! fixed `OffsetDateTime` values (via `time::macros::datetime`) passed
//! explicitly as `claim_due`'s `now`/`reschedule`'s `next_attempt_at`,
//! never wall-clock time — this queue takes every timestamp as an explicit
//! parameter (design.md's signatures), so there is no injected `Clock` to
//! substitute here the way `dedup/tests.rs` substitutes `FixedClock`.

use serde_json::json;
use time::Duration;
use time::macros::datetime;

use super::*;
use crate::test_harness::spawn_test_app;

const SENDER_ACTOR_ID: i64 = 1;

fn base_time() -> time::OffsetDateTime {
    datetime!(2026-07-16 00:00:00 UTC)
}

fn sample_activity(activity_id: &str) -> serde_json::Value {
    json!({ "id": activity_id, "type": "Create" })
}

fn new_job(
    id: i64,
    target_inbox: &str,
    activity_id: &str,
    next_attempt_at: time::OffsetDateTime,
) -> NewDeliveryJob {
    NewDeliveryJob {
        id: Id::from_i64(id),
        sender_actor_id: Id::from_i64(SENDER_ACTOR_ID),
        target_inbox: target_inbox.to_string(),
        activity: sample_activity(activity_id),
        next_attempt_at,
    }
}

async fn read_row(pool: &sqlx::PgPool, job_id: i64) -> DeliveryJob {
    let row: DeliveryJobRow = sqlx::query_as(
        "SELECT id, sender_actor_id, target_inbox, activity, status, attempts, \
                next_attempt_at, created_at, updated_at \
         FROM delivery_jobs WHERE id = $1",
    )
    .bind(job_id)
    .fetch_one(pool)
    .await
    .expect("the job must exist in delivery_jobs");
    row_to_delivery_job(row)
}

// --- 1: enqueue persists a job; claim_due(now >= next_attempt_at) returns
// it, and the row's status becomes 'in_progress' in the DB. ---

#[tokio::test]
async fn enqueue_then_claim_due_returns_the_job_and_marks_it_in_progress_in_the_db() {
    let app = spawn_test_app().await;
    let queue = DbDeliveryQueue::new(app.pool.clone());
    let now = base_time();

    queue
        .enqueue(new_job(
            1,
            "https://remote.example/inbox",
            "https://remote.example/activities/1",
            now,
        ))
        .await
        .expect("enqueue must succeed");

    let claimed = queue
        .claim_due(10, now)
        .await
        .expect("claim_due must succeed");
    assert_eq!(claimed.len(), 1, "the single due job must be claimed");
    assert_eq!(claimed[0].id, Id::from_i64(1));
    assert_eq!(claimed[0].status, DeliveryJobStatus::InProgress);

    let row = read_row(&app.pool, 1).await;
    assert_eq!(
        row.status,
        DeliveryJobStatus::InProgress,
        "the row's status in the DB (not just the returned Vec) must be 'in_progress'"
    );

    app.cleanup().await;
}

// --- 2: a job whose next_attempt_at is in the future is NOT returned. ---

#[tokio::test]
async fn claim_due_does_not_return_a_job_whose_next_attempt_at_is_in_the_future() {
    let app = spawn_test_app().await;
    let queue = DbDeliveryQueue::new(app.pool.clone());
    let now = base_time();
    let future = now + Duration::hours(1);

    queue
        .enqueue(new_job(
            2,
            "https://remote.example/inbox",
            "https://remote.example/activities/2",
            future,
        ))
        .await
        .expect("enqueue must succeed");

    let claimed = queue
        .claim_due(10, now)
        .await
        .expect("claim_due must succeed");
    assert!(
        claimed.is_empty(),
        "a job due only in the future must not be claimed by a now before it"
    );

    let row = read_row(&app.pool, 2).await;
    assert_eq!(
        row.status,
        DeliveryJobStatus::Pending,
        "an unclaimed job's status must remain 'pending'"
    );

    app.cleanup().await;
}

// --- 3: claim_due respects limit. ---

#[tokio::test]
async fn claim_due_respects_limit_when_more_jobs_are_due_than_the_limit() {
    let app = spawn_test_app().await;
    let queue = DbDeliveryQueue::new(app.pool.clone());
    let now = base_time();

    for i in 10..15 {
        queue
            .enqueue(new_job(
                i,
                "https://remote.example/inbox",
                &format!("https://remote.example/activities/{i}"),
                now,
            ))
            .await
            .expect("enqueue must succeed");
    }

    let claimed = queue
        .claim_due(2, now)
        .await
        .expect("claim_due must succeed");
    assert_eq!(
        claimed.len(),
        2,
        "claim_due must return at most `limit` jobs even when more are due"
    );

    app.cleanup().await;
}

// --- 4: mark_done transitions a job to 'done'. ---

#[tokio::test]
async fn mark_done_transitions_the_job_to_done() {
    let app = spawn_test_app().await;
    let queue = DbDeliveryQueue::new(app.pool.clone());
    let now = base_time();

    queue
        .enqueue(new_job(
            20,
            "https://remote.example/inbox",
            "https://remote.example/activities/20",
            now,
        ))
        .await
        .expect("enqueue must succeed");
    queue
        .claim_due(10, now)
        .await
        .expect("claim_due must succeed");

    queue
        .mark_done(Id::from_i64(20))
        .await
        .expect("mark_done must succeed");

    let row = read_row(&app.pool, 20).await;
    assert_eq!(row.status, DeliveryJobStatus::Done);

    app.cleanup().await;
}

// --- 5: reschedule updates next_attempt_at (pushed later) and attempts,
// and the job becomes claimable again only once now reaches the new
// next_attempt_at. ---

#[tokio::test]
async fn reschedule_pushes_next_attempt_at_later_and_increments_attempts_and_is_reclaimable_only_after_it()
 {
    let app = spawn_test_app().await;
    let queue = DbDeliveryQueue::new(app.pool.clone());
    let now = base_time();

    queue
        .enqueue(new_job(
            30,
            "https://remote.example/inbox",
            "https://remote.example/activities/30",
            now,
        ))
        .await
        .expect("enqueue must succeed");
    queue
        .claim_due(10, now)
        .await
        .expect("claim_due must succeed");

    let rescheduled_next_attempt_at = now + Duration::minutes(5);
    queue
        .reschedule(Id::from_i64(30), rescheduled_next_attempt_at, 1)
        .await
        .expect("reschedule must succeed");

    let row = read_row(&app.pool, 30).await;
    assert_eq!(row.status, DeliveryJobStatus::Pending);
    assert_eq!(row.attempts, 1);
    assert_eq!(row.next_attempt_at, rescheduled_next_attempt_at);

    // Not claimable before the new next_attempt_at.
    let too_early = queue
        .claim_due(10, now + Duration::minutes(1))
        .await
        .expect("claim_due must succeed");
    assert!(
        too_early.is_empty(),
        "a rescheduled job must not be claimable before its new next_attempt_at"
    );

    // Claimable once now reaches the new next_attempt_at.
    let claimed = queue
        .claim_due(10, rescheduled_next_attempt_at)
        .await
        .expect("claim_due must succeed");
    assert_eq!(
        claimed.len(),
        1,
        "the rescheduled job must be claimable once now reaches its new next_attempt_at"
    );
    assert_eq!(claimed[0].attempts, 1);

    app.cleanup().await;
}

// --- 6: exponential backoff — increasing attempts produce increasing
// (non-decreasing) durations; pure function, no DB. ---

#[test]
fn backoff_delay_grows_with_increasing_attempts_and_saturates_at_the_documented_max() {
    let mut previous = backoff_delay(0);
    assert_eq!(
        previous, DEFAULT_DELIVERY_BASE_DELAY,
        "attempts == 0 must yield exactly the documented base delay"
    );

    for attempts in 1..8 {
        let delay = backoff_delay(attempts);
        assert!(
            delay >= previous,
            "backoff_delay must be non-decreasing as attempts grow: attempts={attempts} \
             produced {delay:?}, previous was {previous:?}"
        );
        previous = delay;
    }
    assert!(
        previous > DEFAULT_DELIVERY_BASE_DELAY,
        "backoff_delay must actually grow past the base delay for larger attempts, not stay flat"
    );

    // Very large attempts must saturate at the documented max, never
    // overflow/panic.
    let saturated = backoff_delay(1_000);
    assert_eq!(saturated, DEFAULT_DELIVERY_MAX_DELAY);
    let saturated_negative_proof = backoff_delay(i32::MAX);
    assert_eq!(saturated_negative_proof, DEFAULT_DELIVERY_MAX_DELAY);
}

#[test]
fn backoff_delay_clamps_a_negative_attempts_to_the_base_delay() {
    assert_eq!(backoff_delay(-1), DEFAULT_DELIVERY_BASE_DELAY);
    assert_eq!(backoff_delay(i32::MIN), DEFAULT_DELIVERY_BASE_DELAY);
}

// --- 7: mark_failed transitions a job to 'failed', as the terminal step of
// a simulated retry lifecycle: enqueue -> claim_due -> reschedule (several
// times, growing delay via backoff_delay) -> mark_failed. ---

#[tokio::test]
async fn simulated_retry_lifecycle_ends_in_failed_after_reschedules_exhaust_via_mark_failed() {
    let app = spawn_test_app().await;
    let queue = DbDeliveryQueue::new(app.pool.clone());
    let now = base_time();
    let job_id = Id::from_i64(40);

    queue
        .enqueue(new_job(
            40,
            "https://remote.example/inbox",
            "https://remote.example/activities/40",
            now,
        ))
        .await
        .expect("enqueue must succeed");

    let mut current_time = now;
    let mut attempts = 0;
    // Simulate what the (later, task 4.3) DeliveryWorker will do: claim,
    // fail, reschedule with a growing delay, repeated up to
    // DEFAULT_MAX_DELIVERY_ATTEMPTS, then give up via mark_failed.
    while attempts < DEFAULT_MAX_DELIVERY_ATTEMPTS {
        let claimed = queue
            .claim_due(10, current_time)
            .await
            .expect("claim_due must succeed");
        assert_eq!(
            claimed.len(),
            1,
            "the job must be claimable at the start of each retry round"
        );

        attempts += 1;
        let delay = backoff_delay(attempts);
        current_time += delay;
        queue
            .reschedule(job_id, current_time, attempts)
            .await
            .expect("reschedule must succeed");
    }

    let after_retries = read_row(&app.pool, 40).await;
    assert_eq!(after_retries.attempts, DEFAULT_MAX_DELIVERY_ATTEMPTS);
    assert_eq!(after_retries.status, DeliveryJobStatus::Pending);

    // Retry budget exhausted: the (simulated) worker gives up permanently.
    queue
        .mark_failed(job_id)
        .await
        .expect("mark_failed must succeed");

    let row = read_row(&app.pool, 40).await;
    assert_eq!(
        row.status,
        DeliveryJobStatus::Failed,
        "after exhausting the retry budget, the job's final status must be 'failed'"
    );
    assert_eq!(
        row.attempts, DEFAULT_MAX_DELIVERY_ATTEMPTS,
        "mark_failed must not itself alter the attempts count"
    );

    app.cleanup().await;
}

// --- 8: dedup index sanity — enqueueing two jobs with the same
// (target_inbox, activity.id) surfaces a clean AppError (409 Conflict),
// never a panic. ---

#[tokio::test]
async fn enqueue_on_dedup_conflict_returns_a_conflict_app_error_not_a_panic() {
    let app = spawn_test_app().await;
    let queue = DbDeliveryQueue::new(app.pool.clone());
    let now = base_time();
    let target_inbox = "https://remote.example/shared-inbox";
    let activity_id = "https://remote.example/activities/dup";

    queue
        .enqueue(new_job(50, target_inbox, activity_id, now))
        .await
        .expect("the first enqueue for this (target_inbox, activity id) pair must succeed");

    // A second, distinct job id, but the same (target_inbox, activity.id)
    // pair: hits delivery_jobs_dedup_idx.
    let conflict = queue
        .enqueue(new_job(51, target_inbox, activity_id, now))
        .await;
    assert!(
        conflict.is_err(),
        "enqueueing a duplicate (target_inbox, activity.id) pair must return an error, not succeed silently"
    );
    let err = conflict.unwrap_err();
    assert_eq!(
        err.status,
        axum::http::StatusCode::CONFLICT,
        "a dedup-index conflict must surface as a caller-facing 409, not a generic 5xx"
    );

    // The original row is untouched and there is still exactly one row for
    // this activity id.
    let count: (i64,) = sqlx::query_as(
        "SELECT count(*) FROM delivery_jobs WHERE target_inbox = $1 AND activity->>'id' = $2",
    )
    .bind(target_inbox)
    .bind(activity_id)
    .fetch_one(&app.pool)
    .await
    .expect("counting delivery_jobs must succeed");
    assert_eq!(
        count.0, 1,
        "the conflicting insert must not have created a second row"
    );

    app.cleanup().await;
}

#[test]
fn default_max_delivery_attempts_is_documented_as_ten() {
    assert_eq!(DEFAULT_MAX_DELIVERY_ATTEMPTS, 10);
}
