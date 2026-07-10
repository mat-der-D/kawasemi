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
//! - Task 2.2 (`Boundary: ActorRepository`): the local actor's persistence,
//!   state transitions, and handle/id/owner-scoped lookups — see
//!   [`repository`].
//! - Task 2.3 (`Boundary: ActorSigningKeyRepository`): the per-actor signing
//!   key's persistence — active-key insertion, retirement, active-public-key
//!   lookup, and the startup bulk load of every active key — see the `keys`
//!   submodule's [`keys::repository`].
//! - Task 5.1 (`Boundary: ActorService`): actor creation (handle validation
//!   via the `Handle` type -> owner-existence check -> active-initialized
//!   insert -> signing-key provisioning, all in one transaction) and basic
//!   lifecycle (deactivation) — see [`service`].
//! - Task 5.2 (`Boundary: ActorDirectory`): downstream-facing actor
//!   reference operations — management-layer owner-scoped listing
//!   (`list_actors_for_owner`) and protocol-layer handle resolution /
//!   public-key supply (`resolve_actor_by_handle`, `actor_public_key`),
//!   neither of which surfaces owner information — see [`directory`].
//!
//! `keys`'s `material`/`cipher`/`service`/`cache`/`provider` submodules are
//! later/already-landed tasks per design.md's File Structure Plan.

pub mod directory;
pub mod keys;
pub mod model;
pub mod owner;
pub mod repository;
pub mod service;

pub use directory::ActorDirectory;
pub use model::{
    ActorPublicKey, ActorState, ActorSummary, ActorType, Handle, LocalActor, Owner, ResolvedActor,
};
pub use service::{ActorService, NewActor};
