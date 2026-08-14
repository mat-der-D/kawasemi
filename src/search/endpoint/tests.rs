//! Pure, DB-independent unit tests for `endpoint.rs`'s query-parameter
//! parsing/rounding helpers:
//! [`super::resolve_search_limit`]/[`super::resolve_search_offset`]/
//! [`super::parse_search_type`]/[`super::parse_optional_bool_query`]/
//! [`super::parse_optional_account_id`] — exhaustively proving the
//! api-foundation `limit`/`offset` rounding convention
//! (default/clamp/malformed-422) directly. None of them needs a running
//! instance, so this is where they belong. Four of the five are private to
//! `endpoint.rs` and unreachable from outside this crate; the exception is
//! `parse_optional_bool_query`, which is `crate::api::query`'s public
//! helper that `endpoint.rs` merely imports.
//!
//! The other layer of this module's coverage — a real, test-only axum
//! `Router` driven via `tower::ServiceExt::oneshot` against a real
//! `crate::test_harness`-backed Postgres schema, proving auth/scope
//! enforcement, response codes, and query-parameter -> `SearchParams`
//! wiring — requires a running instance and therefore lives in
//! `tests/search_endpoint_it.rs`, moved there by
//! `.kiro/specs/test-placement-migration` task 3.3 so that steering
//! `structure.md`'s test layout rule ("DB込みの実起動インスタンスを要する検証
//! は `tests/` 直下の `*_it.rs` に置く") holds in fact and not only on paper.

use super::*;

// ==== Pure unit tests: parsing/rounding helpers (no DB) ====================

mod parsing {
    use super::*;

    #[test]
    fn resolve_search_limit_defaults_when_absent() {
        assert_eq!(resolve_search_limit(None).unwrap(), DEFAULT_LIMIT);
    }

    #[test]
    fn resolve_search_limit_clamps_to_max_when_over_limit() {
        assert_eq!(resolve_search_limit(Some("9999")).unwrap(), MAX_LIMIT);
    }

    #[test]
    fn resolve_search_limit_passes_through_a_value_within_bounds() {
        assert_eq!(resolve_search_limit(Some("5")).unwrap(), 5);
    }

    #[test]
    fn resolve_search_limit_rejects_a_malformed_value_with_422() {
        let err = resolve_search_limit(Some("not-a-number")).unwrap_err();
        assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn resolve_search_offset_defaults_to_zero_when_absent() {
        assert_eq!(resolve_search_offset(None).unwrap(), 0);
    }

    #[test]
    fn resolve_search_offset_passes_through_a_present_value_unclamped() {
        assert_eq!(resolve_search_offset(Some("500")).unwrap(), 500);
    }

    #[test]
    fn resolve_search_offset_rejects_a_malformed_value_with_422() {
        let err = resolve_search_offset(Some("not-a-number")).unwrap_err();
        assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn parse_search_type_accepts_all_three_known_values() {
        assert_eq!(parse_search_type("accounts").unwrap(), SearchType::Accounts);
        assert_eq!(parse_search_type("statuses").unwrap(), SearchType::Statuses);
        assert_eq!(parse_search_type("hashtags").unwrap(), SearchType::Hashtags);
    }

    #[test]
    fn parse_search_type_rejects_an_unknown_value_with_422() {
        let err = parse_search_type("bogus").unwrap_err();
        assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn parse_optional_bool_query_defaults_to_false_when_absent() {
        assert!(!parse_optional_bool_query("resolve", None).unwrap());
    }

    #[test]
    fn parse_optional_bool_query_accepts_true_and_false_spellings() {
        assert!(parse_optional_bool_query("resolve", Some("true")).unwrap());
        assert!(parse_optional_bool_query("resolve", Some("1")).unwrap());
        assert!(!parse_optional_bool_query("resolve", Some("false")).unwrap());
        assert!(!parse_optional_bool_query("resolve", Some("0")).unwrap());
    }

    #[test]
    fn parse_optional_bool_query_rejects_an_unrecognized_value_with_422() {
        let err = parse_optional_bool_query("resolve", Some("maybe")).unwrap_err();
        assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn parse_optional_account_id_defaults_to_none_when_absent() {
        assert_eq!(parse_optional_account_id(None).unwrap(), None);
    }

    #[test]
    fn parse_optional_account_id_parses_a_present_numeric_value() {
        assert_eq!(
            parse_optional_account_id(Some("42")).unwrap(),
            Some(Id::from_i64(42))
        );
    }

    #[test]
    fn parse_optional_account_id_rejects_a_non_numeric_value_with_422() {
        let err = parse_optional_account_id(Some("not-an-id")).unwrap_err();
        assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
    }
}
