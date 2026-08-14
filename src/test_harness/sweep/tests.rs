//! Unit tests for the reclaim predicate.
//!
//! No database, no sleeping, no `SystemTime::now()`: `is_reclaimable` takes
//! the current instant as a parameter, so every case here — including the
//! ones real time cannot be made to produce on demand, like a schema stamped
//! in the *future* by a skewed clock — is exercised as pure logic.
//!
//! The three basic cases (an old name, a fresh name, an unparseable name)
//! are the first three tests; the rest close
//! the ways a predicate can be accidentally right — matching too broadly,
//! ignoring the threshold, saturating on overflow — which is what makes the
//! judgement safe to point at a shared database holding other people's
//! schemas.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::{HARNESS_SCHEMA_PREFIX, is_reclaimable};
use crate::test_harness::unique_schema_name;

/// Arbitrary fixed "now", far enough from the epoch that tests can place
/// creation instants both well before and well after it.
const NOW_SECS: u64 = 1_700_000_000;

/// Threshold used by the age cases. Deliberately unrelated to the real
/// `RECLAIM_THRESHOLD`: here it only needs to be a duration the tests can
/// straddle.
const THRESHOLD: Duration = Duration::from_secs(600);

fn now() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(NOW_SECS)
}

/// Builds the name `unique_schema_name` would have produced `age_secs`
/// before [`now`], with an arbitrary sequence number.
fn schema_name_aged(age_secs: u64) -> String {
    let nanos = u128::from(NOW_SECS - age_secs) * 1_000_000_000;
    format!("{HARNESS_SCHEMA_PREFIX}{nanos}_7")
}

#[test]
fn reclaims_a_schema_older_than_the_threshold() {
    assert!(is_reclaimable(&schema_name_aged(3600), now(), THRESHOLD));
}

#[test]
fn keeps_a_schema_newer_than_the_threshold() {
    // A live process is the expected owner of a fresh schema.
    assert!(!is_reclaimable(&schema_name_aged(60), now(), THRESHOLD));
}

#[test]
fn keeps_a_name_it_cannot_parse() {
    assert!(!is_reclaimable("public", now(), THRESHOLD));
}

#[test]
fn treats_an_age_of_exactly_the_threshold_as_reclaimable() {
    // Documents the boundary as inclusive; see `is_reclaimable`'s contract.
    assert!(is_reclaimable(
        &schema_name_aged(THRESHOLD.as_secs()),
        now(),
        THRESHOLD
    ));
    assert!(!is_reclaimable(
        &schema_name_aged(THRESHOLD.as_secs() - 1),
        now(),
        THRESHOLD
    ));
}

#[test]
fn keeps_a_schema_stamped_in_the_future() {
    // Clock skew between machines sharing the test database. A future stamp
    // most likely belongs to a process running right now, and subtracting it
    // must not underflow into an enormous age either.
    let future_nanos = u128::from(NOW_SECS + 3600) * 1_000_000_000;
    let name = format!("{HARNESS_SCHEMA_PREFIX}{future_nanos}_0");
    assert!(!is_reclaimable(&name, now(), THRESHOLD));
}

#[test]
fn keeps_schemas_belonging_to_other_naming_families() {
    // Real, currently-live conventions in the same shared database.
    let foreign = [
        // `src/migrate/tests.rs`
        "kawasemi_migrate_test_idempotent_1700000000000000000_3",
        // plain application/developer schemas
        "public",
        "information_schema",
        "pg_catalog",
        "kawasemi",
    ];
    for name in foreign {
        assert!(
            !is_reclaimable(name, now(), THRESHOLD),
            "must not claim `{name}`"
        );
    }
}

#[test]
fn keeps_names_that_only_resemble_the_harness_pattern() {
    // One second past the epoch: old enough that a name built from it would
    // be reclaimed on age alone, so each case below fails only on its shape.
    let ancient_nanos: u128 = 1_000_000_000;
    let malformed = [
        // truncated prefix
        "kawasemi_test_harnes_1000000000_0".to_string(),
        // prefix with nothing after it
        HARNESS_SCHEMA_PREFIX.trim_end_matches('_').to_string(),
        HARNESS_SCHEMA_PREFIX.to_string(),
        // no sequence field
        format!("{HARNESS_SCHEMA_PREFIX}{ancient_nanos}"),
        // empty fields
        format!("{HARNESS_SCHEMA_PREFIX}_0"),
        format!("{HARNESS_SCHEMA_PREFIX}{ancient_nanos}_"),
        // non-numeric fields
        format!("{HARNESS_SCHEMA_PREFIX}abc_0"),
        format!("{HARNESS_SCHEMA_PREFIX}{ancient_nanos}_abc"),
        // sign-prefixed / whitespace-padded numbers `str::parse` would accept
        format!("{HARNESS_SCHEMA_PREFIX}+{ancient_nanos}_0"),
        format!("{HARNESS_SCHEMA_PREFIX}-{ancient_nanos}_0"),
        format!("{HARNESS_SCHEMA_PREFIX} {ancient_nanos}_0"),
        // extra segments
        format!("{HARNESS_SCHEMA_PREFIX}{ancient_nanos}_0_0"),
        // trailing text after a valid-looking name
        format!("{HARNESS_SCHEMA_PREFIX}{ancient_nanos}_0x"),
        // the sibling temp-directory name, which shares the prefix
        format!("{HARNESS_SCHEMA_PREFIX}media_storage_{ancient_nanos}_0"),
        // non-ASCII digits
        format!("{HARNESS_SCHEMA_PREFIX}１２３_0"),
        // empty and garbage
        String::new(),
        "_".to_string(),
        "🦀".to_string(),
    ];
    for name in malformed {
        assert!(
            !is_reclaimable(&name, now(), THRESHOLD),
            "must not claim `{name}`"
        );
    }
}

#[test]
fn keeps_a_name_whose_timestamp_overflows() {
    // A digit run longer than `u128` (or one that fits `u128` but lands past
    // `SystemTime`'s range) must not saturate into an ancient — and therefore
    // reclaimable — instant.
    let overflowing = [
        format!("{HARNESS_SCHEMA_PREFIX}{}_0", "9".repeat(40)),
        format!("{HARNESS_SCHEMA_PREFIX}{}_0", u128::MAX),
    ];
    for name in overflowing {
        assert!(
            !is_reclaimable(&name, now(), THRESHOLD),
            "must not claim `{name}`"
        );
    }
}

#[test]
fn ignores_the_threshold_for_nothing() {
    // Guards against a predicate that parses correctly but never consults the
    // threshold: one and the same name must flip answers as the threshold moves.
    let name = schema_name_aged(3600);
    assert!(is_reclaimable(&name, now(), Duration::from_secs(1800)));
    assert!(!is_reclaimable(&name, now(), Duration::from_secs(7200)));
}

#[test]
fn parses_what_the_real_generator_produces() {
    // Binds this predicate to `unique_schema_name`'s actual output rather than
    // to this file's re-creation of it: if the naming convention changes, this
    // fails even though every hand-built case above still passes.
    let name = unique_schema_name();
    let observed_now = SystemTime::now();
    assert!(
        is_reclaimable(&name, observed_now, Duration::ZERO),
        "a just-minted name must be parseable: `{name}`"
    );
    assert!(
        !is_reclaimable(&name, observed_now, Duration::from_secs(3600)),
        "a just-minted name must not be old enough to reclaim: `{name}`"
    );
}
