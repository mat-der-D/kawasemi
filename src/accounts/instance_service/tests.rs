//! Integration tests for `InstanceService` (task 5.5), per this task's own
//! observable completion condition: "運用設定が反映された Instance(v2) を返す
//! サービステストが green".
//!
//! Mirrors `src/accounts/settings_repository/tests.rs`'s established
//! convention: `spawn_test_app` for an isolated, already-migrated schema,
//! seeding `instance_settings` directly with raw `sqlx::query` `INSERT`s
//! (this crate never seeds that table itself — see
//! `settings_repository.rs`'s own doc comment, "Read-only, by
//! construction"). Also mirrors `src/accounts/account_service/tests.rs`'s
//! own `media_config` fixture convention: a small, explicit `MediaConfig`
//! built here rather than reused from `test_harness.rs`'s own (private)
//! default, so `configuration.media_attachments`' expected values are
//! visible at the assertion site.

use std::path::PathBuf;

use super::InstanceService;
use crate::accounts::instance_serializer::{InstanceSerializer, ServerCapabilities};
use crate::config::MediaConfig;
use crate::domain::Id;
use crate::test_harness::spawn_test_app;

/// A `MediaConfig` fixture with distinct, non-default-looking upload
/// constraints, so `configuration.media_attachments` in the assertions below
/// can only pass if it actually reflects *this* config, not some other
/// hard-coded value (Requirement 8.4).
fn media_config() -> MediaConfig {
    MediaConfig {
        storage_root: PathBuf::from("/nonexistent-kawasemi-instance-service-test-root"),
        max_upload_size_bytes: 7 * 1024 * 1024,
        thumbnail_target_width: 400,
        thumbnail_target_height: 400,
        supported_formats: vec!["image/png".to_string(), "image/webp".to_string()],
        worker_concurrency: 1,
        max_retry_attempts: 5,
        lease_duration: std::time::Duration::from_secs(5 * 60),
    }
}

fn service(pool: sqlx::PgPool) -> InstanceService {
    InstanceService::new(
        pool,
        InstanceSerializer::new("kawasemi.example"),
        ServerCapabilities::from_media_config(&media_config()),
    )
}

/// Requirement 8.2: operator-configured `instance_settings` values (title,
/// description, contact, rules, registrations, thumbnail, languages) are
/// reflected verbatim in the Instance(v2) JSON the service returns.
#[tokio::test]
async fn instance_v2_reflects_operator_configured_settings() {
    let app = spawn_test_app().await;
    let now = app.runtime.clock.now();
    let contact_account_id = Id::from_i64(99);

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
    .bind("A single-owner Mastodon-compatible server.")
    .bind("owner@kawasemi.example")
    .bind(contact_account_id.as_i64())
    .bind(serde_json::json!([
        "Be excellent to each other.",
        "No spam."
    ]))
    .bind(true)
    .bind(true)
    .bind("Applications reviewed manually.")
    .bind("https://kawasemi.example/thumbnail.png")
    .bind(serde_json::json!(["en", "ja"]))
    .bind(now)
    .execute(&app.pool)
    .await
    .expect("seeding a full instance_settings row must succeed");

    let svc = service(app.pool.clone());
    let json = svc
        .instance_v2()
        .await
        .expect("instance_v2 must succeed with a seeded settings row");

    assert_eq!(json["domain"], serde_json::json!("kawasemi.example"));
    assert_eq!(json["title"], serde_json::json!("Kawasemi"));
    assert_eq!(
        json["description"],
        serde_json::json!("A single-owner Mastodon-compatible server.")
    );
    assert_eq!(
        json["contact"]["email"],
        serde_json::json!("owner@kawasemi.example")
    );
    assert_eq!(
        json["contact"]["account_id"],
        serde_json::json!(contact_account_id.as_i64().to_string())
    );
    assert_eq!(
        json["rules"],
        serde_json::json!([
            {"id": "1", "text": "Be excellent to each other."},
            {"id": "2", "text": "No spam."},
        ])
    );
    assert_eq!(json["registrations"]["enabled"], serde_json::json!(true));
    assert_eq!(
        json["registrations"]["approval_required"],
        serde_json::json!(true)
    );
    assert_eq!(
        json["registrations"]["message"],
        serde_json::json!("Applications reviewed manually.")
    );
    assert_eq!(
        json["thumbnail"],
        serde_json::json!("https://kawasemi.example/thumbnail.png")
    );
    assert_eq!(json["languages"], serde_json::json!(["en", "ja"]));

    app.cleanup().await;
}

/// Requirement 8.3: with no `instance_settings` row present at all (this
/// crate never seeds one — `settings_repository.rs`'s own "Read-only, by
/// construction" doc comment), the service must still return a fully
/// populated, valid Instance(v2) JSON — every field present with its safe
/// default, proven end to end through `InstanceService`, not just through
/// the repository/serializer in isolation.
#[tokio::test]
async fn instance_v2_with_no_settings_row_returns_a_fully_defaulted_instance() {
    let app = spawn_test_app().await;

    let row_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM instance_settings")
        .fetch_one(&app.pool)
        .await
        .expect("counting rows must succeed");
    assert_eq!(
        row_count.0, 0,
        "the test database must start with no instance_settings row"
    );

    let svc = service(app.pool.clone());
    let json = svc
        .instance_v2()
        .await
        .expect("instance_v2 must succeed even when no instance_settings row exists");

    for field in [
        "domain",
        "title",
        "version",
        "source_url",
        "description",
        "usage",
        "thumbnail",
        "languages",
        "configuration",
        "registrations",
        "contact",
        "rules",
    ] {
        assert!(
            json.get(field).is_some(),
            "missing Instance(v2) field: {field}"
        );
    }

    assert_eq!(json["domain"], serde_json::json!("kawasemi.example"));
    assert_eq!(json["title"], serde_json::json!(""));
    assert_eq!(json["description"], serde_json::json!(""));
    assert_eq!(json["thumbnail"], serde_json::json!(null));
    assert_eq!(json["languages"], serde_json::json!([]));
    assert_eq!(json["rules"], serde_json::json!([]));
    assert_eq!(json["registrations"]["enabled"], serde_json::json!(false));
    assert_eq!(json["contact"]["email"], serde_json::json!(""));
    assert_eq!(json["contact"]["account_id"], serde_json::json!(null));
    assert_eq!(
        json["version"],
        serde_json::json!(env!("CARGO_PKG_VERSION"))
    );

    app.cleanup().await;
}

/// Requirement 8.4: `configuration` must align with this server's actual
/// media-pipeline constraints (`MediaConfig`), not an independently invented
/// number — proven by using a `MediaConfig` with distinct, non-default
/// values and asserting the JSON echoes exactly those.
#[tokio::test]
async fn instance_v2_configuration_reflects_the_real_media_config_limits() {
    let app = spawn_test_app().await;

    let svc = service(app.pool.clone());
    let json = svc.instance_v2().await.expect("instance_v2 must succeed");

    let config = media_config();
    assert_eq!(
        json["configuration"]["media_attachments"]["supported_mime_types"],
        serde_json::json!(config.supported_formats)
    );
    assert_eq!(
        json["configuration"]["media_attachments"]["image_size_limit"],
        serde_json::json!(config.max_upload_size_bytes)
    );

    app.cleanup().await;
}
