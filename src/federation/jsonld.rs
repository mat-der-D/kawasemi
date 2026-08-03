//! `JsonLdCodec` (design.md "Serialization / 直列化層" -> "JsonLdCodec";
//! Requirements 9.1, 9.2, 9.3, 9.4; task 1.2): ActivityPub `@context`
//! stamping on serialization, unknown-property-tolerant safe expansion plus
//! required-property (`type`/`id`) validation on parse, and ActivityPub
//! media-type judgment for `Accept`-header content negotiation.
//!
//! Per steering's Rust module convention (`mod.rs` は使わない), this file
//! (`src/federation/jsonld.rs`, a sibling of `src/federation/jsonld/`, not
//! `src/federation/jsonld/mod.rs`) plays the role design.md's directory-style
//! listing shows as `jsonld/mod.rs`: it declares and re-exports the `jsonld`
//! submodule's own children — mirrors `src/actor/keys.rs`'s established
//! precedent for the same convention.
//!
//! Scope so far (task 1.2, `Boundary: JsonLdCodec`):
//! - [`context`]: the ActivityPub JSON-LD context constant and the pure
//!   `@context`-stamping mutation (Requirement 9.1).
//! - [`serialize`]: canonical ActivityPub document serialization, built on
//!   `context` (Requirement 9.1).
//! - [`parse`]: safe JSON-LD interpretation — unknown properties never fail
//!   parsing (Requirement 9.2), missing `type`/`id` is a validation error
//!   (Requirement 9.3).
//! - [`accepts_activitypub`] (this file): ActivityPub media-type judgment for
//!   `Accept` header values (Requirement 9.4). It lives directly on this
//!   orchestrator file rather than inside `context`/`serialize`/`parse`
//!   because it is not about `@context` construction, outgoing
//!   serialization, or JSON-LD body parsing at all — it is a pure `Accept`
//!   header predicate, and design.md's File Structure Plan does not carve
//!   out a fourth file for it.

mod context;
mod parse;
mod serialize;

#[cfg(test)]
mod tests;

pub use context::ACTIVITYSTREAMS_CONTEXT;
pub use parse::{ParsedActivity, parse_activity};
pub use serialize::serialize;

/// ActivityPub's primary media type (Requirement 9.4).
const ACTIVITY_JSON_MEDIA_TYPE: &str = "application/activity+json";
/// The generic JSON-LD media type ActivityPub also accepts as an AP
/// representation request (Requirement 9.4).
const LD_JSON_MEDIA_TYPE: &str = "application/ld+json";

/// Judges whether an HTTP `Accept` header value requests an ActivityPub
/// representation (Requirement 9.4): true if any comma-separated
/// media-range entry's media type — ignoring `;`-delimited parameters such
/// as `q=` or `profile=`, and ignoring case — is `application/activity+json`
/// or `application/ld+json`.
pub fn accepts_activitypub(accept: &str) -> bool {
    accept.split(',').any(|candidate| {
        let media_type = candidate
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        media_type == ACTIVITY_JSON_MEDIA_TYPE || media_type == LD_JSON_MEDIA_TYPE
    })
}
