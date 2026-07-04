//! Actor domain module (actor-model spec).
//!
//! Scope so far (task 1.2, `Boundary: model`): the pure domain/value types
//! that represent a local actor, the management-layer owner concept, and
//! the protocol-layer reference types downstream (api-foundation /
//! federation-core) will consume — see [`model`]. This module owns no
//! persistence, business logic, or key material yet: `OwnerRepository` /
//! `ActorRepository` / `ActorService` / `ActorDirectory` / the `keys`
//! submodule are later tasks (2.x-6.x) per design.md's File Structure Plan,
//! and are deliberately not declared here until those tasks land.

pub mod model;

pub use model::{
    ActorPublicKey, ActorState, ActorSummary, ActorType, Handle, LocalActor, Owner, ResolvedActor,
};
