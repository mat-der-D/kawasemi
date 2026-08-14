//! Tests for `endpoints.rs`.
//!
//! [`wire_shape_tests`] covers this module's own pure, DB/network-free
//! wire-shape helpers (`parse_visibility`/`parse_media_ids`/
//! `parse_optional_limit`/`parse_id`), mirroring
//! `accounts::endpoints::tests`'s established convention. They are private
//! to this module, so this is the only place that can reach them.
//!
//! The router-level, auth/scope/response-code/`Link`-header behavior this
//! surface's own observable-completion condition names — which needs a real,
//! `spawn_test_app`-backed Postgres schema — lives in
//! `tests/statuses_endpoints_it.rs`, per steering `structure.md`'s test
//! layout rule.

use super::*;

mod wire_shape_tests {
    use super::*;

    #[test]
    fn parse_visibility_accepts_every_canonical_variant() {
        assert_eq!(parse_visibility("public").unwrap(), Visibility::Public);
        assert_eq!(parse_visibility("unlisted").unwrap(), Visibility::Unlisted);
        assert_eq!(parse_visibility("private").unwrap(), Visibility::Private);
        assert_eq!(parse_visibility("direct").unwrap(), Visibility::Direct);
    }

    #[test]
    fn parse_visibility_rejects_unknown_value() {
        let err = parse_visibility("bogus").expect_err("must be rejected");
        assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn parse_media_ids_parses_decimal_strings() {
        let ids = parse_media_ids(&["1".to_string(), "2".to_string()]).unwrap();
        assert_eq!(ids, vec![Id::from_i64(1), Id::from_i64(2)]);
    }

    #[test]
    fn parse_media_ids_rejects_a_non_numeric_value() {
        let err = parse_media_ids(&["abc".to_string()]).expect_err("must be rejected");
        assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn parse_optional_limit_is_none_when_absent() {
        assert_eq!(parse_optional_limit(None).unwrap(), None);
    }

    #[test]
    fn parse_optional_limit_rejects_a_non_numeric_value() {
        let err = parse_optional_limit(Some("abc")).expect_err("must be rejected");
        assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn parse_id_treats_an_unparseable_segment_as_404() {
        let err = parse_id("not-a-number").expect_err("must be rejected");
        assert_eq!(err.status, StatusCode::NOT_FOUND);
    }
}
