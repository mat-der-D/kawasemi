//! Pure, instance-free unit tests for `endpoints.rs`'s wire-shape parsing
//! helpers ([`super::parse_follow_options`], [`super::parse_mute_options`]
//! and the `parse_optional_limit` it re-uses) -- the only verifications in
//! this module that need no running instance at all.
//!
//! The router-level verifications of this module's nine handlers (a real,
//! test-only axum `Router` dispatched via `tower::ServiceExt::oneshot`
//! against a real, `spawn_test_app`-backed Postgres schema; Requirements
//! 1.8, 2.7, 4.6, 5.6, 10.1, 10.2, 10.4, 10.5) now live in
//! `tests/social_graph_endpoints_it.rs`.

use super::*;

// --- pure helper unit tests ---------------------------------------------

#[test]
fn parse_follow_options_defaults_reblogs_true_on_empty_body() {
    let opts = parse_follow_options(&[]).expect("empty body must parse to defaults");
    assert!(opts.reblogs);
    assert!(!opts.notify);
    assert!(opts.languages.is_empty());
}

#[test]
fn parse_follow_options_rejects_malformed_json_as_422() {
    let err = parse_follow_options(b"not json").expect_err("malformed body must be rejected");
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[test]
fn parse_mute_options_defaults_notifications_true_on_empty_body() {
    let opts = parse_mute_options(&[]).expect("empty body must parse to defaults");
    assert!(opts.notifications);
    assert_eq!(opts.duration, None);
}

#[test]
fn parse_optional_limit_rejects_non_numeric_value_as_422() {
    let err = parse_optional_limit(Some("abc")).expect_err("non-numeric limit must be rejected");
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
}
