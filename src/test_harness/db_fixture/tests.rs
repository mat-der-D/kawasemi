//! Unit tests for the lightweight fixture tier (task 3.1, Requirement 4.2).
//!
//! The completion condition has two halves, and a test that only exercised
//! the pool would prove just one of them. "A migrated pool is available" is
//! the easy half — a query against a table only the embedded migrations
//! create settles it. "No real instance was started" is the half that needs
//! deliberate evidence, because *not doing something* leaves no trace by
//! default. Three independent observations are asserted for it:
//!
//! 1. **Nothing was spawned beyond the pools.** `spawn_test_app` spawns an
//!    `axum::serve` task plus the federation and media background loops
//!    (measured at six tasks past its pools); `spawn_test_db` must spawn
//!    nothing of its own. The runtime's live task count, sampled either side
//!    of the call and compared against the per-pool cost measured in the same
//!    test, is a direct measurement of that rather than an inference.
//! 2. **The signing-key boundary was never swapped.** `spawn_test_app`
//!    deliberately replaces `RuntimeContext::deterministic`'s placeholder
//!    provider with the real, DB-backed `DbSigningKeyProvider` (which reads
//!    an actor's key out of the schema and fails with `KeyError::NotFound`
//!    for an actor that has none). A `TestDb` that answered a key request for
//!    an actor that was never created would be running the deterministic
//!    placeholder — which is exactly the "署名鍵生成を行わない" constraint,
//!    observed through behavior instead of through a type name.
//! 3. **The struct has nowhere to put an instance.** `TestDb` exposes only
//!    `pool` and `runtime`; there is no `address`, `state` or `actor` field a
//!    running instance could be reached through. That one is enforced by the
//!    compiler, in `test_db_exposes_only_a_pool_and_a_runtime` below.
//!
//! Like every other real-database test in this crate, these skip (with a
//! diagnostic) when no local PostgreSQL is reachable, reusing
//! `super::super::tests`'s existing preflight probe rather than a second copy
//! of it.

use std::time::{Duration, Instant};

use crate::db;
use crate::domain::Id;
use crate::runtime::{KeyRef, RuntimeContext};
use crate::test_harness::sweep::HARNESS_SCHEMA_PREFIX;
use crate::test_harness::tests::should_run_against_real_database;
use crate::test_harness::{admin_db_config, default_test_seed};

use super::{TestDb, spawn_test_db};

/// How long the drop path is given to complete before the test fails.
/// Mirrors `reaper::tests`'s own observation budget: the reaper serves every
/// fixture in the process from one background runtime, so a reclaim can queue
/// behind other work.
const RECLAIM_OBSERVATION_TIMEOUT: Duration = Duration::from_secs(30);

/// Interval between observation attempts, matching `reaper::tests`.
const OBSERVATION_INTERVAL: Duration = Duration::from_millis(100);

/// Live task count on the runtime this test is running on. The reaper's own
/// tasks are invisible here — it owns a separate runtime on its own thread —
/// so this measures only what the caller's runtime was made to start.
fn alive_tasks() -> usize {
    tokio::runtime::Handle::current()
        .metrics()
        .num_alive_tasks()
}

/// Whether `schema` still exists, observed through a *fresh* admin connection
/// that shares nothing with the fixture's own pool.
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

/// The schema a fixture's own pool is pinned to, asked of the server rather
/// than read off the struct: this is the same evidence
/// `tests/harness_release_it.rs` uses, and it proves the `search_path`
/// pinning actually took effect instead of merely having been requested.
async fn current_schema(db: &TestDb) -> String {
    sqlx::query_scalar::<_, String>("SELECT current_schema()")
        .fetch_one(&db.pool)
        .await
        .expect("the fixture's pool must report the schema it is pinned to")
}

/// Requirement 4.2 and this task's stated completion condition: a migrated
/// pool, with no real instance started to obtain it.
#[tokio::test]
async fn spawn_test_db_yields_a_migrated_pool_without_starting_a_real_instance() {
    if !should_run_against_real_database(
        "spawn_test_db_yields_a_migrated_pool_without_starting_a_real_instance",
    ) {
        return;
    }

    // Calibration: what one live `PgPool` alone costs in spawned tasks. Taken
    // from a real pool rather than hardcoded, because the number is sqlx's
    // internal business and may change; the assertion below is expressed as a
    // multiple of it so it keeps meaning the same thing if it does. Sampled
    // while the pool is still open, so the measurement cannot read low.
    let baseline_before = alive_tasks();
    let calibration_pool = db::establish_pool(&admin_db_config())
        .await
        .expect("establishing a pool to calibrate the per-pool task cost must succeed");
    let tasks_per_pool = alive_tasks() - baseline_before;
    calibration_pool.close().await;
    drop(calibration_pool);

    let tasks_before = alive_tasks();
    let db = spawn_test_db().await;
    let tasks_after = alive_tasks();

    // Half one: the embedded migrations really ran against this schema. Both
    // sqlx's own bookkeeping table and a table only a migration creates are
    // checked — the former alone would also be satisfied by a bookkeeping
    // table left behind with no migration applied.
    let migration_count: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
        .fetch_one(&db.pool)
        .await
        .expect("_sqlx_migrations must exist and be queryable: migrations must be pre-applied");
    assert!(
        migration_count > 0,
        "spawn_test_db must leave the embedded migrations applied, but _sqlx_migrations is empty"
    );
    let local_actors: i64 = sqlx::query_scalar("SELECT count(*) FROM local_actors")
        .fetch_one(&db.pool)
        .await
        .expect("a table created by the embedded migrations must be queryable");
    assert_eq!(
        local_actors, 0,
        "a freshly migrated schema must start empty"
    );

    // The pool is pinned to an isolated schema of this harness's own naming
    // convention, so the sweeper can recognize it as residue if it ever leaks.
    let schema = current_schema(&db).await;
    assert!(
        schema.starts_with(HARNESS_SCHEMA_PREFIX),
        "spawn_test_db's pool must be pinned to an isolated harness schema, got {schema}"
    );

    // Half two, evidence 1: nothing was spawned beyond the pools themselves.
    // `spawn_test_db` opens exactly two — a throwaway admin pool to issue
    // `CREATE SCHEMA`, and the fixture's own — and spawns no task of its own,
    // so two pools' worth is the whole budget. A real instance cannot fit
    // inside it: `spawn_test_app` additionally spawns the `axum::serve` task,
    // the federation delivery and pruning loops, and the media processing
    // workers (measured at six tasks beyond its pools). An upper bound rather
    // than an equality because a closed pool's task exits asynchronously, so
    // the admin pool's may already be gone by the time this is sampled.
    let spawned = tasks_after - tasks_before;
    assert!(
        spawned <= 2 * tasks_per_pool,
        "spawn_test_db must start no instance: it spawned {spawned} tasks, more than the \
         {tasks_per_pool}-task-per-pool cost of the two pools it opens"
    );

    // Half two, evidence 2: the deterministic placeholder key provider is
    // still in place, so no DB-backed signing-key supply was wired and no key
    // was generated. `DbSigningKeyProvider` would answer `NotFound` for an
    // actor that does not exist in this freshly migrated schema.
    let key_ref = KeyRef(Id::from_i64(1));
    let key =
        db.runtime.keys.signing_key(key_ref).expect(
            "TestDb's keys boundary must be the deterministic placeholder, not a DB lookup",
        );
    let expected = RuntimeContext::deterministic(default_test_seed())
        .keys
        .signing_key(key_ref)
        .expect("the deterministic placeholder never fails");
    assert_eq!(
        key.expose_pem_bytes(),
        expected.expose_pem_bytes(),
        "TestDb's keys boundary must be exactly what RuntimeContext::deterministic supplies"
    );

    // Same deterministic convention as `spawn_test_app` for clock/id/rng: the
    // fixed seed reproduces the identical sequence, so two fixtures in one
    // binary observe the same values.
    let reference = RuntimeContext::deterministic(default_test_seed());
    assert_eq!(db.runtime.clock.now(), reference.clock.now());
    assert_eq!(db.runtime.ids.next_id(), reference.ids.next_id());

    db.cleanup().await;

    assert!(
        !schema_exists(&schema).await,
        "cleanup() must drop the isolated schema {schema} synchronously"
    );
}

/// Requirement 2.3/2.4 for this fixture: omitting `cleanup()` must not leak.
/// `Drop` cannot do the work itself (it is synchronous, the work is not), so
/// it hands the pool and schema to the resident reaper; this test observes
/// the reclaim actually completing.
#[tokio::test]
async fn dropping_without_cleanup_delegates_release_to_the_reaper() {
    if !should_run_against_real_database("dropping_without_cleanup_delegates_release_to_the_reaper")
    {
        return;
    }

    let db = spawn_test_db().await;
    let schema = current_schema(&db).await;
    // Outlives the fixture purely to observe `is_closed()`; `Pool::close`
    // acts on the inner state every clone shares.
    let observer_pool = db.pool.clone();

    let dropped_at = Instant::now();
    drop(db);
    let drop_cost = dropped_at.elapsed();
    assert!(
        drop_cost < Duration::from_secs(1),
        "Drop for TestDb must never block the dropping thread, but took {drop_cost:?}"
    );

    let deadline = Instant::now() + RECLAIM_OBSERVATION_TIMEOUT;
    loop {
        let pool_closed = observer_pool.is_closed();
        let still_present = schema_exists(&schema).await;
        if pool_closed && !still_present {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the reaper did not reclaim the dropped TestDb within \
             {RECLAIM_OBSERVATION_TIMEOUT:?}: pool_closed={pool_closed}, schema {schema} still \
             present={still_present}"
        );
        tokio::time::sleep(OBSERVATION_INTERVAL).await;
    }
}

/// The structural half of "no real instance": `TestDb` is exhaustively
/// destructurable into exactly a pool and a runtime. Adding an `address`,
/// `state` or `actor` field — anything a running instance would be reached
/// through — breaks this pattern at compile time. Runs without a database
/// because it asserts on the type, not on a fixture.
#[test]
fn test_db_exposes_only_a_pool_and_a_runtime() {
    // By reference rather than by value: `TestDb` implements `Drop`, so an
    // owning destructuring pattern would not compile at all — and the point
    // here is the field list, which a borrowing pattern checks just as
    // exhaustively.
    fn assert_shape(db: &TestDb) {
        let TestDb {
            pool: _,
            runtime: _,
            schema: _,
        } = db;
    }
    // Never called: the exhaustive pattern above is the assertion, and it is
    // checked when this file compiles.
    let _ = assert_shape;
}
