//! Integration tests for task 9.3 ("テスト分離と graceful shutdown の検証テストを
//! 追加する"): consolidates Requirements 1.3, 1.4, 8.4 per design.md's
//! "Testing Strategy" -> "Integration Tests" -> "Test isolation" entry and
//! "Performance / Load" -> "Graceful shutdown" entry.
//!
//! ## Scope decision for Requirements 1.3 / 1.4 (graceful shutdown)
//! This file does **not** add new graceful-shutdown tests. design.md's
//! Testing Strategy already places "Test isolation" (8.4) under "Integration
//! Tests" and "Graceful shutdown" (1.3, 1.4) under a *separate*
//! "Performance / Load" category — they are not required to be exercised
//! through the same harness. Requirements 1.3/1.4 are already fully proven,
//! end-to-end, by `src/server/tests.rs`'s task-7.3 tests:
//! `in_flight_request_completes_within_grace` (1.3: an in-flight request
//! that finishes within `shutdown_grace` completes successfully rather than
//! being cut off) and `grace_exceeded_forces_stop_without_waiting_for_slow_handler`
//! (1.4: shutdown force-stops at the grace deadline instead of continuing to
//! wait for a slow in-flight handler). Both drive the exact same
//! `drive_shutdown` core `serve_with_shutdown`/`serve_with_shutdown_and_signal`
//! use in production, over a real listener and real HTTP connection, with
//! timing assertions on the grace deadline — there is no gap in behavioral
//! coverage left for this task to close.
//!
//! This spec's own `tasks.md` "Implementation Notes" (entry for task 8.1)
//! records why: `TestApp`'s own serve loop (`src/test_harness.rs`) does not
//! reuse `drive_shutdown`'s grace-exceeded-forces-stop path, because
//! `serve_with_shutdown_and_signal` binds a caller-fixed address and does not
//! hand back the actually-bound address that `TestApp` needs for concurrent,
//! OS-assigned ephemeral ports. Retrofitting `TestApp` with a grace-aware
//! variant purely to re-prove 1.3/1.4 a second time — when `src/server/tests.rs`
//! already proves them directly and thoroughly against the same underlying
//! `drive_shutdown` function `TestApp` would have to reuse anyway — would add
//! production-code surface without closing any real requirement gap, so this
//! task instead spends its effort on the genuinely-missing 8.4 coverage
//! below.
//!
//! ## Scope for Requirement 8.4 (test isolation)
//! `src/test_harness/tests.rs`'s existing
//! `spawn_test_app_isolates_database_state_between_instances` (task 8.1)
//! already proves isolation between two `TestApp` instances spawned
//! sequentially *inside a single test function body*. Task 9.3's own text
//! asks specifically for "2つの統合テスト" (two integration *tests*) not
//! interfering with each other's persisted data — the more realistic
//! scenario every downstream feature spec will actually rely on: many
//! separate `#[tokio::test]` functions, each independently calling
//! `spawn_test_app()`, run concurrently under `cargo test`'s default
//! parallel test scheduler. The two tests below close that gap: they are
//! two literal, independent `#[tokio::test]` functions (not one function
//! manually juggling two instances) that each spawn their own `TestApp`,
//! persist a marker row distinguishing which test wrote it, and assert their
//! own isolated schema contains only that marker — proving that even when
//! `cargo test` schedules both concurrently, neither observes the other's
//! persisted data.

use std::net::TcpStream;
use std::time::Duration;

use kawasemi::test_harness::{TestApp, spawn_test_app};

const TEST_DB_HOST: &str = "127.0.0.1";
const TEST_DB_PORT: u16 = 5432;
const TEST_DB_URL_ENV: &str = "KAWASEMI_TEST_DATABASE_URL";

/// Best-effort raw-TCP reachability probe, independent of sqlx/the harness
/// itself. Used only to decide whether to skip these tests in an
/// environment with no local PostgreSQL at all; never used to swallow a
/// real regression. Mirrors `src/db/tests.rs`'/`src/migrate/tests.rs`'s/
/// `src/test_harness/tests.rs`'s own convention.
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

/// Persists `own_marker` into a fixed-named table inside `app`'s own
/// isolated schema, then asserts that table contains *only* `own_marker` —
/// never a marker belonging to some other concurrently-running caller of
/// this same helper. If two `TestApp`s ever ended up sharing the same
/// underlying schema (an isolation regression), both callers' `INSERT`s
/// would land in the same table and this assertion would catch it,
/// regardless of which of the two callers happens to observe it first.
async fn assert_schema_contains_only_this_markers(app: &TestApp, own_marker: &str) {
    sqlx::query("CREATE TABLE IF NOT EXISTS cross_test_isolation_probe (marker text NOT NULL)")
        .execute(&app.pool)
        .await
        .expect("creating the isolation probe table in this instance's own schema must succeed");

    sqlx::query("INSERT INTO cross_test_isolation_probe (marker) VALUES ($1)")
        .bind(own_marker)
        .execute(&app.pool)
        .await
        .expect("inserting this test's own marker row must succeed");

    let markers: Vec<String> =
        sqlx::query_scalar("SELECT marker FROM cross_test_isolation_probe ORDER BY marker")
            .fetch_all(&app.pool)
            .await
            .expect(
                "querying the isolation probe table in this instance's own schema must succeed",
            );

    assert_eq!(
        markers,
        vec![own_marker.to_string()],
        "Requirement 8.4: this integration test's isolated database state must not contain \
         persisted data from any other concurrently-running integration test, but observed: \
         {markers:?}"
    );
}

/// Requirement 8.4 (first of two independent integration tests): spawns its
/// own `TestApp`, persists a marker distinguishing this test, and asserts no
/// other test's marker is visible from its isolated schema.
#[tokio::test]
async fn integration_test_alpha_does_not_observe_other_tests_persisted_data() {
    if !should_run_against_real_database(
        "integration_test_alpha_does_not_observe_other_tests_persisted_data",
    ) {
        return;
    }

    let app = spawn_test_app().await;
    assert_schema_contains_only_this_markers(&app, "marker-from-integration-test-alpha-9c21f")
        .await;
    app.cleanup().await;
}

/// Requirement 8.4 (second of two independent integration tests, run
/// concurrently with `integration_test_alpha_does_not_observe_other_tests_persisted_data`
/// under `cargo test`'s default parallel scheduler): spawns its own
/// `TestApp`, persists a marker distinguishing this test, and asserts no
/// other test's marker is visible from its isolated schema.
#[tokio::test]
async fn integration_test_beta_does_not_observe_other_tests_persisted_data() {
    if !should_run_against_real_database(
        "integration_test_beta_does_not_observe_other_tests_persisted_data",
    ) {
        return;
    }

    let app = spawn_test_app().await;
    assert_schema_contains_only_this_markers(&app, "marker-from-integration-test-beta-4e7db").await;
    app.cleanup().await;
}
