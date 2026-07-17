//! Unit tests for `outbox.rs`'s pure helper functions (Requirements 8.2,
//! 9.4; task 5.2, `Boundary: outbox`).
//!
//! [`outbox_get`] itself needs a real [`ActivityPubDocumentBuilder`] wired
//! to an `OutboxSourceRegistry` -- that full end-to-end proof, including
//! this task's own "outbox がページで返り...outbox は空コレクションになる"
//! completion condition, lives in `tests/ap_get_outbox_it.rs`. This module
//! only proves the pure logic that does not need any of that:
//! [`cursor_from_query`]'s query-to-`PageCursor` mapping (the documented
//! inverse of `document.rs`'s private `page_url`) and
//! [`require_ap_accept`]'s content-negotiation judgment.

use axum::http::{HeaderMap, HeaderValue, StatusCode, header};

use super::*;

#[test]
fn cursor_from_query_with_no_page_param_starts_at_the_head() {
    assert_eq!(cursor_from_query(None), PageCursor::start());
}

#[test]
fn cursor_from_query_with_page_true_starts_at_the_head() {
    assert_eq!(
        cursor_from_query(Some("true".to_string())),
        PageCursor::start()
    );
}

#[test]
fn cursor_from_query_with_an_opaque_token_passes_it_through_untouched() {
    assert_eq!(
        cursor_from_query(Some("abc123".to_string())),
        PageCursor::token("abc123")
    );
}

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
fn require_ap_accept_rejects_a_non_ap_accept_header_with_406() {
    let headers = headers_with_accept("text/html");
    let err = require_ap_accept(&headers).expect_err("text/html must not be accepted as AP");
    assert_eq!(err.status, StatusCode::NOT_ACCEPTABLE);
}

#[test]
fn not_found_returns_404() {
    assert_eq!(not_found().status, StatusCode::NOT_FOUND);
}
