//! `ProcessingWorker` (design.md "Media Runtime / ワーカー層" ->
//! `ProcessingWorker`, and the "ワーカーによる派生物生成（DB キュー消費）"
//! flowchart directly above it; Requirements 4.2, 4.3, 4.4, 4.5, 4.6, 6.1,
//! 6.5; task 4.3, `Boundary: ProcessingWorker`): the resident poll loop that
//! drives `media_processing_jobs` to completion — exclusive claim (via
//! `job_queue::claim_due`, task 3.2, already implemented) -> load the
//! original object -> `MediaProcessor::process_image` -> store the
//! thumbnail derivative -> `media_repository::set_ready` -> `job_queue::
//! complete`, with a transient-failure retry/backoff path and a
//! decode-failure/attempts-exhausted terminal-failure path.
//!
//! ## Scope
//! This module owns exactly [`ProcessingWorker`] and its two entry points:
//! [`ProcessingWorker::run_once`] (claim-and-resolve exactly one due job,
//! independently callable and the unit this module's own tests drive
//! directly — mirroring `federation::outbound::worker::DeliveryWorker::
//! run_once`'s identical "no internal timer, caller controls cadence"
//! precedent) and [`ProcessingWorker::run`] (the actual resident loop:
//! `run_once` repeatedly, sleeping between empty polls, until an injected
//! shutdown signal resolves). It does not implement `job_queue`/
//! `media_repository`/`store`/`processor` (tasks 3.2/3.1/2.2/2.3, all
//! already implemented and composed here unchanged, with two narrow,
//! documented signature extensions to `job_queue::fail_or_retry` and a new
//! `media_repository::find_by_id` — see those modules' own doc comments),
//! `MediaService` (task 4.1), `MediaAttachmentSerializer` (task 4.2), any
//! HTTP surface (`MediaEndpoints`, task 5.1), or runtime wiring (`AppState`/
//! `bootstrap.rs`/`server.rs`, task 5.2 — this is a standalone,
//! independently-constructible component with no live caller yet, the same
//! "not wired in" posture `DeliveryWorker`'s own doc comment documents for
//! itself).
//!
//! ## `ProcessingWorker<S, P>` generic type parameters, not `Arc<dyn ...>`
//! Neither `MediaStore` (task 2.2) nor `MediaProcessor` (task 2.3) is
//! `dyn`-object-safe in the way this crate consumes them (`MediaStore` is
//! `#[allow(async_fn_in_trait)]`-based per its own doc comment;
//! `MediaProcessor` is a plain sync trait but `MediaService<S: MediaStore>`,
//! task 4.1, already established the "generic type parameter, not `Arc<dyn
//! ...>`" convention for this exact pairing of ports in this exact feature).
//! `ProcessingWorker<S: MediaStore, P: MediaProcessor>` mirrors that
//! precedent (and `federation::outbound::worker::DeliveryWorker<Q, H>`'s
//! identical multi-generic-parameter shape) rather than introducing `dyn`
//! boxing this task's boundary does not require. Unlike `DeliveryWorker<Q,
//! H>`, there is no generic `Q`/`R` parameter for `ProcessingJobQueue`/
//! `MediaRepository`: both of those components (tasks 3.2/3.1) are already
//! implemented as plain free functions against a bare `&PgPool` (design.md's
//! own Service Interface sketch for both, `enqueue(pool: &PgPool, ...)` /
//! `insert_media(pool: &PgPool, ...)`), never as a trait `DeliveryQueue`-
//! style component — so `ProcessingWorker` simply holds a `PgPool` directly
//! and calls `job_queue::*`/`media_repository::*` the same way
//! `MediaService` (task 4.1) already does, rather than inventing a new trait
//! boundary neither design.md nor any prior task in this spec asked for.
//!
//! ## Transient vs. decode/terminal failure classification (design.md's
//! flowchart, Requirement 6.5)
//! The flowchart draws two distinct non-success edges out of the `Process`
//! step: `-->|transient fail| Retry` and `-->|decode fail| Failed` — a
//! decode/generation failure never goes through the retry-budget check at
//! all, unlike a transient failure. This module's [`attempt`] classifies
//! every failure it can produce into exactly one of [`FailureKind::Transient`]
//! or [`FailureKind::Terminal`]:
//! - **Transient** (retried up to `config.max_retry_attempts`, per the
//!   `Retry` branch): [`MediaStore::get`] failing to fetch the original
//!   object, and [`MediaStore::put`] failing to store the generated
//!   thumbnail. Both are I/O-shaped failures against the storage boundary
//!   (a momentarily-unavailable disk/future-remote-store hiccup, or —
//!   `LocalFsStore::get`'s own documented case — a `404`-shaped "not found
//!   yet" that a retry might outlast if it is actually a benign race) with
//!   no reason to believe the *same* bytes would fail identically on a
//!   later attempt. A [`media_repository::find_by_id`] query failure (a
//!   genuine DB-layer `AppError`, not the "row absent" `Ok(None)` case) is
//!   classified the same way, mirroring `DeliveryWorker::attempt`'s own
//!   documented precedent ("A DB failure... is an infrastructure hiccup,
//!   not a structural... fact -- treat it the same as a failed send
//!   attempt").
//! - **Terminal** (immediately failed, bypassing the retry budget entirely,
//!   per the `Failed` branch reached directly from `Process`):
//!   [`MediaProcessor::process_image`] returning `Err` — this is exactly
//!   design.md's "decode fail" edge (a corrupt/unsupported original can
//!   never decode differently on a later attempt; `PureRustImageProcessor`
//!   is a pure, deterministic function of its input bytes, Requirement 6.4,
//!   so retrying it against the same bytes can only ever reproduce the same
//!   failure). This module additionally classifies `job.media_id` resolving
//!   to no row at all (`find_by_id` returning `Ok(None)`) as Terminal too: a
//!   `media_processing_jobs` row's `media_id REFERENCES media(id)` makes
//!   this structurally near-impossible in current operation (no code path
//!   in this crate ever deletes a `media` row), so — mirroring
//!   `DeliveryWorker::attempt`'s identical `Ok(None)` -> `Unrecoverable`
//!   precedent for its own structurally-impossible-but-still-handled case —
//!   this is handled defensively as an immediate failure rather than
//!   panicking or burning through the retry budget on something retrying
//!   can never fix.
//!
//! To force an immediate terminal failure through [`job_queue::fail_or_retry`]
//! (which only natively distinguishes "below budget" from "at/above
//! budget"), [`process_job`] calls it with `max_attempts = 0` for a
//! [`FailureKind::Terminal`] — see that function's own updated doc comment
//! for why this always takes the `Failed` branch on the very first call
//! regardless of the job's actual prior `attempts` count or the worker's
//! configured `config.max_retry_attempts`.
//!
//! ## The `last_error` diagnostic gap (Requirement 4.5) is resolved here
//! tasks.md's "3.2 レビュー所見" flagged that `fail_or_retry` (task 3.2) had
//! no way to persist `media_processing_jobs.last_error` at all, and that
//! task 4.3 (this task) must supply that write path or Requirement 4.5's
//! "原因特定に十分な診断情報を出力する" would stay permanently unsatisfied.
//! This is resolved two ways, together: (1) `job_queue::fail_or_retry`
//! (task 3.2's module, extended here — see its own doc comment) now accepts
//! and persists an `error_message: &str` into `last_error` on *both* its
//! branches, and [`diagnostic_message`] builds that string from the
//! classified [`FailureKind`]'s underlying [`AppError`] (its `source`, when
//! present — a `Server` error's internal cause, safe to persist in our own
//! DB even though `AppError`'s own `IntoResponse` conversion never leaks
//! `source` to an HTTP caller — else its `public_message`, e.g. a decode
//! error's already-descriptive text); (2) every failure path additionally
//! emits a `tracing::error!`/`tracing::warn!` event carrying the same
//! message plus `job_id`/`media_id` (this crate's established structured-
//! logging convention, `src/error.rs::AppError::log_if_server`'s identical
//! shape), so an operator has both a persisted-and-queryable diagnostic and
//! a log line to correlate against, not just one or the other.
//!
//! ## Idempotency (Requirement 4.6)
//! [`attempt`] loads the claimed job's [`Media`] row first and, if its
//! `state` is no longer [`MediaState::Processing`] (i.e. some earlier claim
//! of this same job — a slow-but-still-alive original worker racing this
//! reclaim, or a duplicate claim under some future concurrency bug —
//! already drove it to `Ready` or `Failed`), returns
//! [`AttemptOutcome::AlreadySettled`] without re-fetching the original,
//! re-running `process_image`, or re-`put`-ing a thumbnail: [`process_job`]
//! then simply calls `job_queue::complete` to retire the now-redundant job
//! row, touching no other state. This is design.md's own documented
//! resolution for the accepted reclaim race ("`complete`/`fail_or_retry`
//! targeting an already-reclaimed-and-since-transitioned row simply affect
//! zero or the wrong-generation row, which `ProcessingWorker`... is
//! responsible for tolerating via idempotent derivative writes keyed off
//! `Media`'s own state" — `job_queue.rs`'s own doc comment, "Postcondition
//! (design.md)") — `Media::state` is the truth source a re-run checks
//! itself against, exactly as design.md's Batch Contract "Idempotency &
//! recovery" note prescribes.
//!
//! ## `run`'s shutdown signal: mirrors `server.rs::serve_with_shutdown_and_signal`
//! [`ProcessingWorker::run`] takes `signal: impl Future<Output = ()> +
//! Send` — the same injectable-future shape `crate::server::
//! serve_with_shutdown_and_signal` already established as this codebase's
//! "graceful shutdown" convention (task 5.2, wiring this worker into the
//! app's actual lifecycle, is expected to pass the same real-or-test signal
//! future it already threads through to the HTTP server). This is
//! deliberately not `federation::module::FederationBackgroundTasks::spawn`'s
//! shape (a detached `tokio::spawn` loop with no shutdown hook at all): this
//! task's own acceptance text explicitly requires `run`'s entry point to
//! "accept a shutdown signal/cancellation mechanism... so that task 5.2 can
//! later wire it into the app's lifecycle without modification", so the
//! more complete `server.rs` precedent is the one this module follows.
//! `run` never itself decides *when* the OS/test-harness fires that signal
//! (`crate::server::os_shutdown_signal`-style real-signal construction is
//! out of this task's boundary, task 5.2's job) — it only reacts to it: no
//! new poll begins once `signal` resolves, and an in-flight `run_once` call
//! is allowed to finish (never aborted mid-job), matching design.md's
//! "graceful shutdown は core-runtime のライフサイクルに従う" note (drain,
//! don't sever). A worker that stops responding entirely (a genuine crash,
//! not a graceful shutdown) is *not* this module's concern at all — no
//! explicit lock-release code runs on that path because none needs to:
//! `job_queue::claim_due`'s own reclaim logic (task 3.2, already
//! implemented) is what notices a lease-expired `processing` job and lets
//! another worker's `run_once` pick it back up, exactly as design.md's
//! Batch Contract "Idempotency & recovery" note describes and as this
//! module's own `worker_reclaims_a_job_whose_lease_expired_after_a_simulated_crash`
//! test proves end to end.

#[cfg(test)]
mod tests;

use std::future::Future;
use std::time::Duration as StdDuration;

use sqlx::postgres::PgPool;
use time::OffsetDateTime;

use crate::config::MediaConfig;
use crate::error::AppError;
use crate::media::job_queue::{self, JobOutcome};
use crate::media::media_repository;
use crate::media::model::{MediaMeta, MediaState, ProcessingJob};
use crate::media::processor::{MediaProcessor, ThumbnailSpec};
use crate::media::store::{MediaStore, ObjectKey};
use crate::runtime::RuntimeContext;

/// Default cadence [`ProcessingWorker::run`]'s caller may pass as
/// `poll_interval` when nothing more specific is configured: how long the
/// resident loop sleeps after an empty `claim_due` before polling again.
/// Neither design.md nor `crate::config::MediaConfig` names a startup
/// setting for this (unlike `lease_duration`/`max_retry_attempts`), so this
/// is this task's own judgment call — short enough that a freshly-enqueued
/// upload's processing starts promptly (a client is actively polling
/// Requirement 2's status endpoint waiting for it), long enough not to
/// hammer the database with an empty `claim_due` query in a tight loop.
pub const DEFAULT_POLL_INTERVAL: StdDuration = StdDuration::from_millis(500);

/// Converts a `std::time::Duration`-typed startup setting
/// (`MediaConfig::lease_duration`) into the `time::Duration` `job_queue::
/// claim_due` actually takes, mirroring `bootstrap.rs`'s identical
/// `time::Duration::seconds(std_duration.as_secs() as i64)` conversion
/// idiom for `FederationConfig::public_key_cache_ttl` (both config fields
/// are only ever parsed from a whole-seconds startup value, `config.rs`'s
/// `parse_secs`, so truncating away any sub-second component here loses
/// nothing a startup config value could ever have carried).
fn lease_duration_as_time_duration(std_duration: StdDuration) -> time::Duration {
    time::Duration::seconds(std_duration.as_secs() as i64)
}

/// Builds a diagnostic string for `err`, suitable for both
/// `media_processing_jobs.last_error` persistence and a `tracing` event
/// field (Requirement 4.5; see this module's doc comment, "The `last_error`
/// diagnostic gap"). A `Server`-kind `AppError` always carries `Some(source)`
/// (`AppError::server`'s own constructor contract) — that internal cause is
/// safe to fold into the message here even though `AppError`'s HTTP
/// `IntoResponse` conversion never lets it reach a caller, because this
/// string only ever lands in our own database/logs, not an HTTP response
/// body. A `Client`-kind `AppError` (e.g. a decode failure's already
/// descriptive `public_message`) never carries a `source`
/// (`AppError::client`'s own constructor contract), so `public_message`
/// alone is already the full diagnostic in that case.
fn diagnostic_message(err: &AppError) -> String {
    match &err.source {
        Some(source) => format!("{}: {source}", err.public_message),
        None => err.public_message.clone(),
    }
}

/// A failure [`ProcessingWorker::attempt`] classified into one of the two
/// design.md flowchart edges out of `Process` — see this module's doc
/// comment ("Transient vs. decode/terminal failure classification") for the
/// exact rule.
enum FailureKind {
    /// The `-->|transient fail| Retry` edge: worth retrying up to
    /// `config.max_retry_attempts` (Requirement 4.4).
    Transient(AppError),
    /// The `-->|decode fail| Failed` edge: never worth retrying
    /// (Requirement 6.5). See [`ProcessingWorker::process_job`] for how this
    /// forces an immediate `job_queue::fail_or_retry` terminal outcome.
    Terminal(AppError),
}

/// What [`ProcessingWorker::attempt`] found once it loaded the claimed
/// job's [`crate::media::model::Media`] row and, if still `Processing`,
/// finished processing it.
enum AttemptOutcome {
    /// The media was already `Ready` or `Failed` by the time this claim was
    /// resolved (Requirement 4.6's idempotency case — see this module's doc
    /// comment). Nothing was re-fetched, re-processed, or re-stored.
    AlreadySettled,
    /// A fresh, successful processing pass: the derivative was generated
    /// and stored; the caller ([`ProcessingWorker::process_job`]) still owns
    /// applying `media_repository::set_ready`/`job_queue::complete`.
    Processed {
        meta: MediaMeta,
        blurhash: String,
        thumb_key: ObjectKey,
    },
}

/// [`ProcessingWorker::run_once`]'s per-job result, for callers (this
/// module's own tests, primarily) that want to observe exactly what
/// happened without re-querying `media`/`media_processing_jobs` directly —
/// mirroring `federation::outbound::worker::WorkerRunSummary`'s identical
/// "design.md gives this component only a Batch contract, no concrete
/// return shape, so this is this task's own choice" rationale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerOutcome {
    /// The claimed job's media reached (or already was in)
    /// [`MediaState::Ready`], and the job row was retired via
    /// `job_queue::complete`.
    Completed,
    /// A transient failure was recorded and the job was rescheduled with
    /// backoff (`job_queue::JobOutcome::Retried`).
    Retried,
    /// The job (and its media) reached a terminal failure — either the
    /// retry budget was exhausted on a transient failure, or a decode/
    /// non-retryable failure forced immediate termination.
    Failed,
}

/// The resident DB-queue-consuming worker (design.md's `ProcessingWorker`,
/// task 4.3). See this module's doc comment for the full contract.
pub struct ProcessingWorker<S: MediaStore, P: MediaProcessor> {
    pool: PgPool,
    runtime: RuntimeContext,
    config: MediaConfig,
    store: S,
    processor: P,
}

impl<S: MediaStore, P: MediaProcessor> ProcessingWorker<S, P> {
    /// Builds a worker against `pool` (passed through to `job_queue`/
    /// `media_repository`, mirroring `MediaService::new`'s identical
    /// shape), `runtime` (the injected clock boundary this worker's own
    /// "now" always comes from — see [`Self::run_once`]), `config`
    /// (`AppConfig.media`: `lease_duration`, `max_retry_attempts`,
    /// `thumbnail_target_width`/`height`), `store` (original fetch +
    /// thumbnail storage), and `processor` (decode/resize/BlurHash).
    pub fn new(
        pool: PgPool,
        runtime: RuntimeContext,
        config: MediaConfig,
        store: S,
        processor: P,
    ) -> Self {
        Self {
            pool,
            runtime,
            config,
            store,
            processor,
        }
    }

    /// Claims and fully resolves at most one due job (design.md's Batch
    /// Contract Trigger: "常駐ループ（`run_at <= now` のジョブ出現時）"),
    /// returning `Ok(None)` when `job_queue::claim_due` found nothing due.
    /// Independently callable, no internal timer — the same "caller
    /// controls cadence" shape `DeliveryWorker::run_once` already
    /// established in this crate; [`Self::run`] is the actual resident loop
    /// built on top of this.
    ///
    /// Reads [`crate::runtime::Clock::now`] exactly once per call and reuses
    /// that single value for `claim_due`'s due-check and for every
    /// subsequent `set_ready`/`set_failed`/`fail_or_retry` timestamp this
    /// call performs (mirroring `DeliveryWorker::run_once`'s identical "one
    /// clock read per call" documented convention) — never a second,
    /// inconsistent wall-clock-adjacent read partway through resolving one
    /// job.
    pub async fn run_once(&self) -> Result<Option<WorkerOutcome>, AppError> {
        let now = self.runtime.clock.now();
        let lease_duration = lease_duration_as_time_duration(self.config.lease_duration);

        let job = job_queue::claim_due(&self.pool, now, lease_duration).await?;
        match job {
            Some(job) => self.process_job(job, now).await.map(Some),
            None => Ok(None),
        }
    }

    /// Runs the resident poll loop (design.md's "常駐ループでジョブを排他
    /// 取得し...という流れを実行する") until `signal` resolves: repeatedly
    /// calls [`Self::run_once`], looping again immediately when a job was
    /// found (to drain a backlog promptly) or sleeping `poll_interval`
    /// (interruptibly — a pending shutdown is observed even mid-sleep, never
    /// only between whole iterations) when nothing was due. A `run_once`
    /// error (an infrastructure failure — the job's own outcome is always
    /// resolved one way or another before `run_once` can return `Err`) is
    /// logged and treated the same as an empty poll: back off for
    /// `poll_interval`, then try again, rather than tearing down the whole
    /// resident loop over one transient DB hiccup.
    ///
    /// See this module's doc comment ("`run`'s shutdown signal") for why
    /// `signal`'s shape mirrors `crate::server::serve_with_shutdown_and_signal`
    /// rather than `federation::module::FederationBackgroundTasks::spawn`'s
    /// detached-with-no-shutdown-hook shape. An in-flight `run_once` call is
    /// always allowed to finish before this function returns — shutdown is
    /// only ever observed between iterations (including mid-sleep), never by
    /// aborting work already in progress.
    pub async fn run(&self, poll_interval: StdDuration, signal: impl Future<Output = ()> + Send) {
        tokio::pin!(signal);

        loop {
            let outcome = tokio::select! {
                _ = &mut signal => {
                    tracing::info!(
                        "processing worker received shutdown signal; stopping poll loop"
                    );
                    return;
                }
                result = self.run_once() => result,
            };

            let should_poll_again_immediately = match outcome {
                Ok(Some(_)) => true,
                Ok(None) => false,
                Err(err) => {
                    tracing::error!(
                        error = ?err,
                        "processing worker poll iteration failed; will retry after the poll interval"
                    );
                    false
                }
            };

            if should_poll_again_immediately {
                continue;
            }

            tokio::select! {
                _ = &mut signal => {
                    tracing::info!(
                        "processing worker received shutdown signal; stopping poll loop"
                    );
                    return;
                }
                _ = tokio::time::sleep(poll_interval) => {}
            }
        }
    }

    /// Resolves one already-claimed `job` end to end: applies whatever
    /// [`Self::attempt`] found (idempotent skip, or a fresh success) or the
    /// [`FailureKind`] it classified, and returns the resulting
    /// [`WorkerOutcome`]. See this module's doc comment for the terminal-
    /// vs-transient forcing rule and the `last_error`/`tracing` diagnostic
    /// resolution.
    async fn process_job(
        &self,
        job: ProcessingJob,
        now: OffsetDateTime,
    ) -> Result<WorkerOutcome, AppError> {
        match self.attempt(&job).await {
            Ok(AttemptOutcome::AlreadySettled) => {
                job_queue::complete(&self.pool, job.id).await?;
                tracing::debug!(
                    job_id = job.id.as_i64(),
                    media_id = job.media_id.as_i64(),
                    "processing job's media had already settled (Ready/Failed); completed the \
                     now-redundant job without reprocessing (Requirement 4.6 idempotency)"
                );
                Ok(WorkerOutcome::Completed)
            }
            Ok(AttemptOutcome::Processed {
                meta,
                blurhash,
                thumb_key,
            }) => {
                media_repository::set_ready(
                    &self.pool,
                    job.media_id,
                    &meta,
                    &blurhash,
                    thumb_key.as_str(),
                    now,
                )
                .await?;
                job_queue::complete(&self.pool, job.id).await?;
                tracing::info!(
                    job_id = job.id.as_i64(),
                    media_id = job.media_id.as_i64(),
                    "processing job completed; media is now ready with derivatives stored"
                );
                Ok(WorkerOutcome::Completed)
            }
            Err(FailureKind::Transient(err)) => {
                let message = diagnostic_message(&err);
                let outcome = job_queue::fail_or_retry(
                    &self.pool,
                    &job,
                    self.config.max_retry_attempts,
                    now,
                    &message,
                )
                .await?;
                match outcome {
                    JobOutcome::Retried => {
                        tracing::warn!(
                            job_id = job.id.as_i64(),
                            media_id = job.media_id.as_i64(),
                            error = %message,
                            "processing job failed transiently; scheduled for retry with backoff"
                        );
                        Ok(WorkerOutcome::Retried)
                    }
                    JobOutcome::Failed => {
                        media_repository::set_failed(&self.pool, job.media_id, now).await?;
                        tracing::error!(
                            job_id = job.id.as_i64(),
                            media_id = job.media_id.as_i64(),
                            error = %message,
                            "processing job exhausted its retry budget; media marked failed"
                        );
                        Ok(WorkerOutcome::Failed)
                    }
                }
            }
            Err(FailureKind::Terminal(err)) => {
                let message = diagnostic_message(&err);
                // `max_attempts = 0` forces the `Failed` branch on this
                // very first call, bypassing the retry budget entirely --
                // see `job_queue::fail_or_retry`'s own updated doc comment
                // and this module's doc comment ("Transient vs.
                // decode/terminal failure classification").
                let outcome = job_queue::fail_or_retry(&self.pool, &job, 0, now, &message).await?;
                debug_assert_eq!(
                    outcome,
                    JobOutcome::Failed,
                    "max_attempts = 0 must always terminate on the first call"
                );
                media_repository::set_failed(&self.pool, job.media_id, now).await?;
                tracing::error!(
                    job_id = job.id.as_i64(),
                    media_id = job.media_id.as_i64(),
                    error = %message,
                    "processing job failed with a non-retryable error; media marked failed \
                     immediately without consuming the retry budget"
                );
                Ok(WorkerOutcome::Failed)
            }
        }
    }

    /// Performs the actual processing attempt for `job`: loads its
    /// [`crate::media::model::Media`] row (unscoped by actor —
    /// `media_repository::find_by_id`, this task's own addition, see that
    /// function's doc comment), short-circuits if it is no longer
    /// `Processing` (idempotency), else fetches the original object,
    /// processes it, and stores the thumbnail derivative. Never itself
    /// touches `job_queue`/applies a `media_repository::set_ready`/
    /// `set_failed` state transition -- that is [`Self::process_job`]'s job,
    /// keeping "what happened" separate from "what state transition
    /// follows" (mirroring `DeliveryWorker::attempt`/`process_job`'s
    /// identical split).
    async fn attempt(&self, job: &ProcessingJob) -> Result<AttemptOutcome, FailureKind> {
        let media = media_repository::find_by_id(&self.pool, job.media_id)
            .await
            .map_err(FailureKind::Transient)?;

        let media = match media {
            Some(media) => media,
            // Structurally near-impossible (`media_processing_jobs.media_id
            // REFERENCES media(id)`, and nothing in this crate ever deletes
            // a `media` row) -- handled defensively as an immediate,
            // non-retryable failure rather than a panic. See this module's
            // doc comment for why this is classified `Terminal`.
            None => {
                return Err(FailureKind::Terminal(AppError::client(
                    axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                    format!(
                        "media {} referenced by processing job {} no longer exists",
                        job.media_id.as_i64(),
                        job.id.as_i64()
                    ),
                )));
            }
        };

        if media.state != MediaState::Processing {
            // Requirement 4.6: already `Ready` or `Failed` -- nothing to
            // redo. See this module's doc comment ("Idempotency").
            return Ok(AttemptOutcome::AlreadySettled);
        }

        let original_key = ObjectKey::original(job.media_id);
        let original_bytes = self
            .store
            .get(&original_key)
            .await
            .map_err(FailureKind::Transient)?;

        let thumb_target = ThumbnailSpec::new(
            self.config.thumbnail_target_width,
            self.config.thumbnail_target_height,
        );
        let processed = self
            .processor
            .process_image(&original_bytes, thumb_target)
            .map_err(FailureKind::Terminal)?;

        let thumb_key = ObjectKey::small(job.media_id);
        self.store
            .put(&thumb_key, &processed.thumbnail, &processed.content_type)
            .await
            .map_err(FailureKind::Transient)?;

        let meta = MediaMeta {
            original: processed.original_dims,
            small: Some(processed.thumbnail_dims),
        };

        Ok(AttemptOutcome::Processed {
            meta,
            blurhash: processed.blurhash,
            thumb_key,
        })
    }
}
