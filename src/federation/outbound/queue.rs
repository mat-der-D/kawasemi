//! `DeliveryQueue` (design.md "DeliveryQueue / DeliveryWorker" -> Service
//! Interface; Requirements 11.1, 11.2, 11.3, 11.5; task 3.3,
//! `Boundary: DeliveryQueue`): persists outbound delivery jobs in
//! `delivery_jobs` (`migrations/0004_federation.sql`), lets a caller
//! exclusively claim due jobs (`claim_due`), and offers the independent
//! `mark_done` / `reschedule` / `mark_failed` state-transition primitives a
//! (later, task 4.3) delivery worker drives.
//!
//! This module intentionally stops at the queue's own CRUD/state-transition
//! primitives — see design.md's exact Service Interface, reproduced above
//! this module's trait: `claim_due` takes an explicit `now: OffsetDateTime`
//! and `reschedule` takes an explicit `next_attempt_at`/`attempts`, both
//! supplied by the caller, not computed internally. Deciding *when* to poll,
//! *when* attempts are exhausted (and therefore calling `mark_failed`
//! instead of `reschedule`), and actually performing the signed HTTP send is
//! `DeliveryWorker`'s job (task 4.3), out of this task's boundary.

#[cfg(test)]
mod tests;

use axum::http::StatusCode;
use sqlx::postgres::PgPool;
use time::{Duration, OffsetDateTime};

use crate::domain::primitives::Id;
use crate::error::AppError;

/// Postgres unique-index name backing `delivery_jobs`'s shared-inbox
/// deduplication guard (`migrations/0004_federation.sql`:
/// `delivery_jobs_dedup_idx ON delivery_jobs(target_inbox, (activity->>'id'))`),
/// used to distinguish a dedup-index violation from any other unique-index
/// violation in [`map_insert_error`].
const DELIVERY_JOBS_DEDUP_CONSTRAINT: &str = "delivery_jobs_dedup_idx";

/// Documented default base delay for [`backoff_delay`] (attempts == 0):
/// the delay before the very first retry after an initial delivery
/// failure. 30 seconds is short enough that a merely-transient failure
/// (a brief network blip, a receiving instance mid-restart) recovers
/// quickly, while still not hammering a struggling remote inbox
/// immediately.
pub const DEFAULT_DELIVERY_BASE_DELAY: Duration = Duration::seconds(30);

/// Documented cap for [`backoff_delay`]: no computed retry delay ever
/// exceeds this, regardless of how many attempts have accumulated. 6 hours
/// bounds worst-case staleness of a still-retryable delivery to a fraction
/// of a day, while still being far longer than any transient outage this
/// spec expects to recover from — consistent with
/// `DEFAULT_RECEIVED_ACTIVITY_RETENTION`'s 14-day retention window
/// comfortably outlasting the longest possible per-attempt gap.
pub const DEFAULT_DELIVERY_MAX_DELAY: Duration = Duration::hours(6);

/// Documented suggested max-attempts threshold for task 4.3's
/// `DeliveryWorker` to reference when deciding whether to call
/// `reschedule` again or `mark_failed` instead. This queue does not read or
/// enforce this constant itself (design.md's `reschedule`/`mark_failed`
/// signatures take no attempts-limit parameter; the queue only offers both
/// primitives independently) — it is exposed purely so task 4.3 has a
/// single documented source of truth instead of re-deriving a threshold.
/// 10 attempts, combined with [`backoff_delay`]'s doubling schedule capped
/// at [`DEFAULT_DELIVERY_MAX_DELAY`], spans from 30 seconds up to
/// several days of total elapsed retrying before a job is given up on —
/// long enough to ride out a multi-hour remote outage, short enough not to
/// retry indefinitely against a permanently gone inbox (Requirement 11.5).
pub const DEFAULT_MAX_DELIVERY_ATTEMPTS: i32 = 10;

/// Computes the exponential-backoff delay before the next delivery attempt,
/// given how many attempts have been made so far (`attempts`, matching
/// `reschedule`'s own `attempts` parameter — the caller passes the
/// **updated** attempts count, i.e. the count *after* incrementing for the
/// attempt that just failed).
///
/// Formula: `DEFAULT_DELIVERY_BASE_DELAY * 2^attempts`, capped at
/// [`DEFAULT_DELIVERY_MAX_DELAY`] — a standard doubling backoff. `attempts`
/// is clamped to `0` from below (a negative `attempts` is treated as `0`,
/// i.e. the base delay) and the `2^attempts` multiplier itself is computed
/// with a saturating shift so an unexpectedly large `attempts` value can
/// never overflow or panic; it simply saturates at
/// [`DEFAULT_DELIVERY_MAX_DELAY`] the same as any other large value would.
///
/// Pure function, no I/O, no injected [`crate::runtime::Clock`] — it only
/// ever computes a *duration*, never a point in time; the caller (task
/// 4.3's `DeliveryWorker`) is the one that adds this to its own
/// clock-derived "now" to produce the concrete `next_attempt_at` that
/// `reschedule` persists.
pub fn backoff_delay(attempts: i32) -> Duration {
    let attempts = attempts.max(0) as u32;
    // Cap the shift amount itself well below 63 bits: any shift large enough
    // to already exceed DEFAULT_DELIVERY_MAX_DELAY in seconds saturates to
    // the same capped result, so clamping the shift amount avoids ever
    // relying on `i64` overflow behavior.
    let shift = attempts.min(32);
    let multiplier = 1i64.checked_shl(shift).unwrap_or(i64::MAX);
    let scaled_seconds = DEFAULT_DELIVERY_BASE_DELAY
        .whole_seconds()
        .saturating_mul(multiplier);
    Duration::seconds(scaled_seconds).min(DEFAULT_DELIVERY_MAX_DELAY)
}

/// A `delivery_jobs.status` value (design.md: `'pending' | 'in_progress' |
/// 'done' | 'failed'`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryJobStatus {
    Pending,
    InProgress,
    Done,
    Failed,
}

impl DeliveryJobStatus {
    /// Maps to the exact `delivery_jobs.status` `TEXT` value design.md
    /// specifies.
    fn as_str(self) -> &'static str {
        match self {
            DeliveryJobStatus::Pending => "pending",
            DeliveryJobStatus::InProgress => "in_progress",
            DeliveryJobStatus::Done => "done",
            DeliveryJobStatus::Failed => "failed",
        }
    }

    /// Reconstructs a [`DeliveryJobStatus`] from an already-persisted
    /// `delivery_jobs.status` column value. Panics on any other value: such
    /// a row could only exist via a bug in this module's own writes (the
    /// same convention `actor/repository.rs`'s `actor_type_from_str` /
    /// `actor_state_from_str` use for their own status-like columns).
    fn from_str(raw: &str) -> Self {
        match raw {
            "pending" => DeliveryJobStatus::Pending,
            "in_progress" => DeliveryJobStatus::InProgress,
            "done" => DeliveryJobStatus::Done,
            "failed" => DeliveryJobStatus::Failed,
            other => panic!(
                "delivery_jobs.status contained unexpected value {other:?}; expected \
                 'pending', 'in_progress', 'done', or 'failed'"
            ),
        }
    }
}

/// A not-yet-persisted delivery job (design.md's exact `enqueue(job:
/// NewDeliveryJob)` parameter type).
///
/// `id` is minted by the caller's `IdGenerator` (never by the database —
/// `migrations/0004_federation.sql`: "id BIGINT PRIMARY KEY -- core-runtime
/// IdGenerator 採番"). `next_attempt_at` is the initial attempt time
/// (typically "now", but this queue has no injected
/// [`crate::runtime::Clock`] of its own per design.md's signatures, so the
/// caller decides it) — and, since this queue also has no other source of
/// "now" available at `enqueue` time, `next_attempt_at` doubles as the
/// persisted row's initial `created_at`/`updated_at` (see
/// [`DbDeliveryQueue::enqueue`]'s doc comment).
#[derive(Debug, Clone, PartialEq)]
pub struct NewDeliveryJob {
    pub id: Id,
    pub sender_actor_id: Id,
    pub target_inbox: String,
    pub activity: serde_json::Value,
    pub next_attempt_at: OffsetDateTime,
}

/// A persisted `delivery_jobs` row (design.md's exact `claim_due` return
/// element type), mirroring every column so a caller has everything needed
/// to actually attempt the send.
#[derive(Debug, Clone, PartialEq)]
pub struct DeliveryJob {
    pub id: Id,
    pub sender_actor_id: Id,
    pub target_inbox: String,
    pub activity: serde_json::Value,
    pub status: DeliveryJobStatus,
    pub attempts: i32,
    pub next_attempt_at: OffsetDateTime,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// Delivery-job persistence port (design.md's exact `DeliveryQueue` Service
/// Interface; Requirements 11.1, 11.2, 11.3, 11.5). See this module's doc
/// comment for the exact scope boundary against `DeliveryWorker` (task 4.3).
///
/// `#[allow(async_fn_in_trait)]`: mirrors this spec's established rationale
/// (e.g. `key_resolver.rs`'s `PublicKeyResolver`, `dedup.rs`'s
/// `ReceivedActivityStore`): design.md pins these methods as literal `async
/// fn`; boxing/`Send`-pinning concerns belong to whichever later task
/// actually needs `Arc<dyn DeliveryQueue>` across a `tokio::spawn` boundary
/// (task 4.3's `DeliveryWorker`), not this task's `DeliveryQueue` boundary.
#[allow(async_fn_in_trait)]
pub trait DeliveryQueue: Send + Sync {
    /// Persists `job` without waiting for delivery to complete (Requirement
    /// 11.1). See [`DbDeliveryQueue::enqueue`]'s doc comment for this
    /// implementation's exact dedup-conflict contract.
    async fn enqueue(&self, job: NewDeliveryJob) -> Result<(), AppError>;

    /// Atomically selects up to `limit` `'pending'` jobs whose
    /// `next_attempt_at <= now`, transitions them to `'in_progress'` as
    /// part of the same statement, and returns them (Requirement 11.2). See
    /// [`DbDeliveryQueue::claim_due`]'s doc comment for the exact atomicity
    /// contract this must uphold under concurrent callers.
    async fn claim_due(
        &self,
        limit: i64,
        now: OffsetDateTime,
    ) -> Result<Vec<DeliveryJob>, AppError>;

    /// Transitions `job_id` to `'done'` — a delivery that has fully
    /// succeeded.
    async fn mark_done(&self, job_id: Id) -> Result<(), AppError>;

    /// Transitions `job_id` back to `'pending'` with an updated
    /// `next_attempt_at` (pushed later, per the caller's own backoff
    /// calculation — see [`backoff_delay`]) and `attempts` (Requirement
    /// 11.3).
    async fn reschedule(
        &self,
        job_id: Id,
        next_attempt_at: OffsetDateTime,
        attempts: i32,
    ) -> Result<(), AppError>;

    /// Transitions `job_id` to `'failed'` — permanent failure, no further
    /// retries (Requirement 11.5). Only ever called explicitly by the
    /// caller (this queue never auto-transitions a job to `'failed'` on its
    /// own).
    async fn mark_failed(&self, job_id: Id) -> Result<(), AppError>;
}

/// A `delivery_jobs` row as read directly off the wire, before
/// reconstructing a [`DeliveryJob`].
type DeliveryJobRow = (
    i64,
    i64,
    String,
    serde_json::Value,
    String,
    i32,
    OffsetDateTime,
    OffsetDateTime,
    OffsetDateTime,
);

fn row_to_delivery_job(row: DeliveryJobRow) -> DeliveryJob {
    let (
        id,
        sender_actor_id,
        target_inbox,
        activity,
        status,
        attempts,
        next_attempt_at,
        created_at,
        updated_at,
    ) = row;
    DeliveryJob {
        id: Id::from_i64(id),
        sender_actor_id: Id::from_i64(sender_actor_id),
        target_inbox,
        activity,
        status: DeliveryJobStatus::from_str(&status),
        attempts,
        next_attempt_at,
        created_at,
        updated_at,
    }
}

/// Maps a failed `INSERT INTO delivery_jobs` to an [`AppError`]: a unique
/// violation on [`DELIVERY_JOBS_DEDUP_CONSTRAINT`] (the same Activity
/// already enqueued to the same target inbox) becomes a caller-facing
/// (`ErrorKind::Client`) `409 Conflict` — see [`DbDeliveryQueue::enqueue`]'s
/// doc comment for why this is the chosen contract; anything else becomes a
/// `Server` (5xx) `AppError`, mirroring `actor/repository.rs`'s
/// `map_insert_error` convention.
fn map_insert_error(source: sqlx::Error) -> AppError {
    if let Some(db_error) = source.as_database_error()
        && db_error.is_unique_violation()
        && db_error.constraint() == Some(DELIVERY_JOBS_DEDUP_CONSTRAINT)
    {
        return AppError::client(
            StatusCode::CONFLICT,
            "a delivery job for this activity and target inbox is already enqueued",
        );
    }
    AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source)
}

/// `DeliveryQueue` implementation backed by `delivery_jobs`.
///
/// Holds only a `PgPool` — no injected [`crate::runtime::Clock`] — because
/// design.md's Service Interface threads every timestamp this queue's
/// business logic actually reasons about (`claim_due`'s `now`,
/// `reschedule`'s `next_attempt_at`) through as an explicit caller-supplied
/// parameter rather than reading a clock internally. The one place this
/// implementation still needs a timestamp with no caller-supplied value
/// available at all — `updated_at` bookkeeping on `mark_done`/`mark_failed`,
/// whose signatures carry no time parameter — uses Postgres's own `now()`
/// function. This is a deliberate, narrow exception to this spec's
/// "clock is always injected" convention: `updated_at` on those two
/// terminal transitions is pure observability metadata that no business
/// logic in this codebase ever reads back to make a decision (unlike
/// `next_attempt_at`, which directly drives `claim_due`'s eligibility
/// judgment and is always caller-supplied), so using the database's own
/// clock for it does not introduce the nondeterminism the DI boundary
/// guards against.
pub struct DbDeliveryQueue {
    pool: PgPool,
}

impl DbDeliveryQueue {
    /// Builds a queue against `pool` (the `delivery_jobs` table's
    /// connection pool).
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl DeliveryQueue for DbDeliveryQueue {
    /// Inserts `job` as a `'pending'` row with `attempts = 0`. Since this
    /// queue has no injected clock (see this struct's doc comment) and
    /// `NewDeliveryJob` carries no separate `created_at`, `job`'s own
    /// `next_attempt_at` is reused as the initial `created_at`/`updated_at`
    /// too — a reasonable choice given enqueuing and the first eligible
    /// attempt time coincide for a brand-new job (the common case is the
    /// caller passing "now" for `next_attempt_at`).
    ///
    /// A unique-index conflict on `(target_inbox, activity->>'id')`
    /// (`delivery_jobs_dedup_idx`) — the same Activity already enqueued to
    /// the same target inbox — surfaces as a caller-facing `409 Conflict`
    /// rather than a generic 5xx (see [`map_insert_error`]). This is
    /// expected to be rare in practice: task 3.4's `RecipientTargetResolver`
    /// is what is supposed to prevent duplicate targets from ever being
    /// enqueued in the first place. Choosing to surface a clean `AppError`
    /// here (rather than silently succeeding or panicking) keeps this
    /// queue's behavior well-defined and safe even if that upstream
    /// dedup ever has a gap.
    async fn enqueue(&self, job: NewDeliveryJob) -> Result<(), AppError> {
        sqlx::query(
            "INSERT INTO delivery_jobs \
                (id, sender_actor_id, target_inbox, activity, status, attempts, \
                 next_attempt_at, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, 0, $6, $6, $6)",
        )
        .bind(job.id.as_i64())
        .bind(job.sender_actor_id.as_i64())
        .bind(&job.target_inbox)
        .bind(&job.activity)
        .bind(DeliveryJobStatus::Pending.as_str())
        .bind(job.next_attempt_at)
        .execute(&self.pool)
        .await
        .map_err(map_insert_error)?;

        Ok(())
    }

    /// Single atomic `UPDATE ... WHERE id IN (SELECT ... FOR UPDATE SKIP
    /// LOCKED) RETURNING ...` statement (design.md's "Consistency" note:
    /// "配送ジョブ取得は排他更新（`status='in_progress'`）で多重実行を防ぐ"):
    /// the same statement both selects the eligible rows and transitions
    /// them to `'in_progress'`, never a separate SELECT-then-UPDATE from
    /// application code. `FOR UPDATE SKIP LOCKED` inside the subquery means
    /// two concurrent callers racing this same query never claim the same
    /// row: whichever caller's subquery locks a row first, the other
    /// caller's subquery skips it instead of blocking on it or (worse)
    /// double-claiming it.
    ///
    /// Only rows with `status = 'pending'` and `next_attempt_at <= now` are
    /// eligible, ordered by `next_attempt_at` so the most-overdue jobs are
    /// claimed first when `limit` is smaller than the number of due jobs.
    async fn claim_due(
        &self,
        limit: i64,
        now: OffsetDateTime,
    ) -> Result<Vec<DeliveryJob>, AppError> {
        let rows: Vec<DeliveryJobRow> = sqlx::query_as(
            "UPDATE delivery_jobs \
             SET status = $1, updated_at = $2 \
             WHERE id IN ( \
                 SELECT id FROM delivery_jobs \
                 WHERE status = $3 AND next_attempt_at <= $2 \
                 ORDER BY next_attempt_at \
                 LIMIT $4 \
                 FOR UPDATE SKIP LOCKED \
             ) \
             RETURNING id, sender_actor_id, target_inbox, activity, status, attempts, \
                       next_attempt_at, created_at, updated_at",
        )
        .bind(DeliveryJobStatus::InProgress.as_str())
        .bind(now)
        .bind(DeliveryJobStatus::Pending.as_str())
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;

        Ok(rows.into_iter().map(row_to_delivery_job).collect())
    }

    async fn mark_done(&self, job_id: Id) -> Result<(), AppError> {
        sqlx::query("UPDATE delivery_jobs SET status = $1, updated_at = now() WHERE id = $2")
            .bind(DeliveryJobStatus::Done.as_str())
            .bind(job_id.as_i64())
            .execute(&self.pool)
            .await
            .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;

        Ok(())
    }

    /// Updates both `next_attempt_at` (pushed later, per the caller's own
    /// backoff calculation) and `attempts`, and moves the job back to
    /// `'pending'` so it becomes claimable again once `now` reaches the new
    /// `next_attempt_at` (Requirement 11.3). `updated_at` reuses
    /// `next_attempt_at` — unlike `mark_done`/`mark_failed`, this method
    /// does receive a caller-supplied timestamp, so there is no need to
    /// fall back to the database's own clock here.
    async fn reschedule(
        &self,
        job_id: Id,
        next_attempt_at: OffsetDateTime,
        attempts: i32,
    ) -> Result<(), AppError> {
        sqlx::query(
            "UPDATE delivery_jobs \
             SET status = $1, next_attempt_at = $2, attempts = $3, updated_at = $2 \
             WHERE id = $4",
        )
        .bind(DeliveryJobStatus::Pending.as_str())
        .bind(next_attempt_at)
        .bind(attempts)
        .bind(job_id.as_i64())
        .execute(&self.pool)
        .await
        .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;

        Ok(())
    }

    async fn mark_failed(&self, job_id: Id) -> Result<(), AppError> {
        sqlx::query("UPDATE delivery_jobs SET status = $1, updated_at = now() WHERE id = $2")
            .bind(DeliveryJobStatus::Failed.as_str())
            .bind(job_id.as_i64())
            .execute(&self.pool)
            .await
            .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;

        Ok(())
    }
}
