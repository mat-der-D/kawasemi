//! Unit tests for the Mastodon-compatible error body renderer
//! (`MastodonError` boundary, task 6.1).
//!
//! Requirements exercised:
//! - 7.1: every rendered body includes a non-empty `error` field.
//! - 7.2: `error_description` is present when there is additional
//!   caller-authored explanation, and absent (not merely `null`) when
//!   there is none.
//! - 7.3: 422/401/403/404/429 map to the Mastodon-compatible canonical
//!   `error` label table.
//! - 7.4: `mastodon_error_body` is a genuine drop-in for
//!   `AppError::into_response_with`'s extension point.
//! - 7.5: a `Server` (5xx) error's rendered body never contains anything
//!   derived from `source` — proven with a distinctive source string, the
//!   same technique `src/error/tests.rs` uses for the same guarantee on the
//!   default renderer.

use super::*;
use serde_json::Value;

/// Collects a `Response`'s JSON body into a `serde_json::Value` for
/// structural assertions (field presence/absence, not just substring
/// checks).
async fn body_json(response: Response) -> Value {
    let body = response.into_body();
    let bytes = axum::body::to_bytes(body, usize::MAX)
        .await
        .expect("test response body should be readable");
    serde_json::from_slice(&bytes).expect("test response body should be valid JSON")
}

#[test]
fn label_table_covers_requirement_7_3_categories() {
    assert_eq!(
        mastodon_error_label(StatusCode::UNPROCESSABLE_ENTITY),
        Some("Validation failed")
    );
    assert_eq!(
        mastodon_error_label(StatusCode::UNAUTHORIZED),
        Some("The access token is invalid")
    );
    assert_eq!(
        mastodon_error_label(StatusCode::FORBIDDEN),
        Some("This action is outside the authorized scopes")
    );
    assert_eq!(
        mastodon_error_label(StatusCode::NOT_FOUND),
        Some("Record not found")
    );
    assert_eq!(
        mastodon_error_label(StatusCode::TOO_MANY_REQUESTS),
        Some("Too many requests")
    );
}

#[test]
fn label_table_returns_none_outside_the_curated_categories() {
    assert_eq!(mastodon_error_label(StatusCode::BAD_REQUEST), None);
    assert_eq!(mastodon_error_label(StatusCode::CONFLICT), None);
    assert_eq!(
        mastodon_error_label(StatusCode::INTERNAL_SERVER_ERROR),
        None
    );
}

#[tokio::test]
async fn mapped_status_uses_canonical_label_with_public_message_as_description() {
    let cases = [
        (StatusCode::UNPROCESSABLE_ENTITY, "Validation failed"),
        (StatusCode::UNAUTHORIZED, "The access token is invalid"),
        (
            StatusCode::FORBIDDEN,
            "This action is outside the authorized scopes",
        ),
        (StatusCode::NOT_FOUND, "Record not found"),
        (StatusCode::TOO_MANY_REQUESTS, "Too many requests"),
    ];

    for (status, expected_label) in cases {
        let err = AppError::client(status, "specific caller-authored detail");
        let response = mastodon_error_body(&err);
        assert_eq!(response.status(), status);

        let json = body_json(response).await;
        assert_eq!(
            json["error"], expected_label,
            "status {status} should render canonical label"
        );
        assert_eq!(
            json["error_description"], "specific caller-authored detail",
            "status {status} should surface public_message as error_description"
        );
    }
}

#[tokio::test]
async fn unmapped_client_status_falls_back_to_public_message_with_no_description() {
    let err = AppError::client(StatusCode::BAD_REQUEST, "malformed request body");

    let response = mastodon_error_body(&err);
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let json = body_json(response).await;
    assert_eq!(json["error"], "malformed request body");
    assert!(
        json.get("error_description").is_none(),
        "error_description key should be entirely absent, not null, got: {json:?}"
    );
}

#[tokio::test]
async fn empty_public_message_yields_no_description_even_for_mapped_status() {
    let err = AppError::client(StatusCode::NOT_FOUND, "");

    let json = body_json(mastodon_error_body(&err)).await;
    assert_eq!(json["error"], "Record not found");
    assert!(
        json.get("error_description").is_none(),
        "an empty public_message is not additional explanation, got: {json:?}"
    );
}

#[tokio::test]
async fn server_error_body_never_leaks_source_detail() {
    // Same technique as src/error/tests.rs's
    // server_error_body_never_contains_source_detail: a source whose
    // Display output is unmistakably distinctive, so a leak would clearly
    // fail this assertion rather than coincidentally match generic wording.
    let source_text = "super-secret-connection-string-leaked-detail-9182";
    let source = std::io::Error::other(source_text);
    let err = AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source);

    let response = mastodon_error_body(&err);
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

    let json = body_json(response).await;
    let rendered = json.to_string();
    assert!(
        !rendered.contains(source_text),
        "5xx body must not leak internal source detail, got: {rendered}"
    );
    assert_eq!(json["error"], GENERIC_SERVER_MESSAGE);
    assert!(
        json.get("error_description").is_none(),
        "5xx body should never carry error_description, got: {json:?}"
    );
}

#[tokio::test]
async fn server_error_body_identical_regardless_of_source_content() {
    let err_a = AppError::server(StatusCode::BAD_GATEWAY, std::io::Error::other("failure A"));
    let err_b = AppError::server(
        StatusCode::BAD_GATEWAY,
        std::io::Error::other("wildly different failure B, much longer than A"),
    );

    let json_a = body_json(mastodon_error_body(&err_a)).await;
    let json_b = body_json(mastodon_error_body(&err_b)).await;

    assert_eq!(json_a, json_b, "5xx body must not vary with source content");
}

#[tokio::test]
async fn wires_through_app_error_into_response_with_extension_point() {
    // Requirement 7.4: mastodon_error_body must be usable as a genuine
    // drop-in for AppError::into_response_with, not just callable on its
    // own — proving it matches the FnOnce(&AppError) -> Response contract
    // core-runtime's extension point (Requirement 6.5) actually exposes.
    let err = AppError::client(StatusCode::NOT_FOUND, "post not found");

    let response = err.into_response_with(mastodon_error_body);
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let json = body_json(response).await;
    assert_eq!(json["error"], "Record not found");
    assert_eq!(json["error_description"], "post not found");
}

#[tokio::test]
async fn into_response_with_still_logs_server_errors_through_mastodon_renderer() {
    // Mirrors src/error/tests.rs's
    // into_response_with_still_logs_server_errors: even when the Mastodon
    // renderer overrides the body, AppError's 5xx logging must still fire
    // unconditionally (there is no separate opt-in). This can't observe
    // the tracing subscriber directly without installing a global one
    // (out of this module's boundary), so it only proves the call path
    // executes end-to-end and returns the Mastodon-shaped body.
    let err = AppError::server(
        StatusCode::SERVICE_UNAVAILABLE,
        std::io::Error::other("db pool exhausted"),
    );

    let response = err.into_response_with(mastodon_error_body);
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

    let json = body_json(response).await;
    assert_eq!(json["error"], GENERIC_SERVER_MESSAGE);
}
