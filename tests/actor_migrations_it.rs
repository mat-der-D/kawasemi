//! Integration test for task 1.1 ("アクター/オーナー/鍵テーブルのマイグレー
//! ションを追加する", Requirements 1.2, 2.1, 5.3; design.md's "Physical Data
//! Model"): after applying the embedded migration set, `owners`,
//! `local_actors`, and `actor_signing_keys` exist and each constraint
//! design.md specifies is actually enforced by the database.
//!
//! ## Why this lives here, not in `src/migrate/tests.rs`
//! `src/migrate/tests.rs` is core-runtime's own private unit-test module for
//! its `Migrate` boundary (applying an embedded migration set generically).
//! actor-model's design.md explicitly lists "マイグレーション基盤、テスト
//! ハーネス土台（core-runtime が所有）" under "Out of Boundary": this spec
//! may only *consume* that infrastructure through its public API
//! (`kawasemi::migrate::apply_migrations`, `kawasemi::test_harness`), never
//! add tests inside core-runtime's own private module. This file instead
//! drives the exact same production `apply_migrations` code path through
//! `spawn_test_app` (the established public integration-test harness this
//! repo's other `tests/*_it.rs` files already use, e.g.
//! `tests/test_harness_lifecycle_it.rs`), against its own isolated schema,
//! and proves the *content* this spec owns (the three tables and their
//! constraints) rather than the generic migration-application machinery
//! core-runtime already covers.
//!
//! Each constraint is proven behaviorally (attempting the actual violating
//! insert and asserting on the real Postgres SQLSTATE it returns), not by
//! inspecting catalog/information_schema metadata alone, so a constraint
//! that exists but is silently non-functional (wrong column, wrong partial
//! predicate, wrong reference target) would still be caught.

use kawasemi::test_harness::spawn_test_app;
use sqlx::Row;

const TEST_DB_HOST: &str = "127.0.0.1";
const TEST_DB_PORT: u16 = 5432;
const TEST_DB_URL_ENV: &str = "KAWASEMI_TEST_DATABASE_URL";

/// Best-effort raw-TCP reachability probe, independent of sqlx/the harness
/// itself. Mirrors `tests/test_harness_lifecycle_it.rs`'s own convention:
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

/// Requirements 1.2, 2.1, 5.3 (design.md "Physical Data Model"): a
/// `spawn_test_app` instance's isolated database — which already has the
/// embedded migrations (including `migrations/0002_actors.sql`) applied via
/// the real `apply_migrations` production code path — has the `owners`,
/// `local_actors`, and `actor_signing_keys` tables, and each of the
/// constraints design.md specifies on them is actually enforced:
/// - `local_actors.handle` is unique instance-wide (1.2).
/// - `local_actors.owner_id` is a foreign key into `owners` (2.1).
/// - `actor_signing_keys.actor_id` is a foreign key into `local_actors`.
/// - at most one `status = 'active'` signing key exists per actor at a time
///   (5.3), enforced by the partial unique index
///   `actor_signing_keys_active_unique`, while multiple `retired` keys for
///   the same actor are allowed (5.4's "distinguishable, retained" retired
///   keys).
#[tokio::test]
async fn migrated_test_app_has_actor_owner_key_tables_with_constraints() {
    if !should_run_against_real_database(
        "migrated_test_app_has_actor_owner_key_tables_with_constraints",
    ) {
        return;
    }

    let app = spawn_test_app().await;
    let pool = app.pool.clone();

    // The three tables exist (unqualified names resolve via this pool's
    // pinned search_path, so a hit here can only be *this* TestApp's
    // isolated schema's table).
    for table in ["owners", "local_actors", "actor_signing_keys"] {
        let exists: Option<String> = sqlx::query("SELECT to_regclass($1)::text AS r")
            .bind(table)
            .fetch_one(&pool)
            .await
            .expect("querying to_regclass must succeed")
            .get("r");
        assert!(
            exists.is_some(),
            "table `{table}` must exist after applying migration 0002_actors.sql"
        );
    }

    // Seed one owner and one actor to exercise the FK/unique constraints
    // against.
    sqlx::query("INSERT INTO owners (id, created_at) VALUES (1, now())")
        .execute(&pool)
        .await
        .expect("inserting a seed owner must succeed");
    sqlx::query(
        "INSERT INTO local_actors \
         (id, owner_id, handle, actor_type, state, created_at, updated_at) \
         VALUES (1, 1, 'alice', 'person', 'active', now(), now())",
    )
    .execute(&pool)
    .await
    .expect("inserting a seed local actor must succeed");

    // Requirement 1.2: `local_actors.handle` is unique. A second actor with
    // the same handle (different id, same owner) must be rejected as a
    // unique violation (SQLSTATE 23505).
    let dup_handle_err = sqlx::query(
        "INSERT INTO local_actors \
         (id, owner_id, handle, actor_type, state, created_at, updated_at) \
         VALUES (2, 1, 'alice', 'person', 'active', now(), now())",
    )
    .execute(&pool)
    .await
    .expect_err("inserting a second local actor with a duplicate handle must fail");
    assert_eq!(
        sqlstate(&dup_handle_err, "duplicate handle insert"),
        "23505",
        "local_actors_handle_unique must reject a duplicate handle with a unique violation"
    );

    // Requirement 2.1: `local_actors.owner_id` is a foreign key into
    // `owners`. Referencing a non-existent owner must be rejected
    // (SQLSTATE 23503).
    let missing_owner_err = sqlx::query(
        "INSERT INTO local_actors \
         (id, owner_id, handle, actor_type, state, created_at, updated_at) \
         VALUES (3, 999999, 'bob', 'person', 'active', now(), now())",
    )
    .execute(&pool)
    .await
    .expect_err("inserting a local actor with a non-existent owner_id must fail");
    assert_eq!(
        sqlstate(&missing_owner_err, "missing owner FK insert"),
        "23503",
        "local_actors.owner_id must be enforced as a foreign key into owners"
    );

    // Seed one active signing key for the seed actor.
    sqlx::query(
        "INSERT INTO actor_signing_keys \
         (id, actor_id, algorithm, public_key_pem, sealed_private_key, status, created_at) \
         VALUES (1, 1, 'rsa-2048', 'PEM', E'\\\\x00', 'active', now())",
    )
    .execute(&pool)
    .await
    .expect("inserting the seed actor's active signing key must succeed");

    // Requirement 5.3: at most one active signing key per actor. A second
    // active key for the same actor must be rejected as a unique violation
    // via the partial unique index `actor_signing_keys_active_unique`
    // (SQLSTATE 23505).
    let dup_active_key_err = sqlx::query(
        "INSERT INTO actor_signing_keys \
         (id, actor_id, algorithm, public_key_pem, sealed_private_key, status, created_at) \
         VALUES (2, 1, 'rsa-2048', 'PEM', E'\\\\x00', 'active', now())",
    )
    .execute(&pool)
    .await
    .expect_err("inserting a second active signing key for the same actor must fail");
    assert_eq!(
        sqlstate(&dup_active_key_err, "duplicate active key insert"),
        "23505",
        "actor_signing_keys_active_unique must reject a second active key for the same actor"
    );

    // Requirement 5.4: a retired key for the same actor is not blocked by
    // the partial unique index (it only covers status = 'active'), so
    // rotation history can accumulate multiple retired rows.
    sqlx::query(
        "INSERT INTO actor_signing_keys \
         (id, actor_id, algorithm, public_key_pem, sealed_private_key, status, created_at) \
         VALUES (3, 1, 'rsa-2048', 'PEM', E'\\\\x00', 'retired', now())",
    )
    .execute(&pool)
    .await
    .expect(
        "inserting a retired signing key for an actor that already has an active key must \
         succeed: the partial unique index must not cover retired rows",
    );

    // The FK on `actor_signing_keys.actor_id` into `local_actors` is
    // enforced: referencing a non-existent actor must be rejected
    // (SQLSTATE 23503).
    let missing_actor_err = sqlx::query(
        "INSERT INTO actor_signing_keys \
         (id, actor_id, algorithm, public_key_pem, sealed_private_key, status, created_at) \
         VALUES (4, 999999, 'rsa-2048', 'PEM', E'\\\\x00', 'active', now())",
    )
    .execute(&pool)
    .await
    .expect_err("inserting a signing key referencing a non-existent actor must fail");
    assert_eq!(
        sqlstate(&missing_actor_err, "missing actor FK insert"),
        "23503",
        "actor_signing_keys.actor_id must be enforced as a foreign key into local_actors"
    );

    app.cleanup().await;
}
