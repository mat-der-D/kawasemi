//! Unit tests for the resident reaper (task 1.1, Requirements 2.1/2.3).
//!
//! The completion condition for this boundary is a property no
//! same-runtime test can demonstrate: *after* a reclaim request has been
//! submitted, the pool is closed and the schema is dropped **even though the
//! runtime the caller submitted from has already been destroyed**. Both tests
//! below therefore build their own short-lived `tokio` runtime, submit from
//! inside it, drop that runtime, and only then observe the outcome — from a
//! second, independent runtime, through a connection the first runtime never
//! owned.
//!
//! Like every other real-database test in this crate, these skip (with a
//! diagnostic) when no local PostgreSQL is reachable, reusing
//! `super::super::tests`'s existing preflight probe rather than a second copy
//! of it.

use std::time::{Duration, Instant};

use sqlx::postgres::PgPool;

use super::super::tests::should_run_against_real_database;
use super::super::{
    admin_db_config, base_test_db_url, create_schema, schema_scoped_url, unique_schema_name,
};
use super::{HarnessReaper, ReclaimRequest};
use crate::config::{DatabaseConfig, Secret};
use crate::db;

/// How long an observer waits for the reaper to finish a submitted request
/// before failing the test. Generous relative to the work involved (closing a
/// two-connection pool plus one `DROP SCHEMA`), because the reaper processes
/// requests on a single background runtime shared with every other test in
/// the same process.
const RECLAIM_OBSERVATION_TIMEOUT: Duration = Duration::from_secs(30);

/// Interval between observation attempts. Short enough that a passing run
/// costs a fraction of a second, long enough not to hammer the shared test
/// database with admin connections while waiting.
const OBSERVATION_INTERVAL: Duration = Duration::from_millis(100);

/// Builds a runtime of the same flavor `#[tokio::test]` gives a test body, so
/// "the caller's runtime is destroyed" here means exactly what it means for a
/// real test function returning.
fn caller_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("building a current-thread runtime for this test must succeed")
}

/// Creates `schema` and returns a pool pinned to it with at least one
/// connection already established (so there is something real for the reaper
/// to close, not just an empty pool handle).
async fn schema_with_live_pool(schema: &str) -> PgPool {
    create_schema(schema).await;
    let pool = db::establish_pool(&DatabaseConfig {
        url: Secret::new(schema_scoped_url(&base_test_db_url(), schema)),
        max_connections: 2,
        acquire_timeout: Duration::from_secs(5),
    })
    .await
    .expect("establishing a pool pinned to the freshly created schema must succeed");
    let one: i32 = sqlx::query_scalar("SELECT 1")
        .fetch_one(&pool)
        .await
        .expect("the pool must serve at least one live connection before submission");
    assert_eq!(one, 1);
    pool
}

/// Whether `schema` still exists, observed through a *fresh* admin connection
/// that shares nothing with the runtime the request was submitted from.
async fn schema_exists(schema: &str) -> bool {
    let admin_pool = db::establish_pool(&admin_db_config())
        .await
        .expect("opening an admin connection to observe the schema must succeed");
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.schemata WHERE schema_name = $1)",
    )
    .bind(schema)
    .fetch_one(&admin_pool)
    .await
    .expect("querying information_schema for the observed schema must succeed");
    admin_pool.close().await;
    exists
}

/// Blocks (on a runtime of its own, never the submitting one) until both
/// halves of the reclaim contract hold, or fails the test with the state that
/// was still outstanding at the deadline.
fn await_reclaimed(observer_pool: &PgPool, schema: &str) {
    let observer_runtime = caller_runtime();
    observer_runtime.block_on(async {
        let deadline = Instant::now() + RECLAIM_OBSERVATION_TIMEOUT;
        loop {
            let pool_closed = observer_pool.is_closed();
            let still_present = schema_exists(schema).await;
            if pool_closed && !still_present {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the reaper did not finish reclaiming within \
                 {RECLAIM_OBSERVATION_TIMEOUT:?}: pool_closed={pool_closed}, \
                 schema {schema} still present={still_present}"
            );
            tokio::time::sleep(OBSERVATION_INTERVAL).await;
        }
    });
}

/// Requirement 2.1 / 2.3, and this task's stated completion condition: a
/// request submitted from a runtime that is subsequently destroyed still
/// results in the pool being closed and the schema dropped.
#[test]
fn reclaims_pool_and_schema_after_the_submitting_runtime_is_destroyed() {
    if !should_run_against_real_database(
        "reclaims_pool_and_schema_after_the_submitting_runtime_is_destroyed",
    ) {
        return;
    }

    let schema = unique_schema_name();
    // Kept alive past the runtime's death purely to observe `is_closed()`;
    // the reaper holds its own clone, and `Pool::close` acts on the shared
    // inner state both clones point at.
    let observer_pool = {
        let runtime = caller_runtime();
        let pool = runtime.block_on(async {
            let pool = schema_with_live_pool(&schema).await;
            HarnessReaper::global().submit(ReclaimRequest::new(pool.clone(), schema.clone()));
            pool
        });
        // The caller's runtime is destroyed here, immediately after
        // submission — exactly what `#[tokio::test]` does when a test body
        // returns without cleaning up. Nothing the submitter spawned can run
        // past this point.
        drop(runtime);
        pool
    };

    await_reclaimed(&observer_pool, &schema);
}

/// Submits from inside a `Drop` implementation running on a thread with no
/// Tokio runtime reachable at all — the exact shape `Drop for TestApp` will
/// take in task 1.2. Proves the submission path neither panics (which inside
/// a destructor risks a double panic aborting the process) nor blocks waiting
/// for a runtime that is not there, and that the request is still honored.
#[test]
fn submitting_from_a_destructor_outside_any_runtime_still_reclaims() {
    if !should_run_against_real_database(
        "submitting_from_a_destructor_outside_any_runtime_still_reclaims",
    ) {
        return;
    }

    /// Mirrors `TestApp`'s own `Option`-taking release fields: the destructor
    /// owns the only remaining right to submit, and runs whether or not the
    /// test body remembered to release anything.
    struct DropSubmitter {
        pool: Option<PgPool>,
        schema: Option<String>,
    }

    impl Drop for DropSubmitter {
        fn drop(&mut self) {
            let (Some(pool), Some(schema)) = (self.pool.take(), self.schema.take()) else {
                return;
            };
            HarnessReaper::global().submit(ReclaimRequest::new(pool, schema));
        }
    }

    let schema = unique_schema_name();
    let (submitter, observer_pool) = {
        let runtime = caller_runtime();
        let pool = runtime.block_on(schema_with_live_pool(&schema));
        drop(runtime);
        (
            DropSubmitter {
                pool: Some(pool.clone()),
                schema: Some(schema.clone()),
            },
            pool,
        )
    };

    // Runs `DropSubmitter::drop` on this bare test thread: no runtime was
    // ever entered here, and the one the pool was created on is already gone.
    let dropped_at = Instant::now();
    drop(submitter);
    let submission_cost = dropped_at.elapsed();
    assert!(
        submission_cost < Duration::from_secs(1),
        "submitting from a destructor must not block the dropping thread, but took \
         {submission_cost:?}"
    );

    await_reclaimed(&observer_pool, &schema);
}
