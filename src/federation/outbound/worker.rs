//! `DeliveryWorker` (design.md `#### DeliveryQueue / DeliveryWorker` ->
//! Responsibilities & Constraints, and the "配送ワーカーと double-knock"
//! System Flow; Requirements 1.1, 3.1, 3.2, 3.3, 11.2, 11.3, 11.5; task 4.3,
//! `Boundary: DeliveryWorker`): picks up due [`super::queue::DeliveryJob`]s
//! from [`super::queue::DeliveryQueue`], sends each one as a signed HTTP
//! request via [`SignatureNegotiator`] (which internally handles the
//! known-format-first / double-knock-retry negotiation, Requirements 3.1,
//! 3.2, 3.3), and applies this task's own retry/permanent-failure policy to
//! whatever the send attempt reports (Requirements 11.2, 11.3, 11.5).
//!
//! ## Scope
//! This module owns exactly [`DeliveryWorker`] and its single entry point,
//! [`DeliveryWorker::run_once`]: claiming due jobs, building the outbound
//! request, resolving the sender's signing identity, delegating to
//! [`SignatureNegotiator::negotiate_and_send`], and deciding — from that
//! call's result alone — whether a job is done, rescheduled, or permanently
//! failed. It does not implement `SignatureNegotiator` (task 2.4), `RequestSigner`
//! (task 2.2), or `DeliveryQueue` (task 3.3), all already implemented and
//! composed here unchanged. It is not wired into `FederationModule`/
//! bootstrap/`AppState` (task 5.4, out of this task's boundary) — this is a
//! standalone, independently-callable component with no live caller yet.
//!
//! ## The `Id -> Handle` gap: `ActorDirectory::resolve_actor_by_id`
//! [`super::queue::DeliveryJob::sender_actor_id`] is a plain
//! [`crate::domain::Id`] (design.md's physical schema:
//! `delivery_jobs.sender_actor_id BIGINT`), but both
//! [`SignatureNegotiator::negotiate_and_send`] and
//! [`crate::federation::signatures::RequestSigner::sign_request`] take
//! `actor: &Handle`. No existing boundary in this spec resolves
//! `Id -> Handle`: design.md's Allowed Dependencies section for
//! federation-core names only `ActorDirectory::resolve_actor_by_handle`
//! (`Handle -> ResolvedActor`) and `actor_public_key` (`Id ->
//! ActorPublicKey`), and no other component in this codebase exposes the
//! reverse-of-`resolve_actor_by_handle` operation this worker structurally
//! needs to be buildable at all.
//!
//! This module resolves the gap by depending on a new, narrow sibling
//! method, [`crate::actor::ActorDirectory::resolve_actor_by_id`], added by
//! this same task directly onto `ActorDirectory` in actor-model (not
//! reimplemented here, not a new port/trait in this module). This mirrors
//! `resolve_actor_by_handle`'s exact shape and contract (owner-free
//! `ResolvedActor`, `Ok(None)` for absence, delegating to the
//! already-implemented `repository::find_by_id`) and follows the same
//! "narrow upstream addition to an already-implemented, already-reviewed
//! component" precedent api-foundation's task 4.1 already established for
//! this exact component (`ActorDirectory::sole_owner`) — see
//! `src/actor/directory.rs`'s own doc comment (`resolve_actor_by_id`
//! section) for the full reasoning. `DeliveryWorker` holds a plain
//! `Arc<ActorDirectory>` (not a narrow local trait, unlike
//! `target.rs`'s `LocalActorLookup`): this worker's own integration tests
//! already require a real Postgres-backed `DeliveryQueue` and
//! `SignatureNegotiator` (both need a real `PgPool`), so there is no
//! DB-avoidance test concern to justify a mockable indirection here — the
//! same reasoning `RequestSigner` (task 2.2) already used to justify holding
//! `Arc<ActorDirectory>` directly for its own mirror-image `Handle -> Id`
//! need.
//!
//! A local actor's row can only ever be deactivated in this spec, never
//! deleted (`crate::actor::ActorState` has no "deleted" variant, and no
//! actor-model operation removes a `local_actors` row) — so
//! `resolve_actor_by_id` returning `Ok(None)` for a job's `sender_actor_id`
//! should be structurally impossible in current operation. This module
//! still handles it defensively rather than panicking: see
//! [`AttemptOutcome::Unrecoverable`]'s doc comment for why that case is
//! treated as an immediate permanent failure rather than burning through the
//! retry budget on something retrying can never fix.
//!
//! ## State-transition policy (Requirements 11.2, 11.3, 11.5)
//! For each claimed job, [`DeliveryWorker::attempt`] resolves to one of three
//! outcomes ([`AttemptOutcome`]), and [`DeliveryWorker::process_job`] applies
//! exactly one `DeliveryQueue` state transition per outcome:
//! - **Delivered** (the negotiated send's final [`HttpResponse::status`] is
//!   2xx) -> `mark_done`.
//! - **Retryable** (a non-2xx final response, or a transport-level `Err`
//!   from `negotiate_and_send` — see that method's own doc comment for why
//!   both are "not signature-related, caller decides retry policy") -> if
//!   the job's incremented `attempts` is still below
//!   [`super::queue::DEFAULT_MAX_DELIVERY_ATTEMPTS`], `reschedule` with
//!   `next_attempt_at = now + backoff_delay(new_attempts)` (Requirement
//!   11.3); otherwise `mark_failed` instead of rescheduling again
//!   (Requirement 11.5).
//! - **Unrecoverable** (the sender no longer resolves at all — see this
//!   module's doc comment above) -> `mark_failed` immediately, regardless of
//!   the current `attempts` count.
//!
//! ## `now`: one clock read per `run_once` call
//! [`DeliveryWorker::run_once`] reads [`Clock::now`] exactly once, using
//! that same value both as `claim_due`'s `now` and as the base for every
//! `reschedule`'s `next_attempt_at` computed during that call — never a
//! second, independent wall-clock-adjacent read partway through a batch,
//! keeping one `run_once` call's notion of "now" internally consistent and
//! fully attributable to its injected [`Clock`] (steering's "clock is always
//! injected, never read twice inconsistently" determinism convention).
//!
//! ## `host_from_url`: a third duplicate of the same URL-authority parser
//! `signer.rs`'s own doc comment (task 1.5's Implementation Note) already
//! flags this exact string-parsing helper as duplicated once, from
//! `suite.rs`'s `path_and_query`. This module needs the same
//! `host[:port]`-from-absolute-URL extraction (to build `negotiate_and_send`'s
//! `host: &str` argument from `DeliveryJob::target_inbox`) and `signer.rs`'s
//! own copy is private to that module, so this is a third, independent copy
//! rather than either a cross-module refactor (out of this task's narrow
//! boundary) or a dependency on a private helper. If a fourth site ever needs
//! this, extracting a small shared `federation::urls`-adjacent helper is
//! worth reconsidering (flagged here, not acted on, per this task's own
//! boundary).
//!
//! ## Request body: `job.activity` is serialized as-is, `@context` already stamped
//! [`super::sink::CanonicalActivity::as_value`]'s own doc comment states the
//! JSON it exposes (and that `HttpDeliverySink` persists onto
//! `NewDeliveryJob::activity`) "already carr[ies] the stamped `@context`" —
//! `DeliveryService::deliver`'s one-time `JsonLdCodec` serialize step ran
//! before the job was ever enqueued. This worker therefore serializes
//! `job.activity` directly via `serde_json::to_vec` for the request body,
//! never re-running `crate::federation::jsonld::serialize` (which would
//! stamp a second, redundant/conflicting `@context`).

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::http::Method;
use time::OffsetDateTime;

use super::queue::{DEFAULT_MAX_DELIVERY_ATTEMPTS, DeliveryJob, DeliveryQueue, backoff_delay};
use crate::actor::ActorDirectory;
use crate::error::AppError;
use crate::federation::signatures::{FederationHttpClient, OutboundRequest, SignatureNegotiator};
use crate::runtime::Clock;

/// Extracts the `host[:port]` authority portion of an absolute URL, e.g.
/// `"https://example.com/inbox?x=1"` -> `"example.com"`, for
/// `SignatureNegotiator::negotiate_and_send`'s `host` argument. See this
/// module's doc comment ("`host_from_url`: a third duplicate...") for why
/// this is a deliberate, documented duplicate of `signer.rs`'s private
/// helper of the same name/shape rather than a cross-module refactor.
fn host_from_url(url: &str) -> &str {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    &after_scheme[..end]
}

/// The outcome of a single delivery attempt ([`DeliveryWorker::attempt`]),
/// before any `DeliveryQueue` state transition is applied. See this
/// module's doc comment ("State-transition policy") for the exact
/// transition each variant drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttemptOutcome {
    /// The negotiated send's final response was 2xx.
    Delivered,
    /// The negotiated send's final response was non-2xx, or
    /// `negotiate_and_send` itself returned a transport-level `Err` —
    /// either way, not signature-related, and worth retrying per this
    /// worker's own backoff/attempts-limit policy (Requirements 11.3, 11.5).
    Retryable,
    /// The job's `sender_actor_id` no longer resolves to an existing local
    /// actor at all (see this module's doc comment for why this should be
    /// structurally impossible under current actor-model scope, since
    /// actors are only ever deactivated, never deleted). Retrying this job
    /// can never succeed — no signature format or backoff delay changes
    /// whether a nonexistent local actor resolves — so it is treated as an
    /// immediate permanent failure rather than consuming the job's retry
    /// budget on something retrying structurally cannot fix.
    Unrecoverable,
}

/// The actual `DeliveryQueue` state transition [`DeliveryWorker::process_job`]
/// applied to a job — distinct from [`AttemptOutcome`] because an
/// `AttemptOutcome::Retryable` attempt resolves to *either*
/// [`JobFinalState::Rescheduled`] or [`JobFinalState::Failed`] depending on
/// whether the incremented `attempts` count has reached
/// [`DEFAULT_MAX_DELIVERY_ATTEMPTS`] (Requirements 11.3, 11.5) — see this
/// module's doc comment ("State-transition policy").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobFinalState {
    /// `mark_done` fired: the send attempt succeeded.
    Done,
    /// `reschedule` fired: a transient failure, still within the attempts
    /// budget.
    Rescheduled,
    /// `mark_failed` fired: either the attempts budget was exhausted, or the
    /// job was [`AttemptOutcome::Unrecoverable`].
    Failed,
}

/// Per-[`DeliveryWorker::run_once`]-call counts of how each claimed job was
/// resolved (this module's own concrete shape — design.md gives this
/// component only as a Batch contract, no return-value type, so this is
/// this task's own choice, mirroring how `Recipient`/`PageCursor`/
/// `CanonicalActivity` were each owned by the task that first needed a
/// concrete shape design.md only described in prose). Lets a caller (a
/// future poller, or a test) observe exactly what happened without
/// re-querying `delivery_jobs` directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WorkerRunSummary {
    /// How many jobs `claim_due` returned this call.
    pub claimed: usize,
    /// How many of those jobs ended up `mark_done`.
    pub done: usize,
    /// How many of those jobs ended up `reschedule`d.
    pub rescheduled: usize,
    /// How many of those jobs ended up `mark_failed` (attempts exhausted, or
    /// an unrecoverable sender resolution — see [`AttemptOutcome::Unrecoverable`]).
    pub failed: usize,
}

/// Picks up due delivery jobs and drives each through a signed HTTP send
/// attempt and this task's own retry/permanent-failure policy (design.md's
/// `DeliveryWorker` component; Requirements 1.1, 3.1, 3.2, 3.3, 11.2, 11.3,
/// 11.5). See this module's doc comment for the full contract, the
/// `Id -> Handle` gap resolution, and the state-transition policy.
pub struct DeliveryWorker<Q, H>
where
    Q: DeliveryQueue,
    H: FederationHttpClient,
{
    queue: Q,
    negotiator: SignatureNegotiator<H>,
    clock: Arc<dyn Clock>,
    directory: Arc<ActorDirectory>,
}

impl<Q, H> DeliveryWorker<Q, H>
where
    Q: DeliveryQueue,
    H: FederationHttpClient,
{
    /// Builds a worker against `queue` (job persistence/claim/state
    /// transitions), `negotiator` (signed send + double-knock negotiation),
    /// `clock` (this call's "now" — see this module's doc comment, "`now`:
    /// one clock read per `run_once` call"), and `directory` (this task's
    /// own `Id -> Handle` resolution need — see this module's doc comment).
    pub fn new(
        queue: Q,
        negotiator: SignatureNegotiator<H>,
        clock: Arc<dyn Clock>,
        directory: Arc<ActorDirectory>,
    ) -> Self {
        Self {
            queue,
            negotiator,
            clock,
            directory,
        }
    }

    /// Claims up to `limit` due jobs and drives each one through exactly one
    /// send attempt and state transition (design.md's `DeliveryWorker` Batch
    /// contract). Independently callable per call — no internal polling
    /// loop or timer — so a caller (a future poller, task 5.4, or this
    /// module's own tests) controls exactly when and how many jobs a single
    /// pass processes.
    ///
    /// Propagates the first `AppError` any underlying `DeliveryQueue`
    /// call (`claim_due`/`mark_done`/`reschedule`/`mark_failed`) returns,
    /// immediately, without attempting to paper over a genuine
    /// infrastructure failure — any job already resolved earlier in this
    /// same call keeps whatever state it was already transitioned to; any
    /// job not yet reached remains claimed (`'in_progress'`) for a future
    /// recovery pass (design.md's own Idempotency & recovery note: "プロセス
    /// 再起動で未完了ジョブを再取得").
    pub async fn run_once(&self, limit: i64) -> Result<WorkerRunSummary, AppError> {
        let now = self.clock.now();
        let jobs = self.queue.claim_due(limit, now).await?;

        let mut summary = WorkerRunSummary {
            claimed: jobs.len(),
            ..WorkerRunSummary::default()
        };

        for job in jobs {
            match self.process_job(job, now).await? {
                JobFinalState::Done => summary.done += 1,
                JobFinalState::Rescheduled => summary.rescheduled += 1,
                JobFinalState::Failed => summary.failed += 1,
            }
        }

        Ok(summary)
    }

    /// Drives a single already-claimed `job` through [`Self::attempt`] and
    /// applies the resulting [`AttemptOutcome`]'s `DeliveryQueue` state
    /// transition (this module's doc comment, "State-transition policy"),
    /// returning the job's actual final state. This is deliberately a
    /// distinct type from [`AttemptOutcome`]: an `AttemptOutcome::Retryable`
    /// attempt may still end up `mark_failed` (attempts exhausted) rather
    /// than rescheduled, so [`Self::run_once`]'s summary counts must be keyed
    /// off what state transition actually fired, not the intermediate
    /// attempt classification.
    async fn process_job(
        &self,
        job: DeliveryJob,
        now: OffsetDateTime,
    ) -> Result<JobFinalState, AppError> {
        match self.attempt(&job).await {
            AttemptOutcome::Delivered => {
                self.queue.mark_done(job.id).await?;
                Ok(JobFinalState::Done)
            }
            AttemptOutcome::Unrecoverable => {
                self.queue.mark_failed(job.id).await?;
                Ok(JobFinalState::Failed)
            }
            AttemptOutcome::Retryable => {
                let new_attempts = job.attempts + 1;
                if new_attempts >= DEFAULT_MAX_DELIVERY_ATTEMPTS {
                    self.queue.mark_failed(job.id).await?;
                    Ok(JobFinalState::Failed)
                } else {
                    let next_attempt_at = now + backoff_delay(new_attempts);
                    self.queue
                        .reschedule(job.id, next_attempt_at, new_attempts)
                        .await?;
                    Ok(JobFinalState::Rescheduled)
                }
            }
        }
    }

    /// Performs the send attempt itself: resolves `job.sender_actor_id` to a
    /// signable `Handle` (this module's own `Id -> Handle` gap resolution),
    /// builds the outbound signed-delivery request, and classifies
    /// `SignatureNegotiator::negotiate_and_send`'s result into an
    /// [`AttemptOutcome`]. Never itself touches `DeliveryQueue` — that is
    /// [`Self::process_job`]'s job, keeping "what happened" separate from
    /// "what state transition follows".
    async fn attempt(&self, job: &DeliveryJob) -> AttemptOutcome {
        let resolved_sender = match self
            .directory
            .resolve_actor_by_id(job.sender_actor_id)
            .await
        {
            Ok(Some(resolved)) => resolved,
            Ok(None) => return AttemptOutcome::Unrecoverable,
            // A DB failure resolving the sender is an infrastructure hiccup,
            // not a structural "this can never work" fact (unlike `Ok(None)`
            // above) -- treat it the same as a failed send attempt so it
            // gets a bounded number of retries rather than giving up
            // immediately on what may be a transient outage.
            Err(_resolution_error) => return AttemptOutcome::Retryable,
        };

        let host = host_from_url(&job.target_inbox).to_string();
        // `job.activity` is a `serde_json::Value` that already round-tripped
        // through `JsonLdCodec`'s serialize/parse validation before this job
        // was ever enqueued (see this module's doc comment, "Request body");
        // a `Value` built that way can never contain a non-finite float or
        // non-string map key, the only two things that make
        // `serde_json::to_vec` fail, so this cannot actually fail in
        // practice.
        let body = serde_json::to_vec(&job.activity)
            .expect("a previously JSON-round-tripped Value must always re-serialize");
        let request = OutboundRequest::new(Method::POST, job.target_inbox.clone()).with_body(body);

        match self
            .negotiator
            .negotiate_and_send(&resolved_sender.handle, &host, request)
            .await
        {
            Ok(response) if response.status.is_success() => AttemptOutcome::Delivered,
            Ok(_non_success_response) => AttemptOutcome::Retryable,
            Err(_transport_error) => AttemptOutcome::Retryable,
        }
    }
}
