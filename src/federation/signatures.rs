//! `signatures` submodule (design.md's File Structure Plan lists it as
//! `signatures/`; task 1.4, `Boundary: FederationHttpClient, Digest`): the
//! outbound network boundary (`FederationHttpClient` port + production/mock
//! implementations) and body digest computation/verification (`Digest`).
//!
//! Per steering's Rust module convention (`mod.rs` は使わない), this file
//! (`src/federation/signatures.rs`, a sibling of `src/federation/signatures/`)
//! plays the role design.md's directory listing shows as `signatures/mod.rs`
//! — mirrors `src/federation/jsonld.rs` / `src/federation/urls.rs`'s
//! established precedent for the same convention.
//!
//! Scope so far (task 1.4):
//! - [`http_client`]: `FederationHttpClient` port, `OutboundRequest` /
//!   `HttpResponse` request/response shapes, a production implementation
//!   backed by `reqwest` (`ReqwestFederationHttpClient`), and a
//!   deterministic mock implementation (`MockFederationHttpClient`) for
//!   tests (Requirements 2.7, 1.1, 11.2).
//! - [`digest`]: SHA-256 body digest computation and mismatch detection
//!   (`Digest`) (Requirements 1.3, 2.5).
//!
//! Scope so far (task 1.5, `Boundary: SignatureSuite`):
//! - [`suite`]: the `SignatureSuite` abstraction over draft-cavage and RFC
//!   9421 HTTP Signatures — signing-target construction, signature header
//!   assembly, parsing, and received-format detection (Requirements 1.4,
//!   2.2). Pure string/format logic only: no RSA signing or verification
//!   happens here (that is `RequestSigner`/`SignatureVerifier`, later
//!   tasks).
//!
//! Scope so far (task 2.1, `Boundary: PublicKeyResolver`):
//! - [`key_resolver`]: `PublicKeyResolver` port and a DB
//!   (`remote_public_keys`) plus `FederationHttpClient`-backed
//!   implementation (`DbFederationPublicKeyResolver`), resolving a `keyId`
//!   to public-key material with cache-first/force/TTL-staleness semantics
//!   (Requirements 2.3, 2.4).
//!
//! Scope so far (task 2.2, `Boundary: RequestSigner`):
//! - [`signer`]: `RequestSigner` — attaches an HTTP Signature (draft-cavage
//!   or RFC 9421) to an outbound request using a local actor's currently
//!   valid signing key, resolved via `ActorDirectory` +
//!   `core-runtime`'s `SigningKeyProvider` (Requirements 1.1, 1.2, 1.3,
//!   1.5).
//!
//! Scope so far (task 2.3, `Boundary: SignatureVerifier`):
//! - [`verifier`]: `SignatureVerifier`/`HttpSignatureVerifier` — verifies a
//!   received HTTP Signature (draft-cavage or RFC 9421) end-to-end: format
//!   detection, signing-input reconstruction, covered-component validation,
//!   `Date` freshness, body-digest match, public-key resolution (with one
//!   invalidate-and-retry on crypto failure), and the RSA-SHA256
//!   cryptographic check itself, returning the verified signer's identity
//!   (Requirements 2.1, 2.2, 2.5, 2.6, 7.1).
//!
//! Scope so far (task 2.4, `Boundary: SignatureNegotiator`):
//! - [`negotiation`]: `SignatureNegotiator` — double-knocks a signed
//!   outbound request against a host of unknown signature-format support,
//!   retrying with the other format on a `401` (signature-related)
//!   rejection, and remembers the successful format per host in
//!   `instance_signature_capabilities` (Requirements 3.1, 3.2, 3.3).

mod digest;
mod http_client;
mod key_resolver;
mod negotiation;
mod signer;
mod suite;
mod verifier;

pub use digest::Digest;
pub use http_client::{
    FederationHttpClient, HttpResponse, MockFederationHttpClient, OutboundRequest,
    ReqwestFederationHttpClient,
};
pub use key_resolver::{
    DEFAULT_PUBLIC_KEY_CACHE_TTL, DbFederationPublicKeyResolver, PublicKeyResolver, RemotePublicKey,
};
pub use negotiation::SignatureNegotiator;
pub use signer::RequestSigner;
pub use suite::{
    DraftCavageSuite, ParsedSignature, RequestHeaders, Rfc9421Suite, SignableRequest,
    SignatureFormat, SignatureSuite, SigningInput,
};
pub use verifier::{
    DEFAULT_SIGNATURE_MAX_AGE, HttpSignatureVerifier, IncomingRequest, SignatureVerifier,
    VerifiedSigner,
};
