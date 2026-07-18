//! Integration tests for `ProcessingJobQueue` (Requirements 4.1, 4.2, 4.4,
//! 4.5, 4.6), per task 3.2's observable completion condition: "2 つのワー
//! カーが同一ジョブを同時取得しないこと、バックオフで再試行されること、上
//! 限到達でジョブが失敗化すること、ロック期限切れのジョブが reclaim され
//! て試行回数が加算されることを統合テストで確認できる".
//!
//! Mirrors `src/media/media_repository/tests.rs`'s established convention:
//! reuses `crate::test_harness::spawn_test_app` for an isolated,
//! already-migrated schema and a deterministic `RuntimeContext`, and inserts
//! a real owner + local actor + `media` row first (`media_processing_jobs.
//! media_id REFERENCES media(id)` is a real FK, unlike `media.actor_id`'s
//! logical-only reference), via `media_repository::insert_media` (task 3.1,
//! already reviewed, not modified by this task).

use time::Duration;

use super::{JobOutcome, backoff_delay, claim_due, complete, enqueue, fail_or_retry};
use crate::actor::model::{ActorState, ActorType, Handle, LocalActor};
use crate::actor::owner::create_owner;
use crate::actor::repository::insert_actor;
use crate::domain::Id;
use crate::media::media_repository::insert_media;
use crate::media::model::{Focus, JobState, Media, MediaState, MediaType};
use crate::test_harness::{TestApp, spawn_test_app};

/// Creates a real owner + local actor row, returning the actor's `Id` (same
/// helper shape as `media_repository/tests.rs::create_test_actor`).
async fn create_test_actor(app: &TestApp, handle: &str) -> Id {
    let now = app.runtime.clock.now();
    let owner_id = app.runtime.ids.next_id();
    create_owner(&app.pool, owner_id, now)
        .await
        .expect("creating the owner must succeed");

    let actor_id = app.runtime.ids.next_id();
    let actor = LocalActor {
        id: actor_id,
        owner_id,
        handle: Handle::new(handle).expect("test handle must be valid"),
        actor_type: ActorType::Person,
        display_name: "Test Actor".to_string(),
        summary: "a test actor".to_string(),
        state: ActorState::Active,
        created_at: now,
        updated_at: now,
    };
    let mut tx = app
        .pool
        .begin()
        .await
        .expect("opening a transaction must succeed");
    insert_actor(&mut tx, &actor)
        .await
        .expect("insert_actor must succeed");
    tx.commit().await.expect("committing must succeed");

    actor_id
}

/// Creates a real `media` row (a job's `media_id` FK target), returning its
/// `Id`. The exact `Media` field values (beyond `id`/`actor_id`) are
/// irrelevant to this module's own behavior — only the FK target's
/// existence matters here.
async fn create_test_media(app: &TestApp, actor_id: Id, now: time::OffsetDateTime) -> Id {
    let media_id = app.runtime.ids.next_id();
    let media = Media {
        id: media_id,
        actor_id,
        media_type: MediaType::Image,
        state: MediaState::Processing,
        description: None,
        focus: Focus::default(),
        meta: None,
        blurhash: None,
        created_at: now,
    };
    insert_media(
        &app.pool,
        &media,
        &format!("{}/original", media_id.as_i64()),
        "image/png",
    )
    .await
    .expect("insert_media must succeed");
    media_id
}

/// Reads back a job row's `(state, attempts, run_at, locked_at)` directly,
/// for assertions `claim_due`'s own `Option<ProcessingJob>` return shape
/// cannot make (e.g. a terminal `'failed'` row, which `claim_due` never
/// returns again).
async fn read_job_row(
    pool: &sqlx::PgPool,
    job_id: Id,
) -> (
    String,
    i32,
    time::OffsetDateTime,
    Option<time::OffsetDateTime>,
) {
    sqlx::query_as(
        "SELECT state, attempts, run_at, locked_at FROM media_processing_jobs WHERE id = $1",
    )
    .bind(job_id.as_i64())
    .fetch_one(pool)
    .await
    .expect("the job row must still exist")
}

/// Requirements 4.1, 4.2: `enqueue` inserts a `queued` row with `attempts =
/// 0` that `claim_due` can immediately pick up when `run_at <= now`.
#[tokio::test]
async fn enqueue_inserts_a_queued_job_claimable_immediately() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "alice").await;
    let now = app.runtime.clock.now();
    let media_id = create_test_media(&app, actor_id, now).await;

    enqueue(&app.pool, app.runtime.ids.as_ref(), media_id, now)
        .await
        .expect("enqueue must succeed");

    let claimed = claim_due(&app.pool, now, Duration::minutes(5))
        .await
        .expect("claim_due must succeed")
        .expect("the just-enqueued job must be immediately claimable");
    assert_eq!(claimed.media_id, media_id);
    assert_eq!(
        claimed.attempts, 0,
        "a first-ever claim must not touch attempts"
    );
    assert_eq!(claimed.state, JobState::Processing);
    assert_eq!(claimed.locked_at, Some(now));

    app.cleanup().await;
}

/// `claim_due` returns `None` when nothing is due yet (`run_at` in the
/// future, no lease-expired `processing` job either).
#[tokio::test]
async fn claim_due_returns_none_when_nothing_is_due() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "bob").await;
    let now = app.runtime.clock.now();
    let media_id = create_test_media(&app, actor_id, now).await;

    let future_run_at = now + Duration::minutes(10);
    enqueue(&app.pool, app.runtime.ids.as_ref(), media_id, future_run_at)
        .await
        .expect("enqueue must succeed");

    let claimed = claim_due(&app.pool, now, Duration::minutes(5))
        .await
        .expect("claim_due must succeed even with nothing due");
    assert!(
        claimed.is_none(),
        "a job whose run_at is still in the future must not be claimed"
    );

    app.cleanup().await;
}

/// Requirement 4.2 (this task's core concurrency claim): two concurrent
/// `claim_due` calls against the same single queued job, issued from two
/// genuinely separate pooled connections (`tokio::join!`, pool
/// `max_connections = 5` per `test_harness.rs`), never both return the job.
/// `FOR UPDATE SKIP LOCKED` guarantees whichever call's subquery locks the
/// row first wins; the other observes it already gone (state no longer
/// `'queued'`) and returns `None`.
#[tokio::test]
async fn two_concurrent_claim_due_calls_never_both_claim_the_same_job() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "carol").await;
    let now = app.runtime.clock.now();
    let media_id = create_test_media(&app, actor_id, now).await;

    enqueue(&app.pool, app.runtime.ids.as_ref(), media_id, now)
        .await
        .expect("enqueue must succeed");

    let pool_a = app.pool.clone();
    let pool_b = app.pool.clone();
    let lease = Duration::minutes(5);
    let (result_a, result_b) = tokio::join!(
        claim_due(&pool_a, now, lease),
        claim_due(&pool_b, now, lease)
    );
    let claimed_a = result_a.expect("claim_due (a) must succeed");
    let claimed_b = result_b.expect("claim_due (b) must succeed");

    let winners = [claimed_a, claimed_b]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    assert_eq!(
        winners.len(),
        1,
        "exactly one of the two concurrent claims must win the single queued job"
    );
    assert_eq!(winners[0].media_id, media_id);

    app.cleanup().await;
}

/// Requirements 4.4: `fail_or_retry` below `max_attempts` returns `Retried`
/// with `run_at` pushed forward, and a second retry pushes it further still
/// (proving the backoff is actually exponential, not fixed).
#[tokio::test]
async fn fail_or_retry_below_max_attempts_retries_with_growing_backoff() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "dave").await;
    let now = app.runtime.clock.now();
    let media_id = create_test_media(&app, actor_id, now).await;

    enqueue(&app.pool, app.runtime.ids.as_ref(), media_id, now)
        .await
        .expect("enqueue must succeed");
    let job = claim_due(&app.pool, now, Duration::minutes(5))
        .await
        .expect("claim_due must succeed")
        .expect("job must be claimable");
    let original_run_at = job.run_at;

    let outcome1 = fail_or_retry(&app.pool, &job, 10, now)
        .await
        .expect("fail_or_retry must succeed");
    assert_eq!(outcome1, JobOutcome::Retried);
    let (state1, attempts1, run_at1, locked_at1) = read_job_row(&app.pool, job.id).await;
    assert_eq!(state1, "queued");
    assert_eq!(attempts1, 1);
    assert!(
        run_at1 > original_run_at,
        "the first retry's run_at must be pushed later than the original run_at"
    );
    assert!(
        locked_at1.is_none(),
        "a retried job must have its lock cleared"
    );

    // Claim it again (it's due again once we advance `now` to its new
    // run_at) and fail it a second time; the backoff must grow further.
    let job2 = claim_due(&app.pool, run_at1, Duration::minutes(5))
        .await
        .expect("claim_due must succeed")
        .expect("the retried job must be claimable once its new run_at arrives");
    assert_eq!(
        job2.attempts, 1,
        "reclaiming here is a normal due-claim, not a reclaim"
    );

    let outcome2 = fail_or_retry(&app.pool, &job2, 10, run_at1)
        .await
        .expect("fail_or_retry must succeed");
    assert_eq!(outcome2, JobOutcome::Retried);
    let (state2, attempts2, run_at2, _locked_at2) = read_job_row(&app.pool, job.id).await;
    assert_eq!(state2, "queued");
    assert_eq!(attempts2, 2);
    assert!(
        run_at2 - run_at1 > run_at1 - original_run_at,
        "the second backoff interval ({:?}) must be strictly larger than the first ({:?}), \
         proving exponential (not fixed) growth",
        run_at2 - run_at1,
        run_at1 - original_run_at
    );

    app.cleanup().await;
}

/// Requirement 4.5: `fail_or_retry` at/above `max_attempts` returns `Failed`
/// and the row's terminal state is reflected as `'failed'` in the DB.
#[tokio::test]
async fn fail_or_retry_at_max_attempts_fails_the_job() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "erin").await;
    let now = app.runtime.clock.now();
    let media_id = create_test_media(&app, actor_id, now).await;

    enqueue(&app.pool, app.runtime.ids.as_ref(), media_id, now)
        .await
        .expect("enqueue must succeed");
    let job = claim_due(&app.pool, now, Duration::minutes(5))
        .await
        .expect("claim_due must succeed")
        .expect("job must be claimable");

    // max_attempts = 1: the very first failure already reaches the limit.
    let outcome = fail_or_retry(&app.pool, &job, 1, now)
        .await
        .expect("fail_or_retry must succeed");
    assert_eq!(outcome, JobOutcome::Failed);

    let (state, attempts, _run_at, locked_at) = read_job_row(&app.pool, job.id).await;
    assert_eq!(state, "failed");
    assert_eq!(attempts, 1);
    assert!(
        locked_at.is_none(),
        "a failed job must have its lock cleared"
    );

    // A failed job must never be claimable again.
    let reclaimed = claim_due(&app.pool, now + Duration::hours(1), Duration::minutes(5))
        .await
        .expect("claim_due must succeed");
    assert!(
        reclaimed.is_none(),
        "a terminally failed job must never be returned by claim_due again"
    );

    app.cleanup().await;
}

/// Requirements 4.2, 4.4: a `processing` job whose `locked_at` is older
/// than `now - lease_duration` gets reclaimed by `claim_due` with
/// `attempts` incremented versus its pre-reclaim value, while a
/// `processing` job still within its lease is NOT reclaimed.
#[tokio::test]
async fn claim_due_reclaims_lease_expired_processing_jobs_and_increments_attempts() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "frank").await;
    let now = app.runtime.clock.now();
    let media_id = create_test_media(&app, actor_id, now).await;
    let lease = Duration::minutes(5);

    enqueue(&app.pool, app.runtime.ids.as_ref(), media_id, now)
        .await
        .expect("enqueue must succeed");
    let job = claim_due(&app.pool, now, lease)
        .await
        .expect("claim_due must succeed")
        .expect("job must be claimable");
    assert_eq!(job.attempts, 0);

    // Still within the lease: claim_due (from a different, later "now"
    // still inside the lease window) must not reclaim it.
    let still_within_lease = now + Duration::minutes(2);
    let not_reclaimed = claim_due(&app.pool, still_within_lease, lease)
        .await
        .expect("claim_due must succeed");
    assert!(
        not_reclaimed.is_none(),
        "a processing job still within its lease must not be reclaimed"
    );

    // Past the lease: claim_due must now reclaim it, with attempts
    // incremented versus the pre-reclaim value (0 -> 1).
    let past_lease = now + lease + Duration::seconds(1);
    let reclaimed = claim_due(&app.pool, past_lease, lease)
        .await
        .expect("claim_due must succeed")
        .expect("a lease-expired processing job must be reclaimed");
    assert_eq!(reclaimed.id, job.id);
    assert_eq!(reclaimed.media_id, media_id);
    assert_eq!(
        reclaimed.attempts, 1,
        "reclaim must increment attempts versus the pre-reclaim value (0)"
    );
    assert_eq!(reclaimed.state, JobState::Processing);
    assert_eq!(reclaimed.locked_at, Some(past_lease));

    let (state, attempts, _run_at, locked_at) = read_job_row(&app.pool, job.id).await;
    assert_eq!(state, "processing");
    assert_eq!(attempts, 1);
    assert_eq!(locked_at, Some(past_lease));

    app.cleanup().await;
}

/// A second reclaim (still no `complete`/`fail_or_retry` in between) keeps
/// incrementing `attempts`, and `fail_or_retry`'s own accounting picks up
/// from wherever reclaim left it (Requirement 4.2's "reclaim... は通常の失
/// 敗経路と同じ試行回数会計に乗せる" — reclaim and `fail_or_retry` share one
/// counter and one `max_attempts` budget).
#[tokio::test]
async fn repeated_reclaims_accumulate_into_fail_or_retry_s_own_attempts_accounting() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "grace").await;
    let now = app.runtime.clock.now();
    let media_id = create_test_media(&app, actor_id, now).await;
    let lease = Duration::minutes(5);

    enqueue(&app.pool, app.runtime.ids.as_ref(), media_id, now)
        .await
        .expect("enqueue must succeed");
    let job = claim_due(&app.pool, now, lease)
        .await
        .expect("claim_due must succeed")
        .expect("job must be claimable");
    assert_eq!(job.attempts, 0);

    // First reclaim: 0 -> 1.
    let t1 = now + lease + Duration::seconds(1);
    let reclaim1 = claim_due(&app.pool, t1, lease)
        .await
        .expect("claim_due must succeed")
        .expect("must reclaim");
    assert_eq!(reclaim1.attempts, 1);

    // Second reclaim (still crashed, lease expires again): 1 -> 2.
    let t2 = t1 + lease + Duration::seconds(1);
    let reclaim2 = claim_due(&app.pool, t2, lease)
        .await
        .expect("claim_due must succeed")
        .expect("must reclaim again");
    assert_eq!(reclaim2.attempts, 2);

    // A worker finally observes a real transient failure: max_attempts = 3
    // means this third increment (2 -> 3) hits the cap and fails the job.
    let outcome = fail_or_retry(&app.pool, &reclaim2, 3, t2)
        .await
        .expect("fail_or_retry must succeed");
    assert_eq!(
        outcome,
        JobOutcome::Failed,
        "reclaim increments must count toward the same max_attempts budget fail_or_retry checks"
    );
    let (state, attempts, _run_at, _locked_at) = read_job_row(&app.pool, job.id).await;
    assert_eq!(state, "failed");
    assert_eq!(attempts, 3);

    app.cleanup().await;
}

/// Requirement 4.3 (referenced by this component's own design.md
/// Requirements list via `complete`): `complete` deletes the job row.
#[tokio::test]
async fn complete_deletes_the_job_row() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "heidi").await;
    let now = app.runtime.clock.now();
    let media_id = create_test_media(&app, actor_id, now).await;

    enqueue(&app.pool, app.runtime.ids.as_ref(), media_id, now)
        .await
        .expect("enqueue must succeed");
    let job = claim_due(&app.pool, now, Duration::minutes(5))
        .await
        .expect("claim_due must succeed")
        .expect("job must be claimable");

    complete(&app.pool, job.id)
        .await
        .expect("complete must succeed");

    let remaining: Option<(i64,)> =
        sqlx::query_as("SELECT id FROM media_processing_jobs WHERE id = $1")
            .bind(job.id.as_i64())
            .fetch_optional(&app.pool)
            .await
            .expect("query must succeed");
    assert!(
        remaining.is_none(),
        "complete must delete the job row entirely (no 'done' state to hold)"
    );

    // A completed job must never resurface via claim_due either.
    let reclaimed = claim_due(&app.pool, now + Duration::hours(1), Duration::minutes(5))
        .await
        .expect("claim_due must succeed");
    assert!(reclaimed.is_none());

    app.cleanup().await;
}

/// `backoff_delay` grows monotonically with `attempts` and saturates at the
/// documented cap (pure-function unit coverage, mirroring
/// `federation/outbound/queue/tests.rs::backoff_delay_grows_with_increasing_attempts_and_saturates_at_the_documented_max`).
#[test]
fn backoff_delay_grows_with_attempts_and_saturates_at_the_documented_max() {
    let d1 = backoff_delay(1);
    let d2 = backoff_delay(2);
    let d3 = backoff_delay(3);
    assert_eq!(d1, super::DEFAULT_MEDIA_BASE_DELAY);
    assert!(d2 > d1);
    assert!(d3 > d2);

    let huge = backoff_delay(1_000_000);
    assert_eq!(huge, super::DEFAULT_MEDIA_MAX_DELAY);
}

/// `backoff_delay` clamps `attempts == 0` to the same result as `attempts
/// == 1` (the base delay), rather than underflowing.
#[test]
fn backoff_delay_clamps_zero_attempts_to_the_base_delay() {
    assert_eq!(backoff_delay(0), backoff_delay(1));
    assert_eq!(backoff_delay(0), super::DEFAULT_MEDIA_BASE_DELAY);
}
