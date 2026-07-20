//! Integration test for accounts-and-instance task 1.1 ("マイグレーション
//! 0005 で本 spec 所有テーブルを定義する" — implemented as migration
//! `0006_accounts.sql`, see that file's own "Naming note" header comment for
//! why; Requirements 6.5, 7.2, 8.2, 9.1; design.md's "Physical Data Model"):
//! after applying the embedded migration set, `account_profiles`,
//! `remote_accounts`, `custom_emojis`, and `instance_settings` exist and each
//! constraint design.md specifies on them is actually enforced by the
//! database.
//!
//! ## Why this lives here, not in `src/migrate/tests.rs`
//! Mirrors `tests/actor_migrations_it.rs`'s/`tests/media_migrations_it.rs`'s
//! own rationale: `src/migrate/tests.rs` is core-runtime's own private
//! unit-test module for its generic `Migrate` boundary. accounts-and-instance
//! may only *consume* that infrastructure through its public API
//! (`kawasemi::migrate::apply_migrations`, `kawasemi::test_harness`), never
//! add tests inside core-runtime's private module. This file instead drives
//! the exact same production `apply_migrations` code path through
//! `spawn_test_app` (the established public integration-test harness this
//! repo's other `tests/*_it.rs` files already use), against its own isolated
//! schema, and proves the *content* this spec owns (the four tables and their
//! constraints) rather than the generic migration-application machinery
//! core-runtime already covers.
//!
//! Each constraint is proven behaviorally (attempting the actual violating
//! insert and asserting on the real Postgres SQLSTATE it returns, or
//! successfully inserting a minimal valid row when the constraint is a
//! NOT NULL/default/UNIQUE/CHECK/composite-PK requirement), not by inspecting
//! catalog/information_schema metadata alone, so a constraint that exists but
//! is silently non-functional (wrong column, wrong expression, wrong target)
//! would still be caught.

use kawasemi::test_harness::spawn_test_app;
use sqlx::Row;

const TEST_DB_HOST: &str = "127.0.0.1";
const TEST_DB_PORT: u16 = 5432;
const TEST_DB_URL_ENV: &str = "KAWASEMI_TEST_DATABASE_URL";

/// Best-effort raw-TCP reachability probe, independent of sqlx/the harness
/// itself. Mirrors `tests/actor_migrations_it.rs`'s own convention: used only
/// to decide whether to skip these tests in an environment with no local
/// PostgreSQL at all, never to swallow a real regression.
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

/// Requirements 6.5, 7.2, 8.2, 9.1 (design.md "Physical Data Model"): a
/// `spawn_test_app` instance's isolated database — which already has the
/// embedded migrations (including `migrations/0006_accounts.sql`) applied via
/// the real `apply_migrations` production code path — has the
/// `account_profiles`, `remote_accounts`, `custom_emojis`, and
/// `instance_settings` tables, and each of the constraints design.md
/// specifies on them is actually enforced:
/// - `account_profiles.display_name`/`note` exist and default to `''`
///   (design.md's model doc: "Account/CredentialAccount の同名フィールドの
///   供給元", 1.1/2.2/6.1).
/// - `remote_accounts.actor_uri` is unique (7.2).
/// - `custom_emojis` has a composite primary key `(shortcode, domain)`.
/// - `instance_settings` is a single-row table: `id` is constrained to `1`
///   (8.2/8.3).
#[tokio::test]
async fn migrated_test_app_has_accounts_tables_with_constraints() {
    if !should_run_against_real_database("migrated_test_app_has_accounts_tables_with_constraints") {
        return;
    }

    let app = spawn_test_app().await;
    let pool = app.pool.clone();

    // The four tables exist (unqualified names resolve via this pool's
    // pinned search_path, so a hit here can only be *this* TestApp's
    // isolated schema's table).
    for table in [
        "account_profiles",
        "remote_accounts",
        "custom_emojis",
        "instance_settings",
    ] {
        let exists: Option<String> = sqlx::query("SELECT to_regclass($1)::text AS r")
            .bind(table)
            .fetch_one(&pool)
            .await
            .expect("querying to_regclass must succeed")
            .get("r");
        assert!(
            exists.is_some(),
            "table `{table}` must exist after applying migration 0006_accounts.sql"
        );
    }

    // Requirement 6.5 (design.md model doc): `account_profiles.display_name`/
    // `note` exist, are NOT NULL, and default to `''` — a minimal insert
    // supplying only the primary key and `updated_at` must succeed and
    // round-trip empty-string defaults.
    sqlx::query("INSERT INTO account_profiles (actor_id, updated_at) VALUES (1, now())")
        .execute(&pool)
        .await
        .expect(
            "inserting a minimal account_profiles row (relying on display_name/note defaults) \
             must succeed",
        );
    let row = sqlx::query("SELECT display_name, note FROM account_profiles WHERE actor_id = 1")
        .fetch_one(&pool)
        .await
        .expect("selecting the seeded account_profiles row must succeed");
    let display_name: String = row.get("display_name");
    let note: String = row.get("note");
    assert_eq!(
        display_name, "",
        "account_profiles.display_name must default to '' (6.5)"
    );
    assert_eq!(note, "", "account_profiles.note must default to '' (6.5)");

    // `account_profiles.actor_id` is the primary key: a second row with the
    // same actor_id must be rejected as a unique/PK violation (SQLSTATE
    // 23505).
    let dup_actor_err =
        sqlx::query("INSERT INTO account_profiles (actor_id, updated_at) VALUES (1, now())")
            .execute(&pool)
            .await
            .expect_err(
                "inserting a second account_profiles row with a duplicate actor_id must fail",
            );
    assert_eq!(
        sqlstate(&dup_actor_err, "duplicate account_profiles.actor_id insert"),
        "23505",
        "account_profiles.actor_id must be enforced as a primary key"
    );

    // Requirement 7.2: `remote_accounts.actor_uri` is unique. Seed one row,
    // then a second row with the same actor_uri (different id) must be
    // rejected as a unique violation (SQLSTATE 23505).
    sqlx::query(
        "INSERT INTO remote_accounts \
         (id, actor_uri, username, domain, url, fetched_at) \
         VALUES (1, 'https://remote.example/users/alice', 'alice', 'remote.example', \
                 'https://remote.example/@alice', now())",
    )
    .execute(&pool)
    .await
    .expect("inserting a seed remote_accounts row must succeed");
    let dup_actor_uri_err = sqlx::query(
        "INSERT INTO remote_accounts \
         (id, actor_uri, username, domain, url, fetched_at) \
         VALUES (2, 'https://remote.example/users/alice', 'alice2', 'remote.example', \
                 'https://remote.example/@alice2', now())",
    )
    .execute(&pool)
    .await
    .expect_err("inserting a second remote_accounts row with a duplicate actor_uri must fail");
    assert_eq!(
        sqlstate(
            &dup_actor_uri_err,
            "duplicate remote_accounts.actor_uri insert"
        ),
        "23505",
        "remote_accounts.actor_uri must be enforced as UNIQUE (7.2)"
    );

    // Requirement 9.1: `custom_emojis` has a composite primary key
    // `(shortcode, domain)`. A duplicate (shortcode, domain) pair must be
    // rejected (SQLSTATE 23505), while the same shortcode under a different
    // domain must be accepted (distinct composite key).
    sqlx::query(
        "INSERT INTO custom_emojis \
         (shortcode, domain, url, static_url, updated_at) \
         VALUES ('blobcat', '', 'https://example/emoji/blobcat.png', \
                 'https://example/emoji/blobcat_static.png', now())",
    )
    .execute(&pool)
    .await
    .expect("inserting a seed local custom_emojis row (domain = '') must succeed");
    let dup_emoji_err = sqlx::query(
        "INSERT INTO custom_emojis \
         (shortcode, domain, url, static_url, updated_at) \
         VALUES ('blobcat', '', 'https://example/emoji/blobcat2.png', \
                 'https://example/emoji/blobcat2_static.png', now())",
    )
    .execute(&pool)
    .await
    .expect_err(
        "inserting a second custom_emojis row with a duplicate (shortcode, domain) must fail",
    );
    assert_eq!(
        sqlstate(
            &dup_emoji_err,
            "duplicate custom_emojis (shortcode, domain) insert"
        ),
        "23505",
        "custom_emojis must enforce a composite primary key on (shortcode, domain) (9.1)"
    );
    sqlx::query(
        "INSERT INTO custom_emojis \
         (shortcode, domain, url, static_url, updated_at) \
         VALUES ('blobcat', 'remote.example', 'https://remote.example/emoji/blobcat.png', \
                 'https://remote.example/emoji/blobcat_static.png', now())",
    )
    .execute(&pool)
    .await
    .expect(
        "inserting the same shortcode under a different domain must succeed: the composite \
         primary key must not collide across domains",
    );

    // Requirement 8.2: `instance_settings` is a single-row table; `id = 1`
    // must succeed, and any other `id` value must be rejected by the CHECK
    // constraint (SQLSTATE 23514).
    sqlx::query("INSERT INTO instance_settings (id, updated_at) VALUES (1, now())")
        .execute(&pool)
        .await
        .expect("inserting the single instance_settings row with id = 1 must succeed");
    let bad_singleton_err =
        sqlx::query("INSERT INTO instance_settings (id, updated_at) VALUES (2, now())")
            .execute(&pool)
            .await
            .expect_err("inserting an instance_settings row with id != 1 must fail");
    assert_eq!(
        sqlstate(&bad_singleton_err, "instance_settings id != 1 insert"),
        "23514",
        "instance_settings must enforce the id = 1 singleton CHECK constraint (8.2)"
    );

    // `instance_settings.thumbnail`/`languages` columns exist and are
    // queryable (8.1's Instance(v2) `thumbnail`/`languages` fields' supply
    // source).
    let settings_row =
        sqlx::query("SELECT thumbnail, languages FROM instance_settings WHERE id = 1")
            .fetch_one(&pool)
            .await
            .expect("selecting instance_settings.thumbnail/languages must succeed");
    let thumbnail: Option<String> = settings_row.get("thumbnail");
    assert_eq!(
        thumbnail, None,
        "instance_settings.thumbnail must default to NULL (unset) (8.2, 8.3)"
    );

    app.cleanup().await;
}
