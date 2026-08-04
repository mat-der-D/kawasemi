//! Unit tests for the shared query-parameter parsers.
//!
//! These assertions were previously spread across `accounts::endpoints`,
//! `timelines::endpoints`, `search::endpoint`, and `statuses::endpoints`,
//! each covering its own private copy. They live here now because there is
//! only one copy to cover — and because pinning the exact status and
//! message here is what stops a future edit from quietly changing the wire
//! contract for every endpoint at once.

use super::*;

#[test]
fn parse_optional_limit_is_none_when_absent() {
    assert_eq!(
        parse_optional_limit(None).expect("absent is not an error"),
        None
    );
}

#[test]
fn parse_optional_limit_parses_a_valid_decimal_value() {
    assert_eq!(
        parse_optional_limit(Some("40")).expect("a plain decimal parses"),
        Some(40)
    );
}

#[test]
fn parse_optional_limit_accepts_zero() {
    assert_eq!(
        parse_optional_limit(Some("0")).expect("zero is a non-negative integer"),
        Some(0)
    );
}

#[test]
fn parse_optional_limit_rejects_a_non_numeric_value_as_422() {
    let err = parse_optional_limit(Some("abc")).expect_err("a non-numeric limit is rejected");
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        err.public_message,
        "limit must be a non-negative integer, got \"abc\""
    );
}

#[test]
fn parse_optional_limit_rejects_a_negative_value_as_422() {
    let err = parse_optional_limit(Some("-1")).expect_err("a negative limit is rejected");
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[test]
fn parse_loose_bool_accepts_true_and_1_as_true() {
    assert!(parse_loose_bool("pinned", "true").expect("\"true\" parses"));
    assert!(parse_loose_bool("pinned", "1").expect("\"1\" parses"));
}

#[test]
fn parse_loose_bool_accepts_false_and_0_as_false() {
    assert!(!parse_loose_bool("pinned", "false").expect("\"false\" parses"));
    assert!(!parse_loose_bool("pinned", "0").expect("\"0\" parses"));
}

#[test]
fn parse_loose_bool_rejects_anything_else_as_422_naming_the_field() {
    let err = parse_loose_bool("only_media", "yes").expect_err("\"yes\" is not accepted");
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        err.public_message.contains("only_media"),
        "the rejection must name the offending field, got: {}",
        err.public_message
    );
    assert_eq!(
        err.public_message,
        "only_media must be \"true\"/\"1\" or \"false\"/\"0\", got \"yes\""
    );
}

#[test]
fn parse_optional_bool_query_defaults_to_false_when_absent() {
    assert!(!parse_optional_bool_query("pinned", None).expect("absent is not an error"));
}

#[test]
fn parse_optional_bool_query_parses_a_present_value() {
    assert!(parse_optional_bool_query("pinned", Some("1")).expect("a present value parses"));
}

#[test]
fn parse_optional_bool_query_rejects_an_unrecognized_value_with_422() {
    let err = parse_optional_bool_query("exclude_replies", Some("maybe"))
        .expect_err("an unrecognized value is rejected");
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
}
