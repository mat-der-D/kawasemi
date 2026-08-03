//! Unit tests for the `nodeinfo` handlers (Requirements 5.1, 5.2, 5.3, task
//! 5.1, `Boundary: nodeinfo`).
//!
//! Pure in-memory logic -- no DB, no HTTP; plain `#[tokio::test]`s calling
//! the handlers directly (mirrors `webfinger/tests.rs`'s "not wired into a
//! router yet" testing convention).

use axum::extract::{Path, State};
use axum::http::StatusCode;

use super::*;

fn test_state() -> NodeInfoState {
    NodeInfoState {
        domain: "kawasemi.nodeinfo-test.internal".to_string(),
    }
}

/// Requirement 5.1: the discovery document links to this instance's
/// NodeInfo 2.0 document, at an absolute `https://{domain}/nodeinfo/2.0`
/// URL.
#[tokio::test]
async fn nodeinfo_discovery_links_to_the_schema_2_0_document() {
    let body = nodeinfo_discovery(State(test_state())).await.0;

    let links = body["links"].as_array().expect("links must be an array");
    assert_eq!(links.len(), 1);
    assert_eq!(
        links[0]["rel"],
        serde_json::Value::String(NODEINFO_SCHEMA_NAMESPACE.to_string())
    );
    assert_eq!(
        links[0]["href"],
        serde_json::Value::String(
            "https://kawasemi.nodeinfo-test.internal/nodeinfo/2.0".to_string()
        )
    );
}

/// Requirements 5.2, 5.3: the NodeInfo 2.0 document reports this software's
/// name/version and ActivityPub support, and carries no other field (no
/// owner information, no internal counts/config).
#[tokio::test]
async fn nodeinfo_document_reports_minimal_public_stats_only() {
    let body = nodeinfo_document(State(test_state()), Path("2.0".to_string()))
        .await
        .expect("version 2.0 must be supported")
        .0;

    assert_eq!(
        body["version"],
        serde_json::Value::String("2.0".to_string())
    );
    assert_eq!(
        body["software"]["name"],
        serde_json::Value::String("kawasemi".to_string())
    );
    assert_eq!(
        body["software"]["version"],
        serde_json::Value::String(env!("CARGO_PKG_VERSION").to_string())
    );
    assert_eq!(
        body["protocols"],
        serde_json::json!(["activitypub"]),
        "protocols must report exactly ActivityPub support"
    );

    let object = body.as_object().expect("document must be a JSON object");
    let allowed_keys = ["version", "software", "protocols"];
    for key in object.keys() {
        assert!(
            allowed_keys.contains(&key.as_str()),
            "nodeinfo document must not include any field beyond the minimal public stats \
             (Requirement 5.3); unexpected field {key:?}"
        );
    }
}

/// design.md's API Contract (`GET /nodeinfo/{ver}` -> `404`): an
/// unsupported version is not found, not silently downgraded/upgraded.
#[tokio::test]
async fn nodeinfo_document_rejects_an_unsupported_version_with_not_found() {
    let err = nodeinfo_document(State(test_state()), Path("1.0".to_string()))
        .await
        .expect_err("an unsupported nodeinfo version must be rejected");

    assert_eq!(err.status, StatusCode::NOT_FOUND);
}
