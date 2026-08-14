//! Pure unit tests for this module's own private string helpers
//! (`http_date`, `host_from_url`) — no DB, no network, no running instance.
//!
//! The `RequestSigner` tests that need a real, running instance
//! (`spawn_test_app`) now live in
//! `tests/federation_signatures_signer_it.rs`, relocated there by
//! `.kiro/specs/test-placement-migration` task 2.1 so that steering
//! `structure.md`'s test layout rule ("DB込みの実起動インスタンスを要する
//! 検証は `tests/` 直下の `*_it.rs` に置く") holds in fact and not only on
//! paper.

use super::*;

// --- http_date / host_from_url: pure unit tests, no DB/network involved ---

#[test]
fn http_date_matches_rfc_9110s_own_worked_example_shape() {
    use time::macros::datetime;

    // RFC 9110 §5.6.7 gives this exact IMF-fixdate example.
    let when = datetime!(1994-11-06 08:49:37 UTC);
    assert_eq!(http_date(when), "Sun, 06 Nov 1994 08:49:37 GMT");
}

#[test]
fn host_from_url_extracts_the_authority_without_scheme_or_path() {
    assert_eq!(
        host_from_url("https://example.com/inbox?x=1"),
        "example.com"
    );
    assert_eq!(host_from_url("https://example.com"), "example.com");
    assert_eq!(
        host_from_url("https://example.com:8443/x"),
        "example.com:8443"
    );
}
