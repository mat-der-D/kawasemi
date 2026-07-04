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
//!
//! `material` (`KeyMaterial`, task 3.1), `cipher` (`KeyCipher`, task 3.2),
//! `service` (`SigningKeyService`, task 4.1), `cache` (`KeyCache`, task 4.1),
//! and `provider` (`DbSigningKeyProvider`, task 4.2) are later tasks per
//! design.md's File Structure Plan, and are deliberately not declared here
//! until those tasks land.

pub mod repository;
