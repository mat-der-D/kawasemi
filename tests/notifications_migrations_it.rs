//! Integration test for notifications task 1.1 ("通知スキーマのマイグレー
//! ション（0009）", Requirement 8: 通知の重複排除, acceptance criteria 8.1,
//! 8.2; design.md's "Physical Data Model"): after applying the embedded
//! migration set, the `notifications` table exists with the recipient
//! cursor index and the partial unique dedup index, and the dedup index's
//! "未消去限定" (`WHERE NOT dismissed`) semantics are actually enforced by
//! the database, not merely declared.
//!
//! ## Why this lives here, not in `src/migrate/tests.rs`
//! Mirrors `tests/federation_migrations_it.rs`'s/`tests/actor_migrations_it.rs`'s
//! own rationale: `src/migrate/tests.rs` is core-runtime's own private
//! unit-test module for its generic `Migrate` boundary. notifications may
//! only *consume* that infrastructure through its public API
//! (`kawasemi::migrate::apply_migrations`, `kawasemi::test_harness`), never
//! add tests inside core-runtime's private module. This file instead drives
//! the exact same production `apply_migrations` code path through
//! `spawn_test_app` (the established public integration-test harness this
//! repo's other `tests/*_it.rs` files already use), against its own
//! isolated schema, and proves the *content* this spec's migration owns
//! (the `notifications` table and its indexes/constraints) rather than the
//! generic migration-application machinery core-runtime already covers.
//!
//! Each constraint is proven behaviorally (attempting the actual violating
//! insert and asserting on the real Postgres SQLSTATE it returns, per
//! `tests/federation_migrations_it.rs`'s own convention), not by inspecting
//! catalog/information_schema metadata alone, so a partial unique index
//! that exists but is silently non-functional (wrong columns, wrong
//! `WHERE` clause, wrong `COALESCE` expression) would still be caught. The
//! one plain (non-unique) index, `notifications_recipient_idx`, has no
//! violation to trigger, so its existence is confirmed via `to_regclass`
//! catalog lookup instead (indexes, like tables, are relations
//! `to_regclass` can resolve).

use kawasemi::test_harness::spawn_test_app;
use sqlx::Row;

const TEST_DB_HOST: &str = "127.0.0.1";
const TEST_DB_PORT: u16 = 5432;
const TEST_DB_URL_ENV: &str = "KAWASEMI_TEST_DATABASE_URL";

/// Best-effort raw-TCP reachability probe, independent of sqlx/the harness
/// itself. Mirrors `tests/federation_migrations_it.rs`'s own convention:
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

/// The dedup-key components and `dismissed` flag for one seeded
/// notification row, bundled into a single value so
/// [`insert_notification`] stays within clippy's `too_many_arguments`
/// limit while still spelling out every column the test cares about at
/// each call site.
struct NotificationRow<'a> {
    id: i64,
    recipient_id: i64,
    kind: &'a str,
    origin_kind: &'a str,
    origin_id: i64,
    status_id: Option<i64>,
    dismissed: bool,
}

/// Inserts one notification row with the given dedup-key components and
/// `dismissed` flag, using a distinct `id` per call.
async fn insert_notification(
    pool: &sqlx::PgPool,
    row: NotificationRow<'_>,
) -> Result<sqlx::postgres::PgQueryResult, sqlx::Error> {
    sqlx::query(
        "INSERT INTO notifications \
         (id, recipient_id, kind, origin_id, origin_kind, status_id, dismissed, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, now())",
    )
    .bind(row.id)
    .bind(row.recipient_id)
    .bind(row.kind)
    .bind(row.origin_id)
    .bind(row.origin_kind)
    .bind(row.status_id)
    .bind(row.dismissed)
    .execute(pool)
    .await
}

/// Requirement 8 (通知の重複排除, acceptance criteria 8.1, 8.2), design.md's
/// "Physical Data Model": a `spawn_test_app` instance's isolated database --
/// which already has the embedded migrations (including
/// `migrations/0009_notifications.sql`) applied via the real
/// `apply_migrations` production code path -- has the `notifications` table,
/// and each of the indexes/constraints task 1.1 specifies is actually
/// present/enforced:
/// - `notifications_recipient_idx` exists on `(recipient_id, id DESC)` (the
///   受信者カーソルインデックス for list pagination).
/// - `notifications_dedup_idx` is a **partial** unique index on
///   `(recipient_id, kind, origin_kind, origin_id, COALESCE(status_id, 0))`
///   scoped `WHERE NOT dismissed` that:
///   1. rejects a second not-yet-dismissed row with the identical dedup key
///      (8.1: 重複した通知を新規生成しない / 8.2: 冪等), including when
///      `status_id` is `NULL` on both rows (`follow`-shaped notifications);
///   2. still allows a differing dedup key (different `status_id`) to
///      coexist for the same recipient/kind/origin;
///   3. allows a fresh insert with the identical dedup key once the
///      existing row has been marked `dismissed = TRUE` (取り消し -> 再実行
///      の再通知を許容する, matching design.md's "Consistency" note).
#[tokio::test]
async fn migrated_test_app_has_notifications_table_with_dedup_constraint() {
    if !should_run_against_real_database(
        "migrated_test_app_has_notifications_table_with_dedup_constraint",
    ) {
        return;
    }

    let app = spawn_test_app().await;
    let pool = app.pool.clone();

    // The table exists (unqualified name resolves via this pool's pinned
    // search_path, so a hit here can only be *this* TestApp's isolated
    // schema's table).
    let table_exists: Option<String> = sqlx::query("SELECT to_regclass($1)::text AS r")
        .bind("notifications")
        .fetch_one(&pool)
        .await
        .expect("querying to_regclass must succeed")
        .get("r");
    assert!(
        table_exists.is_some(),
        "table `notifications` must exist after applying migration 0009_notifications.sql"
    );

    // The recipient cursor index (a plain, non-unique index with nothing to
    // violate) is confirmed to exist via catalog lookup.
    let recipient_idx_exists: Option<String> = sqlx::query("SELECT to_regclass($1)::text AS r")
        .bind("notifications_recipient_idx")
        .fetch_one(&pool)
        .await
        .expect("querying to_regclass for notifications_recipient_idx must succeed")
        .get("r");
    assert!(
        recipient_idx_exists.is_some(),
        "index `notifications_recipient_idx` on notifications(recipient_id, id DESC) must exist"
    );

    // Seed one not-yet-dismissed `follow`-shaped notification (no
    // status_id): recipient 100, kind 'follow', origin ('local', 7).
    insert_notification(
        &pool,
        NotificationRow {
            id: 1,
            recipient_id: 100,
            kind: "follow",
            origin_kind: "local",
            origin_id: 7,
            status_id: None,
            dismissed: false,
        },
    )
    .await
    .expect("inserting a seed follow notification must succeed");

    // A second not-yet-dismissed notification with the identical dedup key
    // (including both status_id being NULL) must be rejected by
    // notifications_dedup_idx (8.1, 8.2).
    let dup_err = insert_notification(
        &pool,
        NotificationRow {
            id: 2,
            recipient_id: 100,
            kind: "follow",
            origin_kind: "local",
            origin_id: 7,
            status_id: None,
            dismissed: false,
        },
    )
    .await
    .expect_err(
        "inserting a second not-yet-dismissed notification with the identical dedup key \
         must fail",
    );
    assert_eq!(
        sqlstate(&dup_err, "duplicate not-yet-dismissed dedup key insert"),
        "23505",
        "notifications_dedup_idx must reject a duplicate (recipient, kind, origin, status) key \
         among not-yet-dismissed rows"
    );

    // A notification with the same recipient/kind/origin but a different
    // status_id is a different dedup key and must succeed (e.g. a
    // `favourite` notification for a different post).
    insert_notification(
        &pool,
        NotificationRow {
            id: 3,
            recipient_id: 100,
            kind: "favourite",
            origin_kind: "local",
            origin_id: 7,
            status_id: Some(42),
            dismissed: false,
        },
    )
    .await
    .expect(
        "inserting a notification with a different dedup key (different status_id) must \
         succeed",
    );
    let second_status_dup_err = insert_notification(
        &pool,
        NotificationRow {
            id: 4,
            recipient_id: 100,
            kind: "favourite",
            origin_kind: "local",
            origin_id: 7,
            status_id: Some(42),
            dismissed: false,
        },
    )
    .await
    .expect_err(
        "inserting a second not-yet-dismissed notification with the identical \
         status_id-bearing dedup key must fail",
    );
    assert_eq!(
        sqlstate(
            &second_status_dup_err,
            "duplicate not-yet-dismissed status_id dedup key insert"
        ),
        "23505",
        "notifications_dedup_idx must also enforce dedup for status_id-bearing kinds"
    );

    // Dismiss the original follow notification (id 1), then insert a fresh
    // notification with the identical dedup key: the partial index only
    // covers `WHERE NOT dismissed`, so once the existing row is dismissed
    // the key becomes free again (unfollow -> re-follow re-notification
    // semantics).
    let dismissed_rows = sqlx::query("UPDATE notifications SET dismissed = TRUE WHERE id = 1")
        .execute(&pool)
        .await
        .expect("dismissing the seed follow notification must succeed");
    assert_eq!(
        dismissed_rows.rows_affected(),
        1,
        "exactly the seeded follow notification (id 1) must be updated"
    );

    insert_notification(
        &pool,
        NotificationRow {
            id: 5,
            recipient_id: 100,
            kind: "follow",
            origin_kind: "local",
            origin_id: 7,
            status_id: None,
            dismissed: false,
        },
    )
    .await
    .expect(
        "inserting a fresh notification with the same dedup key must succeed once the prior \
         row with that key has been dismissed",
    );

    app.cleanup().await;
}
