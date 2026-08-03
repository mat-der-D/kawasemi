//! Unit tests for `inbox.rs`'s pure helper functions (Requirements 7.1,
//! 7.2; task 5.3, `Boundary: inbox`).
//!
//! [`actor_inbox`]/[`shared_inbox`] themselves need a real
//! `SignatureVerifier` (signature verification has no narrow mockable port
//! producing a genuinely-signed request cheaply outside a real RSA
//! keypair), a `BlockPolicy`, and a `ReceivedActivityStore` wired into a
//! real `InboxService` -- that full end-to-end proof, including every one
//! of this task's own observable completion conditions (202 accept +
//! dispatch, 401 on invalid signature, differing `LocalRecipientContext`
//! per route, duplicate ack without re-dispatch), lives in
//! `tests/inbox_it.rs`. This module only proves the pure logic that does
//! not need any of that: [`build_incoming_request`]'s body-presence mapping
//! and header/method/url pass-through, [`ack_status`]'s outcome-to-status
//! mapping, and [`invalid_handle`]'s status code.

use axum::http::{HeaderMap, HeaderValue, Method, header};

use super::*;

#[test]
fn build_incoming_request_maps_an_empty_body_to_none() {
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, HeaderValue::from_static("kawasemi.example"));

    let req = build_incoming_request(
        Method::POST,
        "https://kawasemi.example/inbox".to_string(),
        headers,
        Bytes::new(),
    );

    assert!(
        req.body.is_none(),
        "an empty extracted body must map to None, not Some(vec![]) -- see this module's doc \
         comment, \"Body handling\""
    );
}

#[test]
fn build_incoming_request_preserves_a_non_empty_body() {
    let headers = HeaderMap::new();
    let body = Bytes::from_static(b"{\"id\":\"https://remote.example/1\",\"type\":\"Follow\"}");

    let req = build_incoming_request(
        Method::POST,
        "https://kawasemi.example/inbox".to_string(),
        headers,
        body.clone(),
    );

    assert_eq!(req.body, Some(body.to_vec()));
}

#[test]
fn build_incoming_request_preserves_method_url_and_headers() {
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, HeaderValue::from_static("kawasemi.example"));
    headers.insert(header::DATE, HeaderValue::from_static("some-date"));

    let req = build_incoming_request(
        Method::POST,
        "https://kawasemi.example/users/alice/inbox".to_string(),
        headers.clone(),
        Bytes::new(),
    );

    assert_eq!(req.method, Method::POST);
    assert_eq!(req.url, "https://kawasemi.example/users/alice/inbox");
    assert_eq!(req.headers, headers);
}

#[test]
fn ack_status_maps_accepted_to_202() {
    assert_eq!(ack_status(InboxOutcome::Accepted), StatusCode::ACCEPTED);
}

#[test]
fn ack_status_maps_duplicate_to_202_as_well() {
    assert_eq!(
        ack_status(InboxOutcome::Duplicate),
        StatusCode::ACCEPTED,
        "a duplicate delivery is still a successful receipt from the caller's perspective \
         (Requirement 7.4) -- both outcomes must ack identically"
    );
}

#[test]
fn invalid_handle_returns_404() {
    assert_eq!(invalid_handle().status, StatusCode::NOT_FOUND);
}
