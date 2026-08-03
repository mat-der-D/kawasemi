//! Unit tests for `RelationshipSerializer` (task 3.2, Requirements 5.1, 5.2,
//! 5.4), per this task's observable completion condition: "既定値で全フラグ
//! false・件数 0・note 空の JSON を生成する単体テストが green".

use serde_json::json;

use super::*;
use crate::domain::Id;

/// The Requirement 5.4 "no relationship" default value — the exact shape
/// `NoRelationshipProvider` (task 1.3) produces for every target when no
/// downstream `RelationshipStateProvider` has been registered yet.
fn no_relationship(id: i64) -> RelationshipView {
    RelationshipView {
        id: Id::from_i64(id),
        following: false,
        showing_reblogs: false,
        notifying: false,
        languages: Vec::new(),
        followed_by: false,
        blocking: false,
        blocked_by: false,
        muting: false,
        muting_notifications: false,
        requested: false,
        requested_by: false,
        domain_blocking: false,
        endorsed: false,
        note: String::new(),
    }
}

/// A relationship with every flag/count/`note` populated with a distinct,
/// non-default value, to prove the mapping is field-by-field rather than a
/// hard-coded default.
fn full_relationship(id: i64) -> RelationshipView {
    RelationshipView {
        id: Id::from_i64(id),
        following: true,
        showing_reblogs: true,
        notifying: true,
        languages: vec!["en".to_string(), "ja".to_string()],
        followed_by: true,
        blocking: true,
        blocked_by: true,
        muting: true,
        muting_notifications: true,
        requested: true,
        requested_by: true,
        domain_blocking: true,
        endorsed: true,
        note: "a private note about this account".to_string(),
    }
}

#[test]
fn default_no_relationship_view_serializes_to_all_false_zero_counts_empty_note() {
    // The task's own observable completion condition, verbatim: default
    // value -> all flags false, all counts 0, note empty.
    let view = no_relationship(42);

    let json = relationship_to_json(&view);

    assert_eq!(
        json,
        json!({
            "id": "42",
            "following": false,
            "showing_reblogs": false,
            "notifying": false,
            "languages": [],
            "followed_by": false,
            "blocking": false,
            "blocked_by": false,
            "muting": false,
            "muting_notifications": false,
            "requested": false,
            "requested_by": false,
            "domain_blocking": false,
            "endorsed": false,
            "note": "",
        })
    );
}

#[test]
fn every_requirement_5_2_field_is_present_with_correct_type() {
    let view = no_relationship(7);
    let json = relationship_to_json(&view);
    let obj = json.as_object().expect("relationship JSON is an object");

    // Requirement 5.2's exact field list.
    for field in [
        "id",
        "following",
        "showing_reblogs",
        "notifying",
        "languages",
        "followed_by",
        "blocking",
        "blocked_by",
        "muting",
        "muting_notifications",
        "requested",
        "requested_by",
        "domain_blocking",
        "endorsed",
        "note",
    ] {
        assert!(obj.contains_key(field), "missing field: {field}");
    }

    assert!(obj["id"].is_string(), "id must serialize as a string");
    assert!(obj["languages"].is_array());
    assert!(obj["note"].is_string());
    for flag in [
        "following",
        "showing_reblogs",
        "notifying",
        "followed_by",
        "blocking",
        "blocked_by",
        "muting",
        "muting_notifications",
        "requested",
        "requested_by",
        "domain_blocking",
        "endorsed",
    ] {
        assert!(obj[flag].is_boolean(), "{flag} must serialize as a bool");
    }
}

#[test]
fn non_default_view_maps_every_field_through_unchanged() {
    // Proves the mapping is field-by-field, not a hard-coded default that
    // happens to pass the all-false test above.
    let view = full_relationship(99);

    let json = to_relationship_json(&view);

    assert_eq!(json.id, Id::from_i64(99));
    assert!(json.following);
    assert!(json.showing_reblogs);
    assert!(json.notifying);
    assert_eq!(json.languages, vec!["en".to_string(), "ja".to_string()]);
    assert!(json.followed_by);
    assert!(json.blocking);
    assert!(json.blocked_by);
    assert!(json.muting);
    assert!(json.muting_notifications);
    assert!(json.requested);
    assert!(json.requested_by);
    assert!(json.domain_blocking);
    assert!(json.endorsed);
    assert_eq!(json.note, "a private note about this account");
}

#[test]
fn same_input_produces_the_same_json_deterministically() {
    let view = full_relationship(5);

    let first = relationship_to_json(&view);
    let second = relationship_to_json(&view);

    assert_eq!(first, second);
}

// ---- Requirements 5.6, 3.5: contract-harness golden registration ----
//
// Registers Relationship's JSON shape as a golden via
// `crate::contract::assert_golden` (task 3.5), reusing `full_relationship`
// (a literal, hand-constructed fixture already used above) -- no clock/id/
// rng source is involved anywhere in this pure mapping, so the literal
// fixture already satisfies "決定的" (deterministic) reproducibility, the
// same precedent `media/serializer/tests.rs` and `accounts/serializer/
// tests.rs` (task 3.5) establish for this crate's other golden tests.

#[test]
fn full_relationship_json_matches_the_registered_contract_golden() {
    let view = full_relationship(99);

    let json = relationship_to_json(&view);

    // Requirement 5.2: every field present with the right type.
    assert!(json["id"].is_string());
    for flag in [
        "following",
        "showing_reblogs",
        "notifying",
        "followed_by",
        "blocking",
        "blocked_by",
        "muting",
        "muting_notifications",
        "requested",
        "requested_by",
        "domain_blocking",
        "endorsed",
    ] {
        assert!(json[flag].is_boolean(), "{flag} must be a bool");
    }
    assert!(json["languages"].is_array());
    assert!(json["note"].is_string());

    crate::contract::assert_golden("tests/golden/accounts/relationship.json", &json);
}

#[test]
fn build_relationship_on_the_serializer_matches_the_free_function() {
    let view = no_relationship(1);
    let serializer = RelationshipSerializer::new();

    assert_eq!(
        serializer.build_relationship(&view),
        relationship_to_json(&view)
    );
}
