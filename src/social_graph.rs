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
//!
//! No Activity generation (`ActivityBuilder`), no state-transition functions
//! (`Transitions`), no relationship -> contract mapping
//! (`RelationshipMapper`), no business services (`FollowService` /
//! `FollowRequestService` / `MuteService` / `BlockService`), no inbound
//! Activity handlers, no delegation-port implementations
//! (`BlockPolicyImpl` / `RelProviderImpl` / `AccountCountsProviderImpl` /
//! `FilterQuery`), and no HTTP surface (`SocialGraphEndpoints`) live here —
//! those consume the types/policy defined in this module but are out of
//! scope for tasks 1.1-2.1. This module is not yet declared on any
//! router/`AppState`/bootstrap wiring point — that remains a later task's
//! boundary (`SocialGraphModule` wiring, design.md's File Structure Plan).

pub mod approval_policy;
pub mod model;
pub mod repository;

pub use approval_policy::{FollowApprovalPolicy, FollowDecision};
pub use model::{
    Block, Follow, FollowOptions, FollowRequest, FollowRequestDirection, Mute, MuteOptions,
};
