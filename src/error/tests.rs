//! Unit tests for the unified `AppError` type and its `IntoResponse`
//! conversion.
//!
//! Requirements exercised:
//! - 6.2 / 6.3: converting an `AppError` yields the expected HTTP status,
//!   and a `Client` (4xx) error's body surfaces `public_message` verbatim.
//! - 6.4: a `Server` (5xx) error's body never contains anything derived
//!   from `source`'s `Display`/`Debug` output — only the generic message.
//! - 6.5: `into_response_with` lets a caller substitute a different body
//!   renderer, proving the conversion has an extension point downstream
//!   specs (e.g. api-foundation) can use for their own wire format.

use super::*;
use axum::body::to_bytes;
use axum::http::StatusCode;
use axum::response::IntoResponse;

/// Collects a `Response`'s body into a UTF-8 string for assertions.
async fn body_text(response: axum::response::Response) -> String {
    let body = response.into_body();
    let bytes = to_bytes(body, usize::MAX)
        .await
        .expect("test response body should be readable");
    String::from_utf8(bytes.to_vec()).expect("test response body should be valid UTF-8")
}

#[tokio::test]
async fn client_error_returns_status_and_public_message() {
    let err = AppError::client(StatusCode::NOT_FOUND, "post not found");

    assert_eq!(err.kind, ErrorKind::Client);
    assert_eq!(err.status, StatusCode::NOT_FOUND);

    let response = err.into_response();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let text = body_text(response).await;
    assert!(
        text.contains("post not found"),
        "4xx body should surface public_message, got: {text}"
    );
}

#[tokio::test]
async fn server_error_body_never_contains_source_detail() {
    // A source whose Display/Debug output is unmistakably distinctive, so
    // if it ever leaked into the body this assertion would clearly catch
    // it rather than coincidentally match generic wording.
    let source_text = "super-secret-connection-string-leaked-detail-4711";
    let source = std::io::Error::other(source_text);
    let err = AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source);

    assert_eq!(err.kind, ErrorKind::Server);
    assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);

    let response = err.into_response();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

    let text = body_text(response).await;
    assert!(
        !text.contains(source_text),
        "5xx body must not leak internal source detail, got: {text}"
    );
    assert!(
        text.contains(GENERIC_SERVER_MESSAGE),
        "5xx body should carry the generic server message, got: {text}"
    );
}

#[tokio::test]
async fn server_error_public_message_is_generic_regardless_of_source_content() {
    // Requirement 6.4: the body must not vary with the source's content —
    // prove this isn't just "the default source's text is filtered out" by
    // trying a second, differently-worded source and checking the body is
    // byte-for-byte identical in both cases.
    let err_a = AppError::server(StatusCode::BAD_GATEWAY, std::io::Error::other("failure A"));
    let err_b = AppError::server(
        StatusCode::BAD_GATEWAY,
        std::io::Error::other("wildly different failure B"),
    );

    let text_a = body_text(err_a.into_response()).await;
    let text_b = body_text(err_b.into_response()).await;

    assert_eq!(text_a, text_b, "5xx body must not vary with source content");
}

#[tokio::test]
async fn into_response_with_lets_caller_override_body_rendering() {
    // Requirement 6.5: downstream specs need an extension point to render
    // a different wire format (e.g. a Mastodon-compatible envelope)
    // without redefining AppError's conversion end-to-end.
    let err = AppError::client(StatusCode::UNPROCESSABLE_ENTITY, "invalid status text");

    let response = err.into_response_with(|error| {
        let custom_body = format!("custom:{}", error.public_message);
        (error.status, custom_body).into_response()
    });

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let text = body_text(response).await;
    assert_eq!(text, "custom:invalid status text");
}

#[tokio::test]
async fn into_response_with_still_logs_server_errors() {
    // Requirement 6.4: even when a downstream renderer overrides the body
    // via the extension point, the 5xx-source-gets-logged behavior must
    // still fire (there's no separate "logging" toggle to forget to wire
    // up). This test can't observe the tracing subscriber directly without
    // installing a global one (out of boundary here), so it only proves
    // the call path executes end-to-end without panicking and returns the
    // caller's custom body, exercising `log_if_server` on the `Server`
    // branch.
    let err = AppError::server(
        StatusCode::SERVICE_UNAVAILABLE,
        std::io::Error::other("db pool exhausted"),
    );

    let response =
        err.into_response_with(|error| (error.status, "custom-5xx-body").into_response());

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let text = body_text(response).await;
    assert_eq!(text, "custom-5xx-body");
}

#[test]
fn client_constructor_never_carries_a_source() {
    let err = AppError::client(StatusCode::BAD_REQUEST, "bad input");
    assert!(err.source.is_none());
}

#[test]
fn server_constructor_always_carries_the_given_source() {
    let err = AppError::server(
        StatusCode::INTERNAL_SERVER_ERROR,
        std::io::Error::other("boom"),
    );
    assert!(err.source.is_some());
}
