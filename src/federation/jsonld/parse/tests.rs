use axum::http::StatusCode;
use serde_json::json;

use super::*;
use crate::error::ErrorKind;

#[test]
fn parses_an_activity_with_the_required_properties() {
    let body = json!({
        "type": "Create",
        "id": "https://example.test/activities/1",
        "actor": "https://example.test/actors/alice",
    })
    .to_string();

    let parsed = parse_activity(body.as_bytes()).expect("valid activity must parse");

    assert_eq!(parsed.id, "https://example.test/activities/1");
    assert_eq!(parsed.activity_type, "Create");
    assert_eq!(
        parsed.raw.get("actor").and_then(Value::as_str),
        Some("https://example.test/actors/alice")
    );
}

#[test]
fn unknown_properties_do_not_fail_interpretation_and_are_preserved_on_raw() {
    let body = json!({
        "type": "Create",
        "id": "https://example.test/activities/1",
        "https://example.test#totallyUnknownExtension": {"nested": "value"},
        "anotherUnknownField": 42,
    })
    .to_string();

    let parsed = parse_activity(body.as_bytes()).expect("unknown properties must not fail parsing");

    assert_eq!(parsed.id, "https://example.test/activities/1");
    assert_eq!(parsed.activity_type, "Create");
    assert!(
        parsed
            .raw
            .get("https://example.test#totallyUnknownExtension")
            .and_then(Value::as_object)
            .is_some()
    );
    assert_eq!(
        parsed
            .raw
            .get("anotherUnknownField")
            .and_then(Value::as_i64),
        Some(42)
    );
}

#[test]
fn missing_id_is_a_validation_error() {
    let body = json!({"type": "Create"}).to_string();

    let err = parse_activity(body.as_bytes()).expect_err("missing id must be rejected");

    assert_eq!(err.kind, ErrorKind::Client);
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[test]
fn missing_type_is_a_validation_error() {
    let body = json!({"id": "https://example.test/activities/1"}).to_string();

    let err = parse_activity(body.as_bytes()).expect_err("missing type must be rejected");

    assert_eq!(err.kind, ErrorKind::Client);
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[test]
fn empty_id_is_treated_as_missing() {
    let body = json!({"type": "Create", "id": ""}).to_string();

    let err = parse_activity(body.as_bytes()).expect_err("empty id must be rejected");

    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[test]
fn empty_type_is_treated_as_missing() {
    let body = json!({"type": "", "id": "https://example.test/activities/1"}).to_string();

    let err = parse_activity(body.as_bytes()).expect_err("empty type must be rejected");

    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[test]
fn malformed_json_body_is_rejected() {
    let body: &[u8] = b"{not valid json";

    let err = parse_activity(body).expect_err("malformed JSON must be rejected");

    assert_eq!(err.status, StatusCode::BAD_REQUEST);
}

#[test]
fn non_object_top_level_is_rejected() {
    let body = json!(["Create", "https://example.test/activities/1"]).to_string();

    let err = parse_activity(body.as_bytes()).expect_err("non-object top level must be rejected");

    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
}
