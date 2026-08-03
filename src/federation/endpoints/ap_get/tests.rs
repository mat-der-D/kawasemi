//! Unit tests for `ap_get.rs`'s pure helper functions (Requirements 6.3,
//! 6.6, 9.4; task 5.2, `Boundary: ap_get`).
//!
//! [`actor_get`]/[`object_get`] themselves need a real `ActorDirectory`
//! (owner-non-exposing actor resolution has no narrow mockable port
//! anywhere in this spec, mirrors `document/tests.rs`'s/`webfinger/tests.rs`'s
//! established precedent) plus a `SignatureVerifier` and the two document
//! registries wired together — that full end-to-end proof, including every
//! one of this task's own observable completion conditions, lives in
//! `tests/ap_get_outbox_it.rs`. This module only proves the pure logic that
//! does not need any of that: [`require_ap_accept`]'s content-negotiation
//! judgment and [`not_found`]'s status code.

use axum::http::{HeaderMap, HeaderValue, StatusCode, header};

use super::*;

fn headers_with_accept(value: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(header::ACCEPT, HeaderValue::from_str(value).unwrap());
    headers
}

#[test]
fn require_ap_accept_accepts_activity_json() {
    let headers = headers_with_accept("application/activity+json");
    assert!(require_ap_accept(&headers).is_ok());
}

#[test]
fn require_ap_accept_accepts_ld_json() {
    let headers = headers_with_accept("application/ld+json");
    assert!(require_ap_accept(&headers).is_ok());
}

#[test]
fn require_ap_accept_accepts_a_media_range_list_containing_activity_json() {
    let headers = headers_with_accept("text/html, application/activity+json;q=0.9");
    assert!(require_ap_accept(&headers).is_ok());
}

#[test]
fn require_ap_accept_rejects_a_non_ap_accept_header_with_406() {
    let headers = headers_with_accept("text/html");
    let err = require_ap_accept(&headers).expect_err("text/html must not be accepted as AP");
    assert_eq!(err.status, StatusCode::NOT_ACCEPTABLE);
}

#[test]
fn require_ap_accept_rejects_a_missing_accept_header() {
    let headers = HeaderMap::new();
    let err = require_ap_accept(&headers).expect_err("a missing Accept header must not pass");
    assert_eq!(err.status, StatusCode::NOT_ACCEPTABLE);
}

#[test]
fn not_found_returns_404() {
    assert_eq!(not_found().status, StatusCode::NOT_FOUND);
}
