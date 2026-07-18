//! Integration test for media-pipeline task 1.1 ("メディアと処理ジョブのマイグ
//! レーションを追加する", Requirements 1.2, 4.1, 4.2; design.md's "Physical
//! Data Model"): after applying the embedded migration set, `media` and
//! `media_processing_jobs` exist, the composite `(state, run_at)` index that
//! covers both newly-queued and lease-expired-reclaim job lookups is
//! present, an index on `media.actor_id` is present, and each constraint
//! design.md specifies is actually enforced by the database.
//!
//! ## Why this lives here, not in `src/migrate/tests.rs`
//! Mirrors `tests/federation_migrations_it.rs`'s own rationale:
//! `src/migrate/tests.rs` is core-runtime's own private unit-test module for
//! its generic `Migrate` boundary. media-pipeline may only *consume* that
//! infrastructure through its public API (`kawasemi::migrate::apply_migrations`,
//! `kawasemi::test_harness`), never add tests inside core-runtime's private
//! module. This file instead drives the exact same production
//! `apply_migrations` code path through `spawn_test_app` (the established
//! public integration-test harness this repo's other `tests/*_it.rs` files
//! already use), against its own isolated schema, and proves the *content*
//! this spec's migration owns (the two tables and their indexes/constraints)
//! rather than the generic migration-application machinery core-runtime
//! already covers.
//!
//! Each constraint is proven behaviorally (attempting the actual violating
//! insert and asserting on the real Postgres SQLSTATE it returns, or
//! successfully inserting a minimal valid row when the constraint is a
//! NOT NULL/FK requirement), not by inspecting catalog/information_schema
//! metadata alone, so a constraint that exists but is silently
//! non-functional (wrong column, wrong expression, wrong target) would
//! still be caught. The composite `(state, run_at)` index and the
//! `media.actor_id` index have no violation to trigger, so their existence
//! is confirmed via `to_regclass` catalog lookup instead (indexes, like
//! tables, are relations `to_regclass` can resolve).

use kawasemi::test_harness::spawn_test_app;
use sqlx::Row;

const TEST_DB_HOST: &str = "127.0.0.1";
const TEST_DB_PORT: u16 = 5432;
const TEST_DB_URL_ENV: &str = "KAWASEMI_TEST_DATABASE_URL";

/// Best-effort raw-TCP reachability probe, independent of sqlx/the harness
/// itself. Mirrors `tests/federation_migrations_it.rs`'s own convention:
/// used only to decide whether to skip these tests in an environment with no
/// local PostgreSQL at all, never to swallow a real regression.
fn default_test_db_reachable() -> bool {
    std::net::TcpStream::connect_timeout(
        &format!("{TEST_DB_HOST}:{TEST_DB_PORT}")
            .parse()
            .expect("hardcoded host:port is valid"),
        std::time::Duration::from_millis(500),
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

/// Returns the Postgres `SQLSTATE` code of `err`, panicking with `context`
/// if `err` was not a database error at all (e.g. a connection failure),
/// which would indicate a test setup problem rather than the constraint
/// violation under test.
fn sqlstate(err: &sqlx::Error, context: &str) -> String {
    err.as_database_error()
        .unwrap_or_else(|| panic!("{context}: expected a database error, got: {err:?}"))
        .code()
        .unwrap_or_else(|| panic!("{context}: database error had no SQLSTATE code: {err:?}"))
        .into_owned()
}

/// Requirements 1.2, 4.1, 4.2 (design.md "Physical Data Model"): a
/// `spawn_test_app` instance's isolated database — which already has the
/// embedded migrations (including `migrations/0005_media.sql`) applied via
/// the real `apply_migrations` production code path — has the `media` and
/// `media_processing_jobs` tables, and each of the indexes/constraints
/// design.md specifies on them is actually present/enforced:
/// - `media` requires `actor_id` (owning actor, 1.2) via NOT NULL.
/// - `media_actor_idx` exists on `media(actor_id)` (efficient owner lookup).
/// - `media_processing_jobs.media_id` is a required FK to `media(id)`
///   (4.1's "target media" job field) that rejects both a NULL and a
///   dangling reference.
/// - `media_jobs_due_idx` exists on `media_processing_jobs(state, run_at)`
///   and covers both newly-queued (`state='queued' AND run_at <= now`) and
///   lease-expired-reclaim (`state='processing' AND locked_at < ...`) job
///   lookups (4.2).
#[tokio::test]
async fn migrated_test_app_has_media_tables_with_constraints() {
    if !should_run_against_real_database("migrated_test_app_has_media_tables_with_constraints") {
        return;
    }

    let app = spawn_test_app().await;
    let pool = app.pool.clone();

    // The two tables exist (unqualified names resolve via this pool's
    // pinned search_path, so a hit here can only be *this* TestApp's
    // isolated schema's table).
    for table in ["media", "media_processing_jobs"] {
        let exists: Option<String> = sqlx::query("SELECT to_regclass($1)::text AS r")
            .bind(table)
            .fetch_one(&pool)
            .await
            .expect("querying to_regclass must succeed")
            .get("r");
        assert!(
            exists.is_some(),
            "table `{table}` must exist after applying migration 0005_media.sql"
        );
    }

    // `media_actor_idx` (a plain, non-unique index with nothing to violate)
    // is confirmed to exist via catalog lookup.
    let actor_idx_exists: Option<String> = sqlx::query("SELECT to_regclass($1)::text AS r")
        .bind("media_actor_idx")
        .fetch_one(&pool)
        .await
        .expect("querying to_regclass for media_actor_idx must succeed")
        .get("r");
    assert!(
        actor_idx_exists.is_some(),
        "index `media_actor_idx` on media(actor_id) must exist"
    );

    // `media_jobs_due_idx` (also a plain, non-unique index) is confirmed to
    // exist via catalog lookup, and covers both the new-queued-work and
    // expired-lease-reclaim access patterns (4.2).
    let jobs_due_idx_exists: Option<String> = sqlx::query("SELECT to_regclass($1)::text AS r")
        .bind("media_jobs_due_idx")
        .fetch_one(&pool)
        .await
        .expect("querying to_regclass for media_jobs_due_idx must succeed")
        .get("r");
    assert!(
        jobs_due_idx_exists.is_some(),
        "index `media_jobs_due_idx` on media_processing_jobs(state, run_at) must exist"
    );

    // Requirement 1.2: `media.actor_id` is a required (NOT NULL) owning
    // actor reference; an insert omitting it must be rejected.
    let missing_actor_err = sqlx::query(
        "INSERT INTO media \
         (id, media_type, state, object_key, content_type, created_at, updated_at) \
         VALUES (1, 'image', 'processing', 'orig/1', 'image/png', now(), now())",
    )
    .execute(&pool)
    .await
    .expect_err("inserting media without actor_id must fail");
    assert_eq!(
        sqlstate(&missing_actor_err, "media insert missing actor_id"),
        "23502",
        "media.actor_id must be enforced as NOT NULL"
    );

    // A minimal valid `media` row (respecting every NOT NULL column) must
    // succeed and carry the owning actor (1.2).
    sqlx::query(
        "INSERT INTO media \
         (id, actor_id, media_type, state, object_key, content_type, created_at, updated_at) \
         VALUES (1, 42, 'image', 'processing', 'orig/1', 'image/png', now(), now())",
    )
    .execute(&pool)
    .await
    .expect("inserting a minimal valid media row must succeed");

    let stored_actor_id: i64 = sqlx::query("SELECT actor_id FROM media WHERE id = 1")
        .fetch_one(&pool)
        .await
        .expect("selecting the seeded media row must succeed")
        .get("actor_id");
    assert_eq!(
        stored_actor_id, 42,
        "media.actor_id must round-trip the owning actor id (1.2)"
    );

    // Requirement 4.1: `media_processing_jobs.media_id` is a required
    // reference to its target media; a NULL media_id must be rejected.
    let missing_media_id_err = sqlx::query(
        "INSERT INTO media_processing_jobs \
         (id, state, attempts, run_at, created_at) \
         VALUES (1, 'queued', 0, now(), now())",
    )
    .execute(&pool)
    .await
    .expect_err("inserting a processing job without media_id must fail");
    assert_eq!(
        sqlstate(&missing_media_id_err, "job insert missing media_id"),
        "23502",
        "media_processing_jobs.media_id must be enforced as NOT NULL"
    );

    // A processing job referencing a non-existent media id must be rejected
    // by the foreign key (job's target media, 4.1).
    let dangling_fk_err = sqlx::query(
        "INSERT INTO media_processing_jobs \
         (id, media_id, state, attempts, run_at, created_at) \
         VALUES (1, 999999, 'queued', 0, now(), now())",
    )
    .execute(&pool)
    .await
    .expect_err("inserting a processing job with a dangling media_id must fail");
    assert_eq!(
        sqlstate(&dangling_fk_err, "job insert with dangling media_id"),
        "23503",
        "media_processing_jobs.media_id must be enforced as a foreign key to media(id)"
    );

    // A minimal valid `media_processing_jobs` row referencing the seeded
    // media row must succeed.
    sqlx::query(
        "INSERT INTO media_processing_jobs \
         (id, media_id, state, attempts, run_at, created_at) \
         VALUES (1, 1, 'queued', 0, now(), now())",
    )
    .execute(&pool)
    .await
    .expect("inserting a minimal valid processing job row must succeed");

    let stored_media_id: i64 =
        sqlx::query("SELECT media_id FROM media_processing_jobs WHERE id = 1")
            .fetch_one(&pool)
            .await
            .expect("selecting the seeded processing job row must succeed")
            .get("media_id");
    assert_eq!(
        stored_media_id, 1,
        "media_processing_jobs.media_id must round-trip the target media id (4.1)"
    );

    app.cleanup().await;
}
