use serde_json::json;

use super::*;
use crate::error::ErrorKind;

#[test]
fn stamps_activitystreams_context_onto_an_object_without_one() {
    let doc = json!({"type": "Note", "id": "https://example.test/notes/1"});

    let result = with_activitystreams_context(&doc).expect("object doc must accept @context");

    assert_eq!(
        result.get("@context"),
        Some(&Value::String(ACTIVITYSTREAMS_CONTEXT.to_string()))
    );
    assert_eq!(result.get("type"), Some(&Value::String("Note".to_string())));
    assert_eq!(
        result.get("id"),
        Some(&Value::String("https://example.test/notes/1".to_string()))
    );
}

#[test]
fn overwrites_an_existing_context_with_the_activitystreams_context() {
    let doc = json!({
        "@context": "https://example.test/some-other-context",
        "type": "Note",
        "id": "https://example.test/notes/1",
    });

    let result = with_activitystreams_context(&doc).expect("object doc must accept @context");

    assert_eq!(
        result.get("@context"),
        Some(&Value::String(ACTIVITYSTREAMS_CONTEXT.to_string()))
    );
}

#[test]
fn preserves_unrelated_properties() {
    let doc = json!({
        "type": "Note",
        "id": "https://example.test/notes/1",
        "https://example.test#customProperty": "custom value",
    });

    let result = with_activitystreams_context(&doc).expect("object doc must accept @context");

    assert_eq!(
        result.get("https://example.test#customProperty"),
        Some(&Value::String("custom value".to_string()))
    );
}

#[test]
fn rejects_a_non_object_document() {
    let doc = json!(["not", "an", "object"]);

    let err = with_activitystreams_context(&doc).expect_err("non-object doc must be rejected");

    assert_eq!(err.kind, ErrorKind::Client);
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
}
