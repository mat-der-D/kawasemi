//! Integration tests for embedded migration auto-apply on startup
//! (Requirement 4: 埋め込みマイグレーションと起動時自動実行).
//!
//! Each test that needs to run migrations creates a fresh, uniquely-named
//! database (via the always-present `postgres` maintenance database),
//! runs against it, then drops it. This keeps tests isolated from each
//! other's `_sqlx_migrations` history even though `cargo test` runs tests
//! in parallel by default -- a single shared database would let one
//! test's applied-migration history leak into another's assertions.

use std::time::Duration;

use kawasemi::config::{DatabaseConfig, Secret};
use kawasemi::db::establish_pool;
use kawasemi::migrate::run_migrations;
use sqlx::Row;
use sqlx::postgres::PgPoolOptions;

const MAINTENANCE_DB_URL: &str = "postgres://postgres@127.0.0.1:5432/postgres";

/// A uniquely-named scratch database, created on `new()` and dropped on
/// `Drop`, so each test gets its own `_sqlx_migrations` history.
struct ScratchDb {
    name: String,
}

impl ScratchDb {
    async fn new(label: &str) -> Self {
        static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let name = format!("kawasemi_migrate_it_{}_{}_{}", label, std::process::id(), n);

        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(MAINTENANCE_DB_URL)
            .await
            .expect("failed to connect to maintenance database `postgres`");

        sqlx::query(&format!("CREATE DATABASE \"{name}\""))
            .execute(&admin_pool)
            .await
            .expect("failed to create scratch test database");

        Self { name }
    }

    fn url(&self) -> String {
        format!("postgres://postgres@127.0.0.1:5432/{}", self.name)
    }

    fn db_config(&self) -> DatabaseConfig {
        DatabaseConfig {
            url: Secret::new(self.url()),
            max_connections: 5,
            acquire_timeout: Duration::from_secs(5),
        }
    }
}

impl Drop for ScratchDb {
    fn drop(&mut self) {
        let name = self.name.clone();
        // `Drop` cannot be async; spawn a short-lived blocking runtime to
        // clean up the scratch database so tests don't accumulate them.
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("failed to build cleanup runtime");
            rt.block_on(async {
                if let Ok(admin_pool) =
                    PgPoolOptions::new().max_connections(1).connect(MAINTENANCE_DB_URL).await
                {
                    // Terminate any lingering connections first so DROP DATABASE
                    // does not fail with "database is being accessed by other users".
                    let _ = sqlx::query(
                        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
                         WHERE datname = $1 AND pid <> pg_backend_pid()",
                    )
                    .bind(&name)
                    .execute(&admin_pool)
                    .await;
                    let _ = sqlx::query(&format!("DROP DATABASE IF EXISTS \"{name}\""))
                        .execute(&admin_pool)
                        .await;
                }
            });
        })
        .join()
        .ok();
    }
}

#[tokio::test]
async fn applying_migrations_brings_schema_up_to_date() {
    let scratch = ScratchDb::new("apply").await;
    let pool = establish_pool(&scratch.db_config()).await.expect("pool should establish");

    run_migrations(&pool).await.expect("migrations should apply cleanly to a fresh database");

    // Requirement 4.2/4.1: applying the embedded migrations must leave a
    // matching row in sqlx's own `_sqlx_migrations` history table, i.e.
    // the schema has genuinely been brought up to date, not merely
    // "returned Ok" without doing anything.
    let rows = sqlx::query("SELECT version FROM _sqlx_migrations ORDER BY version")
        .fetch_all(&pool)
        .await
        .expect("_sqlx_migrations table should exist after migrating");
    assert!(
        !rows.is_empty(),
        "expected at least the 0001_init_runtime migration to be recorded as applied"
    );
}

#[tokio::test]
async fn no_unapplied_migrations_is_a_no_op_that_continues_startup() {
    let scratch = ScratchDb::new("noop").await;
    let pool = establish_pool(&scratch.db_config()).await.expect("pool should establish");

    run_migrations(&pool).await.expect("first apply should succeed");
    let first_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await
        .expect("should be able to count applied migrations");

    // Requirement 4.3: running again with nothing left unapplied must
    // succeed as a no-op rather than erroring or re-applying anything.
    run_migrations(&pool).await.expect("second, no-op apply should also succeed");
    let second_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await
        .expect("should be able to count applied migrations again");

    assert_eq!(
        first_count, second_count,
        "re-running migrate on an up-to-date database must not add or remove history rows"
    );
}

#[tokio::test]
async fn restart_preserves_existing_data_and_does_not_reapply() {
    let scratch = ScratchDb::new("restart").await;
    let pool = establish_pool(&scratch.db_config()).await.expect("pool should establish");

    run_migrations(&pool).await.expect("initial apply should succeed");

    // Simulate application data written after the schema was migrated.
    sqlx::query("CREATE TABLE restart_probe (id INT PRIMARY KEY)")
        .execute(&pool)
        .await
        .expect("should be able to create a probe table");
    sqlx::query("INSERT INTO restart_probe (id) VALUES (42)")
        .execute(&pool)
        .await
        .expect("should be able to insert a probe row");

    let history_before: Vec<(i64, Vec<u8>)> =
        sqlx::query("SELECT version, checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&pool)
            .await
            .expect("should read migration history")
            .into_iter()
            .map(|row| (row.get("version"), row.get("checksum")))
            .collect();

    // Requirement 4.4: simulate a process restart against the same,
    // already-migrated database.
    run_migrations(&pool).await.expect("re-running migrate on restart should succeed");

    let probe_value: i32 = sqlx::query_scalar("SELECT id FROM restart_probe")
        .fetch_one(&pool)
        .await
        .expect("probe row must still exist after restart");
    assert_eq!(probe_value, 42, "existing data must be preserved across a restart re-apply");

    let history_after: Vec<(i64, Vec<u8>)> =
        sqlx::query("SELECT version, checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&pool)
            .await
            .expect("should read migration history again")
            .into_iter()
            .map(|row| (row.get("version"), row.get("checksum")))
            .collect();
    assert_eq!(
        history_before, history_after,
        "restart must not reapply migrations or alter migration history"
    );
}

#[tokio::test]
async fn checksum_mismatch_against_history_aborts_with_diagnostic() {
    let scratch = ScratchDb::new("checksum").await;
    let pool = establish_pool(&scratch.db_config()).await.expect("pool should establish");

    run_migrations(&pool).await.expect("initial apply should succeed");

    // Requirement 4.6: corrupt the recorded checksum for the already
    // applied migration so it no longer matches the embedded definition,
    // then verify the migrator detects the mismatch instead of silently
    // proceeding.
    sqlx::query("UPDATE _sqlx_migrations SET checksum = '\\x00' WHERE version = 1")
        .execute(&pool)
        .await
        .expect("should be able to corrupt the stored checksum for this test");

    let err = run_migrations(&pool)
        .await
        .expect_err("a checksum mismatch against history must abort, not succeed");

    let message = err.to_string();
    assert!(
        message.contains('1'),
        "diagnostic should identify the mismatched migration version (1), got: {message}"
    );
    assert!(
        std::error::Error::source(&err).is_some(),
        "MigrationApplyError should preserve the underlying sqlx cause for diagnosis"
    );
}

#[tokio::test]
async fn apply_failure_aborts_startup_with_diagnostic_identifying_migration() {
    let scratch = ScratchDb::new("failure").await;
    let pool = establish_pool(&scratch.db_config()).await.expect("pool should establish");

    // Requirement 4.5: this exercises a *separate*, test-only migration
    // set (tests/migrate_failure_migrations/) whose single migration is
    // deliberately broken SQL. It is never embedded into the production
    // binary -- only `src/migrate.rs`'s `sqlx::migrate!("./migrations")`
    // is, and that points at the real `migrations/` directory.
    let migrator = sqlx::migrate!("tests/migrate_failure_migrations");
    let err = migrator.run(&pool).await.expect_err("the deliberately broken migration must fail to apply");

    let message = err.to_string();
    assert!(
        message.contains('1'),
        "diagnostic should identify the failed migration version (1), got: {message}"
    );

    // The HTTP-listener-not-started half of Requirement 4.5 is a
    // Bootstrap composition-root concern (task 7.4): this module's
    // contract is only to surface a fatal, migration-identifying error
    // for the caller to act on, which is what we just observed.
}
