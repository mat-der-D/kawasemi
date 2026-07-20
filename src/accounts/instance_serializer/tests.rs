//! Unit tests for `InstanceSerializer` (task 3.3, Requirements 8.1, 8.2, 8.3,
//! 8.4), per this task's observable completion condition: "運用設定値が反映
//! され、未設定項目が既定で埋まり、`version`/`source_url`/
//! `usage.users.active_month` が決定的に再現され、`configuration` が実制約と
//! 整合する単体テストが green".

use serde_json::json;

use super::*;
use crate::domain::Id;

/// The same "fresh database, zero rows" default shape
/// `InstanceSettingsRepository::load_instance_settings` (task 2.4) always
/// returns, replicated here rather than imported (`settings_repository.rs`'s
/// `default_instance_settings` is a private helper, not part of this task's
/// boundary to expose) — see this task's own observable completion
/// condition, "未設定項目が既定で埋まり".
fn empty_settings() -> InstanceSettings {
    InstanceSettings {
        title: String::new(),
        description: String::new(),
        contact_email: String::new(),
        contact_account_id: None,
        rules: Vec::new(),
        registrations_enabled: false,
        registrations_approval_required: false,
        registrations_message: None,
        thumbnail: None,
        languages: Vec::new(),
    }
}

/// Every operational field populated with a distinct, non-default value, to
/// prove the mapping is field-by-field rather than a hard-coded default.
fn full_settings() -> InstanceSettings {
    InstanceSettings {
        title: "Kawasemi Test Instance".to_string(),
        description: "A single-owner Mastodon-compatible server.".to_string(),
        contact_email: "owner@kawasemi.example".to_string(),
        contact_account_id: Some(Id::from_i64(7)),
        rules: vec![
            "Be excellent to each other.".to_string(),
            "No spam.".to_string(),
        ],
        registrations_enabled: true,
        registrations_approval_required: true,
        registrations_message: Some("Applications reviewed manually.".to_string()),
        thumbnail: Some("https://kawasemi.example/thumbnail.png".to_string()),
        languages: vec!["en".to_string(), "ja".to_string()],
    }
}

/// The real default `MediaConfig` upload constraints
/// (`crate::config::default_media_supported_formats`/
/// `DEFAULT_MEDIA_MAX_UPLOAD_SIZE_BYTES`) mirrored here as a
/// `ServerCapabilities`, matching this task's "align with the server's real
/// constraints" requirement (8.4).
fn real_default_caps() -> ServerCapabilities {
    ServerCapabilities::new(
        vec![
            "image/jpeg".to_string(),
            "image/png".to_string(),
            "image/gif".to_string(),
            "image/webp".to_string(),
        ],
        10 * 1024 * 1024,
    )
}

#[test]
fn operational_settings_are_reflected_in_the_instance_v2_json() {
    let settings = full_settings();
    let caps = real_default_caps();

    let json = to_instance_json("kawasemi.example", &settings, &caps);

    assert_eq!(json.title, "Kawasemi Test Instance");
    assert_eq!(
        json.description,
        "A single-owner Mastodon-compatible server."
    );
    assert_eq!(json.contact.email, "owner@kawasemi.example");
    assert_eq!(json.contact.account_id, Some(Id::from_i64(7)));
    assert_eq!(
        json.rules,
        vec![
            RuleJson {
                id: "1".to_string(),
                text: "Be excellent to each other.".to_string(),
            },
            RuleJson {
                id: "2".to_string(),
                text: "No spam.".to_string(),
            },
        ]
    );
    assert!(json.registrations.enabled);
    assert!(json.registrations.approval_required);
    assert_eq!(
        json.registrations.message,
        Some("Applications reviewed manually.".to_string())
    );
    assert_eq!(
        json.thumbnail,
        Some("https://kawasemi.example/thumbnail.png".to_string())
    );
    assert_eq!(json.languages, vec!["en".to_string(), "ja".to_string()]);
}

#[test]
fn unset_operational_settings_are_filled_with_safe_defaults() {
    // The task's own observable completion condition: "未設定項目が既定で
    // 埋まり" — driven by an all-default InstanceSettings (the exact shape
    // task 2.4's repository returns on a fresh database).
    let settings = empty_settings();
    let caps = real_default_caps();

    let json = to_instance_json("kawasemi.example", &settings, &caps);

    assert_eq!(json.title, "");
    assert_eq!(json.description, "");
    assert_eq!(json.contact.email, "");
    assert_eq!(json.contact.account_id, None);
    assert!(json.rules.is_empty());
    assert!(!json.registrations.enabled);
    assert!(!json.registrations.approval_required);
    assert_eq!(json.registrations.message, None);
    assert_eq!(
        json.thumbnail, None,
        "thumbnail defaults to null, never omitted"
    );
    assert!(
        json.languages.is_empty(),
        "languages defaults to [], never omitted"
    );
}

#[test]
fn version_is_the_build_time_cargo_package_version() {
    let settings = empty_settings();
    let caps = real_default_caps();

    let json = to_instance_json("kawasemi.example", &settings, &caps);

    assert_eq!(json.version, env!("CARGO_PKG_VERSION"));
}

#[test]
fn source_url_is_a_fixed_deterministic_constant() {
    let settings = empty_settings();
    let caps = real_default_caps();

    let first = to_instance_json("kawasemi.example", &settings, &caps);
    let second = to_instance_json("kawasemi.example", &settings, &caps);

    assert_eq!(first.source_url, second.source_url);
    assert!(first.source_url.starts_with("https://"));
    assert!(!first.source_url.is_empty());
}

#[test]
fn usage_users_active_month_is_a_fixed_deterministic_placeholder() {
    let settings = empty_settings();
    let caps = real_default_caps();

    let first = to_instance_json("kawasemi.example", &settings, &caps);
    let second = to_instance_json("kawasemi.example", &settings, &caps);

    assert_eq!(
        first.usage.users.active_month,
        second.usage.users.active_month
    );
    assert_eq!(
        first.usage.users.active_month,
        ACTIVE_MONTH_USERS_PLACEHOLDER
    );
}

#[test]
fn configuration_media_attachments_aligns_with_server_capabilities() {
    // Requirement 8.4: configuration must align with this server's actual
    // constraints, not an independently invented number.
    let settings = empty_settings();
    let caps = ServerCapabilities::new(
        vec!["image/png".to_string(), "image/webp".to_string()],
        5 * 1024 * 1024,
    );

    let json = to_instance_json("kawasemi.example", &settings, &caps);

    assert_eq!(
        json.configuration.media_attachments.supported_mime_types,
        vec!["image/png".to_string(), "image/webp".to_string()]
    );
    assert_eq!(
        json.configuration.media_attachments.image_size_limit,
        5 * 1024 * 1024
    );
}

#[test]
fn server_capabilities_from_media_config_mirrors_the_real_media_config() {
    let media_config = MediaConfig {
        storage_root: std::path::PathBuf::from("media_storage"),
        max_upload_size_bytes: 10 * 1024 * 1024,
        thumbnail_target_width: 400,
        thumbnail_target_height: 400,
        supported_formats: vec![
            "image/jpeg".to_string(),
            "image/png".to_string(),
            "image/gif".to_string(),
            "image/webp".to_string(),
        ],
        worker_concurrency: 2,
        max_retry_attempts: 5,
        lease_duration: std::time::Duration::from_secs(5 * 60),
    };

    let caps = ServerCapabilities::from_media_config(&media_config);

    assert_eq!(caps.media_image_size_limit, 10 * 1024 * 1024);
    assert_eq!(
        caps.media_supported_mime_types,
        vec![
            "image/jpeg".to_string(),
            "image/png".to_string(),
            "image/gif".to_string(),
            "image/webp".to_string(),
        ]
    );
}

#[test]
fn same_input_produces_the_same_json_deterministically() {
    let settings = full_settings();
    let caps = real_default_caps();

    let first = instance_to_json("kawasemi.example", &settings, &caps);
    let second = instance_to_json("kawasemi.example", &settings, &caps);

    assert_eq!(first, second);
}

#[test]
fn every_requirement_8_1_field_is_present() {
    let settings = full_settings();
    let caps = real_default_caps();

    let json = instance_to_json("kawasemi.example", &settings, &caps);
    let obj = json.as_object().expect("instance v2 JSON is an object");

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
        assert!(obj.contains_key(field), "missing field: {field}");
    }

    assert_eq!(obj["domain"], json!("kawasemi.example"));
    assert!(obj["usage"]["users"]["active_month"].is_i64());
    assert!(obj["configuration"]["media_attachments"]["supported_mime_types"].is_array());
    assert!(obj["configuration"]["media_attachments"]["image_size_limit"].is_u64());
}

#[test]
fn domain_comes_from_the_serializer_not_from_instance_settings() {
    let settings = full_settings();
    let caps = real_default_caps();

    let json = to_instance_json("other.example", &settings, &caps);

    assert_eq!(json.domain, "other.example");
}

#[test]
fn build_instance_v2_on_the_serializer_matches_the_free_function() {
    let settings = full_settings();
    let caps = real_default_caps();
    let serializer = InstanceSerializer::new("kawasemi.example");

    assert_eq!(
        serializer.build_instance_v2(&settings, &caps),
        instance_to_json("kawasemi.example", &settings, &caps)
    );
}
