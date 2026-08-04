//! Unit tests for request-free origin resolution.

use super::*;

#[test]
fn uses_https_and_the_configured_domain() {
    let origin = self_origin("kawasemi.example");
    assert_eq!(origin.scheme, "https");
    assert_eq!(origin.host, "kawasemi.example");
}

/// Equivalence with the call it replaces is the whole contract: every
/// migrated call site must keep minting byte-identical URLs.
#[test]
fn matches_the_forwarded_origin_call_it_replaces() {
    let expected = ForwardedOrigin::resolve("https", "kawasemi.example", None, None);
    let actual = self_origin("kawasemi.example");

    assert_eq!(actual.scheme, expected.scheme);
    assert_eq!(actual.host, expected.host);
}

#[test]
fn preserves_a_domain_carrying_an_explicit_port() {
    let origin = self_origin("kawasemi.example:8443");
    assert_eq!(origin.host, "kawasemi.example:8443");
}
