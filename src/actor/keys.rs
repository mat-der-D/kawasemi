//! `keys` submodule (design.md "File Structure Plan": `src/actor/keys/`).
//!
//! Per steering's Rust module convention ("`mod.rs` は使わない"), this file
//! (`src/actor/keys.rs`, a sibling of `src/actor.rs`, not
//! `src/actor/keys/mod.rs`) plays the role design.md's directory-style
//! listing shows as `keys/mod.rs`: it declares and re-exports the `keys`
//! submodule's own children.
//!
//! Scope so far:
//! - Task 2.3 (`Boundary: ActorSigningKeyRepository`): signing-key
//!   persistence — active-key insertion, retirement, active-public-key
//!   lookup, and the startup bulk load of every active key — see
//!   [`repository`].
//! - Task 3.1 (`Boundary: KeyMaterial`): RSA-2048 key pair generation from
//!   an injected `core-runtime` random-byte boundary, PEM-encoded
//!   (public/SPKI, private/PKCS#8 wrapped in a `Secret`) — see [`material`].
//!
//! - Task 3.2 (`Boundary: KeyCipher`): private-key at-rest sealing/opening
//!   via AEAD, keyed by a boot-config KEK and an injected-rng nonce — see
//!   [`cipher`].
//!
//! - Task 4.1 (`Boundary: SigningKeyService, KeyCache`): the in-memory
//!   `KeyRef` -> active-`SigningKey` store that a synchronous supply
//!   boundary reads from — see [`cache`] — and the business service that
//!   generates/seals/persists a key at actor-creation time and drives
//!   at-most-one-active-key rotation, keeping [`cache`] in sync with every
//!   write — see [`service`].
//!
//! `provider` (`DbSigningKeyProvider`, task 4.2) is a later task per
//! design.md's File Structure Plan, and is deliberately not declared here
//! until that task lands.

pub mod cache;
pub mod cipher;
pub mod material;
pub mod repository;
pub mod service;
