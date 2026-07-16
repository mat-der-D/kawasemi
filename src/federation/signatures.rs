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
//! Sibling files this spec's later tasks own (`suite.rs` at 1.5, `signer.rs`
//! at 1.5, `verifier.rs` at 2.3, `key_resolver.rs` at 2.1, `negotiation.rs`
//! at 3.x — design.md's File Structure Plan) are deliberately not declared
//! here yet; each is added by the task that actually implements it. In
//! particular, `PublicKeyResolver` (task 2.1) is *not* implemented by this
//! task even though design.md documents it in the same "PublicKeyResolver /
//! FederationHttpClient（モック可能境界）" section — this task's boundary is
//! `FederationHttpClient, Digest` only.

mod digest;
mod http_client;

pub use digest::Digest;
pub use http_client::{
    FederationHttpClient, HttpResponse, MockFederationHttpClient, OutboundRequest,
    ReqwestFederationHttpClient,
};
