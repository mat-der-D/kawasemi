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
//! - Task 2.4 (`Boundary: RelationshipMapper`): the single relationship-state
//!   -> accounts-and-instance `RelationshipView` mapping point
//!   ([`relationship_mapper::RelationshipMapper::to_view`]) — see
//!   [`relationship_mapper`].
//! - Task 3.1 (`Boundary: FollowService`): the follow/unfollow business
//!   aggregate ([`follow_service::FollowService`]) — see [`follow_service`].
//! - Task 3.2 (`Boundary: FollowRequestService`): the inbound
//!   pending-follow-request aggregate — paginated listing, authorize
//!   (promote + Accept delivery), reject (drop + Reject delivery)
//!   ([`follow_request_service::FollowRequestService`]) — see
//!   [`follow_request_service`]. This task also widened
//!   [`transitions::Transitions::promote_pending`]/
//!   [`transitions::Transitions::drop_pending`]'s return type (see that
//!   module's doc comment, "Task 3.2 additions").
//! - Task 3.3 (`Boundary: MuteService`): the mute/unmute business aggregate
//!   ([`mute_service::MuteService`]) — a DB-state-only update with no
//!   federation Activity/delivery involved (Requirement 4.5) — see
//!   [`mute_service`].
//! - Task 3.4 (`Boundary: BlockService`): the block/unblock business
//!   aggregate ([`block_service::BlockService`]) — unconditional
//!   relationship-clearing block/unblock with Block/Undo(Block) delivery —
//!   see [`block_service`]. This task also widened
//!   [`transitions::Transitions::clear_block`]'s return type and added
//!   [`repository::take_block`] (see those modules' own doc comments, "Task
//!   3.4 addition").
//! - Task 4.1 (`Boundary: InboundHandler, Transitions, ActivityBuilder`):
//!   federation-core's `InboundActivityHandler` implementation for received
//!   Follow / Accept / Reject / Block / Undo(Follow|Block)
//!   ([`inbound::SocialGraphInboundHandler`]), converging onto the same
//!   `FollowApprovalPolicy`/`Transitions` the API path uses — see
//!   [`inbound`]. This task also widened
//!   [`activity_builder::LocalActorLookup::resolve_handle`]/
//!   [`activity_builder::RemoteActorLookup::resolve_actor_uri`] to declare
//!   `-> impl Future<..> + Send` (see those methods' own doc comments, and
//!   [`inbound`]'s own doc comment, "`deliver: BoxedDeliver`") — required for
//!   `SocialGraphInboundHandler::handle`'s `Send`-boxed
//!   `InboundActivityHandler` contract; source-compatible with every
//!   existing `async fn`-bodied implementation (`ActivityBuilder`'s own
//!   `ActorDirectory`/`PgRemoteActorLookup` impls, and every test double).
//!
//! No delegation-port implementations (`BlockPolicyImpl` / `RelProviderImpl`
//! / `AccountCountsProviderImpl` / `FilterQuery`), and no HTTP surface
//! (`SocialGraphEndpoints`) live here yet — those consume the types/policy/
//! transitions/Activity-builder/mapper/services/inbound-handler defined in
//! this module but are out of scope through task 4.1. This module is not yet
//! declared on any router/`AppState`/bootstrap wiring point, and
//! [`inbound::SocialGraphInboundHandler`] is not yet registered against a
//! live `InboundActivityDispatcher` — that remains a later task's boundary
//! (`SocialGraphModule` wiring, design.md's File Structure Plan, task 5.2).

pub mod activity_builder;
pub mod approval_policy;
pub mod block_service;
pub mod follow_request_service;
pub mod follow_service;
pub mod inbound;
pub mod model;
pub mod mute_service;
pub mod relationship_mapper;
pub mod repository;
pub mod transitions;

pub use activity_builder::{ActivityBuilder, LocalActorLookup, RemoteActorLookup};
pub use approval_policy::{FollowApprovalPolicy, FollowDecision};
pub use block_service::BlockService;
pub use follow_request_service::FollowRequestService;
pub use follow_service::FollowService;
pub use inbound::{ActorUriResolver, ProdActorUriResolver, SocialGraphInboundHandler};
pub use model::{
    Block, Follow, FollowOptions, FollowRequest, FollowRequestDirection, Mute, MuteOptions,
};
pub use mute_service::MuteService;
pub use relationship_mapper::RelationshipMapper;
pub use transitions::Transitions;
