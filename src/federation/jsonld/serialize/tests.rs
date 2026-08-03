use axum::http::StatusCode;
use serde_json::json;

use super::*;
use crate::federation::jsonld::ACTIVITYSTREAMS_CONTEXT;

#[test]
fn serialized_output_contains_the_activitystreams_context() {
    let doc = json!({"type": "Note", "id": "https://example.test/notes/1"});

    let bytes = serialize(&doc).expect("valid object doc must serialize");
    let value: Value = serde_json::from_slice(&bytes).expect("output must be valid JSON");

    assert_eq!(
        value.get("@context").and_then(Value::as_str),
        Some(ACTIVITYSTREAMS_CONTEXT)
    );
    assert_eq!(value.get("type").and_then(Value::as_str), Some("Note"));
    assert_eq!(
        value.get("id").and_then(Value::as_str),
        Some("https://example.test/notes/1")
    );
}

#[test]
fn preserves_unknown_properties_through_serialization() {
    let doc = json!({
        "type": "Note",
        "id": "https://example.test/notes/1",
        "https://example.test#customProperty": "custom value",
    });

    let bytes = serialize(&doc).expect("valid object doc must serialize");
    let value: Value = serde_json::from_slice(&bytes).expect("output must be valid JSON");

    assert_eq!(
        value
            .get("https://example.test#customProperty")
            .and_then(Value::as_str),
        Some("custom value")
    );
}

#[test]
fn propagates_the_context_injection_error_for_a_non_object_document() {
    let doc = json!("just a string, not an object");

    let err = serialize(&doc).expect_err("non-object doc must fail to serialize");

    assert_eq!(err.status, StatusCode::BAD_REQUEST);
}
