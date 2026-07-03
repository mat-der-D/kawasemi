//! Integration-style tests for the Migrate boundary (Requirements 4.2, 4.3,
//! 4.4, 4.5, 4.6), per task 4.2's acceptance bullets: "schema is up to date
//! after applying", "re-running does not reapply and preserves data", and
//! "inconsistency/failure aborts startup".
//!
//! Mirrors `src/db/tests.rs`'s (task 4.1) conventions: a cheap raw-TCP
//! preflight probe against the default local test database, independent of
//! sqlx, gates the real assertions so this suite skips with a diagnostic
//! rather than failing in an environment with no local PostgreSQL (and an
//! explicit `KAWASEMI_TEST_DATABASE_URL` override opts out of the skip). The
//! target database/role (`kawasemi_test`) is the same fixed local-only,
//! non-production credential task 4.1 provisioned.
//!
//! Isolation: unlike task 4.1's tests (which only ever *read* via a trivial
//! `SELECT 1` and never touch schema state), applying migrations mutates
//! `_sqlx_migrations` history, and Rust runs `#[tokio::test]` functions
//! concurrently by default. The `kawasemi_test` role has no `CREATEDB`
//! privilege (verified against the sandbox this task was implemented in),
//! so per-test isolation cannot use a fresh throwaway *database* the way a
//! richer harness (task 8.1's `spawn_test_app`) eventually will. Instead,
//! each test creates its own throwaway PostgreSQL *schema* and opens a
//! dedicated pool whose connections pin `search_path` to only that schema
//! (no `public` fallback), so the unqualified `_sqlx_migrations` bookkeeping
//! table sqlx creates for [`apply_migrations`] lands in a schema private to
//! that test and never collides with another test's history. This runs the
//! exact same `apply_migrations` production code path (default table name,
//! default embedded `migrations/`), not a test-only substitute.

use std::net::TcpStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sqlx::migrate::{MigrateError as SqlxMigrateError, Migration, MigrationType, Migrator};
use sqlx::postgres::PgPoolOptions;
use sqlx::{Executor, SqlSafeStr};

use super::*;

const TEST_DB_HOST: &str = "127.0.0.1";
const TEST_DB_PORT: u16 = 5432;
const TEST_DB_URL_ENV: &str = "KAWASEMI_TEST_DATABASE_URL";
const DEFAULT_TEST_DB_URL: &str =
    "postgres://kawasemi_test:kawasemi_test_pw@127.0.0.1:5432/kawasemi_test";

fn test_db_url() -> String {
    std::env::var(TEST_DB_URL_ENV).unwrap_or_else(|_| DEFAULT_TEST_DB_URL.to_string())
}

/// Best-effort raw-TCP reachability probe, independent of sqlx/`apply_migrations`.
/// Used only to decide whether to skip these tests in environments with no
/// local PostgreSQL at all; never used to swallow a real regression.
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

/// Generates a schema name unique to this test process/run: a monotonic
/// counter plus wall-clock nanos, so concurrently-running `#[tokio::test]`
/// functions (and repeated `cargo test` invocations) never collide.
fn unique_schema_name(label: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("kawasemi_migrate_test_{label}_{nanos}_{seq}")
}

/// Creates `schema` (via a throwaway bootstrap connection to the shared test
/// database) and returns a dedicated pool whose connections pin
/// `search_path` to exactly that schema. Every unqualified table reference
/// `apply_migrations` issues (including sqlx's default `_sqlx_migrations`)
/// therefore lands in `schema`, isolated from every other test.
async fn isolated_test_pool(schema: &str) -> sqlx::PgPool {
    let bootstrap = PgPoolOptions::new()
        .max_connections(1)
        .connect(&test_db_url())
        .await
        .expect("bootstrap connection to the test database must succeed");
    // Safe to assert: `schema` is always this file's own
    // `unique_schema_name` output (a fixed prefix plus numeric
    // timestamp/counter), never untrusted input.
    bootstrap
        .execute(sqlx::query(sqlx::AssertSqlSafe(format!(
            r#"CREATE SCHEMA "{schema}""#
        ))))
        .await
        .expect("creating the throwaway test schema must succeed");
    bootstrap.close().await;

    let schema_owned = schema.to_string();
    PgPoolOptions::new()
        .max_connections(2)
        .after_connect(move |conn, _meta| {
            let schema = schema_owned.clone();
            Box::pin(async move {
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    r#"SET search_path TO "{schema}""#
                )))
                .execute(conn)
                .await?;
                Ok(())
            })
        })
        .connect(&test_db_url())
        .await
        .expect("isolated per-test pool must connect successfully")
}

/// Best-effort cleanup: drops the throwaway schema (and everything in it,
/// including the test's private `_sqlx_migrations` table) so repeated local
/// test runs do not accumulate schemas. Failures here are logged, not
/// panicked on, since they never indicate a regression in the code under
/// test.
async fn cleanup_schema(schema: &str) {
    if let Ok(bootstrap) = PgPoolOptions::new()
        .max_connections(1)
        .connect(&test_db_url())
        .await
    {
        if let Err(err) = bootstrap
            .execute(sqlx::query(sqlx::AssertSqlSafe(format!(
                r#"DROP SCHEMA IF EXISTS "{schema}" CASCADE"#
            ))))
            .await
        {
            eprintln!("cleanup_schema: failed to drop schema {schema}: {err}");
        }
        bootstrap.close().await;
    }
}

/// Requirements 4.2, 4.3, 4.4: applying the embedded migrations against a
/// fresh, isolated schema brings `_sqlx_migrations` up to date with exactly
/// the embedded migration set (schema is "up to date" after applying), and
/// running `apply_migrations` again against the same pool/database ("a
/// restart") is a harmless no-op: it neither errors, nor re-applies, nor
/// changes the recorded history row(s) — the observable proof available
/// given migration 0001 owns no domain table yet (its own doc comment notes
/// it is intentionally a near no-op).
#[tokio::test]
async fn apply_migrations_updates_schema_and_second_run_is_idempotent_noop() {
    if !should_run_against_real_database(
        "apply_migrations_updates_schema_and_second_run_is_idempotent_noop",
    ) {
        return;
    }

    let schema = unique_schema_name("idempotent");
    let pool = isolated_test_pool(&schema).await;

    apply_migrations(&pool)
        .await
        .expect("first apply_migrations run must succeed against a fresh isolated schema");

    let after_first: Vec<(i64, Vec<u8>)> =
        sqlx::query_as("SELECT version, checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&pool)
            .await
            .expect("_sqlx_migrations must exist and be queryable after applying");

    assert!(
        !after_first.is_empty(),
        "schema must be up to date: _sqlx_migrations must record at least the embedded \
         migration(s) after applying"
    );
    assert!(
        MIGRATOR.version_exists(after_first[0].0),
        "the recorded version must be one of the embedded migrations"
    );

    apply_migrations(&pool)
        .await
        .expect("re-running apply_migrations (simulating a restart) must be a harmless no-op");

    let after_second: Vec<(i64, Vec<u8>)> =
        sqlx::query_as("SELECT version, checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&pool)
            .await
            .expect("_sqlx_migrations must still be queryable after the second run");

    assert_eq!(
        after_first, after_second,
        "Requirement 4.4: re-running apply_migrations must not reapply migrations or alter \
         recorded history — a restart must preserve existing data"
    );

    pool.close().await;
    cleanup_schema(&schema).await;
}

/// Requirement 4.6: when an already-applied migration's checksum recorded
/// in `_sqlx_migrations` no longer matches the embedded definition (here
/// simulated by directly tampering with the stored checksum, the most
/// direct way to construct a genuine inconsistency without hand-editing the
/// committed `migrations/0001_init_runtime.sql` file), `apply_migrations`
/// detects the inconsistency and returns an `Err(MigrateError)` — it must
/// not panic or silently continue.
#[tokio::test]
async fn apply_migrations_detects_checksum_mismatch_and_aborts() {
    if !should_run_against_real_database("apply_migrations_detects_checksum_mismatch_and_aborts") {
        return;
    }

    let schema = unique_schema_name("checksum_mismatch");
    let pool = isolated_test_pool(&schema).await;

    apply_migrations(&pool)
        .await
        .expect("initial apply_migrations run must succeed before tampering");

    let tampered_rows = sqlx::query(
        "UPDATE _sqlx_migrations SET checksum = decode('deadbeef', 'hex') WHERE version = \
         (SELECT version FROM _sqlx_migrations ORDER BY version LIMIT 1)",
    )
    .execute(&pool)
    .await
    .expect("tampering with the recorded checksum must succeed")
    .rows_affected();
    assert_eq!(
        tampered_rows, 1,
        "the tamper statement must touch exactly the one recorded migration row"
    );

    let err = apply_migrations(&pool)
        .await
        .expect_err("apply_migrations must return Err when history is checksum-inconsistent");

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

    pool.close().await;
    cleanup_schema(&schema).await;
}

/// Requirement 4.5: a genuine migration execution failure must surface as
/// `MigrateError` with enough detail to identify which migration failed —
/// not a panic, not a silently-ignored error. The single embedded migration
/// (`migrations/0001_init_runtime.sql`) is a deliberate no-op per its own
/// doc comment, and this task's boundary forbids adding new files under
/// `migrations/` to force a failure there, so this test exercises
/// `MigrateError`'s wrapping behavior directly against a deliberately
/// invalid ad hoc migration built in-memory via `Migrator::with_migrations`
/// (never touching the `migrations/` directory or the embedded static
/// `MIGRATOR`), run for real against an isolated schema. This proves the
/// identical `MigrateError { source }` construction this module's
/// `apply_migrations` uses preserves sqlx's own `ExecuteMigration(_, version)`
/// identifying detail; see this test file's module doc comment and this
/// task's status report for why `apply_migrations` itself could not be
/// driven through a genuine execution failure within this task's boundary.
#[tokio::test]
async fn migrate_error_wraps_execute_migration_failure_with_identifying_detail() {
    if !should_run_against_real_database(
        "migrate_error_wraps_execute_migration_failure_with_identifying_detail",
    ) {
        return;
    }

    let schema = unique_schema_name("execute_failure");
    let pool = isolated_test_pool(&schema).await;

    const BROKEN_VERSION: i64 = 999_999_999;
    const BROKEN_SQL: &str = "THIS IS NOT VALID SQL AND MUST FAIL;";
    let broken_migrator = Migrator::with_migrations(vec![Migration::new(
        BROKEN_VERSION,
        "deliberately_invalid".into(),
        MigrationType::Simple,
        BROKEN_SQL.into_sql_str(),
        false,
    )]);

    let sqlx_err = broken_migrator
        .run(&pool)
        .await
        .expect_err("running a deliberately invalid ad hoc migration must fail");

    assert!(
        matches!(
            sqlx_err,
            SqlxMigrateError::ExecuteMigration(_, BROKEN_VERSION)
        ),
        "Requirement 4.5: an execution failure must surface as sqlx's ExecuteMigration \
         variant naming the failed migration's version: {sqlx_err:?}"
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

    pool.close().await;
    cleanup_schema(&schema).await;
}
