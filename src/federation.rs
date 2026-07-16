//! Federation domain module (federation-core spec).
//!
//! Scope so far:
//! - Task 1.2 (`Boundary: JsonLdCodec`): ActivityPub JSON-LD serialization
//!   (`@context` stamping), safe parsing (unknown-property-tolerant,
//!   required-property validation), and ActivityPub media-type judgment for
//!   content negotiation — see [`jsonld`].
//!
//! Later tasks in this spec (`config`, `urls`, `signatures`, `inbound`,
//! `outbound`, `endpoints` — see design.md's File Structure Plan) are out of
//! this task's boundary and deliberately not declared here yet; each is
//! added by the task that actually implements it.

pub mod jsonld;

pub use jsonld::{ParsedActivity, accepts_activitypub, parse_activity, serialize};
