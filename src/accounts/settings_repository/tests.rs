//! Integration tests for `InstanceSettingsRepository` (Requirements 8.1,
//! 8.2, 8.3), per task 2.4's observable completion condition: "設定未投入
//! でも全項目（`thumbnail`/`languages` を含む）が既定で埋まった値が返る統合
//! テストが green".
//!
//! Mirrors `src/accounts/profile_repository/tests.rs`'s established
//! convention: reuses `crate::test_harness::spawn_test_app` for an
//! isolated, already-migrated schema. Unlike the profile/remote/emoji
//! repository tests, this module never calls any write function of its
//! own (there is none — see `settings_repository.rs`'s doc comment,
//! "Read-only, by construction") — the "row present" tests instead seed
//! `instance_settings` directly with raw `sqlx::query` `INSERT`s, exactly
//! as `emoji_repository.rs`'s own tests seed `custom_emojis` rows.

use super::load_instance_settings;
use crate::domain::Id;
use crate::test_harness::spawn_test_app;

/// The task's own named observable completion condition: against a
/// freshly-migrated database where the `instance_settings` singleton row
/// was never inserted (there is no seed/bootstrap `INSERT` anywhere in
/// this crate), `load_instance_settings` must still return every field at
/// its safe default — explicitly including `thumbnail: None` and
/// `languages: vec![]` (Requirement 8.1).
#[tokio::test]
async fn load_instance_settings_returns_all_defaults_when_no_row_exists() {
    let app = spawn_test_app().await;

    let row_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM instance_settings")
        .fetch_one(&app.pool)
        .await
        .expect("counting rows must succeed");
    assert_eq!(
        row_count.0, 0,
        "the test database must start with no instance_settings row"
    );

    let settings = load_instance_settings(&app.pool)
        .await
        .expect("load_instance_settings must succeed even when no row exists");

    assert_eq!(settings.title, "");
    assert_eq!(settings.description, "");
    assert_eq!(settings.contact_email, "");
    assert!(settings.contact_account_id.is_none());
    assert!(settings.rules.is_empty());
    assert!(!settings.registrations_enabled);
    assert!(!settings.registrations_approval_required);
    assert!(settings.registrations_message.is_none());
    assert!(settings.thumbnail.is_none());
    assert!(settings.languages.is_empty());

    app.cleanup().await;
}

/// Requirement 8.2/8.3: with a row present that sets only *some* fields
/// (via a raw `INSERT` naming only those columns), the remaining columns
/// keep the table's own `DEFAULT`s (applied by Postgres at `INSERT` time),
/// and `load_instance_settings` reports the set values verbatim alongside
/// the still-default unset ones — proving per-field default-merging
/// behavior, not just "row exists vs. doesn't".
#[tokio::test]
async fn load_instance_settings_returns_set_values_and_defaults_for_the_rest() {
    let app = spawn_test_app().await;
    let now = app.runtime.clock.now();

    sqlx::query(
        "INSERT INTO instance_settings (id, title, registrations_enabled, updated_at) \
         VALUES (1, $1, $2, $3)",
    )
    .bind("Kawasemi Test Instance")
    .bind(true)
    .bind(now)
    .execute(&app.pool)
    .await
    .expect("seeding a partial instance_settings row must succeed");

    let settings = load_instance_settings(&app.pool)
        .await
        .expect("load_instance_settings must succeed");

    // Set explicitly.
    assert_eq!(settings.title, "Kawasemi Test Instance");
    assert!(settings.registrations_enabled);

    // Left at the table's own default.
    assert_eq!(settings.description, "");
    assert_eq!(settings.contact_email, "");
    assert!(settings.contact_account_id.is_none());
    assert!(settings.rules.is_empty());
    assert!(!settings.registrations_approval_required);
    assert!(settings.registrations_message.is_none());
    assert!(settings.thumbnail.is_none());
    assert!(settings.languages.is_empty());

    app.cleanup().await;
}

/// Requirement 8.1, 8.2: `rules`/`languages` (both `JSONB` arrays of
/// strings) round-trip correctly into `Vec<String>`, and every other
/// scalar/nullable column set by the seed row (including `thumbnail`, a
/// nullable `TEXT` column, and `contact_account_id`, a nullable `BIGINT`)
/// is reported verbatim.
#[tokio::test]
async fn load_instance_settings_round_trips_rules_languages_and_nullable_fields() {
    let app = spawn_test_app().await;
    let now = app.runtime.clock.now();
    let contact_account_id = Id::from_i64(4242);

    sqlx::query(
        "INSERT INTO instance_settings ( \
             id, title, description, contact_email, contact_account_id, rules, \
             registrations_enabled, registrations_approval_required, registrations_message, \
             thumbnail, languages, updated_at \
         ) VALUES ( \
             1, $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11 \
         )",
    )
    .bind("Kawasemi")
    .bind("A single-user instance.")
    .bind("admin@kawasemi.example")
    .bind(contact_account_id.as_i64())
    .bind(serde_json::json!([
        "Be excellent to each other.",
        "No spam."
    ]))
    .bind(false)
    .bind(true)
    .bind("Approval required for new accounts.")
    .bind("https://kawasemi.example/thumbnail.png")
    .bind(serde_json::json!(["en", "ja"]))
    .bind(now)
    .execute(&app.pool)
    .await
    .expect("seeding a full instance_settings row must succeed");

    let settings = load_instance_settings(&app.pool)
        .await
        .expect("load_instance_settings must succeed");

    assert_eq!(settings.title, "Kawasemi");
    assert_eq!(settings.description, "A single-user instance.");
    assert_eq!(settings.contact_email, "admin@kawasemi.example");
    assert_eq!(settings.contact_account_id, Some(contact_account_id));
    assert_eq!(
        settings.rules,
        vec![
            "Be excellent to each other.".to_string(),
            "No spam.".to_string()
        ]
    );
    assert!(!settings.registrations_enabled);
    assert!(settings.registrations_approval_required);
    assert_eq!(
        settings.registrations_message.as_deref(),
        Some("Approval required for new accounts.")
    );
    assert_eq!(
        settings.thumbnail.as_deref(),
        Some("https://kawasemi.example/thumbnail.png")
    );
    assert_eq!(settings.languages, vec!["en".to_string(), "ja".to_string()]);

    app.cleanup().await;
}

/// The `instance_settings_singleton` `CHECK (id = 1)` constraint means a
/// second row can never be inserted; `load_instance_settings` always reads
/// exactly the `id = 1` row when one exists, never averaging/aggregating
/// across rows.
#[tokio::test]
async fn load_instance_settings_reads_the_singleton_row_by_id() {
    let app = spawn_test_app().await;
    let now = app.runtime.clock.now();

    sqlx::query("INSERT INTO instance_settings (id, title, updated_at) VALUES (1, $1, $2)")
        .bind("Singleton Title")
        .bind(now)
        .execute(&app.pool)
        .await
        .expect("seeding must succeed");

    let attempt_second_row =
        sqlx::query("INSERT INTO instance_settings (id, title, updated_at) VALUES (2, $1, $2)")
            .bind("Should Never Exist")
            .bind(now)
            .execute(&app.pool)
            .await;
    assert!(
        attempt_second_row.is_err(),
        "the instance_settings_singleton CHECK (id = 1) constraint must reject a second row"
    );

    let settings = load_instance_settings(&app.pool)
        .await
        .expect("load_instance_settings must succeed");
    assert_eq!(settings.title, "Singleton Title");

    app.cleanup().await;
}
