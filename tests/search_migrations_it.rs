//! Integration test for search task 1.1 ("検索用テーブルのマイグレーションを
//! 追加する", Requirement 8: 拡張・日本語対応の後付けマイグレーション経路,
//! acceptance criteria 8.1, 8.2, 8.3; design.md's "Physical Data Model"):
//! after applying the embedded migration set, all three tables this spec
//! owns — `search_tags`, `search_status_tags`, `search_index_watermark` —
//! exist with the indexes/constraints task 1.1 specifies, none of them
//! require any PostgreSQL extension (`pg_bigm` or otherwise) to be
//! installed, and `spawn_test_app`'s bootstrap (which runs the real
//! `apply_migrations` production code path) succeeds in this environment
//! regardless of whether such an extension happens to be installed.
//!
//! ## Migration filename deviation from tasks.md/design.md (0010 -> 0013)
//! `tasks.md`/`design.md` both literally say `migrations/0010_search.sql`,
//! but `0010` was never actually free: `migrations/0012_social_graph.sql`'s
//! own Implementation Notes record that social-graph originally expected
//! `0006` and, upon finding it and several neighbors already taken,
//! reserved-but-did-not-materialize `0008`-`0010` as "other spec"
//! placeholders and landed on `0012`. By the time this task run began,
//! `migrations/` already contained 0001-0007, 0009, 0011, and 0012 with no
//! file at 0008 or 0010 (both are documented-skipped reserved numbers, not
//! available slots — reusing either after 0011/0012 are already applied
//! would break sqlx's strictly-ascending, never-reused embedded migrator).
//! This migration therefore claims the next actually-free slot instead:
//! `migrations/0013_search.sql`. See that file's own header comment for the
//! same note from the other direction.
//!
//! ## Why this lives here, not in `src/migrate/tests.rs`
//! Mirrors `tests/notifications_migrations_it.rs`'s/`tests/federation_migrations_it.rs`'s
//! own rationale: `src/migrate/tests.rs` is core-runtime's own private
//! unit-test module for its generic `Migrate` boundary. search may only
//! *consume* that infrastructure through its public API
//! (`kawasemi::migrate::apply_migrations`, `kawasemi::test_harness`), never
//! add tests inside core-runtime's private module. This file instead drives
//! the exact same production `apply_migrations` code path through
//! `spawn_test_app` (the established public integration-test harness this
//! repo's other `tests/*_it.rs` files already use), against its own
//! isolated schema, and proves the *content* this spec's migration owns
//! (the three tables and their indexes/constraints) rather than the generic
//! migration-application machinery core-runtime already covers.
//!
//! Each constraint is proven behaviorally (attempting the actual violating
//! insert/delete and asserting on the real Postgres SQLSTATE it returns,
//! per `tests/notifications_migrations_it.rs`'s own convention), not by
//! inspecting catalog/information_schema metadata alone, so a constraint
//! that exists in name but is silently non-functional would still be
//! caught. The two plain (non-unique) indexes
//! (`search_tags_name_idx`/`search_status_tags_status_idx`) have nothing to
//! violate, so their existence — and, for the prefix-match index, that it
//! was actually declared with `text_pattern_ops` rather than the default
//! opclass — is confirmed via catalog lookup instead.

use kawasemi::test_harness::spawn_test_app;
use sqlx::Row;

const TEST_DB_HOST: &str = "127.0.0.1";
const TEST_DB_PORT: u16 = 5432;
const TEST_DB_URL_ENV: &str = "KAWASEMI_TEST_DATABASE_URL";

/// Best-effort raw-TCP reachability probe, independent of sqlx/the harness
/// itself. Mirrors `tests/notifications_migrations_it.rs`'s own convention:
/// used only to decide whether to skip these tests in an environment with
/// no local PostgreSQL at all, never to swallow a real regression.
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

/// Confirms a relation (table or index) with `name` exists in the current
/// session's (schema-pinned) search_path.
async fn assert_relation_exists(pool: &sqlx::PgPool, name: &str, what: &str) {
    let exists: Option<String> = sqlx::query("SELECT to_regclass($1)::text AS r")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap_or_else(|e| panic!("querying to_regclass for {name} must succeed: {e:?}"))
        .get("r");
    assert!(exists.is_some(), "{what} `{name}` must exist");
}

/// Requirement 8 (拡張・日本語対応の後付けマイグレーション経路, acceptance
/// criteria 8.1, 8.2), design.md's "Physical Data Model": a
/// `spawn_test_app` instance's isolated database — which already has the
/// embedded migrations (including `migrations/0013_search.sql`) applied via
/// the real `apply_migrations` production code path — has all three tables
/// task 1.1 specifies, and their indexes exist:
/// - `search_tags` (name UNIQUE, usage aggregation) with
///   `search_tags_name_idx` declared using `text_pattern_ops` (prefix-match,
///   extension-free).
/// - `search_status_tags` (tag_id -> status_id) with
///   `search_status_tags_status_idx` on `status_id`, and a
///   `tag_id`-referencing foreign key that cascades on delete.
/// - `search_index_watermark`, a singleton table.
#[tokio::test]
async fn migrated_test_app_has_search_tables_and_indexes() {
    if !should_run_against_real_database("migrated_test_app_has_search_tables_and_indexes") {
        return;
    }

    let app = spawn_test_app().await;
    let pool = app.pool.clone();

    // All three tables exist (unqualified names resolve via this pool's
    // pinned search_path, so a hit here can only be *this* TestApp's
    // isolated schema's table).
    assert_relation_exists(&pool, "search_tags", "table").await;
    assert_relation_exists(&pool, "search_status_tags", "table").await;
    assert_relation_exists(&pool, "search_index_watermark", "table").await;

    // The prefix-match index on search_tags(name) exists...
    assert_relation_exists(&pool, "search_tags_name_idx", "index").await;
    // ...and was actually declared with text_pattern_ops (standard
    // PostgreSQL opclass, no extension), not merely a same-named default
    // btree index that would silently fail to accelerate LIKE 'prefix%'
    // matching under a non-C locale.
    let name_idx_def: String = sqlx::query("SELECT pg_get_indexdef($1::regclass) AS def")
        .bind("search_tags_name_idx")
        .fetch_one(&pool)
        .await
        .expect("pg_get_indexdef for search_tags_name_idx must succeed")
        .get("def");
    assert!(
        name_idx_def.contains("text_pattern_ops"),
        "search_tags_name_idx must be declared with text_pattern_ops for prefix matching, got: \
         {name_idx_def}"
    );

    // The status_id index on search_status_tags exists (plain index, no
    // violation to trigger, confirmed via catalog lookup as per
    // notifications' own convention for its recipient cursor index).
    assert_relation_exists(&pool, "search_status_tags_status_idx", "index").await;

    // search_tags.name UNIQUE is genuinely enforced: insert one tag, then
    // attempt a second with the identical name.
    sqlx::query(
        "INSERT INTO search_tags (id, name, last_status_at, statuses_count, updated_at) \
         VALUES (1, 'rustlang', NULL, 0, now())",
    )
    .execute(&pool)
    .await
    .expect("inserting the first search_tags row must succeed");

    let dup_name_err = sqlx::query(
        "INSERT INTO search_tags (id, name, last_status_at, statuses_count, updated_at) \
         VALUES (2, 'rustlang', NULL, 0, now())",
    )
    .execute(&pool)
    .await
    .expect_err("inserting a second search_tags row with a duplicate name must fail");
    assert_eq!(
        sqlstate(&dup_name_err, "duplicate search_tags.name insert"),
        "23505",
        "search_tags.name must be UNIQUE"
    );

    // search_status_tags: (tag_id, status_id) primary key uniqueness is
    // genuinely enforced, and the tag_id foreign key genuinely cascades on
    // delete.
    sqlx::query(
        "INSERT INTO search_status_tags (tag_id, status_id, created_at) VALUES (1, 100, now())",
    )
    .execute(&pool)
    .await
    .expect("inserting the first search_status_tags row must succeed");

    let dup_pair_err = sqlx::query(
        "INSERT INTO search_status_tags (tag_id, status_id, created_at) VALUES (1, 100, now())",
    )
    .execute(&pool)
    .await
    .expect_err("inserting a duplicate (tag_id, status_id) pair must fail");
    assert_eq!(
        sqlstate(&dup_pair_err, "duplicate (tag_id, status_id) insert"),
        "23505",
        "search_status_tags must have a (tag_id, status_id) primary key"
    );

    let cascade_count_before: i64 = sqlx::query("SELECT count(*) AS c FROM search_status_tags")
        .fetch_one(&pool)
        .await
        .expect("counting search_status_tags rows must succeed")
        .get("c");
    assert_eq!(
        cascade_count_before, 1,
        "exactly the one seeded search_status_tags row must exist before the cascade delete"
    );

    sqlx::query("DELETE FROM search_tags WHERE id = 1")
        .execute(&pool)
        .await
        .expect("deleting the referenced search_tags row must succeed");

    let cascade_count_after: i64 = sqlx::query("SELECT count(*) AS c FROM search_status_tags")
        .fetch_one(&pool)
        .await
        .expect("counting search_status_tags rows after cascade delete must succeed")
        .get("c");
    assert_eq!(
        cascade_count_after, 0,
        "search_status_tags rows referencing a deleted search_tags row must be cascade-deleted \
         (ON DELETE CASCADE)"
    );

    app.cleanup().await;
}

/// Requirement 8 (拡張・日本語対応の後付けマイグレーション経路, acceptance
/// criteria 8.1, 8.3), design.md's "Physical Data Model": `search_index_watermark`
/// is a genuine singleton — the `id BOOLEAN PRIMARY KEY DEFAULT TRUE` column
/// plus the `search_index_watermark_singleton CHECK (id)` constraint
/// together enforce that at most one row (with `id = TRUE`) can ever exist.
/// Without the CHECK, `id BOOLEAN PRIMARY KEY` alone would still allow a
/// *second*, distinct row with `id = FALSE` (a boolean primary key has only
/// two possible values, and PostgreSQL primary keys permit both to exist as
/// long as they differ from each other), defeating the singleton contract —
/// so this test proves the CHECK is what actually forecloses that second
/// row, not merely that the table has a primary key.
#[tokio::test]
async fn search_index_watermark_enforces_single_row() {
    if !should_run_against_real_database("search_index_watermark_enforces_single_row") {
        return;
    }

    let app = spawn_test_app().await;
    let pool = app.pool.clone();

    assert_relation_exists(&pool, "search_index_watermark", "table").await;

    // The first row (default id = TRUE) inserts cleanly.
    sqlx::query(
        "INSERT INTO search_index_watermark (status_created_at, status_id, updated_at) \
         VALUES (NULL, NULL, now())",
    )
    .execute(&pool)
    .await
    .expect("inserting the first (and only permitted) search_index_watermark row must succeed");

    // A second row with the same id = TRUE collides on the primary key.
    let pk_dup_err = sqlx::query(
        "INSERT INTO search_index_watermark (id, status_created_at, status_id, updated_at) \
         VALUES (TRUE, NULL, NULL, now())",
    )
    .execute(&pool)
    .await
    .expect_err("inserting a second row with id = TRUE must fail on the primary key");
    assert_eq!(
        sqlstate(
            &pk_dup_err,
            "duplicate search_index_watermark id = TRUE insert"
        ),
        "23505",
        "search_index_watermark must have id as its primary key"
    );

    // A second row with id = FALSE is a *different* primary key value, so
    // it would NOT collide with the existing row on the primary key alone
    // -- this is exactly the gap the singleton CHECK constraint must close.
    let check_violation_err = sqlx::query(
        "INSERT INTO search_index_watermark (id, status_created_at, status_id, updated_at) \
         VALUES (FALSE, NULL, NULL, now())",
    )
    .execute(&pool)
    .await
    .expect_err(
        "inserting a second, distinct row with id = FALSE must be rejected by the singleton \
         CHECK constraint",
    );
    assert_eq!(
        sqlstate(
            &check_violation_err,
            "search_index_watermark id = FALSE insert"
        ),
        "23514",
        "search_index_watermark_singleton CHECK (id) must reject any row where id is not TRUE"
    );

    // Confirm exactly one row survives.
    let row_count: i64 = sqlx::query("SELECT count(*) AS c FROM search_index_watermark")
        .fetch_one(&pool)
        .await
        .expect("counting search_index_watermark rows must succeed")
        .get("c");
    assert_eq!(
        row_count, 1,
        "search_index_watermark must contain exactly one row after both rejected inserts"
    );

    app.cleanup().await;
}

/// Requirement 8.1 (初期配布で拡張・外部エンジンを必須とせず、標準
/// PostgreSQL のみで検索エンドポイントが機能する状態を提供する): the
/// `pg_bigm` extension referenced by design.md's documented future
/// extension-based path is NOT installed in this migrated database (proving
/// the default migration genuinely does not `CREATE EXTENSION` it), and
/// `spawn_test_app`'s bootstrap -- which runs the real `apply_migrations`
/// production code path against this same PostgreSQL server -- already
/// succeeded above/below without it, demonstrating that startup does not
/// depend on the extension being installed.
#[tokio::test]
async fn search_migration_does_not_require_pg_bigm_extension() {
    if !should_run_against_real_database("search_migration_does_not_require_pg_bigm_extension") {
        return;
    }

    // `spawn_test_app` succeeding at all (no panic/expect failure) is
    // itself the "startup succeeds without the extension installed"
    // integration check: it drives migrations/0013_search.sql through the
    // real production apply_migrations code path against this
    // environment's PostgreSQL server, which -- per this crate's CI/dev
    // environment -- does not have pg_bigm installed.
    let app = spawn_test_app().await;
    let pool = app.pool.clone();

    let pg_bigm_installed: Option<String> =
        sqlx::query("SELECT extname FROM pg_catalog.pg_extension WHERE extname = 'pg_bigm'")
            .fetch_optional(&pool)
            .await
            .expect("querying pg_catalog.pg_extension must succeed")
            .map(|row| row.get::<String, _>("extname"));
    assert!(
        pg_bigm_installed.is_none(),
        "pg_bigm must not be installed by the default migration set (8.1); found: \
         {pg_bigm_installed:?}"
    );

    // The three search tables are nonetheless fully present and usable
    // (sanity check that the extension-free migration did in fact apply,
    // not just that startup happened to succeed for unrelated reasons).
    assert_relation_exists(&pool, "search_tags", "table").await;
    assert_relation_exists(&pool, "search_status_tags", "table").await;
    assert_relation_exists(&pool, "search_index_watermark", "table").await;

    app.cleanup().await;
}
