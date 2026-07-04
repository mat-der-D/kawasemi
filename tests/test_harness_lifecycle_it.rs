//! Integration tests for task 9.1 ("ライフサイクルとマイグレーション安全性の統合テ
//! ストを追加する"): lifecycle and migration-safety guarantees observed
//! *through* the TestHarness boundary itself (`spawn_test_app` /
//! `TestApp::cleanup`), consolidating Requirements 1.1, 1.5, 4.4, 4.5, 4.6,
//! 8.1, 8.2 per design.md's "Testing Strategy" -> "Integration Tests" ->
//! "Lifecycle" / "Migration safety" entries.
//!
//! ## Why this lives in its own `tests/*.rs` binary, not `src/test_harness/tests.rs`
//! `spawn_test_app` itself never calls `telemetry::init_telemetry` (see
//! `src/test_harness.rs`'s own doc comment: it deliberately recomposes the
//! same building blocks `bootstrap()` does, minus telemetry, precisely so
//! many `#[tokio::test]` functions across many specs' own test binaries can
//! each call it repeatedly without ever touching the process-global,
//! install-once `tracing` subscriber `tests/bootstrap_lifecycle_it.rs`'s doc
//! comment describes). So the process-isolation constraint that forces that
//! file into its own binary does not technically apply here. This file is
//! still placed under `tests/` (rather than as `#[cfg(test)] mod tests`
//! inside `src/test_harness.rs`) because task 9.1 sits under this spec's "9.
//! 統合と検証" phase: a consolidated integration-level pass that exercises
//! the already-implemented Migrate/TestHarness components' cross-cutting
//! guarantees together, distinct from task 8.1's own component-level unit
//! tests (`src/test_harness/tests.rs`, which already covers spawning,
//! health, migrations-applied, deterministic runtime, isolation, and
//! `cleanup()`'s resource release in isolation). This file deliberately does
//! not re-duplicate those assertions verbatim; it additionally proves the
//! migration-safety properties (4.4, 4.5, 4.6) through a `TestApp`'s own
//! pool/schema, and a consolidated lifecycle round trip (health check +
//! `cleanup()`) as this task's own text requires.
//!
//! ## Strategy for 4.5/4.6 through `spawn_test_app`
//! The single embedded migration (`migrations/0001_init_runtime.sql`) is a
//! deliberate no-op (see `src/migrate.rs`'s module doc comment and this
//! spec's `tasks.md` "Implementation Notes" entry for task 4.2), so a
//! genuine execution failure or checksum mismatch can never arise merely
//! from calling `spawn_test_app()` against a fresh schema — there is nothing
//! in the embedded set that can fail, and nothing pre-existing to tamper
//! with, before the harness itself already recorded it as applied. Task
//! 4.2's own `src/migrate/tests.rs` already established the reusable pattern
//! for proving this regardless: exercise `apply_migrations`'s error surface
//! a second time against an already-migrated pool/schema (for checksum
//! tampering), or against a deliberately-broken ad hoc `Migrator` built via
//! `Migrator::with_migrations` (for an execution failure) — never touching
//! the committed `migrations/` directory itself. This file applies that
//! exact, previously-validated strategy, but against the isolated
//! schema/pool a real `spawn_test_app()` call actually produced, tying the
//! coverage to the TestHarness boundary this task is scoped to
//! (`_Boundary: TestHarness_`) rather than to a bespoke schema/pool
//! assembled from scratch the way `src/migrate/tests.rs` had to.

use std::net::TcpStream;
use std::time::Duration;

use kawasemi::migrate::{self, MigrateError};
use kawasemi::test_harness::spawn_test_app;
use sqlx::Row;
use sqlx::SqlSafeStr;
use sqlx::migrate::{MigrateError as SqlxMigrateError, Migration, MigrationType, Migrator};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

/// Same raw-HTTP-over-TCP probe `tests/bootstrap_lifecycle_it.rs` and
/// `src/test_harness/tests.rs` already use, so this proves `spawn_test_app`
/// serves the real foundation router, not merely that some socket accepted
/// a TCP connection.
async fn raw_http_get(addr: std::net::SocketAddr, path: &str) -> String {
    let mut stream =
        tokio::time::timeout(Duration::from_secs(5), tokio::net::TcpStream::connect(addr))
            .await
            .expect("connecting to spawn_test_app's address must not time out")
            .expect("connect");
    let request = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write request");
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut buf))
        .await
        .expect("reading the response must not time out")
        .expect("read response");
    String::from_utf8_lossy(&buf).to_string()
}

/// Requirements 1.1, 1.5, 8.1, 8.2: `spawn_test_app` boots a real,
/// connectable instance whose database already has the embedded migrations
/// applied and whose `/health` route actually responds, and the explicit
/// `TestApp::cleanup()` call the test code makes actually releases the
/// resources acquired (listener stops accepting connections, pool closes).
#[tokio::test]
async fn spawn_test_app_lifecycle_health_check_and_cleanup_round_trip() {
    if !should_run_against_real_database(
        "spawn_test_app_lifecycle_health_check_and_cleanup_round_trip",
    ) {
        return;
    }

    let app = spawn_test_app().await;
    let address = app.address;
    let pool = app.pool.clone();

    // Requirements 1.1 / 8.1: a real, connectable instance serving the
    // actual foundation router.
    let health_response = raw_http_get(address, "/health").await;
    assert!(
        health_response.starts_with("HTTP/1.1 200"),
        "spawn_test_app's address must serve /health with 200: {health_response}"
    );

    // Requirement 8.2: the database it serves against already has the
    // embedded migrations applied.
    let migration_count: i64 = sqlx::query("SELECT count(*) AS c FROM _sqlx_migrations")
        .fetch_one(&app.pool)
        .await
        .expect("_sqlx_migrations must exist and be queryable: migrations must be pre-applied")
        .get("c");
    assert!(
        migration_count > 0,
        "spawn_test_app must provide a database with the embedded migrations already applied"
    );

    // Requirement 1.5 / 8.5: the explicit cleanup() the test code calls
    // actually releases the acquired resources.
    app.cleanup().await;

    // Give the (now-signaled) listener task a brief moment to actually stop
    // accepting connections; cleanup() itself already awaits it, but leave a
    // small safety margin for the OS to release the socket (mirrors
    // `src/test_harness/tests.rs`'s own convention for this exact check).
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert!(
        tokio::net::TcpStream::connect(address).await.is_err(),
        "the listener must no longer accept connections after cleanup() completes"
    );
    assert!(
        pool.is_closed(),
        "the connection pool must be closed after cleanup() completes"
    );
}

/// Requirement 4.4: re-running `apply_migrations` against an
/// already-migrated `TestApp` database (simulating what a restart's
/// automatic re-apply step would do) preserves existing data and does not
/// re-apply or alter the recorded migration history.
#[tokio::test]
async fn spawn_test_app_migration_reapply_preserves_data_and_history() {
    if !should_run_against_real_database(
        "spawn_test_app_migration_reapply_preserves_data_and_history",
    ) {
        return;
    }

    let app = spawn_test_app().await;

    let before: Vec<(i64, Vec<u8>)> =
        sqlx::query_as("SELECT version, checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&app.pool)
            .await
            .expect("_sqlx_migrations must be queryable on a freshly-spawned TestApp");
    assert!(
        !before.is_empty(),
        "a freshly-spawned TestApp must already have at least the embedded migration recorded"
    );

    // Simulate a restart's automatic re-apply step running against the same
    // already-migrated database this TestApp booted with.
    migrate::apply_migrations(&app.pool).await.expect(
        "re-running apply_migrations against an already-migrated TestApp database must be \
             a harmless no-op",
    );

    let after: Vec<(i64, Vec<u8>)> =
        sqlx::query_as("SELECT version, checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&app.pool)
            .await
            .expect("_sqlx_migrations must still be queryable after the second run");

    assert_eq!(
        before, after,
        "Requirement 4.4: re-applying migrations against an already-migrated TestApp database \
         must not reapply migrations or alter recorded history — existing data must be \
         preserved"
    );

    app.cleanup().await;
}

/// Requirement 4.6: when an already-applied migration's checksum recorded in
/// a `TestApp`'s own `_sqlx_migrations` history no longer matches the
/// embedded definition, re-running `apply_migrations` against that same
/// database detects the inconsistency and aborts with `Err`, never
/// panicking or silently continuing.
#[tokio::test]
async fn spawn_test_app_detects_checksum_mismatch_and_aborts() {
    if !should_run_against_real_database("spawn_test_app_detects_checksum_mismatch_and_aborts") {
        return;
    }

    let app = spawn_test_app().await;

    let tampered_rows = sqlx::query(
        "UPDATE _sqlx_migrations SET checksum = decode('deadbeef', 'hex') WHERE version = \
         (SELECT version FROM _sqlx_migrations ORDER BY version LIMIT 1)",
    )
    .execute(&app.pool)
    .await
    .expect("tampering with the TestApp's recorded checksum must succeed")
    .rows_affected();
    assert_eq!(
        tampered_rows, 1,
        "the tamper statement must touch exactly the one recorded migration row"
    );

    let err = migrate::apply_migrations(&app.pool).await.expect_err(
        "apply_migrations must return Err when a TestApp's history is checksum-inconsistent",
    );

    assert!(
        matches!(err.source, SqlxMigrateError::VersionMismatch(_)),
        "Requirement 4.6: a checksum inconsistency must surface as sqlx's dedicated \
         VersionMismatch variant, not a generic execution failure: {:?}",
        err.source
    );

    let rendered = format!("{err}");
    assert!(
        rendered.contains("modified") || rendered.contains("mismatch"),
        "MigrateError's Display must retain sqlx's own identifying diagnostic text: {rendered}"
    );

    use std::error::Error as _;
    assert!(
        err.source().is_some(),
        "MigrateError must expose the underlying sqlx::migrate::MigrateError via Error::source"
    );

    app.cleanup().await;
}

/// Requirement 4.5: a genuine migration execution failure against a
/// `TestApp`'s own database must surface as `MigrateError` with enough
/// detail to identify which migration failed — not a panic, not a
/// silently-ignored error. As documented in this file's module doc comment
/// (and this spec's `tasks.md` Implementation Notes for task 4.2), the
/// single embedded migration is a deliberate no-op that cannot itself be
/// made to fail without editing the committed `migrations/` directory, so
/// this exercises `MigrateError`'s wrapping behavior directly against a
/// deliberately-invalid ad hoc migration built in-memory via
/// `Migrator::with_migrations`, run for real against the `TestApp`'s own
/// isolated database/schema.
#[tokio::test]
async fn spawn_test_app_migration_execution_failure_surfaces_identifying_detail() {
    if !should_run_against_real_database(
        "spawn_test_app_migration_execution_failure_surfaces_identifying_detail",
    ) {
        return;
    }

    let app = spawn_test_app().await;

    const BROKEN_VERSION: i64 = 999_999_999;
    const BROKEN_SQL: &str = "THIS IS NOT VALID SQL AND MUST FAIL;";
    let mut broken_migrator = Migrator::with_migrations(vec![Migration::new(
        BROKEN_VERSION,
        "deliberately_invalid".into(),
        MigrationType::Simple,
        BROKEN_SQL.into_sql_str(),
        false,
    )]);
    // Unlike `src/migrate/tests.rs`'s own precedent (which ran its ad hoc
    // migrator against a brand-new, still-unmigrated schema/pool), this
    // `TestApp`'s database already has the real embedded migration (version
    // 1) applied by `spawn_test_app` itself. Without `ignore_missing`,
    // `Migrator::run`'s own `validate_applied_migrations` step would reject
    // this ad hoc migrator with `VersionMissing(1)` before ever attempting
    // to execute the deliberately-broken migration, since this migrator's
    // own list only knows about `BROKEN_VERSION`. Setting `ignore_missing`
    // opts out of that unrelated check so this test can isolate exactly the
    // execution-failure path Requirement 4.5 is about.
    broken_migrator.set_ignore_missing(true);

    let sqlx_err = broken_migrator.run(&app.pool).await.expect_err(
        "running a deliberately invalid ad hoc migration against a TestApp's \
                     database must fail",
    );

    assert!(
        matches!(
            sqlx_err,
            SqlxMigrateError::ExecuteMigration(_, BROKEN_VERSION)
        ),
        "Requirement 4.5: an execution failure must surface as sqlx's ExecuteMigration variant \
         naming the failed migration's version: {sqlx_err:?}"
    );

    let wrapped = MigrateError { source: sqlx_err };

    let rendered = format!("{wrapped}");
    assert!(
        rendered.contains(&BROKEN_VERSION.to_string()),
        "MigrateError's Display must retain sqlx's identifying detail (the failed migration's \
         version): {rendered}"
    );

    use std::error::Error as _;
    assert!(
        wrapped.source().is_some(),
        "MigrateError must expose the underlying sqlx::migrate::MigrateError via Error::source"
    );

    app.cleanup().await;
}
