//! Social graph domain module (social-graph spec, `src/social_graph.rs` +
//! `src/social_graph/`, mirroring the module-with-submodule convention
//! established by `src/media.rs`/`src/media/`, `src/accounts.rs`/
//! `src/accounts/`, and `src/statuses.rs`/`src/statuses/`).
//!
//! Scope so far:
//! - Task 1.1 (`Boundary: migration`, no Rust code): `migrations/
//!   0012_social_graph.sql` — `follows` / `follow_requests` / `mutes` /
//!   `blocks`.
//! - Task 1.2 (`Boundary: model`): the domain value types design.md's
//!   "model" component names — [`model::FollowOptions`], [`model::Follow`],
//!   [`model::FollowRequestDirection`], [`model::FollowRequest`],
//!   [`model::MuteOptions`], [`model::Mute`], and [`model::Block`].
//!   `AccountRef` is not redefined here: it is imported from `crate::domain`
//!   (core-runtime's canonical shared primitives module, mirroring
//!   `src/accounts/model.rs`'s and `src/statuses/model.rs`'s identical
//!   precedent) — see [`model`].
//! - Task 1.3 (`Boundary: RelationshipRepository`): persistence access for
//!   the four relationship tables — see [`repository`].
//! - Task 2.1 (`Boundary: FollowApprovalPolicy`): the single follow-approval
//!   judgment point ([`approval_policy::FollowApprovalPolicy::requires_approval`])
//!   and the sole definition site of the same-server ("both local")
//!   admin-privilege branch — see [`approval_policy`].
//! - Task 2.2 (`Boundary: Transitions, RelationshipRepository`): the common
//!   relationship state-transition functions the API path and the inbound
//!   Activity path both converge on ([`transitions::Transitions`]), plus the
//!   sole point (`establish_follow`/`record_pending`) that emits
//!   notifications' `follow`/`follow_request` `NotificationEvent` after
//!   commit — see [`transitions`]. `RelationshipRepository` ([`repository`])
//!   gained two small, additive extensions this same task needed (see that
//!   module's doc comment, "Task 2.2 additions").
//! - Task 2.3 (`Boundary: ActivityBuilder`): the Follow/Accept/Reject/Block
//!   /Undo canonical Activity generators
//!   ([`activity_builder::ActivityBuilder`]) and the two narrow, DB-free
//!   actor-URI resolution ports they depend on
//!   ([`activity_builder::LocalActorLookup`]/
//!   [`activity_builder::RemoteActorLookup`]) — see [`activity_builder`].
//!
//! No relationship -> contract mapping (`RelationshipMapper`), no business
//! services (`FollowService` / `FollowRequestService` / `MuteService` /
//! `BlockService`), no inbound Activity handlers, no delegation-port
//! implementations (`BlockPolicyImpl` / `RelProviderImpl` /
//! `AccountCountsProviderImpl` / `FilterQuery`), and no HTTP surface
//! (`SocialGraphEndpoints`) live here — those consume the types/policy/
//! transitions/Activity-builder defined in this module but are out of scope
//! for tasks 1.1-2.3. This module is not yet declared on any
//! router/`AppState`/bootstrap wiring point — that remains a later task's
//! boundary (`SocialGraphModule` wiring, design.md's File Structure Plan).

pub mod activity_builder;
pub mod approval_policy;
pub mod model;
pub mod repository;
pub mod transitions;

pub use activity_builder::{ActivityBuilder, LocalActorLookup, RemoteActorLookup};
pub use approval_policy::{FollowApprovalPolicy, FollowDecision};
pub use model::{
    Block, Follow, FollowOptions, FollowRequest, FollowRequestDirection, Mute, MuteOptions,
};
pub use transitions::Transitions;
