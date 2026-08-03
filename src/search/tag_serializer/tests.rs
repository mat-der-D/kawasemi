//! Unit tests for [`super::TagSerializer`] (task 4.1 completion definition:
//! "空種別が `[]` になり...ゴールデンが決定的に再現される契約テストが通る";
//! Requirements 1.3, 1.4, 1.5).

use super::TagSerializer;
use crate::search::model::{TagHistoryEntry, TagView};

fn serializer() -> TagSerializer {
    // "kawasemi.example" is this crate's established test-domain literal
    // (`AccountSerializer`'s/`NotificationService`'s own unit tests).
    TagSerializer::new("kawasemi.example")
}

fn tag_without_history() -> TagView {
    TagView {
        name: "rustlang".to_string(),
        url: "/tags/rustlang".to_string(),
        history: Vec::new(),
    }
}

fn tag_with_history() -> TagView {
    TagView {
        name: "fediverse".to_string(),
        url: "/tags/fediverse".to_string(),
        history: vec![
            TagHistoryEntry {
                day: "1700000000".to_string(),
                uses: "12".to_string(),
                accounts: "5".to_string(),
            },
            TagHistoryEntry {
                day: "1699913600".to_string(),
                uses: "3".to_string(),
                accounts: "2".to_string(),
            },
        ],
    }
}

/// Requirement 1.3: the Tag JSON contract carries `name`/`url`/`history`.
#[test]
fn builds_name_url_and_history_fields() {
    let json = serializer().build_tag(&tag_with_history());
    assert_eq!(json["name"], "fediverse");
    assert_eq!(json["url"], "https://kawasemi.example/tags/fediverse");
    assert_eq!(json["history"][0]["day"], "1700000000");
    assert_eq!(json["history"][0]["uses"], "12");
    assert_eq!(json["history"][0]["accounts"], "5");
    assert_eq!(json["history"][1]["day"], "1699913600");
}

/// `TagView::url`'s domain-relative `/tags/{name}` path (see
/// `hashtag_repository.rs`'s own doc comment) is made absolute using this
/// serializer's own configured domain, not re-derived from `tag.name`.
#[test]
fn url_is_absolute_and_built_from_the_configured_domain() {
    let json = serializer().build_tag(&tag_without_history());
    assert_eq!(json["url"], "https://kawasemi.example/tags/rustlang");
}

/// Requirement 1.4's array-not-null discipline, applied to `history`: an
/// empty `TagView::history` renders as `[]`, never `null`.
#[test]
fn empty_history_renders_as_empty_array_not_null() {
    let json = serializer().build_tag(&tag_without_history());
    assert!(json["history"].is_array());
    assert_eq!(json["history"].as_array().unwrap().len(), 0);
    assert_ne!(json["history"], serde_json::Value::Null);
}

/// The same `TagView` fed to two independently-constructed serializers
/// (same `domain`) produces byte-identical JSON — no hidden non-determinism
/// (Requirement 1.5's "決定的な...再現可能").
#[test]
fn same_input_reproduces_identical_json_across_independent_serializer_instances() {
    let a = TagSerializer::new("kawasemi.example").build_tag(&tag_with_history());
    let b = TagSerializer::new("kawasemi.example").build_tag(&tag_with_history());
    assert_eq!(a, b);
}

// ---- Requirement 1.5: contract-harness golden registration ----
//
// Registers Tag JSON goldens via `crate::contract::assert_golden`, mirroring
// `src/notifications/serializer/tests.rs`'s/`src/accounts/serializer/
// tests.rs`'s identical precedent: a pure serializer has nothing
// non-deterministic upstream to inject a `RuntimeContext` boundary for --
// literal `TagView` fixtures already satisfy Requirement 1.5's "決定的に再
// 現可能".

#[test]
fn tag_with_history_json_matches_the_registered_contract_golden() {
    let json = serializer().build_tag(&tag_with_history());
    crate::contract::assert_golden("tests/golden/search/tag_with_history.json", &json);
}

#[test]
fn tag_without_history_json_matches_the_registered_contract_golden() {
    let json = serializer().build_tag(&tag_without_history());
    crate::contract::assert_golden("tests/golden/search/tag_without_history.json", &json);
}
