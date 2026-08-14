//! Pure unit tests for this module's own private `parse_acct_resource`
//! helper (Requirements 4.1-4.5, task 5.1, `Boundary: webfinger`) -- no DB,
//! no network, no running instance.
//!
//! The handler-level tests, which require a real `ActorDirectory` and hence
//! a real, running instance (`spawn_test_app`), now live in
//! `tests/federation_webfinger_endpoint_it.rs`, relocated there by
//! `.kiro/specs/test-placement-migration` task 2.2 so that steering
//! `structure.md`'s test layout rule ("DB込みの実起動インスタンスを要する
//! 検証は `tests/` 直下の `*_it.rs` に置く") holds in fact and not only on
//! paper.

use super::*;

#[test]
fn parse_acct_resource_accepts_a_well_formed_acct_uri() {
    assert_eq!(
        parse_acct_resource("acct:alice@example.com"),
        Some(("alice", "example.com"))
    );
}

#[test]
fn parse_acct_resource_rejects_a_missing_acct_prefix() {
    assert_eq!(parse_acct_resource("alice@example.com"), None);
}

#[test]
fn parse_acct_resource_rejects_a_missing_at_separator() {
    assert_eq!(parse_acct_resource("acct:alice.example.com"), None);
}

#[test]
fn parse_acct_resource_rejects_an_empty_user_segment() {
    assert_eq!(parse_acct_resource("acct:@example.com"), None);
}

#[test]
fn parse_acct_resource_rejects_an_empty_domain_segment() {
    assert_eq!(parse_acct_resource("acct:alice@"), None);
}
