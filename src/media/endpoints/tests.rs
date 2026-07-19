//! Pure-function unit tests for `endpoints.rs`'s small wire-format helpers
//! (Requirements 3.2, 2.3, 7.4) — [`parse_focus_param`]/[`parse_media_id`].
//! Full HTTP-level behavior (auth/scope/status-code/body coverage for
//! `upload_media`/`show_media`/`update_media`) lives in
//! `tests/media_endpoints_it.rs` (a real, `spawn_test_app`-backed
//! integration test, mirroring this crate's established precedent for a
//! not-yet-router-wired endpoint task — see this module's own doc comment).

use super::*;

#[test]
fn parse_focus_param_accepts_a_well_formed_pair() {
    let (x, y) = parse_focus_param("-0.5,0.3").expect("well-formed pair must parse");
    assert_eq!(x, -0.5);
    assert_eq!(y, 0.3);
}

#[test]
fn parse_focus_param_accepts_surrounding_whitespace_around_each_component() {
    let (x, y) = parse_focus_param(" 0.25 , -0.75 ").expect("whitespace must be tolerated");
    assert_eq!(x, 0.25);
    assert_eq!(y, -0.75);
}

#[test]
fn parse_focus_param_rejects_a_missing_second_component() {
    let err = parse_focus_param("0.5").expect_err("single component must be rejected");
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[test]
fn parse_focus_param_rejects_a_third_component() {
    let err = parse_focus_param("0.1,0.2,0.3").expect_err("three components must be rejected");
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[test]
fn parse_focus_param_rejects_non_numeric_components() {
    let err = parse_focus_param("abc,def").expect_err("non-numeric components must be rejected");
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[test]
fn parse_focus_param_rejects_an_empty_string() {
    let err = parse_focus_param("").expect_err("empty string must be rejected");
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[test]
fn parse_focus_param_does_not_itself_enforce_the_range_invariant() {
    // Range validation (`-1.0..=1.0`) is `MediaService`'s job via
    // `Focus::new`, not this wire-format parser's — an out-of-range but
    // otherwise well-formed pair must still parse successfully here.
    let (x, y) = parse_focus_param("5.0,-5.0").expect("out-of-range but well-formed pair parses");
    assert_eq!(x, 5.0);
    assert_eq!(y, -5.0);
}

#[test]
fn parse_media_id_accepts_a_decimal_string() {
    let id = parse_media_id("42").expect("decimal string must parse");
    assert_eq!(id, Id::from_i64(42));
}

#[test]
fn parse_media_id_rejects_a_non_decimal_string_as_not_found() {
    let err = parse_media_id("not-a-number").expect_err("non-decimal string must be rejected");
    assert_eq!(err.status, StatusCode::NOT_FOUND);
}

#[test]
fn parse_media_id_rejects_an_empty_string_as_not_found() {
    let err = parse_media_id("").expect_err("empty string must be rejected");
    assert_eq!(err.status, StatusCode::NOT_FOUND);
}
