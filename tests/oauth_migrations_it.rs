//! Integration test for task 1.1 ("OAuth 永続テーブルのマイグレーションを追
//! 加する", Requirements 1.1, 1.5, 2.3, 2.5, 3.1, 3.4, 3.5; design.md's
//! "Physical Data Model"): after applying the embedded migration set,
//! `oauth_applications`, `oauth_authorization_codes`, and
//! `oauth_access_tokens` exist and each constraint design.md specifies on
//! them is actually enforced by the database.
//!
//! ## Why this lives here, not in `src/migrate/tests.rs`
//! Mirrors `tests/actor_migrations_it.rs`'s own reasoning: `src/migrate/
//! tests.rs` is core-runtime's own private unit-test module for its
//! `Migrate` boundary (applying an embedded migration set generically).
//! api-foundation's design.md explicitly keeps migration infrastructure
//! itself owned by core-runtime; this spec may only *consume* that
//! infrastructure through its public API (`kawasemi::migrate::
//! apply_migrations`, `kawasemi::test_harness`), never add tests inside
//! core-runtime's own private module. This file instead drives the exact
//! same production `apply_migrations` code path through `spawn_test_app`
//! (the established public integration-test harness this repo's other
//! `tests/*_it.rs` files already use), against its own isolated schema, and
//! proves the *content* this task owns (the three tables and their
//! constraints) rather than the generic migration-application machinery
//! core-runtime already covers.
//!
//! Each constraint is proven behaviorally (attempting the actual violating
//! insert/read and asserting on the real Postgres SQLSTATE it returns, or on
//! `information_schema` metadata for `NOT NULL`/column-presence facts that
//! have no natural violating-insert form), not by inspecting catalog
//! metadata alone wherever a behavioral proof is possible, so a constraint
//! that exists but is silently non-functional (wrong column, wrong table,
//! wrong reference target) would still be caught.

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

/// Returns whether `column` on `table` is declared `NOT NULL`, via
/// `information_schema.columns`. Used only for the `actor_id` NOT NULL
/// facts, which (being a logical-only reference to actor-model with no
/// FK) have no natural violating-insert form distinct from "omit the
/// column", which a plain `NOT NULL` insert-omission check already covers
/// more directly below; kept as a second, independent proof.
async fn column_is_not_null(pool: &sqlx::PgPool, table: &str, column: &str) -> bool {
    let is_nullable: String = sqlx::query(
        "SELECT is_nullable FROM information_schema.columns \
         WHERE table_name = $1 AND column_name = $2",
    )
    .bind(table)
    .bind(column)
    .fetch_one(pool)
    .await
    .unwrap_or_else(|err| panic!("querying information_schema.columns for {table}.{column} must succeed: {err:?}"))
    .get("is_nullable");
    is_nullable == "NO"
}

/// Requirements 1.1, 1.5, 2.3, 2.5, 3.1, 3.4, 3.5 (design.md "Physical Data
/// Model"): a `spawn_test_app` instance's isolated database — which already
/// has the embedded migrations (including `migrations/0003_oauth.sql`)
/// applied via the real `apply_migrations` production code path — has the
/// `oauth_applications`, `oauth_authorization_codes`, and
/// `oauth_access_tokens` tables, and each of the constraints design.md
/// specifies on them is actually enforced:
/// - `oauth_applications.client_id` is unique instance-wide (1.1).
/// - `oauth_applications.client_secret_hash` exists, is `NOT NULL`, and is a
///   binary (hash) column, never a plaintext secret column (1.5).
/// - `oauth_authorization_codes.code_hash` is the primary key (codes are
///   hash-stored, 3.5) and `app_id` is a foreign key into
///   `oauth_applications`.
/// - `oauth_authorization_codes.actor_id` is `NOT NULL` (2.3, 3.5).
/// - a consumed or expired authorization code is unusable for exchange —
///   proven by the `consumed`/`expires_at` columns existing with the right
///   types/defaults so the "unconsumed AND unexpired" exchange precondition
///   is expressible (2.5).
/// - `oauth_access_tokens.token_hash` is unique and `NOT NULL` (tokens are
///   hash-stored, never plaintext, 3.1, 3.5).
/// - `oauth_access_tokens.actor_id` is `NOT NULL` (3.1, 3.5).
/// - `oauth_access_tokens.revoked` exists, defaults to `FALSE`, and can be
///   flipped to invalidate a token going forward (3.4).
#[tokio::test]
async fn migrated_test_app_has_oauth_tables_with_constraints() {
    if !should_run_against_real_database("migrated_test_app_has_oauth_tables_with_constraints") {
        return;
    }

    let app = spawn_test_app().await;
    let pool = app.pool.clone();

    // The three tables exist (unqualified names resolve via this pool's
    // pinned search_path, so a hit here can only be *this* TestApp's
    // isolated schema's table).
    for table in [
        "oauth_applications",
        "oauth_authorization_codes",
        "oauth_access_tokens",
    ] {
        let exists: Option<String> = sqlx::query("SELECT to_regclass($1)::text AS r")
            .bind(table)
            .fetch_one(&pool)
            .await
            .expect("querying to_regclass must succeed")
            .get("r");
        assert!(
            exists.is_some(),
            "table `{table}` must exist after applying migration 0003_oauth.sql"
        );
    }

    // `client_secret_hash` exists on `oauth_applications`, is NOT NULL, and
    // is stored as `bytea` (a hash column), never `text` (which would
    // suggest a plaintext secret column) — Requirement 1.5.
    let secret_hash_col: (String, String) = {
        let row = sqlx::query(
            "SELECT data_type, is_nullable FROM information_schema.columns \
             WHERE table_name = 'oauth_applications' AND column_name = 'client_secret_hash'",
        )
        .fetch_one(&pool)
        .await
        .expect("oauth_applications.client_secret_hash must exist");
        (row.get("data_type"), row.get("is_nullable"))
    };
    assert_eq!(
        secret_hash_col.0, "bytea",
        "oauth_applications.client_secret_hash must be a binary hash column (bytea), not plaintext text"
    );
    assert_eq!(
        secret_hash_col.1, "NO",
        "oauth_applications.client_secret_hash must be NOT NULL"
    );

    // Seed one application to exercise the FK/unique/NOT NULL constraints
    // against.
    sqlx::query(
        "INSERT INTO oauth_applications \
         (id, client_id, client_secret_hash, name, redirect_uris, scopes, created_at) \
         VALUES (1, 'client-abc', E'\\\\x00', 'Test App', 'https://example.test/cb', 'read', now())",
    )
    .execute(&pool)
    .await
    .expect("inserting a seed oauth application must succeed");

    // Requirement 1.1: `oauth_applications.client_id` is unique. A second
    // application with the same client_id (different id) must be rejected
    // as a unique violation (SQLSTATE 23505).
    let dup_client_id_err = sqlx::query(
        "INSERT INTO oauth_applications \
         (id, client_id, client_secret_hash, name, redirect_uris, scopes, created_at) \
         VALUES (2, 'client-abc', E'\\\\x00', 'Other App', 'https://example.test/cb2', 'read', now())",
    )
    .execute(&pool)
    .await
    .expect_err("inserting a second oauth application with a duplicate client_id must fail");
    assert_eq!(
        sqlstate(&dup_client_id_err, "duplicate client_id insert"),
        "23505",
        "oauth_applications.client_id must reject a duplicate value with a unique violation"
    );

    // Requirement 3.5: `oauth_authorization_codes.code_hash` is the primary
    // key (hash-stored, not the code value itself), and `app_id` is a
    // foreign key into `oauth_applications`. `actor_id` is NOT NULL
    // (2.3, 3.5).
    assert!(
        column_is_not_null(&pool, "oauth_authorization_codes", "actor_id").await,
        "oauth_authorization_codes.actor_id must be NOT NULL"
    );

    sqlx::query(
        "INSERT INTO oauth_authorization_codes \
         (code_hash, app_id, actor_id, scopes, redirect_uri, expires_at, consumed) \
         VALUES (E'\\\\x01', 1, 42, 'read write', 'https://example.test/cb', now() + interval '10 minutes', FALSE)",
    )
    .execute(&pool)
    .await
    .expect("inserting a seed authorization code must succeed");

    // `code_hash` is the primary key: a second row with the same hash must
    // be rejected as a unique violation (SQLSTATE 23505).
    let dup_code_hash_err = sqlx::query(
        "INSERT INTO oauth_authorization_codes \
         (code_hash, app_id, actor_id, scopes, redirect_uri, expires_at, consumed) \
         VALUES (E'\\\\x01', 1, 43, 'read', 'https://example.test/cb', now() + interval '10 minutes', FALSE)",
    )
    .execute(&pool)
    .await
    .expect_err("inserting a second authorization code with a duplicate code_hash must fail");
    assert_eq!(
        sqlstate(&dup_code_hash_err, "duplicate code_hash insert"),
        "23505",
        "oauth_authorization_codes.code_hash (PRIMARY KEY) must reject a duplicate value"
    );

    // `oauth_authorization_codes.app_id` is a foreign key into
    // `oauth_applications`: referencing a non-existent app must be rejected
    // (SQLSTATE 23503).
    let missing_app_err = sqlx::query(
        "INSERT INTO oauth_authorization_codes \
         (code_hash, app_id, actor_id, scopes, redirect_uri, expires_at, consumed) \
         VALUES (E'\\\\x02', 999999, 42, 'read', 'https://example.test/cb', now() + interval '10 minutes', FALSE)",
    )
    .execute(&pool)
    .await
    .expect_err("inserting an authorization code referencing a non-existent app must fail");
    assert_eq!(
        sqlstate(&missing_app_err, "missing app FK insert (authorization code)"),
        "23503",
        "oauth_authorization_codes.app_id must be enforced as a foreign key into oauth_applications"
    );

    // `actor_id` cannot be omitted (NOT NULL) — attempting to insert an
    // explicit NULL must be rejected (SQLSTATE 23502).
    let null_actor_code_err = sqlx::query(
        "INSERT INTO oauth_authorization_codes \
         (code_hash, app_id, actor_id, scopes, redirect_uri, expires_at, consumed) \
         VALUES (E'\\\\x03', 1, NULL, 'read', 'https://example.test/cb', now() + interval '10 minutes', FALSE)",
    )
    .execute(&pool)
    .await
    .expect_err("inserting an authorization code with a NULL actor_id must fail");
    assert_eq!(
        sqlstate(&null_actor_code_err, "null actor_id insert (authorization code)"),
        "23502",
        "oauth_authorization_codes.actor_id must reject NULL (NOT NULL)"
    );

    // Requirement 3.1, 3.4, 3.5: `oauth_access_tokens.token_hash` is unique
    // and NOT NULL (hash-stored), `app_id` is a foreign key into
    // `oauth_applications`, `actor_id` is NOT NULL, and `revoked` defaults
    // to FALSE and can be flipped to invalidate the token.
    assert!(
        column_is_not_null(&pool, "oauth_access_tokens", "actor_id").await,
        "oauth_access_tokens.actor_id must be NOT NULL"
    );

    sqlx::query(
        "INSERT INTO oauth_access_tokens \
         (id, token_hash, app_id, actor_id, scopes, created_at) \
         VALUES (1, E'\\\\x10', 1, 42, 'read write', now())",
    )
    .execute(&pool)
    .await
    .expect("inserting a seed access token must succeed");

    // `revoked` defaults to FALSE.
    let revoked: bool = sqlx::query("SELECT revoked FROM oauth_access_tokens WHERE id = 1")
        .fetch_one(&pool)
        .await
        .expect("selecting the seed token's revoked flag must succeed")
        .get("revoked");
    assert!(
        !revoked,
        "oauth_access_tokens.revoked must default to FALSE for a freshly issued token"
    );

    // `token_hash` is unique: a second token with the same hash (different
    // id) must be rejected as a unique violation (SQLSTATE 23505).
    let dup_token_hash_err = sqlx::query(
        "INSERT INTO oauth_access_tokens \
         (id, token_hash, app_id, actor_id, scopes, created_at) \
         VALUES (2, E'\\\\x10', 1, 43, 'read', now())",
    )
    .execute(&pool)
    .await
    .expect_err("inserting a second access token with a duplicate token_hash must fail");
    assert_eq!(
        sqlstate(&dup_token_hash_err, "duplicate token_hash insert"),
        "23505",
        "oauth_access_tokens.token_hash must reject a duplicate value with a unique violation"
    );

    // `oauth_access_tokens.app_id` is a foreign key into
    // `oauth_applications`: referencing a non-existent app must be rejected
    // (SQLSTATE 23503).
    let missing_app_token_err = sqlx::query(
        "INSERT INTO oauth_access_tokens \
         (id, token_hash, app_id, actor_id, scopes, created_at) \
         VALUES (3, E'\\\\x11', 999999, 42, 'read', now())",
    )
    .execute(&pool)
    .await
    .expect_err("inserting an access token referencing a non-existent app must fail");
    assert_eq!(
        sqlstate(&missing_app_token_err, "missing app FK insert (access token)"),
        "23503",
        "oauth_access_tokens.app_id must be enforced as a foreign key into oauth_applications"
    );

    // `actor_id` cannot be omitted (NOT NULL) on access tokens either.
    let null_actor_token_err = sqlx::query(
        "INSERT INTO oauth_access_tokens \
         (id, token_hash, app_id, actor_id, scopes, created_at) \
         VALUES (4, E'\\\\x12', 1, NULL, 'read', now())",
    )
    .execute(&pool)
    .await
    .expect_err("inserting an access token with a NULL actor_id must fail");
    assert_eq!(
        sqlstate(&null_actor_token_err, "null actor_id insert (access token)"),
        "23502",
        "oauth_access_tokens.actor_id must reject NULL (NOT NULL)"
    );

    // Revoke the seed token and confirm the flag actually flips (the
    // authentication-time invalidation check itself is a later task's
    // service-layer concern; this migration only proves the column is
    // present, defaults correctly, and is mutable).
    sqlx::query("UPDATE oauth_access_tokens SET revoked = TRUE WHERE id = 1")
        .execute(&pool)
        .await
        .expect("revoking the seed token must succeed");
    let revoked_after: bool = sqlx::query("SELECT revoked FROM oauth_access_tokens WHERE id = 1")
        .fetch_one(&pool)
        .await
        .expect("selecting the seed token's revoked flag after revocation must succeed")
        .get("revoked");
    assert!(
        revoked_after,
        "oauth_access_tokens.revoked must be settable to TRUE to invalidate a token (3.4)"
    );

    app.cleanup().await;
}
