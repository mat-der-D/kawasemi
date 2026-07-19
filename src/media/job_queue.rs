//! `ProcessingJobQueue` (design.md "Data / データ層" -> `ProcessingJobQueue`,
//! Requirements 4.1, 4.2, 4.4, 4.5, 4.6; task 3.2, `Boundary:
//! ProcessingJobQueue`): the DB-backed job queue for asynchronous media
//! processing (`migrations/0005_media.sql`'s `media_processing_jobs` table,
//! already applied, unmodified by this task) — job enqueue, exclusive
//! `FOR UPDATE SKIP LOCKED` claim (covering both a freshly-queued job and a
//! lease-expired `processing` job reclaimed from a crashed worker),
//! completion, and the temporary-failure retry/backoff/permanent-failure
//! transition.
//!
//! Scope: this module owns exactly the four operations design.md's Service
//! Interface sketches for this component — [`enqueue`], [`claim_due`],
//! [`complete`], [`fail_or_retry`] — against a plain `&PgPool`. It does not
//! implement `MediaService` (task 4.1) or `ProcessingWorker` (task 4.3) —
//! deciding *when* to poll, what counts as a "transient" vs. a "decode"
//! failure, and actually performing the image processing are out of this
//! task's boundary (mirroring `src/federation/outbound/queue.rs`'s identical
//! `DeliveryQueue`/`DeliveryWorker` split, this module's closest in-repo
//! precedent). It does not modify `src/media/model.rs` (task 2.1) or
//! `src/media/media_repository.rs` (task 3.1).
//!
//! ## `attempts` accounting (Requirements 4.2, 4.4)
//! `attempts` is incremented in exactly two places, matching design.md's
//! Responsibilities note ("reclaim 時は... attempts++ する（クラッシュに
//! よる再取得も1試行として消費し、fail_or_retry の会計と整合させる）") and
//! this task's own text ("reclaim... は通常の失敗経路と同じ試行回数会計に
//! 乗せるため、reclaim 時にも試行回数を加算する"):
//! - [`claim_due`] increments it only when the claimed row was already
//!   `processing` (i.e. a reclaim of a lease-expired job from a presumed-
//!   crashed worker) — a freshly-`queued` job's first claim leaves
//!   `attempts` untouched (it starts, and stays, at `0` until either a
//!   reclaim or a real processing failure happens to it).
//! - [`fail_or_retry`] increments it on every call (a real, observed
//!   temporary-failure event), regardless of whether prior increments came
//!   from reclaims or previous `fail_or_retry` calls — both event kinds
//!   share the same counter and the same `max_attempts` budget, per
//!   design.md's "整合させる" instruction.
//!
//! ## `JobOutcome` (placement judgment call)
//! design.md's Service Interface sketches [`fail_or_retry`]'s return type as
//! bare `JobOutcome` with an inline comment `// Retried | Failed`, but never
//! spells out the type's exact definition or which module is meant to own
//! it — no other component in design.md references or re-exports it either.
//! Since [`JobOutcome`] exists purely to report [`fail_or_retry`]'s own
//! outcome and nothing outside this module's `Service Interface` touches it,
//! it is defined right here, as a plain two-variant enum with no payload
//! (matching the excerpt's bare `Retried | Failed` literally — callers that
//! need the resulting `run_at`/state re-query the row, e.g. via
//! [`claim_due`], the same way this module's own tests do).
//!
//! ## `claim_due`'s query shape (index-exploiting, single-statement atomic)
//! Mirrors `src/federation/outbound/queue.rs::DbDeliveryQueue::claim_due`'s
//! established pattern: one atomic `UPDATE ... WHERE id IN (SELECT ... FOR
//! UPDATE SKIP LOCKED) RETURNING ...` statement, never a separate
//! SELECT-then-UPDATE from application code, so two concurrent callers
//! racing this query never claim the same row (whichever caller's subquery
//! locks a row first, the other caller's subquery skips it instead of
//! blocking or double-claiming). The subquery's `WHERE (state = 'queued' AND
//! run_at <= $1) OR (state = 'processing' AND locked_at < $2)` predicate
//! shape is written to match `media_jobs_due_idx`'s exact partial-index
//! predicate (`migrations/0005_media.sql`: `(state, run_at) WHERE state IN
//! ('queued', 'processing')`) — see that migration's own comment for why the
//! `locked_at` half can only ever be a residual filter over the
//! index-narrowed `processing` subset, never folded into the index
//! predicate itself (`now() - lease_duration` is not immutable). The lease
//! threshold (`now - lease_duration`) is computed in Rust and bound as a
//! plain `OffsetDateTime` (`$2`), not as a bound `INTERVAL` — simpler than
//! threading `time::Duration`'s Postgres `INTERVAL` encoding through sqlx
//! for no benefit, since the subtraction is equally well-defined on either
//! side.
//!
//! ## `fail_or_retry`/`complete` need no `FOR UPDATE`/subquery dance
//! Unlike `claim_due`, [`fail_or_retry`] and [`complete`] are only ever
//! called by the single worker that already exclusively holds the job (a
//! `claim_due`-returned [`ProcessingJob`] with `state = Processing`) — no
//! other worker's `claim_due` can match that same row again until its
//! `locked_at` ages past `lease_duration`, which cannot happen while the
//! original worker is still actively calling `fail_or_retry`/`complete` on
//! it moments later. Plain `UPDATE ... WHERE id = $n` (mirroring
//! `DbDeliveryQueue::mark_done`/`reschedule`/`mark_failed`'s identical
//! reasoning) is therefore sufficient; no locking dance is needed.
//!
//! ## Backoff formula/constants (judgment call, recorded in tasks.md)
//! Requirement 4.4 only mandates "指数バックオフ" (exponential backoff)
//! without a specific formula or cap. Per this task's own instructions,
//! [`backoff_delay`] follows `src/federation/outbound/queue.rs::
//! backoff_delay`'s established doubling-with-a-cap shape for consistency
//! within the codebase, with its own base delay/cap constants
//! ([`DEFAULT_MEDIA_BASE_DELAY`], [`DEFAULT_MEDIA_MAX_DELAY`]) sized for
//! media processing rather than reused verbatim — see those constants' own
//! doc comments for the sizing rationale.
//!
//! ## `fail_or_retry` gains an `error_message: &str` parameter (task 4.3
//! signature extension, closing the `last_error` gap tasks.md's "3.2 レビ
//! ュー所見" flagged)
//! `media_processing_jobs.last_error` (`migrations/0005_media.sql`) exists
//! specifically to hold Requirement 4.5's "原因特定に十分な診断情報"
//! (diagnostic detail sufficient to identify the cause of a failure), but
//! this module's original signature had no error-message parameter for
//! [`fail_or_retry`] to persist there at all — task 3.2's own review note
//! flagged this as a gap `ProcessingWorker` (task 4.3) must resolve, since
//! task 3.2's acceptance text never asked for it and design.md's excerpted
//! `fail_or_retry(pool, job, max_attempts, now) -> Result<JobOutcome,
//! AppError>` signature has no such parameter either. Following the same
//! "extend a previously-committed signature in the direction the schema/a
//! later task actually needs, document why" convention this module's own
//! doc comment already used for `claim_due`'s attempts accounting and
//! `media_repository.rs`/`store.rs` used for their own documented
//! deviations, [`fail_or_retry`] now takes `error_message: &str` and writes
//! it into `last_error` on *both* branches (a reschedule and a terminal
//! failure both get a diagnostic recorded, not just the terminal one) —
//! `ProcessingWorker` (task 4.3) is the sole caller and supplies a message
//! derived from the underlying `AppError` it classified as transient or
//! decode/terminal (see `worker.rs`'s `diagnostic_message` helper).

#[cfg(test)]
mod tests;

use axum::http::StatusCode;
use sqlx::postgres::PgPool;
use time::{Duration, OffsetDateTime};

use crate::domain::Id;
use crate::error::AppError;
use crate::media::model::{JobState, ProcessingJob};
use crate::runtime::IdGenerator;

/// Documented default base delay for [`backoff_delay`] (`attempts == 1`,
/// the smallest value [`fail_or_retry`] ever calls it with — see that
/// function's doc comment for why `attempts` is always >= 1 at the call
/// site): the delay before the very first retry after a transient
/// processing failure (e.g. a momentarily-unavailable original object, a
/// decode hiccup worth a second try). 15 seconds is short enough that a
/// single-server, locally-hosted media pipeline recovers a merely-transient
/// failure quickly, while still not hammering the same job in a tight loop.
pub const DEFAULT_MEDIA_BASE_DELAY: Duration = Duration::seconds(15);

/// Documented cap for [`backoff_delay`]: no computed retry delay ever
/// exceeds this, regardless of how many attempts have accumulated. 30
/// minutes bounds worst-case staleness of a still-retryable, still-
/// `processing`-state media to a fraction of an hour — short enough that an
/// operator polling a still-`processing` upload does not wait unreasonably
/// long between retries, unlike `federation`'s multi-hour remote-outage
/// tolerance (a local processing failure is expected to either clear up
/// fast or hit `max_attempts` and fail outright, not linger for hours).
pub const DEFAULT_MEDIA_MAX_DELAY: Duration = Duration::minutes(30);

/// Computes the exponential-backoff delay before a job's next retry
/// attempt, given the updated `attempts` count (i.e. the count *after*
/// incrementing for the attempt that just failed — matching
/// `src/federation/outbound/queue.rs::backoff_delay`'s identical
/// "caller passes the post-increment count" convention).
///
/// Formula: [`DEFAULT_MEDIA_BASE_DELAY`] `* 2^(attempts - 1)`, capped at
/// [`DEFAULT_MEDIA_MAX_DELAY`] — a standard doubling backoff, `attempts - 1`
/// (rather than `attempts`) so the first retry (`attempts == 1`) uses
/// exactly the base delay instead of already doubling it once. `attempts`
/// is clamped to `1` from below so a caller accidentally passing `0` still
/// gets a well-defined (base) delay rather than underflowing, and the
/// `2^n` multiplier is computed with a saturating shift so an unexpectedly
/// large `attempts` value can never overflow or panic — it simply
/// saturates at [`DEFAULT_MEDIA_MAX_DELAY`] the same as any other large
/// value would.
///
/// Pure function, no I/O, no injected [`crate::runtime::Clock`] — it only
/// ever computes a *duration*; [`fail_or_retry`] is the one that adds this
/// to its own caller-supplied `now` to produce the concrete `run_at` it
/// persists.
pub fn backoff_delay(attempts: u32) -> Duration {
    let attempts = attempts.max(1);
    let shift = (attempts - 1).min(32);
    let multiplier = 1i64.checked_shl(shift).unwrap_or(i64::MAX);
    let scaled_seconds = DEFAULT_MEDIA_BASE_DELAY
        .whole_seconds()
        .saturating_mul(multiplier);
    Duration::seconds(scaled_seconds).min(DEFAULT_MEDIA_MAX_DELAY)
}

/// [`fail_or_retry`]'s outcome (design.md's exact `JobOutcome` return type;
/// see this module's doc comment, "`JobOutcome` (placement judgment call)",
/// for why it is defined here with no payload).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobOutcome {
    /// The job was rescheduled: `attempts` was incremented and `run_at` was
    /// pushed forward by [`backoff_delay`] (Requirement 4.4). The job's
    /// `state` returns to `'queued'` and `locked_at` is cleared so a future
    /// `claim_due` call can pick it up again once `run_at` is reached.
    Retried,
    /// The job exhausted its retry budget (`attempts >= max_attempts`) and
    /// was moved to `'failed'` — a terminal state; no further
    /// `claim_due`/`fail_or_retry` will ever match this row again
    /// (Requirement 4.5). The caller (`ProcessingWorker`, task 4.3) is
    /// responsible for also transitioning the owning `Media` to
    /// `MediaState::Failed` (`media_repository::set_failed`) — this queue
    /// only owns the job row's own terminal state.
    Failed,
}

/// Reconstructs a [`JobState`] from an already-persisted
/// `media_processing_jobs.state` column value. Panics on any other value —
/// such a row could only exist via a bug in this module's own writes (same
/// convention as `media_repository.rs::media_state_from_str`).
fn job_state_from_str(raw: &str) -> JobState {
    match raw {
        "queued" => JobState::Queued,
        "processing" => JobState::Processing,
        "failed" => JobState::Failed,
        other => panic!(
            "media_processing_jobs.state contained unexpected value {other:?}; expected one \
             of 'queued'/'processing'/'failed'"
        ),
    }
}

/// A `media_processing_jobs` row's [`ProcessingJob`]-reconstructible
/// columns, as read directly off the wire (shared shape between
/// [`claim_due`]'s `UPDATE ... RETURNING`).
type JobRow = (
    i64,
    i64,
    i32,
    OffsetDateTime,
    Option<OffsetDateTime>,
    String,
);

fn row_to_job(row: JobRow) -> ProcessingJob {
    let (id, media_id, attempts, run_at, locked_at, state) = row;
    ProcessingJob {
        id: Id::from_i64(id),
        media_id: Id::from_i64(media_id),
        attempts: attempts.max(0) as u32,
        run_at,
        locked_at,
        state: job_state_from_str(&state),
    }
}

fn map_insert_error(source: sqlx::Error) -> AppError {
    AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source)
}

fn map_query_error(source: sqlx::Error) -> AppError {
    AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source)
}

/// Persists a new processing job for `media_id` in state `'queued'` with
/// `attempts = 0` and `run_at = now` — immediately claimable (Requirement
/// 1.6, 4.1: no external broker required, an accepted upload's derivative
/// generation is driven entirely by this DB-backed queue). `id` is minted
/// by `ids` (never by the database, matching every other table in this
/// crate's `IdGenerator` convention). `now` also stamps `created_at` (this
/// row has no separate "scheduled for later" concept at insert time — a
/// freshly-accepted upload's processing job is always immediately due).
pub async fn enqueue(
    pool: &PgPool,
    ids: &dyn IdGenerator,
    media_id: Id,
    now: OffsetDateTime,
) -> Result<(), AppError> {
    let id = ids.next_id();

    sqlx::query(
        "INSERT INTO media_processing_jobs \
            (id, media_id, state, attempts, run_at, locked_at, last_error, created_at) \
         VALUES ($1, $2, 'queued', 0, $3, NULL, NULL, $3)",
    )
    .bind(id.as_i64())
    .bind(media_id.as_i64())
    .bind(now)
    .execute(pool)
    .await
    .map_err(map_insert_error)?;

    Ok(())
}

/// Exclusively claims one due job — either a freshly-`queued` job whose
/// `run_at <= now`, or a lease-expired `processing` job (`locked_at <
/// now - lease_duration`) reclaimed from a presumed-crashed worker
/// (Requirement 4.2) — via a single atomic `FOR UPDATE SKIP LOCKED`
/// statement. Returns `Ok(None)` when no job is currently due (neither
/// branch matches any row, or every matching row is already locked by a
/// concurrent claimant). See this module's doc comment for the exact query
/// shape and its index-exploitation rationale, and for the `attempts`
/// accounting this performs (incremented only on the reclaim branch).
///
/// Postcondition (design.md): the returned job is exclusively held by the
/// caller until it calls [`complete`] or [`fail_or_retry`] on it, or until
/// `lease_duration` elapses and another worker reclaims it instead
/// (Requirement 4.6: media state remains the truth source, so a reclaim
/// racing a slow-but-still-alive worker's own eventual `complete`/
/// `fail_or_retry` call never corrupts anything worse than a redundant
/// no-op — `complete`/`fail_or_retry` targeting an already-reclaimed-and-
/// since-transitioned row simply affect zero or the wrong-generation row,
/// which `ProcessingWorker`, task 4.3, is responsible for tolerating via
/// idempotent derivative writes keyed off `Media`'s own state).
pub async fn claim_due(
    pool: &PgPool,
    now: OffsetDateTime,
    lease_duration: Duration,
) -> Result<Option<ProcessingJob>, AppError> {
    let lease_threshold = now - lease_duration;

    let row: Option<JobRow> = sqlx::query_as(
        "UPDATE media_processing_jobs \
         SET state = 'processing', \
             locked_at = $1, \
             attempts = attempts + CASE WHEN state = 'processing' THEN 1 ELSE 0 END \
         WHERE id IN ( \
             SELECT id FROM media_processing_jobs \
             WHERE (state = 'queued' AND run_at <= $1) \
                OR (state = 'processing' AND locked_at < $2) \
             ORDER BY run_at \
             LIMIT 1 \
             FOR UPDATE SKIP LOCKED \
         ) \
         RETURNING id, media_id, attempts, run_at, locked_at, state",
    )
    .bind(now)
    .bind(lease_threshold)
    .fetch_optional(pool)
    .await
    .map_err(map_query_error)?;

    Ok(row.map(row_to_job))
}

/// Finalizes a successfully-completed job by deleting its row (Requirement
/// 4.3): `media_processing_jobs.state` has no `'done'` value to hold instead
/// (`migrations/0005_media.sql`'s column comment, `src/media/model.rs`'s
/// `JobState` doc comment) — a completed job simply stops existing as
/// pending work. Does not itself verify `job_id` exists (mirrors
/// `media_repository.rs::set_ready`'s "no error for absence" convention at
/// this data layer) — deleting an already-gone/never-existed `job_id`
/// affects zero rows, no error.
pub async fn complete(pool: &PgPool, job_id: Id) -> Result<(), AppError> {
    sqlx::query("DELETE FROM media_processing_jobs WHERE id = $1")
        .bind(job_id.as_i64())
        .execute(pool)
        .await
        .map_err(map_query_error)?;

    Ok(())
}

/// Handles a temporary processing failure for `job` (a value the caller
/// already exclusively holds, typically straight from [`claim_due`]):
/// increments `attempts` by exactly one (Requirement 4.4 — see this
/// module's doc comment, "`attempts` accounting", for why `fail_or_retry`
/// always adds exactly one regardless of how `job.attempts` got to its
/// current value) and either:
/// - reschedules it (returns [`JobOutcome::Retried`]): `state` back to
///   `'queued'`, `locked_at` cleared, `run_at` pushed forward by
///   [`backoff_delay`] applied to the *new* (post-increment) attempts count
///   — when the new attempts count is still below `max_attempts`; or
/// - permanently fails it (returns [`JobOutcome::Failed`]): `state` to
///   `'failed'`, `locked_at` cleared, `run_at` left as-is (irrelevant once
///   terminal) — when the new attempts count has reached or exceeded
///   `max_attempts` (Requirement 4.5).
///
/// `now` is the caller-supplied "current time" (Requirement's "時刻は時刻
/// 境界から取得する") that [`backoff_delay`]'s computed delay is added to;
/// this function never reads a wall clock itself. See this module's doc
/// comment ("`fail_or_retry`/`complete` need no `FOR UPDATE`/subquery
/// dance") for why a plain `UPDATE ... WHERE id = $n` is sufficient here.
///
/// `error_message` is persisted into `last_error` on both branches
/// (Requirement 4.5; see this module's doc comment, "`fail_or_retry` gains
/// an `error_message` parameter", for why this signature was extended past
/// design.md's excerpt). A caller that forces an immediate terminal failure
/// regardless of its own configured retry budget (a decode/non-retryable
/// error, per `worker.rs`'s classification) passes `max_attempts = 0`:
/// `attempts` (always `>= 1` after the unconditional increment below) then
/// always satisfies `attempts >= max_attempts`, so this always takes the
/// `Failed` branch on the very first call regardless of `job.attempts`.
pub async fn fail_or_retry(
    pool: &PgPool,
    job: &ProcessingJob,
    max_attempts: u32,
    now: OffsetDateTime,
    error_message: &str,
) -> Result<JobOutcome, AppError> {
    let attempts = job.attempts.saturating_add(1);

    if attempts >= max_attempts {
        sqlx::query(
            "UPDATE media_processing_jobs \
             SET state = 'failed', attempts = $1, locked_at = NULL, last_error = $2 \
             WHERE id = $3",
        )
        .bind(attempts as i32)
        .bind(error_message)
        .bind(job.id.as_i64())
        .execute(pool)
        .await
        .map_err(map_query_error)?;

        Ok(JobOutcome::Failed)
    } else {
        let run_at = now + backoff_delay(attempts);

        sqlx::query(
            "UPDATE media_processing_jobs \
             SET state = 'queued', attempts = $1, run_at = $2, locked_at = NULL, \
                 last_error = $3 \
             WHERE id = $4",
        )
        .bind(attempts as i32)
        .bind(run_at)
        .bind(error_message)
        .bind(job.id.as_i64())
        .execute(pool)
        .await
        .map_err(map_query_error)?;

        Ok(JobOutcome::Retried)
    }
}
