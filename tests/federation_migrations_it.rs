//! Integration test for federation-core task 1.1 ("連合用テーブルのマイグレー
//! ションを追加する", Requirements 7.4, 11.1, 11.4; design.md's "Physical
//! Data Model"): after applying the embedded migration set, `delivery_jobs`,
//! `received_activities`, `remote_public_keys`, and
//! `instance_signature_capabilities` exist, the delivery due-index and
//! target_inbox/Activity-id dedup unique index are present, and each
//! constraint design.md specifies is actually enforced by the database.
//!
//! ## Why this lives here, not in `src/migrate/tests.rs`
//! Mirrors `tests/actor_migrations_it.rs`'s own rationale:
//! `src/migrate/tests.rs` is core-runtime's own private unit-test module for
//! its generic `Migrate` boundary. federation-core may only *consume* that
//! infrastructure through its public API (`kawasemi::migrate::apply_migrations`,
//! `kawasemi::test_harness`), never add tests inside core-runtime's private
//! module. This file instead drives the exact same production
//! `apply_migrations` code path through `spawn_test_app` (the established
//! public integration-test harness this repo's other `tests/*_it.rs` files
//! already use), against its own isolated schema, and proves the *content*
//! this spec's migration owns (the four tables and their indexes/
//! constraints) rather than the generic migration-application machinery
//! core-runtime already covers.
//!
//! Each constraint is proven behaviorally (attempting the actual violating
//! insert and asserting on the real Postgres SQLSTATE it returns), not by
//! inspecting catalog/information_schema metadata alone, so a constraint
//! that exists but is silently non-functional (wrong column, wrong
//! expression, wrong target) would still be caught. The one plain
//! (non-unique) index, `delivery_jobs_due_idx`, has no violation to trigger,
//! so its existence is confirmed via `to_regclass` catalog lookup instead
//! (indexes, like tables, are relations `to_regclass` can resolve).

use kawasemi::test_harness::spawn_test_app;
use sqlx::Row;

const TEST_DB_HOST: &str = "127.0.0.1";
const TEST_DB_PORT: u16 = 5432;
const TEST_DB_URL_ENV: &str = "KAWASEMI_TEST_DATABASE_URL";

/// Best-effort raw-TCP reachability probe, independent of sqlx/the harness
/// itself. Mirrors `tests/actor_migrations_it.rs`'s own convention: used
/// only to decide whether to skip these tests in an environment with no
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

/// Requirements 7.4, 11.1, 11.4 (design.md "Physical Data Model"): a
/// `spawn_test_app` instance's isolated database — which already has the
/// embedded migrations (including `migrations/0004_federation.sql`) applied
/// via the real `apply_migrations` production code path — has the
/// `delivery_jobs`, `received_activities`, `remote_public_keys`, and
/// `instance_signature_capabilities` tables, and each of the indexes/
/// constraints design.md specifies on them is actually present/enforced:
/// - `delivery_jobs_due_idx` exists on `(status, next_attempt_at)` (11.2's
///   delivery-worker due-job lookup).
/// - `delivery_jobs_dedup_idx` is a unique index on
///   `(target_inbox, (activity->>'id'))` that rejects a second delivery job
///   for the same Activity id to the same target inbox (11.4).
/// - `received_activities.activity_id` is a primary key that rejects a
///   second row for the same Activity id (7.4's duplicate-Activity
///   idempotency).
/// - `remote_public_keys.key_id` is a primary key that rejects a second row
///   for the same keyId.
/// - `instance_signature_capabilities.host` is a primary key that rejects a
///   second row for the same host.
#[tokio::test]
async fn migrated_test_app_has_federation_tables_with_constraints() {
    if !should_run_against_real_database("migrated_test_app_has_federation_tables_with_constraints")
    {
        return;
    }

    let app = spawn_test_app().await;
    let pool = app.pool.clone();

    // The four tables exist (unqualified names resolve via this pool's
    // pinned search_path, so a hit here can only be *this* TestApp's
    // isolated schema's table).
    for table in [
        "delivery_jobs",
        "received_activities",
        "remote_public_keys",
        "instance_signature_capabilities",
    ] {
        let exists: Option<String> = sqlx::query("SELECT to_regclass($1)::text AS r")
            .bind(table)
            .fetch_one(&pool)
            .await
            .expect("querying to_regclass must succeed")
            .get("r");
        assert!(
            exists.is_some(),
            "table `{table}` must exist after applying migration 0004_federation.sql"
        );
    }

    // The delivery due-index (a plain, non-unique index with nothing to
    // violate) is confirmed to exist via catalog lookup.
    let due_idx_exists: Option<String> = sqlx::query("SELECT to_regclass($1)::text AS r")
        .bind("delivery_jobs_due_idx")
        .fetch_one(&pool)
        .await
        .expect("querying to_regclass for delivery_jobs_due_idx must succeed")
        .get("r");
    assert!(
        due_idx_exists.is_some(),
        "index `delivery_jobs_due_idx` on delivery_jobs(status, next_attempt_at) must exist"
    );

    // Seed one delivery job.
    sqlx::query(
        "INSERT INTO delivery_jobs \
         (id, sender_actor_id, target_inbox, activity, status, attempts, next_attempt_at, \
          created_at, updated_at) \
         VALUES \
         (1, 1, 'https://remote.example/inbox', '{\"id\": \"https://local.example/activities/1\"}', \
          'pending', 0, now(), now(), now())",
    )
    .execute(&pool)
    .await
    .expect("inserting a seed delivery job must succeed");

    // Requirement 11.4: `delivery_jobs_dedup_idx` rejects a second delivery
    // job for the same (target_inbox, activity id) pair, even with a
    // different job id.
    let dup_dedup_err = sqlx::query(
        "INSERT INTO delivery_jobs \
         (id, sender_actor_id, target_inbox, activity, status, attempts, next_attempt_at, \
          created_at, updated_at) \
         VALUES \
         (2, 1, 'https://remote.example/inbox', '{\"id\": \"https://local.example/activities/1\"}', \
          'pending', 0, now(), now(), now())",
    )
    .execute(&pool)
    .await
    .expect_err(
        "inserting a second delivery job with the same target_inbox and activity id must fail",
    );
    assert_eq!(
        sqlstate(
            &dup_dedup_err,
            "duplicate (target_inbox, activity id) insert"
        ),
        "23505",
        "delivery_jobs_dedup_idx must reject a duplicate (target_inbox, activity->>'id') pair"
    );

    // A delivery job for the *same* activity id but a *different*
    // target_inbox must still succeed (dedup is scoped per destination
    // inbox, not globally per Activity).
    sqlx::query(
        "INSERT INTO delivery_jobs \
         (id, sender_actor_id, target_inbox, activity, status, attempts, next_attempt_at, \
          created_at, updated_at) \
         VALUES \
         (3, 1, 'https://other.example/inbox', '{\"id\": \"https://local.example/activities/1\"}', \
          'pending', 0, now(), now(), now())",
    )
    .execute(&pool)
    .await
    .expect(
        "inserting a delivery job with the same activity id but a different target_inbox must \
         succeed: the dedup index is scoped per target_inbox",
    );

    // Requirement 7.4: `received_activities.activity_id` is a primary key
    // and thus rejects a duplicate insert for the same Activity id.
    sqlx::query(
        "INSERT INTO received_activities (activity_id, received_at) \
         VALUES ('https://remote.example/activities/1', now())",
    )
    .execute(&pool)
    .await
    .expect("inserting a seed received activity must succeed");
    let dup_received_err = sqlx::query(
        "INSERT INTO received_activities (activity_id, received_at) \
         VALUES ('https://remote.example/activities/1', now())",
    )
    .execute(&pool)
    .await
    .expect_err("inserting a duplicate received activity id must fail");
    assert_eq!(
        sqlstate(
            &dup_received_err,
            "duplicate received_activities.activity_id insert"
        ),
        "23505",
        "received_activities.activity_id must be enforced as a primary key"
    );

    // `remote_public_keys.key_id` is a primary key and thus rejects a
    // duplicate insert for the same keyId.
    sqlx::query(
        "INSERT INTO remote_public_keys (key_id, actor_uri, public_key_pem, fetched_at) \
         VALUES ('https://remote.example/actors/1#main-key', 'https://remote.example/actors/1', \
          'PEM', now())",
    )
    .execute(&pool)
    .await
    .expect("inserting a seed remote public key must succeed");
    let dup_key_err = sqlx::query(
        "INSERT INTO remote_public_keys (key_id, actor_uri, public_key_pem, fetched_at) \
         VALUES ('https://remote.example/actors/1#main-key', 'https://remote.example/actors/1', \
          'PEM-2', now())",
    )
    .execute(&pool)
    .await
    .expect_err("inserting a duplicate remote_public_keys.key_id must fail");
    assert_eq!(
        sqlstate(&dup_key_err, "duplicate remote_public_keys.key_id insert"),
        "23505",
        "remote_public_keys.key_id must be enforced as a primary key"
    );

    // `instance_signature_capabilities.host` is a primary key and thus
    // rejects a duplicate insert for the same host.
    sqlx::query(
        "INSERT INTO instance_signature_capabilities (host, format, updated_at) \
         VALUES ('remote.example', 'rfc9421', now())",
    )
    .execute(&pool)
    .await
    .expect("inserting a seed instance signature capability must succeed");
    let dup_host_err = sqlx::query(
        "INSERT INTO instance_signature_capabilities (host, format, updated_at) \
         VALUES ('remote.example', 'draft_cavage', now())",
    )
    .execute(&pool)
    .await
    .expect_err("inserting a duplicate instance_signature_capabilities.host must fail");
    assert_eq!(
        sqlstate(
            &dup_host_err,
            "duplicate instance_signature_capabilities.host insert"
        ),
        "23505",
        "instance_signature_capabilities.host must be enforced as a primary key"
    );

    app.cleanup().await;
}
