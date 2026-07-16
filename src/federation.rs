//! Federation domain module (federation-core spec).
//!
//! Scope so far:
//! - Task 1.2 (`Boundary: JsonLdCodec`): ActivityPub JSON-LD serialization
//!   (`@context` stamping), safe parsing (unknown-property-tolerant,
//!   required-property validation), and ActivityPub media-type judgment for
//!   content negotiation — see [`jsonld`].
//! - Task 1.3 (`Boundary: ActorUrls`): builds the ActivityPub actor / inbox
//!   / outbox / shared-inbox / object / collection URLs and the keyId URL
//!   from this instance's configured server domain — see [`urls`].
//!
//! Later tasks in this spec (`config`, `signatures`, `inbound`, `outbound`,
//! `endpoints` — see design.md's File Structure Plan) are out of this
//! task's boundary and deliberately not declared here yet; each is added by
//! the task that actually implements it.

pub mod jsonld;
pub mod urls;

pub use jsonld::{ParsedActivity, accepts_activitypub, parse_activity, serialize};
pub use urls::{ActorUrls, ObjectKind};
