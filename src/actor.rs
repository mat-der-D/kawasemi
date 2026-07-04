//! Actor domain module (actor-model spec).
//!
//! Scope so far:
//! - Task 1.2 (`Boundary: model`): the pure domain/value types that
//!   represent a local actor, the management-layer owner concept, and the
//!   protocol-layer reference types downstream (api-foundation /
//!   federation-core) will consume — see [`model`].
//! - Task 2.1 (`Boundary: OwnerRepository`): the owner concept's
//!   persistence — creation and lookup against a plain `&PgPool` — see
//!   [`owner`].
//!
//! `ActorRepository` / `ActorSigningKeyRepository` / `ActorService` /
//! `ActorDirectory` / the `keys` submodule are later tasks (2.2-6.x) per
//! design.md's File Structure Plan, and are deliberately not declared here
//! until those tasks land.

pub mod model;
pub mod owner;

pub use model::{
    ActorPublicKey, ActorState, ActorSummary, ActorType, Handle, LocalActor, Owner, ResolvedActor,
};
