//! Pure unit tests for this module's own private format-mapping helpers
//! (`format_to_db`, `format_from_db`, `other_format`) — no DB, no network, no
//! running instance.
//!
//! The `SignatureNegotiator` tests that need a real, running instance
//! (`spawn_test_app`) now live in
//! `tests/federation_signatures_negotiation_it.rs`, relocated there by
//! `.kiro/specs/test-placement-migration` task 2.1 so that steering
//! `structure.md`'s test layout rule ("DB込みの実起動インスタンスを要する
//! 検証は `tests/` 直下の `*_it.rs` に置く") holds in fact and not only on
//! paper.

use super::*;

// --- format_to_db / format_from_db: pure unit tests, no DB/network involved ---

#[test]
fn format_to_db_and_back_round_trips_both_variants() {
    assert_eq!(format_to_db(SignatureFormat::DraftCavage), "draft_cavage");
    assert_eq!(format_to_db(SignatureFormat::Rfc9421), "rfc9421");
    assert_eq!(
        format_from_db("draft_cavage"),
        Some(SignatureFormat::DraftCavage)
    );
    assert_eq!(format_from_db("rfc9421"), Some(SignatureFormat::Rfc9421));
    assert_eq!(format_from_db("something_else"), None);
}

#[test]
fn other_format_is_the_opposite_variant() {
    assert_eq!(
        other_format(SignatureFormat::DraftCavage),
        SignatureFormat::Rfc9421
    );
    assert_eq!(
        other_format(SignatureFormat::Rfc9421),
        SignatureFormat::DraftCavage
    );
}
