//! Unit tests for `endpoints.rs`'s own pure, DB/network-free wire-shape
//! helpers (this module's doc comment documents every judgment call these
//! functions implement) — the router-level, auth/scope/public-response/
//! error-shape behavior itself is proven by
//! `tests/accounts_endpoints_wiring_it.rs` (task 6's own new integration
//! test, driven through the real, `spawn_test_app`-booted router, since
//! that behavior genuinely needs a real Postgres-backed `AccountService`/
//! `InstanceService`/`CustomEmojiService` and cannot be proven by a pure
//! unit test).

use std::collections::BTreeMap;

use axum::http::StatusCode;

use super::*;

// ---- parse_loose_bool ----

#[test]
fn parse_loose_bool_accepts_true_and_1_as_true() {
    assert!(parse_loose_bool("locked", "true").unwrap());
    assert!(parse_loose_bool("locked", "1").unwrap());
}

#[test]
fn parse_loose_bool_accepts_false_and_0_as_false() {
    assert!(!parse_loose_bool("locked", "false").unwrap());
    assert!(!parse_loose_bool("locked", "0").unwrap());
}

#[test]
fn parse_loose_bool_rejects_anything_else_as_422() {
    let err = parse_loose_bool("locked", "yes").expect_err("\"yes\" must be rejected");
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(err.public_message.contains("locked"));
}

// ---- parse_visibility ----

#[test]
fn parse_visibility_accepts_every_canonical_variant() {
    assert_eq!(parse_visibility("public").unwrap(), Visibility::Public);
    assert_eq!(parse_visibility("unlisted").unwrap(), Visibility::Unlisted);
    assert_eq!(parse_visibility("private").unwrap(), Visibility::Private);
    assert_eq!(parse_visibility("direct").unwrap(), Visibility::Direct);
}

#[test]
fn parse_visibility_rejects_an_unknown_value_as_422() {
    let err = parse_visibility("bogus").expect_err("\"bogus\" must be rejected");
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
}

// ---- parse_field_attr_key ----

#[test]
fn parse_field_attr_key_parses_name_and_value_suffixes() {
    assert_eq!(
        parse_field_attr_key("fields_attributes[0][name]"),
        Some((0, FieldAttrKind::Name))
    );
    assert_eq!(
        parse_field_attr_key("fields_attributes[3][value]"),
        Some((3, FieldAttrKind::Value))
    );
}

#[test]
fn parse_field_attr_key_rejects_unrelated_or_malformed_keys() {
    assert_eq!(parse_field_attr_key("display_name"), None);
    assert_eq!(parse_field_attr_key("fields_attributes[abc][name]"), None);
    assert_eq!(parse_field_attr_key("fields_attributes[0][bogus]"), None);
    assert_eq!(parse_field_attr_key("fields_attributes[0]"), None);
}

// ---- build_fields_attributes ----

#[test]
fn build_fields_attributes_is_none_when_nothing_was_sent() {
    assert_eq!(build_fields_attributes(BTreeMap::new()), None);
}

#[test]
fn build_fields_attributes_preserves_ascending_index_order() {
    let mut fields = BTreeMap::new();
    fields.insert(
        1,
        (
            Some("Blog".to_string()),
            Some("https://example.blog".to_string()),
        ),
    );
    fields.insert(
        0,
        (Some("Pronouns".to_string()), Some("she/her".to_string())),
    );

    let built = build_fields_attributes(fields).expect("at least one entry was sent");
    assert_eq!(built.len(), 2);
    assert_eq!(built[0].name, "Pronouns");
    assert_eq!(built[1].name, "Blog");
}

#[test]
fn build_fields_attributes_defaults_a_missing_half_to_an_empty_string() {
    let mut fields = BTreeMap::new();
    fields.insert(0, (Some("Pronouns".to_string()), None));

    let built = build_fields_attributes(fields).expect("one entry was sent");
    assert_eq!(built[0].name, "Pronouns");
    assert_eq!(built[0].value, "");
}

// ---- extract_relationship_ids ----

#[test]
fn extract_relationship_ids_accepts_both_plain_and_bracketed_repeated_keys() {
    let pairs = vec![
        ("id".to_string(), "1".to_string()),
        ("id[]".to_string(), "2".to_string()),
        ("unrelated".to_string(), "ignored".to_string()),
    ];
    assert_eq!(
        extract_relationship_ids(pairs),
        vec!["1".to_string(), "2".to_string()]
    );
}

#[test]
fn extract_relationship_ids_is_empty_when_no_id_pairs_are_present() {
    let pairs = vec![("other".to_string(), "value".to_string())];
    assert!(extract_relationship_ids(pairs).is_empty());
}

// ---- parse_optional_limit ----

#[test]
fn parse_optional_limit_is_none_when_absent() {
    assert_eq!(parse_optional_limit(None).unwrap(), None);
}

#[test]
fn parse_optional_limit_parses_a_valid_decimal_value() {
    assert_eq!(parse_optional_limit(Some("40")).unwrap(), Some(40));
}

#[test]
fn parse_optional_limit_rejects_a_non_numeric_value_as_422_not_a_raw_axum_rejection() {
    let err = parse_optional_limit(Some("abc")).expect_err("\"abc\" must be rejected");
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
}

// ---- parse_optional_bool_query ----

#[test]
fn parse_optional_bool_query_defaults_to_false_when_absent() {
    assert!(!parse_optional_bool_query("pinned", None).unwrap());
}

#[test]
fn parse_optional_bool_query_parses_a_present_value() {
    assert!(parse_optional_bool_query("pinned", Some("true")).unwrap());
    assert!(!parse_optional_bool_query("only_media", Some("0")).unwrap());
}
