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
//!
//! No persistence (`RelationshipRepository`, task 1.3), no approval policy
//! (`FollowApprovalPolicy`), no Activity generation (`ActivityBuilder`), no
//! state-transition functions (`Transitions`), no relationship -> contract
//! mapping (`RelationshipMapper`), no business services (`FollowService` /
//! `FollowRequestService` / `MuteService` / `BlockService`), no inbound
//! Activity handlers, no delegation-port implementations
//! (`BlockPolicyImpl` / `RelProviderImpl` / `AccountCountsProviderImpl` /
//! `FilterQuery`), and no HTTP surface (`SocialGraphEndpoints`) live here —
//! those consume the types defined in this module but are out of scope for
//! task 1.2 (`Boundary: model`). This module is not yet declared on any
//! router/`AppState`/bootstrap wiring point — that remains a later task's
//! boundary (`SocialGraphModule` wiring, design.md's File Structure Plan).

pub mod model;

pub use model::{
    Block, Follow, FollowOptions, FollowRequest, FollowRequestDirection, Mute, MuteOptions,
};
