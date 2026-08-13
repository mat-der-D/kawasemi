//! Integration tests for test-infrastructure task 1.2 ("TestApp の解放を
//! リーパー委譲に置き換える"), covering Requirements 2.1, 2.2, 2.3, 2.4 and
//! 2.5 — design.md's "Testing Strategy" -> "Integration Tests", whose first
//! entry ("`cleanup()` を呼ばずに多数の `TestApp` を生成・破棄したとき、保持
//! 接続数が生存インスタンス数に比例する範囲に留まる") it calls **本 spec の
//! 中核**.
//!
//! ## Why this can only be an integration test
//! The property under test is a *process-wide, server-observed* one: how many
//! backends the shared `kawasemi_test` database is still holding open for
//! this process after N fixtures have been created and dropped. Nothing
//! inside the harness can answer that — `PgPool` reports its own idle/size
//! counters, but the leak this spec exists to fix is precisely the case where
//! the pool object is gone while its server-side backends are not. So the
//! measurement is taken from `pg_stat_activity` over a connection that
//! belongs to no fixture at all (see [`AdminProbe`]), and the fixtures are
//! booted through the real `spawn_test_app` entry point, which only a
//! `tests/*.rs` binary sees the way an ordinary caller does. Placement
//! follows steering `structure.md`'s "テストレイアウト" rule: real, running
//! instances plus a database belong in `tests/*_it.rs`.
//!
//! ## What makes the assertion decisive rather than incidental
//! Before this task, `Drop for TestApp` closed no connections at all: it
//! detached a schema-drop onto whatever runtime happened to be current and
//! left the pool entirely alone (requirements.md's Introduction records the
//! measurement — 8 dropped instances still holding 40 connections three
//! seconds later). With `spawn_test_app`'s current `max_connections: 2`, the
//! loop below would therefore end holding roughly `2 * DROPPED_INSTANCES`
//! extra backends against a server whose global ceiling is 100. The
//! tolerances here ([`SETTLED_ALLOWANCE`], [`PEAK_ALLOWANCE`]) are far below
//! that, so the test cannot pass unless `Drop` genuinely hands the pool to
//! something that outlives the per-test runtime and closes it.
//!
//! ## Why every test in this binary observes only its own fixtures
//! Both measures taken here are *server-wide*: `pg_stat_activity` counts the
//! whole role's backends, and `information_schema.schemata` lists every
//! harness schema on the server. Neither can be scoped by a `WHERE` clause to
//! "the instances this test created", so a test that waits for one of them to
//! return to baseline is really waiting for every other test's fixtures too.
//! Two independent mechanisms keep that from making this binary flaky, and
//! both are required:
//!
//! 1. [`EXCLUSIVE_DATABASE_ACCESS`] serializes the tests, so no sibling test
//!    holds live fixtures while another is measuring. It lives in this file
//!    rather than in an invocation flag because nothing would enforce a
//!    `--test-threads=1` that the caller has to remember to pass.
//! 2. Every schema assertion is stated over the schemas the asserting test
//!    *itself* created — identified exactly, by asking each fixture's own
//!    `search_path`-pinned pool for its `current_schema()` (see
//!    [`isolated_schema_of`]), never by diffing a server-wide listing and
//!    hoping the difference belongs to us. A failure therefore always names
//!    residue this test is genuinely responsible for.
//!
//! Mechanism 2 alone would leave the connection counts contaminated;
//! mechanism 1 alone would leave a failure message able to blame this test
//! for another's residue. Steering `tech.md`: 「非決定的（flaky）なテストは
//! 自律ループを壊す」.

use std::collections::BTreeSet;
use std::net::TcpStream;
use std::time::{Duration, Instant};

use kawasemi::test_harness::spawn_test_app;
use sqlx::Row;
use sqlx::postgres::{PgPool, PgPoolOptions};
use tokio::sync::{Mutex, oneshot};

const TEST_DB_HOST: &str = "127.0.0.1";
const TEST_DB_PORT: u16 = 5432;
const TEST_DB_URL_ENV: &str = "KAWASEMI_TEST_DATABASE_URL";
const DEFAULT_TEST_DB_URL: &str =
    "postgres://kawasemi_test:kawasemi_test_pw@127.0.0.1:5432/kawasemi_test";

/// Prefix `src/test_harness.rs`'s `unique_schema_name` gives every isolated
/// schema. Duplicated here (rather than exported) deliberately: this test is
/// an outside observer of the harness and must keep working off what the
/// *database* shows, not off a constant the harness could redefine while the
/// assertion silently stops looking at anything.
const HARNESS_SCHEMA_PREFIX: &str = "kawasemi_test_harness_";

/// Serializes the tests in this binary against each other.
///
/// `cargo test` runs a binary's tests concurrently by default, and every
/// measurement here reads a server-wide counter (see this module's doc
/// comment): a sibling test's live fixtures show up in another test's
/// `pg_stat_activity` count, and a sibling's not-yet-reclaimed schema shows
/// up in another test's schema listing. Held for the whole body of each test,
/// this lock removes that overlap at the source instead of absorbing it into
/// wider tolerances — which would cost exactly the discriminating power
/// Requirement 2.5's bound is there to have.
///
/// A `tokio::sync::Mutex` rather than a `std::sync::Mutex`: it has no poison
/// state, so the deliberately panicking test below cannot turn a lock it once
/// held into a cascade of unrelated failures, and it yields rather than blocks
/// a runtime worker while waiting.
static EXCLUSIVE_DATABASE_ACCESS: Mutex<()> = Mutex::const_new(());

/// How many fixtures the leak test creates and drops without cleanup. Chosen
/// so the pre-fix behavior is unambiguously out of tolerance (40 instances x
/// `max_connections: 2` is ~80 backends against a 100-connection server),
/// while the run still costs well under a minute of migrations.
const DROPPED_INSTANCES: usize = 40;

/// Connections above baseline still tolerated once every fixture is gone and
/// the reaper has drained. Not zero, because a reclaim in flight legitimately
/// holds one short-lived admin connection (and the harness opens one to create
/// each schema). Since [`EXCLUSIVE_DATABASE_ACCESS`] keeps sibling tests from
/// contributing, this covers only the asserting test's own transients — two
/// orders of magnitude below what the pre-fix `Drop` leaves behind.
const SETTLED_ALLOWANCE: i64 = 6;

/// Connections above baseline tolerated *during* the loop. This is the
/// "生存インスタンス数に比例する" bound of Requirement 2.5 stated concretely:
/// the loop keeps exactly one instance alive at a time, so the steady-state
/// cost is that instance's pool plus the handful of transient admin
/// connections the harness and the reaper open to create and drop schemas —
/// not a quantity that grows with the number of instances already dropped.
const PEAK_ALLOWANCE: i64 = 20;

/// Upper bound on the wait for *the asserting test's own* reclaim requests to
/// clear the reaper's queue after its last drop.
///
/// It is not, and must not become, a budget for waiting out a concurrently
/// running test's fixtures — [`EXCLUSIVE_DATABASE_ACCESS`] is what removes
/// those, and a timeout large enough to outlast them would be unbounded in
/// principle (it would have to exceed the slowest sibling on the slowest
/// machine). What has to fit here is one test's own backlog: at most
/// [`DROPPED_INSTANCES`] requests, each a `Pool::close` plus a `DROP SCHEMA`
/// that the reaper processes sequentially by design (see
/// `src/test_harness/reaper.rs`), and which a passing run drains in a few
/// hundred milliseconds because all but the last few already ran during the
/// loop. Sixty seconds is therefore generous by two orders of magnitude, and
/// the assertion's meaning still comes from the state it settles at rather
/// than from how fast it got there.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Interval between settle polls. Short enough that a passing run adds no
/// perceptible time, long enough not to spin on the database.
const SETTLE_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Best-effort raw-TCP reachability probe, mirroring the convention
/// `tests/test_harness_lifecycle_it.rs` and `src/db/tests.rs` already use:
/// skip where no local PostgreSQL exists at all, never swallow a real
/// regression.
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

/// The isolated schema a fixture is pinned to, asked of the fixture's *own*
/// pool.
///
/// `spawn_test_app` builds that pool with a `search_path` startup option
/// naming the instance's isolated schema (see `schema_scoped_url` in
/// `src/test_harness.rs`), so `current_schema()` on any of its connections
/// answers exactly which schema this instance owns — no guessing, no diffing a
/// server-wide listing, and therefore no chance of attributing another test's
/// schema to this one. `TestApp::schema` itself is private to the harness
/// module, which is why the pool is asked rather than the struct read.
async fn isolated_schema_of(pool: &PgPool) -> String {
    let schema: String = sqlx::query("SELECT current_schema()::text AS s")
        .fetch_one(pool)
        .await
        .expect("asking a TestApp's own pool for its search_path schema must succeed")
        .get("s");
    assert!(
        schema.starts_with(HARNESS_SCHEMA_PREFIX),
        "a TestApp's pool must be pinned to an isolated harness schema, but current_schema() \
         reported {schema:?} — this test can no longer tell which schemas it owns"
    );
    schema
}

/// A single connection owned by the test itself rather than by any fixture,
/// used to observe the server's own view of this process.
///
/// It must be a *separate* connection: measuring through a `TestApp`'s pool
/// would mean the observer disappears together with the thing being observed,
/// and would add the observed instance's own accounting to every reading.
/// Exactly one connection, opened once and held for the whole test, so the
/// probe itself is a constant offset that cancels out of every
/// baseline-relative comparison below.
struct AdminProbe {
    pool: PgPool,
}

impl AdminProbe {
    async fn connect() -> Self {
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(5))
            .connect(&base_test_db_url())
            .await
            .expect("the test's own observation connection to the shared test database");
        Self { pool }
    }

    /// Backends this process's test role currently holds against the shared
    /// test database, the probe's own connection included. Counting by
    /// role+database (rather than by schema, which `pg_stat_activity` does
    /// not expose) is what makes this a measure of the resource Requirement
    /// 2.5 is about: the server-side connection, which survives its pool
    /// object being dropped.
    async fn connection_count(&self) -> i64 {
        sqlx::query(
            "SELECT count(*) AS c FROM pg_stat_activity \
             WHERE datname = current_database() AND usename = current_user",
        )
        .fetch_one(&self.pool)
        .await
        .expect("querying pg_stat_activity must succeed")
        .get("c")
    }

    /// Which of `owned` are still present on the server. Used to prove the
    /// second half of a reclaim (the `DROP SCHEMA`) also happened, which the
    /// connection count alone cannot show.
    ///
    /// Takes the caller's own schema names as input rather than returning
    /// every harness schema on the server: the caller must never be able to
    /// wait on, or be failed by, residue that belongs to something else.
    async fn surviving_schemas(&self, owned: &BTreeSet<String>) -> Vec<String> {
        if owned.is_empty() {
            return Vec::new();
        }
        let names: Vec<String> = owned.iter().cloned().collect();
        sqlx::query(
            "SELECT schema_name FROM information_schema.schemata \
             WHERE schema_name = ANY($1)",
        )
        .bind(&names)
        .fetch_all(&self.pool)
        .await
        .expect("listing this test's own harness schemas must succeed")
        .into_iter()
        .map(|row| row.get::<String, _>("schema_name"))
        .collect()
    }

    async fn close(self) {
        self.pool.close().await;
    }
}

/// What the database still shows once the reaper is given time to drain.
struct SettledState {
    connections: i64,
    /// Of the schemas the waiting test created, the ones still present.
    surviving_schemas: Vec<String>,
}

/// Polls until both measures have returned to baseline or [`SETTLE_TIMEOUT`]
/// elapses, then reports whatever the last reading was — the caller asserts
/// on it, so a timeout surfaces as a failed assertion carrying the observed
/// numbers rather than as an opaque hang or a bare "timed out".
///
/// `own_schemas` are the isolated schemas the calling test created, and are
/// the only ones considered: waiting for *any* harness schema to disappear
/// would mean waiting on whatever else touches this server.
///
/// Both measures are waited on together on purpose: the reaper closes a
/// batch's pools before dropping that batch's schemas, so connections settle
/// strictly earlier, and reading the schemas at that moment would report
/// reclaims that are merely still running as if they had failed.
async fn wait_until_reclaimed(
    probe: &AdminProbe,
    baseline_connections: i64,
    own_schemas: &BTreeSet<String>,
) -> SettledState {
    let deadline = Instant::now() + SETTLE_TIMEOUT;
    loop {
        let state = SettledState {
            connections: probe.connection_count().await,
            surviving_schemas: probe.surviving_schemas(own_schemas).await,
        };
        let settled = state.connections <= baseline_connections + SETTLED_ALLOWANCE
            && state.surviving_schemas.is_empty();
        if settled || Instant::now() >= deadline {
            return state;
        }
        tokio::time::sleep(SETTLE_POLL_INTERVAL).await;
    }
}

/// Requirements 2.1, 2.3, 2.4, 2.5: creating and dropping many `TestApp`s
/// **without ever calling `cleanup()`** must not accumulate connections. The
/// count is allowed to sit at "baseline + what the one live instance costs"
/// throughout, and must return to baseline once none are live — never to grow
/// with the number of instances already dropped.
///
/// This is the completion condition of task 1.2 and, per design.md, the
/// central integration test of this spec.
#[tokio::test]
async fn dropping_many_test_apps_without_cleanup_does_not_accumulate_connections() {
    let _exclusive = EXCLUSIVE_DATABASE_ACCESS.lock().await;
    if !should_run_against_real_database(
        "dropping_many_test_apps_without_cleanup_does_not_accumulate_connections",
    ) {
        return;
    }

    let probe = AdminProbe::connect().await;
    let baseline_connections = probe.connection_count().await;

    // The whole point of the loop: `app` is bound and then dropped at the end
    // of each iteration with no `cleanup()` anywhere — the exact usage
    // Requirement 2.4 says must not leak, and which 131 call sites in this
    // repository already exhibit.
    let mut own_schemas = BTreeSet::new();
    let mut peak_connections = baseline_connections;
    for index in 0..DROPPED_INSTANCES {
        let app = spawn_test_app().await;
        // Recorded before the drop, from the instance's own pool: this is
        // what makes the settle assertion below speak about this test's
        // fixtures specifically rather than about the server's state.
        own_schemas.insert(isolated_schema_of(&app.pool).await);
        drop(app);

        let observed = probe.connection_count().await;
        peak_connections = peak_connections.max(observed);
        assert!(
            observed <= baseline_connections + PEAK_ALLOWANCE,
            "Requirement 2.5: after dropping instance {index} of {DROPPED_INSTANCES}, the \
             connections held ({observed}) exceeded the baseline ({baseline_connections}) by \
             more than the {PEAK_ALLOWANCE} attributable to the single live instance and the \
             harness's transient admin connections — held connections are growing with the \
             number of *dropped* instances, not with the number of live ones"
        );
    }

    // The reaper works off a queue on its own runtime, so the last few
    // reclaims are legitimately still in flight the instant the loop ends.
    // What is asserted is where the two measures settle, not how quickly.
    let settled = wait_until_reclaimed(&probe, baseline_connections, &own_schemas).await;
    probe.close().await;

    assert!(
        settled.connections <= baseline_connections + SETTLED_ALLOWANCE,
        "Requirements 2.1/2.4: {DROPPED_INSTANCES} instances were created and dropped without \
         cleanup(); after waiting {SETTLE_TIMEOUT:?} the process still holds {} connections \
         against a baseline of {baseline_connections} (allowance {SETTLED_ALLOWANCE}, peak \
         observed during the loop {peak_connections}). Dropping a TestApp is not releasing its \
         pool.",
        settled.connections
    );
    assert!(
        // Exactly zero, not a tolerance: every name checked here was read off
        // one of this test's own fixtures, so anything still present after
        // the wait is genuine residue from a reclaim that never completed —
        // it cannot be another test's schema that merely has not been
        // reclaimed yet.
        settled.surviving_schemas.is_empty(),
        "Requirement 2.1: {} of the {} isolated schemas created by this test's dropped \
         instances are still present, so the reclaim's DROP SCHEMA half did not run: {:?}",
        settled.surviving_schemas.len(),
        own_schemas.len(),
        settled.surviving_schemas
    );
}

/// Requirement 2.2: a test that ends by panicking must still have its
/// connections and schema released. The panic is staged inside a spawned task
/// so this test can observe the aftermath — `TestApp::drop` runs during that
/// task's unwind, which is the situation in which a panicking destructor
/// would abort the entire process rather than fail one test, so "this test
/// reports a result at all" is itself part of the assertion.
#[tokio::test]
async fn a_panicking_test_still_releases_its_connections_and_schema() {
    let _exclusive = EXCLUSIVE_DATABASE_ACCESS.lock().await;
    if !should_run_against_real_database(
        "a_panicking_test_still_releases_its_connections_and_schema",
    ) {
        return;
    }

    let probe = AdminProbe::connect().await;
    let baseline_connections = probe.connection_count().await;

    // The panicking task's own schema name has to leave the task *before* the
    // panic, because unwinding discards everything the task would otherwise
    // have returned — and without it this test would have to guess which
    // schema was its own.
    let (schema_tx, schema_rx) = oneshot::channel();
    let panicked = tokio::spawn(async move {
        // Deliberately never cleaned up: the panic unwinds straight past any
        // release call a well-written test would have made, which is exactly
        // the abnormal termination Requirement 2.2 covers.
        let app = spawn_test_app().await;
        let _ = schema_tx.send(isolated_schema_of(&app.pool).await);
        panic!("deliberate panic standing in for a failing test assertion");
    })
    .await
    .expect_err("the spawned task must have panicked");
    assert!(
        panicked.is_panic(),
        "the task must have ended in a panic, not a cancellation — otherwise this test is not \
         exercising release-during-unwind at all"
    );
    let own_schema = schema_rx
        .await
        .expect("the panicking task must have reported its isolated schema before panicking");
    let own_schemas = BTreeSet::from([own_schema.clone()]);

    let settled = wait_until_reclaimed(&probe, baseline_connections, &own_schemas).await;
    probe.close().await;

    assert!(
        settled.connections <= baseline_connections + SETTLED_ALLOWANCE,
        "Requirement 2.2: after a panicking test, {} connections are still held against a \
         baseline of {baseline_connections} (allowance {SETTLED_ALLOWANCE})",
        settled.connections
    );
    assert!(
        settled.surviving_schemas.is_empty(),
        "Requirement 2.2: the panicking task's own isolated schema {own_schema} is still \
         present after waiting {SETTLE_TIMEOUT:?} — detected residue: {:?}",
        settled.surviving_schemas
    );
}

/// Third bullet of task 1.2: the explicit path and the reaper path must
/// coexist on the same pool without breaking. `cleanup()` takes `self` by
/// value, so `Drop` *always* runs immediately afterwards on an
/// already-released instance — the one case where both paths meet on the same
/// value, on every single one of the 619 existing `cleanup()` call sites.
///
/// Nothing observable may go wrong: no panic (which, in a `Drop` running
/// during an unwind, would abort the whole test process), no reopened
/// connection, and no resurrection or double-drop of the isolated schema.
#[tokio::test]
async fn cleanup_followed_by_drop_releases_exactly_once() {
    let _exclusive = EXCLUSIVE_DATABASE_ACCESS.lock().await;
    if !should_run_against_real_database("cleanup_followed_by_drop_releases_exactly_once") {
        return;
    }

    let probe = AdminProbe::connect().await;
    let baseline_connections = probe.connection_count().await;

    let app = spawn_test_app().await;
    let pool = app.pool.clone();
    let schema = isolated_schema_of(&app.pool).await;
    let own_schemas = BTreeSet::from([schema.clone()]);

    // The explicit path runs to completion first; `Drop` then runs on the
    // returned unit's stack frame, against a pool this call already closed
    // and a schema it already dropped.
    app.cleanup().await;
    assert!(
        pool.is_closed(),
        "cleanup() must still close the pool itself — Drop's delegation is an addition to the \
         explicit path, not a replacement for it"
    );

    // Give any (unwanted) reclaim submitted by the subsequent `Drop` more
    // than enough time to run, so its effects would be visible below rather
    // than racing past the assertions.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let surviving = probe.surviving_schemas(&own_schemas).await;
    let settled_connections = probe.connection_count().await;
    probe.close().await;

    assert!(
        surviving.is_empty(),
        "the isolated schema {schema} must be gone after cleanup(), and the following Drop must \
         not have recreated or resurrected it"
    );
    assert!(
        pool.is_closed(),
        "the pool must still be closed after Drop ran on the cleaned-up instance"
    );
    assert!(
        settled_connections <= baseline_connections + SETTLED_ALLOWANCE,
        "cleanup() followed by Drop must leave no connections behind: {settled_connections} held \
         against a baseline of {baseline_connections} (allowance {SETTLED_ALLOWANCE})"
    );
}
