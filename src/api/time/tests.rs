//! Unit tests for the shared RFC 3339 timestamp formatter.

use super::*;
use time::macros::datetime;

#[test]
fn formats_utc_as_rfc3339_with_a_z_offset() {
    let when = datetime!(2026-08-04 10:30:00 UTC);
    assert_eq!(format_time(when), "2026-08-04T10:30:00Z");
}

#[test]
fn preserves_sub_second_precision() {
    let when = datetime!(2026-08-04 10:30:00.123456 UTC);
    assert_eq!(format_time(when), "2026-08-04T10:30:00.123456Z");
}

#[test]
fn renders_a_non_utc_offset_explicitly_rather_than_normalizing_it() {
    let when = datetime!(2026-08-04 19:30:00 +9:00);
    assert_eq!(format_time(when), "2026-08-04T19:30:00+09:00");
}
