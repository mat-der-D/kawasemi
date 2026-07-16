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
//! - Task 1.4 (`Boundary: FederationHttpClient, Digest`): the outbound
//!   network boundary (mockable send/fetch port, production `reqwest`
//!   implementation, deterministic mock implementation) and SHA-256 body
//!   digest computation/mismatch detection — see [`signatures`].
//! - Task 2.1 (`Boundary: PublicKeyResolver`): resolves a `keyId` to
//!   cached/fetched public-key material (`remote_public_keys`), with
//!   cache-first/force/TTL-staleness semantics — see [`signatures`].
//! - Task 2.2 (`Boundary: RequestSigner`): attaches an HTTP Signature
//!   (draft-cavage or RFC 9421) to an outbound request using a local
//!   actor's currently valid signing key — see [`signatures`].
//!
//! Later tasks in this spec (`config`, `inbound`, `outbound`, `endpoints` —
//! see design.md's File Structure Plan) are out of this task's boundary and
//! deliberately not declared here yet; each is added by the task that
//! actually implements it.

pub mod jsonld;
pub mod signatures;
pub mod urls;

pub use jsonld::{ParsedActivity, accepts_activitypub, parse_activity, serialize};
pub use signatures::{
    DEFAULT_PUBLIC_KEY_CACHE_TTL, DbFederationPublicKeyResolver, Digest, FederationHttpClient,
    HttpResponse, MockFederationHttpClient, OutboundRequest, PublicKeyResolver, RemotePublicKey,
    RequestSigner, ReqwestFederationHttpClient,
};
pub use urls::{ActorUrls, ObjectKind};
