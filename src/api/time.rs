//! Timestamp formatting shared by every entity serializer.
//!
//! Mastodon clients parse `created_at`/`edited_at`/`expires_at` and friends
//! as RFC 3339. Each serializer used to hold its own private copy of this
//! one-liner; the risk was never that a copy would be written wrong, but
//! that one of them would later be "improved" (a different precision, a
//! different offset rendering) and produce an entity whose timestamps no
//! longer match the rest of the API.

#[cfg(test)]
mod tests;

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// Formats `when` as RFC 3339 — the single timestamp representation every
/// entity in this API uses.
///
/// Infallible in practice: `OffsetDateTime` cannot hold a value RFC 3339
/// is unable to represent, so the formatting error branch is unreachable
/// rather than merely unlikely.
pub fn format_time(when: OffsetDateTime) -> String {
    when.format(&Rfc3339)
        .expect("a valid OffsetDateTime always formats as RFC 3339")
}
