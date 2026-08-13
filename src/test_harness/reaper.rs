//! Resident reclaim executor (HarnessReaper boundary, test-infrastructure
//! Requirements 2.1-2.5, design.md's "Components and Interfaces" ->
//! "Test Infrastructure" -> "HarnessReaper").
//!
//! ## Why a resident executor at all
//! A fixture's `Drop` runs synchronously, typically on a `#[tokio::test]`
//! runtime that is *about to be destroyed*: the release work it needs done
//! (`pool.close()`, then `DROP SCHEMA`) is asynchronous, and anything the
//! destructor spawns onto that dying runtime never runs to completion. That
//! is the exact mechanism behind this spec's connection leak — 750
//! `spawn_test_app` calls against 619 `cleanup()` calls, with `Drop`
//! releasing not one connection.
//!
//! This module moves the *execution* of release off the caller's runtime
//! entirely. [`HarnessReaper::global`] owns one Tokio runtime on one
//! dedicated thread, created at most once per process and deliberately never
//! shut down; [`HarnessReaper::submit`] only hands a
//! [`ReclaimRequest`] to that runtime over an unbounded channel. Sending is a
//! lock-free push that cannot block and cannot panic, which is what makes it
//! safe to call from inside `Drop` (a panic there, during an unwind, aborts
//! the process). The submitted work then outlives the caller's runtime by
//! construction, because it was never on it.
//!
//! ## Why the caller's dead runtime does not matter
//! The pool handed over is a *clone*. `sqlx`'s `Pool` clones share one inner
//! state, so closing the clone the reaper holds closes the pool the (already
//! dropped) test held. The sockets those idle connections own were registered
//! with the caller's now-dead I/O driver, so shutting them down politely may
//! fail or stall rather than complete; that is expected and handled — see
//! [`RECLAIM_STEP_TIMEOUT`] — and never blocks the *next* request, because a
//! failed close still frees the server-side backend once the process-wide
//! socket is torn down, and the schema drop proceeds regardless.
//!
//! ## Failure policy (design.md: "解放の成否でテストを失敗させない")
//! Nothing in this module returns an error to a caller or panics. Every
//! failure — a full channel receiver gone, a stuck close, a `DROP SCHEMA`
//! that could not be issued — is reported on stderr and abandoned. Whatever
//! this module fails to reclaim stays a stale schema, which is precisely the
//! residue `OrphanSchemaSweeper` (task 2.2) exists to collect on a later run.

use std::sync::OnceLock;
use std::time::Duration;

use sqlx::postgres::PgPool;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

#[cfg(test)]
mod tests;

/// Upper bound on each individual reclaim step (closing one pool, dropping
/// one schema). Neither step is expected to come anywhere near it; the bound
/// exists because both can legitimately hang forever in this module's normal
/// operating conditions — `Pool::close` awaits connections whose I/O driver
/// the caller already destroyed, and `DROP SCHEMA ... CASCADE` blocks on
/// locks still held by a backend that has not noticed its client is gone.
/// Without the bound, one such request would stall every request queued
/// behind it and the leak this spec exists to fix would come back in a new
/// shape.
const RECLAIM_STEP_TIMEOUT: Duration = Duration::from_secs(15);

/// Maximum number of requests taken off the channel per iteration. Batching
/// exists so that a burst of leaked fixtures gets its *pools closed* — the
/// half that actually frees connections, and the fast half — before the
/// reaper spends time on the batch's `DROP SCHEMA` statements, rather than
/// interleaving one slow drop between every two closes.
const RECLAIM_BATCH_SIZE: usize = 32;

/// Name given to the reaper's dedicated thread, so it is identifiable in a
/// backtrace or a `ps` listing as test scaffolding rather than a stray
/// application thread.
const REAPER_THREAD_NAME: &str = "kawasemi-harness-reaper";

/// A single reclaim unit: a clone of the pool to close and the isolated
/// schema to drop once it is closed. Ordering between the two is the whole
/// point of bundling them — dropping a schema while a pool still holds
/// connections pinned to it is what `TestApp::cleanup`'s own ordering avoids,
/// and the reaper must not lose that guarantee just because it runs later.
pub(crate) struct ReclaimRequest {
    pool: PgPool,
    schema: String,
}

impl ReclaimRequest {
    /// `pool` should be a clone; the submitter is free to drop its own handle
    /// immediately afterwards. `schema` is the isolated schema that pool is
    /// pinned to (always [`super::unique_schema_name`] output).
    pub(crate) fn new(pool: PgPool, schema: String) -> Self {
        Self { pool, schema }
    }
}

/// The process-wide reclaim executor. Obtained through
/// [`HarnessReaper::global`]; never constructed directly by callers, because
/// a second instance would mean a second resident runtime and thread with no
/// one to own them.
pub(crate) struct HarnessReaper {
    /// Multi-producer sending half. Unbounded on purpose: a bounded channel
    /// would force `submit` to either block (forbidden inside `Drop`) or drop
    /// requests under exactly the burst conditions — many fixtures released
    /// at once — that this component exists to survive.
    sender: UnboundedSender<ReclaimRequest>,
}

impl HarnessReaper {
    /// Returns the process's one reaper, starting its thread and runtime on
    /// the first call.
    ///
    /// The returned reference is `'static` and the runtime behind it is
    /// intentionally never shut down: it must stay usable for the destructor
    /// of the very last fixture in the process, and there is no point in the
    /// lifetime of a test binary at which it is provably safe to stop it.
    /// Requests still queued when the process exits are simply lost, and the
    /// startup sweep (task 2.2) reclaims their schemas on a later run.
    pub(crate) fn global() -> &'static HarnessReaper {
        static REAPER: OnceLock<HarnessReaper> = OnceLock::new();
        REAPER.get_or_init(HarnessReaper::start)
    }

    /// Starts the dedicated thread and its runtime, returning the handle that
    /// feeds them.
    ///
    /// If the thread cannot be spawned at all, this still returns a usable
    /// (if inert) reaper rather than panicking: `global()` is reached from
    /// destructors, so a failure here must degrade to "reclaims nothing, says
    /// so on stderr", never to a panic during an unwind. Sends against the
    /// resulting orphaned channel fail the same way, and report it.
    fn start() -> HarnessReaper {
        let (sender, receiver) = mpsc::unbounded_channel();
        let spawned = std::thread::Builder::new()
            .name(REAPER_THREAD_NAME.to_string())
            .spawn(move || run_reaper_thread(receiver));
        if let Err(err) = spawned {
            eprintln!(
                "test_harness: failed to start the reclaim reaper thread ({err}); leaked pools \
                 and schemas will be left for a future startup sweep"
            );
        }
        HarnessReaper { sender }
    }

    /// Hands `request` to the resident runtime.
    ///
    /// Never blocks and never panics, so it is safe to call from inside
    /// `Drop` (design.md: "`Drop` から呼ばれるため、送信操作はブロックせず、
    /// パニックしてはならない"). Returns as soon as the request is queued;
    /// the reclaim itself completes later, independently of whether the
    /// caller's runtime still exists.
    ///
    /// Submitting a pool that some other path already closed is harmless:
    /// `Pool::close` is idempotent and `DROP SCHEMA` is issued with
    /// `IF EXISTS`.
    pub(crate) fn submit(&self, request: ReclaimRequest) {
        if let Err(err) = self.sender.send(request) {
            // Only reachable if the reaper thread never started or has died,
            // since the receiver otherwise lives as long as the process.
            let ReclaimRequest { schema, .. } = err.0;
            eprintln!(
                "test_harness: the reclaim reaper is not running; leaving schema {schema} for a \
                 future startup sweep"
            );
        }
    }
}

/// Body of the dedicated reaper thread: builds this module's own runtime and
/// drives the reclaim loop on it until the process exits.
///
/// A current-thread runtime is enough — the loop is I/O-bound against one
/// database and deliberately does one thing at a time (see
/// [`reclaim_loop`]) — and costs one thread rather than one per core, which
/// matters in a test binary that already runs many tests concurrently.
fn run_reaper_thread(receiver: UnboundedReceiver<ReclaimRequest>) {
    match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime.block_on(reclaim_loop(receiver)),
        Err(err) => {
            eprintln!(
                "test_harness: failed to build the reclaim reaper's runtime ({err}); leaked pools \
                 and schemas will be left for a future startup sweep"
            );
        }
    }
}

/// Consumes reclaim requests until every sender is gone (i.e. never, in
/// practice: [`HarnessReaper::global`]'s sender is `'static`).
///
/// Each batch is processed in two passes — close every pool, then drop every
/// schema — so that the connection-releasing half of the work is never queued
/// behind another request's schema drop. Within a batch each step still runs
/// sequentially rather than concurrently: every `drop_schema` opens its own
/// admin connection, and firing a batch of them at once would spend, on the
/// shared test server, exactly the connection budget this component exists to
/// protect.
async fn reclaim_loop(mut receiver: UnboundedReceiver<ReclaimRequest>) {
    let mut batch = Vec::with_capacity(RECLAIM_BATCH_SIZE);
    while receiver.recv_many(&mut batch, RECLAIM_BATCH_SIZE).await > 0 {
        for request in &batch {
            close_pool(&request.pool, &request.schema).await;
        }
        for request in &batch {
            drop_isolated_schema(&request.schema).await;
        }
        batch.clear();
    }
}

/// Closes `pool`, giving up (loudly) after [`RECLAIM_STEP_TIMEOUT`].
///
/// A timeout here is not a reason to skip the schema drop: the pool's
/// connections belong to a runtime that no longer exists, so waiting longer
/// would not make them close, while the schema they pin is exactly what needs
/// reclaiming.
async fn close_pool(pool: &PgPool, schema: &str) {
    if tokio::time::timeout(RECLAIM_STEP_TIMEOUT, pool.close())
        .await
        .is_err()
    {
        eprintln!(
            "test_harness: reaper timed out after {RECLAIM_STEP_TIMEOUT:?} closing the pool for \
             schema {schema}; dropping the schema anyway"
        );
    }
}

/// Drops `schema` via [`super::drop_schema`] (the same best-effort teardown
/// `TestApp::cleanup` uses — deliberately reused rather than reimplemented,
/// so both release paths issue the identical statement through the identical
/// admin-connection setup), bounded by [`RECLAIM_STEP_TIMEOUT`] because a
/// `DROP SCHEMA` can block indefinitely on a lock held by a connection whose
/// client is gone.
async fn drop_isolated_schema(schema: &str) {
    if tokio::time::timeout(RECLAIM_STEP_TIMEOUT, super::drop_schema(schema))
        .await
        .is_err()
    {
        eprintln!(
            "test_harness: reaper timed out after {RECLAIM_STEP_TIMEOUT:?} dropping schema \
             {schema}; leaving it for a future startup sweep"
        );
    }
}
