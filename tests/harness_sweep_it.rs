//! Integration tests for the startup sweep: planting old orphan schemas
//! before a process starts must leave them reclaimed and newer ones
//! untouched, and a sweep that cannot reclaim something must still let the
//! run proceed.
//!
//! ## Why this can only be an integration test
//! The sweep's contract is stated over *the database*, not over any value the
//! harness returns: after it runs, a schema whose embedded timestamp is old is
//! gone and every other schema on the server is untouched. Only a test that
//! plants schemas behind the harness's back — through its own admin
//! connection, with names it constructed itself — can state that, and only a
//! `tests/*.rs` binary reaches `spawn_test_app` the way an ordinary caller
//! does, which is what makes the once-per-process trigger observable at all.
//! Placement follows steering `structure.md`'s "テストレイアウト" rule.
//!
//! ## Why every assertion names its own schemas
//! The sweep is a *server-wide* operation: it lists every schema in the shared
//! `kawasemi_test` database and drops the ones it claims. So a test that
//! asserted on a server-wide listing ("no old harness schema remains") would be
//! asserting on residue belonging to whatever else touches this server, and a
//! test that ran concurrently with a sibling would sweep the sibling's planted
//! schemas out from under it. Two mechanisms keep that from making this binary
//! flaky, and both are required — the same pair `tests/harness_release_it.rs`
//! established for the same reason (steering `tech.md`: 「非決定的（flaky）な
//! テストは自律ループを壊す」):
//!
//! 1. [`EXCLUSIVE_DATABASE_ACCESS`] serializes the tests here, and every test
//!    removes the schemas it planted *before* releasing the lock.
//! 2. Every assertion is scoped by `schema_name = ANY($1)` to the exact names
//!    the asserting test minted (see [`SchemaProbe::surviving`]), never to a
//!    prefix scan.
//!
//! ## Why a control schema appears in the main test
//! The failure mode that matters here is not "reclaims too little" — that
//! merely defers cleanup to the next run — but "reclaims too much", which
//! silently destroys the database state of a concurrently running process (see
//! `src/test_harness/sweep.rs`'s own doc comment). So the reclaim test plants a
//! schema *outside* the harness naming convention alongside the two harness
//! ones and asserts it survives, making over-claiming a failure rather than an
//! invisible side effect.

use std::collections::BTreeSet;
use std::net::TcpStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use kawasemi::test_harness::spawn_test_app;
use kawasemi::test_harness::sweep::{startup_sweeps_performed, sweep_orphans_now};
use sqlx::Row;
use sqlx::postgres::{PgPool, PgPoolOptions};
use tokio::sync::Mutex;

const TEST_DB_HOST: &str = "127.0.0.1";
const TEST_DB_PORT: u16 = 5432;
const TEST_DB_URL_ENV: &str = "KAWASEMI_TEST_DATABASE_URL";
const DEFAULT_TEST_DB_URL: &str =
    "postgres://kawasemi_test:kawasemi_test_pw@127.0.0.1:5432/kawasemi_test";

/// Prefix `src/test_harness.rs`'s `unique_schema_name` gives every isolated
/// schema, and therefore the only family `sweep_orphans_now` may claim.
/// Duplicated here rather than exported, following the convention
/// `tests/harness_release_it.rs` set: this binary is an outside observer of the
/// harness, and plants its fixtures the way a *previous process* would have
/// left them — by spelling the convention out, so that a harness that
/// redefined the constant would be caught rather than followed.
const HARNESS_SCHEMA_PREFIX: &str = "kawasemi_test_harness_";

/// How far back a planted "old" schema's embedded timestamp is set.
///
/// Deliberately far beyond any threshold the sweeper could reasonably choose
/// (`src/test_harness/sweep.rs`'s is hours, not weeks), so these tests state
/// the property they are actually about — old is reclaimed, new is kept —
/// rather than pinning the threshold's exact value, which is a tuning decision
/// documented at its definition and free to change without rewriting this file.
const PLANTED_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Serializes the tests in this binary against each other.
///
/// Required, not defensive: `sweep_orphans_now` acts on every schema in the
/// shared database, so two of these tests running concurrently would each
/// reclaim the other's planted fixtures. A `tokio::sync::Mutex` for the reason
/// `tests/harness_release_it.rs` gives — no poison state, so one failing test
/// cannot cascade into unrelated failures.
static EXCLUSIVE_DATABASE_ACCESS: Mutex<()> = Mutex::const_new(());

/// Best-effort raw-TCP reachability probe, mirroring the convention
/// `tests/harness_release_it.rs` and `src/db/tests.rs` already use: skip where
/// no local PostgreSQL exists at all, never swallow a real regression.
fn default_test_db_reachable() -> bool {
    TcpStream::connect_timeout(
        &format!("{TEST_DB_HOST}:{TEST_DB_PORT}")
            .parse()
            .expect("hardcoded host:port is valid"),
        Duration::from_millis(500),
    )
    .is_ok()
}

/// Returns `true` if the caller should proceed, `false` if it should skip
/// (having already printed a diagnostic).
fn should_run_against_real_database(test_name: &str) -> bool {
    let overridden = std::env::var(TEST_DB_URL_ENV).is_ok();
    if !overridden && !default_test_db_reachable() {
        eprintln!(
            "skipping {test_name}: no PostgreSQL reachable at {TEST_DB_HOST}:{TEST_DB_PORT} \
             and {TEST_DB_URL_ENV} was not set"
        );
        return false;
    }
    true
}

fn base_test_db_url() -> String {
    std::env::var(TEST_DB_URL_ENV).unwrap_or_else(|_| DEFAULT_TEST_DB_URL.to_string())
}

/// Mints a schema name in the harness's own `{prefix}{nanos}_{seq}` format
/// whose embedded creation time is `age` in the past.
///
/// Constructing the name directly is the point: it is how this test forges the
/// residue a *previous, already-exited* process would have left behind, which
/// is the only situation the startup sweep exists for and the one situation a
/// live process cannot produce on demand. The counter keeps two names minted
/// within the same nanosecond distinct, exactly as the harness's own
/// `unique_schema_name` does.
fn planted_schema_name(age: Duration) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let stamped_at = SystemTime::now()
        .checked_sub(age)
        .expect("the planted age must be representable before the current instant");
    let nanos = stamped_at
        .duration_since(UNIX_EPOCH)
        .expect("the planted instant must be after the Unix epoch")
        .as_nanos();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{HARNESS_SCHEMA_PREFIX}{nanos}_{seq}")
}

/// Mints a name that is *not* in the harness's family, standing in for the
/// schemas this sweeper must never touch (`src/migrate/tests.rs`'s
/// `kawasemi_migrate_test_*`, `src/db/tests.rs`'s fixtures, a developer's own).
fn planted_foreign_schema_name() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("kawasemi_sweep_it_foreign_{seq}")
}

/// A connection owned by the test itself, used to plant schemas behind the
/// harness's back and to read back which of them survived.
struct SchemaProbe {
    pool: PgPool,
}

impl SchemaProbe {
    async fn connect() -> Self {
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(5))
            .connect(&base_test_db_url())
            .await
            .expect("the test's own connection to the shared test database");
        Self { pool }
    }

    /// Creates `schema` with one table in it. The table is not decoration: a
    /// schema that still contains objects can only be removed by a `CASCADE`
    /// drop, so its disappearance proves the sweeper issues the same statement
    /// the harness's own teardown does rather than a bare `DROP SCHEMA` that
    /// would silently fail on every non-empty orphan — which is every real
    /// orphan, since a real one has migrations applied.
    async fn plant(&self, schema: &str) {
        self.execute(&format!(r#"CREATE SCHEMA "{schema}""#)).await;
        self.execute(&format!(r#"CREATE TABLE "{schema}"."residue" (id int)"#))
            .await;
    }

    /// Which of `planted` still exist. Scoped to the caller's own names by
    /// binding them, never a prefix scan: a sibling's residue must be unable
    /// to fail this test, and this test must be unable to wait on it.
    async fn surviving(&self, planted: &BTreeSet<String>) -> BTreeSet<String> {
        if planted.is_empty() {
            return BTreeSet::new();
        }
        let names: Vec<String> = planted.iter().cloned().collect();
        sqlx::query(
            "SELECT schema_name::text AS schema_name FROM information_schema.schemata \
             WHERE schema_name = ANY($1)",
        )
        .bind(&names)
        .fetch_all(&self.pool)
        .await
        .expect("listing this test's own planted schemas must succeed")
        .into_iter()
        .map(|row| row.get::<String, _>("schema_name"))
        .collect()
    }

    /// Removes whatever of `planted` is left, so the next test starts from a
    /// database this one did not pollute.
    async fn remove_all(&self, planted: &BTreeSet<String>) {
        for schema in planted {
            self.execute(&format!(r#"DROP SCHEMA IF EXISTS "{schema}" CASCADE"#))
                .await;
        }
    }

    async fn execute(&self, statement: &str) {
        sqlx::query(sqlx::AssertSqlSafe(statement.to_string()))
            .execute(&self.pool)
            .await
            .unwrap_or_else(|err| panic!("test setup statement {statement:?} must succeed: {err}"));
    }

    async fn close(self) {
        self.pool.close().await;
    }
}

/// A sweep reclaims the schemas an earlier run left
/// behind, and leaves alone both a freshly created harness schema (which may
/// belong to a process running right now) and a schema outside the harness's
/// naming convention (which belongs to something else entirely).
///
/// The test would still pass with the sweep removed on its second and third
/// assertions — those are the "keep" half — but not on the first, which no
/// other mechanism in this crate performs.
#[tokio::test]
async fn old_orphan_schemas_are_reclaimed_while_new_and_foreign_ones_are_kept() {
    let _exclusive = EXCLUSIVE_DATABASE_ACCESS.lock().await;
    if !should_run_against_real_database(
        "old_orphan_schemas_are_reclaimed_while_new_and_foreign_ones_are_kept",
    ) {
        return;
    }

    let probe = SchemaProbe::connect().await;
    let old = planted_schema_name(PLANTED_AGE);
    let fresh = planted_schema_name(Duration::ZERO);
    let foreign = planted_foreign_schema_name();
    let planted = BTreeSet::from([old.clone(), fresh.clone(), foreign.clone()]);
    for schema in &planted {
        probe.plant(schema).await;
    }

    sweep_orphans_now().await;

    let surviving = probe.surviving(&planted).await;
    probe.remove_all(&planted).await;
    probe.close().await;

    assert!(
        !surviving.contains(&old),
        "the sweep must reclaim {old}, whose embedded timestamp is \
         {PLANTED_AGE:?} old, but it survived — surviving: {surviving:?}"
    );
    assert!(
        surviving.contains(&fresh),
        "the sweep must keep {fresh}, stamped just now and therefore possibly \
         in use by a process running right now, but it was dropped — surviving: {surviving:?}"
    );
    assert!(
        surviving.contains(&foreign),
        "the sweep must not touch {foreign}, which is outside the harness naming convention and \
         belongs to another schema family entirely — surviving: {surviving:?}"
    );
}

/// A failing reclaim must not stop the test run.
///
/// The failure is real rather than mocked: a transaction on a separate
/// connection holds `ACCESS EXCLUSIVE` on a table inside one planted orphan, so
/// the sweeper's `DROP SCHEMA ... CASCADE` for it genuinely cannot proceed and
/// ends in a lock-timeout error from the server. Two consequences are asserted,
/// and they are what "妨げない" means concretely: the sweep neither propagates
/// the error to its caller nor abandons the rest of its work (the second,
/// droppable orphan is still reclaimed), and a fixture spawned afterwards still
/// comes up.
#[tokio::test]
async fn a_failing_reclaim_stops_neither_the_rest_of_the_sweep_nor_the_test_run() {
    let _exclusive = EXCLUSIVE_DATABASE_ACCESS.lock().await;
    if !should_run_against_real_database(
        "a_failing_reclaim_stops_neither_the_rest_of_the_sweep_nor_the_test_run",
    ) {
        return;
    }

    let probe = SchemaProbe::connect().await;
    let locked = planted_schema_name(PLANTED_AGE);
    let droppable = planted_schema_name(PLANTED_AGE);
    let planted = BTreeSet::from([locked.clone(), droppable.clone()]);
    for schema in &planted {
        probe.plant(schema).await;
    }

    // A connection of its own, held open across the sweep: the lock must
    // outlive the sweeper's attempt, and taking it on `probe`'s single
    // connection would deadlock the probe's own later reads.
    let blocker = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&base_test_db_url())
        .await
        .expect("the blocking connection must be established");
    let mut blocking_tx = blocker
        .begin()
        .await
        .expect("the blocking transaction must start");
    sqlx::query(sqlx::AssertSqlSafe(format!(
        r#"LOCK TABLE "{locked}"."residue" IN ACCESS EXCLUSIVE MODE"#
    )))
    .execute(&mut *blocking_tx)
    .await
    .expect("taking the blocking lock must succeed");

    // Must return normally. A panic or a propagated error here fails the test
    // by itself, which is this test's primary assertion.
    sweep_orphans_now().await;

    let surviving = probe.surviving(&planted).await;
    blocking_tx
        .rollback()
        .await
        .expect("releasing the blocking lock must succeed");
    blocker.close().await;

    // The run continues: a fixture spawned after a failed reclaim still works.
    let app = spawn_test_app().await;
    let alive: i32 = sqlx::query("SELECT 1 AS one")
        .fetch_one(&app.pool)
        .await
        .expect("a TestApp spawned after a failed reclaim must have a working pool")
        .get("one");
    app.cleanup().await;

    probe.remove_all(&planted).await;
    probe.close().await;

    assert_eq!(
        alive, 1,
        "the fixture spawned after a failed reclaim must work"
    );
    assert!(
        surviving.contains(&locked),
        "the un-droppable orphan {locked} must simply survive the failed reclaim — surviving: \
         {surviving:?}"
    );
    assert!(
        !surviving.contains(&droppable),
        "one failed reclaim must not abandon the rest of the sweep, but \
         {droppable} was left behind — surviving: {surviving:?}"
    );
}

/// The sweep runs exactly once per process.
///
/// Stated over the real trigger rather than over the sweep function: several
/// fixtures are spawned through `spawn_test_app`, which is where the startup
/// sweep is hooked, and the process's completed-startup-sweep count must still
/// be exactly one. Without the once-guard this reads at least as many as there
/// were spawns — in this binary and in every other, which is the cost the
/// requirement exists to avoid.
#[tokio::test]
async fn the_startup_sweep_runs_exactly_once_per_process() {
    let _exclusive = EXCLUSIVE_DATABASE_ACCESS.lock().await;
    if !should_run_against_real_database("the_startup_sweep_runs_exactly_once_per_process") {
        return;
    }

    // Two spawns, and possibly more from the sibling tests in this binary that
    // already ran. The expected count is one regardless of how many there were
    // in total, which is exactly the property under test.
    for _ in 0..2 {
        let app = spawn_test_app().await;
        app.cleanup().await;
    }

    assert_eq!(
        startup_sweeps_performed(),
        1,
        "the startup sweep must have run exactly once in this process, no \
         matter how many fixtures were created"
    );
}

/// A run completes without a manual pre-run cleanup step.
///
/// A database that already holds a pile of old orphans when the run starts is
/// the normal state of affairs after any abnormal exit, and it must simply not
/// matter: fixtures come up, do their work, and tear down as usual. Planting
/// the orphans first is what makes the test state that, rather than silently
/// depending on someone having cleaned the server beforehand.
#[tokio::test]
async fn a_run_that_starts_with_old_orphans_present_needs_no_manual_cleanup() {
    let _exclusive = EXCLUSIVE_DATABASE_ACCESS.lock().await;
    if !should_run_against_real_database(
        "a_run_that_starts_with_old_orphans_present_needs_no_manual_cleanup",
    ) {
        return;
    }

    let probe = SchemaProbe::connect().await;
    let planted: BTreeSet<String> = (0..5).map(|_| planted_schema_name(PLANTED_AGE)).collect();
    for schema in &planted {
        probe.plant(schema).await;
    }

    let app = spawn_test_app().await;
    let alive: i32 = sqlx::query("SELECT 1 AS one")
        .fetch_one(&app.pool)
        .await
        .expect("a TestApp must come up with old orphan schemas already present")
        .get("one");
    app.cleanup().await;

    probe.remove_all(&planted).await;
    probe.close().await;

    assert_eq!(
        alive, 1,
        "a run must complete with pre-existing old orphan schemas present, \
         without any manual cleanup step"
    );
}
