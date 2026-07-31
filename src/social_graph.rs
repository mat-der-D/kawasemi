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
//! - Task 4.2 (`Boundary: BlockPolicyImpl`): federation-core's `BlockPolicy`
//!   delegation-boundary implementation ([`providers::BlockPolicyImpl`]),
//!   backed by this spec's own `blocks` table (reusing task 4.1's
//!   [`inbound::ActorUriResolver`] port for signer resolution) -- see
//!   [`providers`]. This task also added [`repository::is_blocked`] (a
//!   minimal, additive existence-query extension -- see that function's own
//!   doc comment).
//! - Task 4.3 (`Boundary: RelProviderImpl, FilterQuery`):
//!   accounts-and-instance's `RelationshipStateProvider` delegation-boundary
//!   implementation ([`providers::RelProviderImpl`]), backed by the already-
//!   implemented [`repository::load_states`] (task 1.3) +
//!   [`relationship_mapper::RelationshipMapper::to_view`] (task 2.4), plus a
//!   query-only façade over the same tables' filter sets for timelines/
//!   notifications ([`providers::FilterQuery`], returning
//!   [`providers::RelationshipSets`]) -- see [`providers`]. This task also
//!   added [`repository::reblogs_hidden_targets`] (a minimal, additive
//!   filter-set-query extension -- see that function's own doc comment).
//! - Task 4.4 (`Boundary: AccountCountsProviderImpl`):
//!   accounts-and-instance's `AccountCountsProvider` delegation-boundary
//!   implementation for the `followers`/`following` fields
//!   ([`providers::AccountCountsProviderImpl`]), backed by the
//!   already-implemented (task 1.3) [`repository::count_followers`]/
//!   [`repository::count_following`] -- see [`providers`]. `statuses`/
//!   `last_status_at` stay at accounts-and-instance's own zero/`None`
//!   defaults (out of this spec's boundary). No new repository query was
//!   needed for this task.
//!
//! - Task 5.1 (`Boundary: SocialGraphEndpoints`): the nine HTTP handlers
//!   ([`endpoints::follow`]/[`endpoints::unfollow`]/
//!   [`endpoints::list_follow_requests`]/[`endpoints::authorize_follow_request`]/
//!   [`endpoints::reject_follow_request`]/[`endpoints::mute`]/
//!   [`endpoints::unmute`]/[`endpoints::block`]/[`endpoints::unblock`]) and
//!   their router-local state bundle ([`endpoints::SocialGraphEndpointsState`])
//!   — see [`endpoints`]. Not yet mounted onto any real router (task 5.2's
//!   own boundary).
//! - Task 5.2 (`Boundary: SocialGraphModule`): this module's own wiring/
//!   assembly point — [`build_social_graph_module`] builds every service
//!   this module's endpoints need ([`SocialGraphModule`]), and
//!   [`register_downstream_handlers`] builds the closure
//!   `federation::build_federation_module`'s own `register_downstream`
//!   parameter expects (mirrors `crate::statuses::register_downstream_handlers`'s
//!   identical precedent), registering [`inbound::SocialGraphInboundHandler`]
//!   against the live `InboundActivityDispatcher` at its one legal
//!   registration point (`src/federation/module.rs`'s own doc comment,
//!   "DOWNSTREAM DISPATCHER REGISTRATION POINT"). [`build_social_graph_module`]
//!   additively registers [`providers::BlockPolicyImpl`] against
//!   federation-core's live [`crate::federation::BlockPolicyRegistry`]
//!   (replacing `NoopBlockPolicy`), [`providers::RelProviderImpl`] against
//!   accounts-and-instance's `RelationshipStateProvider` registry (replacing
//!   `NoRelationshipProvider`), and a [`CombinedAccountCountsProvider`]
//!   (composing [`providers::AccountCountsProviderImpl`]'s own
//!   `followers`/`following` with statuses-core's own
//!   `AccountCountsContribution`'s `statuses`/`last_status_at` — see that
//!   type's own doc comment for why a composing wrapper, not either
//!   registrant alone, is required) against accounts-and-instance's
//!   `AccountCountsProvider` registry (replacing `ZeroCountsProvider`). See
//!   [`PendingDeliveryService`]'s own doc comment for how this task's own
//!   "the inbound handler must be constructed and registered *before*
//!   `federation::build_federation_module` has itself built a
//!   `DeliveryService` to hand back" ordering problem (named in task 4.1's
//!   own Implementation Note) is resolved. `src/state.rs`/`src/bootstrap.rs`/
//!   `src/test_harness.rs`/`src/federation/test_harness.rs`/`src/server.rs`
//!   call these two functions to actually wire this spec into the running
//!   application — see each of those files' own doc comments at their call
//!   sites.

pub mod activity_builder;
pub mod approval_policy;
pub mod block_service;
pub mod endpoints;
pub mod follow_request_service;
pub mod follow_service;
pub mod inbound;
pub mod model;
pub mod mute_service;
pub mod providers;
pub mod relationship_mapper;
pub mod repository;
pub mod transitions;

pub use activity_builder::{
    ActivityBuilder, LocalActorLookup, PgRemoteActorLookup, RemoteActorLookup,
};
pub use approval_policy::{FollowApprovalPolicy, FollowDecision};
pub use block_service::BlockService;
pub use follow_request_service::FollowRequestService;
pub use follow_service::FollowService;
pub use inbound::{ActorUriResolver, ProdActorUriResolver, SocialGraphInboundHandler};
pub use model::{
    Block, Follow, FollowOptions, FollowRequest, FollowRequestDirection, Mute, MuteOptions,
};
pub use mute_service::MuteService;
pub use providers::{
    AccountCountsProviderImpl, BlockPolicyImpl, FilterQuery, RelProviderImpl, RelationshipSets,
};
pub use relationship_mapper::RelationshipMapper;
pub use transitions::Transitions;

// ---- Task 5.2 (Boundary: SocialGraphModule): module wiring --------------

#[cfg(test)]
mod tests;

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

use sqlx::PgPool;

use crate::accounts::RemoteAccountFetcher;
use crate::accounts::account_service::AccountService;
use crate::accounts::model::AccountCounts;
use crate::accounts::ports::{AccountCountsProvider, AccountPortsRegistry};
use crate::actor::ActorDirectory;
use crate::domain::AccountRef;
use crate::error::AppError;
use crate::federation::inbound::BlockPolicyRegistry;
use crate::federation::signatures::ReqwestFederationHttpClient;
use crate::federation::urls::ActorUrls;
use crate::federation::{
    ConcreteBlockPolicy, ConcreteDeliveryService, ConcreteReceivedActivityStore, ConcreteVerifier,
    DbDeliveryQueue, DeliveryRequest, HttpDeliverySink, InboundActivityDispatcher,
    LocalDeliverySink,
};
use crate::media::local_fs::LocalFsStore;
use crate::runtime::RuntimeContext;
use crate::social_graph::inbound::BoxedDeliver;
use crate::statuses::account_provider::AccountCountsContribution;
use crate::statuses::notification_sink::NotificationSinkRegistry;

/// This crate's one concrete in-process `DeliverySink` instantiation for
/// this module's own services -- matches `crate::federation::ConcreteDeliveryService`'s
/// own `L` argument exactly (mirrors `crate::statuses::ConcreteLocalSink`'s
/// identical rationale, `src/statuses.rs`), so
/// `crate::federation::FederationModule::delivery_service`'s
/// `Arc<ConcreteDeliveryService>` can be passed to this module's services
/// unmodified.
pub type ConcreteLocalSink =
    LocalDeliverySink<ConcreteVerifier, ConcreteBlockPolicy, ConcreteReceivedActivityStore>;

/// This crate's one concrete HTTP/queue-backed `DeliverySink` instantiation
/// for this module's own services -- matches `ConcreteDeliveryService`'s own
/// `H` argument exactly.
pub type ConcreteHttpSink = HttpDeliverySink<DbDeliveryQueue, ActorDirectory>;

/// The one concrete `ActivityBuilder` instantiation this instance mounts
/// every service with: `ActorDirectory` for `L`, [`PgRemoteActorLookup`] for
/// `R`.
pub type ConcreteActivityBuilder = ActivityBuilder<ActorDirectory, PgRemoteActorLookup>;

/// The one concrete `FollowService` instantiation this instance mounts.
pub type ConcreteFollowService = FollowService<
    ActorDirectory,
    PgRemoteActorLookup,
    ActorDirectory,
    ConcreteLocalSink,
    ConcreteHttpSink,
>;

/// The one concrete `FollowRequestService` instantiation this instance
/// mounts.
pub type ConcreteFollowRequestService = FollowRequestService<
    ActorDirectory,
    PgRemoteActorLookup,
    ActorDirectory,
    ConcreteLocalSink,
    ConcreteHttpSink,
>;

/// The one concrete `MuteService` instantiation this instance mounts.
pub type ConcreteMuteService = MuteService<ActorDirectory>;

/// The one concrete `BlockService` instantiation this instance mounts.
pub type ConcreteBlockService = BlockService<
    ActorDirectory,
    PgRemoteActorLookup,
    ActorDirectory,
    ConcreteLocalSink,
    ConcreteHttpSink,
>;

/// The one concrete `SocialGraphInboundHandler` instantiation this instance
/// registers against the live `InboundActivityDispatcher`.
pub type ConcreteSocialGraphInboundHandler =
    SocialGraphInboundHandler<ActorDirectory, PgRemoteActorLookup, ProdActorUriResolver>;

/// The one concrete `BlockPolicyImpl` instantiation this instance registers
/// against federation-core's [`BlockPolicyRegistry`].
pub type ConcreteBlockPolicyImpl = BlockPolicyImpl<ProdActorUriResolver>;

/// The one concrete `SocialGraphEndpointsState` instantiation `src/server.rs`
/// mounts every handler in [`endpoints`] with.
pub type ConcreteSocialGraphEndpointsState = endpoints::SocialGraphEndpointsState<
    ActorDirectory,
    PgRemoteActorLookup,
    ActorDirectory,
    ConcreteLocalSink,
    ConcreteHttpSink,
>;

/// The shared service bundle `AppState` stores (design.md's "Modified
/// Files" entry for `src/state.rs`): `FollowService`/`FollowRequestService`/
/// `MuteService`/`BlockService`, the same "one concrete instantiation,
/// `Arc`-cloned accessors" shape `crate::statuses::StatusesModule` already
/// established. Built by [`build_social_graph_module`].
pub struct SocialGraphModule {
    follow: Arc<ConcreteFollowService>,
    follow_requests: Arc<ConcreteFollowRequestService>,
    mute: Arc<ConcreteMuteService>,
    block: Arc<ConcreteBlockService>,
}

impl SocialGraphModule {
    /// The shared `FollowService` handle.
    pub fn follow(&self) -> Arc<ConcreteFollowService> {
        Arc::clone(&self.follow)
    }

    /// The shared `FollowRequestService` handle.
    pub fn follow_requests(&self) -> Arc<ConcreteFollowRequestService> {
        Arc::clone(&self.follow_requests)
    }

    /// The shared `MuteService` handle.
    pub fn mute(&self) -> Arc<ConcreteMuteService> {
        Arc::clone(&self.mute)
    }

    /// The shared `BlockService` handle.
    pub fn block(&self) -> Arc<ConcreteBlockService> {
        Arc::clone(&self.block)
    }
}

/// Composes this spec's own [`providers::AccountCountsProviderImpl`]
/// (`followers`/`following`, Boundary Commitments) with statuses-core's own
/// `AccountCountsContribution` (`statuses`/`last_status_at`, statuses-core
/// task 9.1) into the single `AccountCountsProvider` value
/// [`build_social_graph_module`] registers.
///
/// `AccountPortsRegistry` holds exactly one replaceable slot per port, never
/// a fan-out/merge of several registrants (`crate::accounts::ports`'s own
/// doc comment, "Registry shape") -- and both
/// `AccountCountsProviderImpl::counts`/`AccountCountsContribution::counts`
/// honestly zero the sub-count fields each does not own (either type's own
/// doc comment). Registering either one alone *after* the other has already
/// registered would therefore silently clobber (zero out) the other's real
/// values -- e.g. this spec registering its own `AccountCountsProviderImpl`
/// directly, after `crate::statuses::register_account_ports` has already
/// registered `AccountCountsContribution`, would make `statuses`/
/// `last_status_at` revert to `0`/`None` even though a real value was
/// already being supplied. This wrapper is this task's own minimal,
/// additive fix (design.md's own Boundary Commitments note this exact risk:
/// "この registry がこのタスクを妨げないよう" composition guidance) -- it
/// queries both and merges the two non-overlapping field halves into one
/// `AccountCounts`, and is itself the *only* value ever registered into the
/// `AccountCountsProvider` slot once this module has run.
struct CombinedAccountCountsProvider {
    social: AccountCountsProviderImpl,
    statuses: AccountCountsContribution,
}

impl AccountCountsProvider for CombinedAccountCountsProvider {
    fn counts<'a>(
        &'a self,
        target: &'a AccountRef,
    ) -> Pin<Box<dyn Future<Output = Result<AccountCounts, AppError>> + Send + 'a>> {
        Box::pin(async move {
            let social = self.social.counts(target).await?;
            let statuses = self.statuses.counts(target).await?;
            Ok(AccountCounts {
                followers: social.followers,
                following: social.following,
                statuses: statuses.statuses,
                last_status_at: statuses.last_status_at,
            })
        })
    }
}

/// A not-yet-resolved handle to this instance's own concrete
/// `Arc<ConcreteDeliveryService>` (task 5.2) -- see
/// [`register_downstream_handlers`]'s own doc comment for why this exists:
/// [`SocialGraphInboundHandler`] must be constructed and registered against
/// the live `InboundActivityDispatcher` *before*
/// `federation::build_federation_module` has itself constructed a
/// `DeliveryService` to hand back (dispatcher registration happens strictly
/// before delivery construction inside that function's own body -- see
/// `src/federation/module.rs`'s own doc comment, "DOWNSTREAM DISPATCHER
/// REGISTRATION POINT"). The [`BoxedDeliver`] closure
/// [`register_downstream_handlers`] builds captures a clone of this cell's
/// inner `Arc<OnceLock<_>>` and reads it *lazily*, only when an inbound
/// Activity is actually dispatched -- strictly after the whole composition
/// sequence (and therefore [`Self::resolve`]) has already run and the
/// listener has started serving, so no real request can ever observe an
/// unresolved cell.
pub struct PendingDeliveryService(Arc<OnceLock<Arc<ConcreteDeliveryService>>>);

impl PendingDeliveryService {
    /// Fills in the real, now-constructed delivery service. The composition
    /// root (`src/bootstrap.rs`/`src/test_harness.rs`/
    /// `src/federation/test_harness.rs`) calls this immediately after
    /// `federation::build_federation_module` returns, strictly before the
    /// listener starts serving any request.
    ///
    /// # Panics
    /// If called more than once (a composition-root programming error, not
    /// a runtime condition any caller should recover from).
    pub fn resolve(self, delivery: Arc<ConcreteDeliveryService>) {
        self.0.set(delivery).unwrap_or_else(|_| {
            panic!("PendingDeliveryService::resolve must be called at most once")
        });
    }
}

/// Builds a [`BoxedDeliver`] backed by a not-yet-resolved
/// [`PendingDeliveryService`] -- see that type's own doc comment.
fn pending_boxed_deliver() -> (BoxedDeliver, PendingDeliveryService) {
    let cell: Arc<OnceLock<Arc<ConcreteDeliveryService>>> = Arc::new(OnceLock::new());
    let cell_for_closure = Arc::clone(&cell);
    let deliver: BoxedDeliver = Arc::new(move |req: DeliveryRequest| {
        let cell = Arc::clone(&cell_for_closure);
        Box::pin(async move {
            let delivery = cell.get().cloned().unwrap_or_else(|| {
                panic!(
                    "SocialGraphModule's delivery cell must be resolved (via \
                     PendingDeliveryService::resolve) before any inbound Activity is dispatched"
                )
            });
            delivery.deliver(req).await
        }) as Pin<Box<dyn Future<Output = Result<(), AppError>> + Send>>
    });
    (deliver, PendingDeliveryService(cell))
}

/// Builds the registration closure `crate::federation::build_federation_module`'s
/// own `register_downstream` parameter expects (task 5.2, Requirement 7.1):
/// registers [`ConcreteSocialGraphInboundHandler`] against the live
/// dispatcher, using [`ProdActorUriResolver`] as the production
/// [`ActorUriResolver`] and the concrete [`ActorDirectory`]/
/// [`PgRemoteActorLookup`] pair every other service in this module uses.
///
/// Mirrors `crate::statuses::register_downstream_handlers`'s identical
/// shape and calling convention (`src/bootstrap.rs`/`src/test_harness.rs`/
/// `src/federation/test_harness.rs` compose both specs' own registration
/// closures into the *same* `register_downstream` call, per
/// `src/federation/module.rs`'s own doc comment) with one structural
/// addition: because [`ConcreteSocialGraphInboundHandler`] must reply with
/// `Accept`/`Reject` Activities (unlike statuses-core's own inbound
/// handlers, which never reply), it needs a live `deliver: BoxedDeliver` at
/// construction time -- but `federation::build_federation_module` only
/// constructs its own `DeliveryService` *after* this registration closure
/// has already run (see [`PendingDeliveryService`]'s own doc comment). This
/// function therefore also returns the not-yet-resolved
/// [`PendingDeliveryService`] handle; the caller must call
/// [`PendingDeliveryService::resolve`] with
/// `federation_module.delivery_service()`'s own `Arc` clone immediately
/// after `build_federation_module` returns.
pub fn register_downstream_handlers(
    pool: PgPool,
    runtime: RuntimeContext,
    domain: impl Into<String>,
    directory: Arc<ActorDirectory>,
    remote_actor_fetcher: Arc<RemoteAccountFetcher<ReqwestFederationHttpClient>>,
    notifications: NotificationSinkRegistry,
) -> (
    impl FnOnce(&mut InboundActivityDispatcher),
    PendingDeliveryService,
) {
    let domain = domain.into();
    let urls = ActorUrls::new(domain.clone());

    let activity_builder = ConcreteActivityBuilder::new(
        urls,
        runtime.ids.clone(),
        ActorDirectory::new(pool.clone()),
        PgRemoteActorLookup::new(pool.clone()),
    );
    let transitions = Transitions::new(pool.clone(), runtime.clone(), notifications);
    let actor_uris =
        ProdActorUriResolver::new(domain, Arc::clone(&directory), remote_actor_fetcher);
    let (deliver, pending) = pending_boxed_deliver();

    let handler: ConcreteSocialGraphInboundHandler = SocialGraphInboundHandler::new(
        pool.clone(),
        runtime,
        ActorDirectory::new(pool),
        activity_builder,
        transitions,
        deliver,
        actor_uris,
    );

    let register = move |dispatcher: &mut InboundActivityDispatcher| {
        dispatcher.register(Arc::new(handler));
    };

    (register, pending)
}

/// Assembles the [`SocialGraphModule`] bundle (task 5.2, Requirements 6.1,
/// 7.1, 8.2, 10.1): builds every service this module's own endpoints need,
/// and additively registers this spec's own real implementations against
/// federation-core's [`BlockPolicyRegistry`] and accounts-and-instance's
/// `RelationshipStateProvider`/`AccountCountsProvider` registries --
/// replacing their built-in `NoopBlockPolicy`/`NoRelationshipProvider`/
/// `ZeroCountsProvider` defaults (see [`CombinedAccountCountsProvider`]'s
/// own doc comment for the counts-registration composition this requires).
///
/// Must run *after* both `federation_module` (for `delivery_service()`/
/// `block_policy()`) and `accounts_module` (for `ports()`/`service()`)
/// already exist, and after `crate::statuses::register_account_ports` has
/// already run (so this function's own `account_ports.set_counts_provider`
/// call is the *last* one, and therefore wins) -- mirrors
/// `crate::statuses::register_account_ports`'s identical "runs after both
/// dependencies are already built" ordering constraint. `directory`/
/// `remote_actor_fetcher` should be the exact same instances
/// [`register_downstream_handlers`] was called with, so
/// [`ConcreteBlockPolicyImpl`]'s own signer resolution shares the same
/// `RemoteAccountFetcher` cache the inbound handler's own resolver does.
#[allow(clippy::too_many_arguments)]
pub fn build_social_graph_module(
    pool: PgPool,
    runtime: RuntimeContext,
    domain: impl Into<String>,
    directory: Arc<ActorDirectory>,
    remote_actor_fetcher: Arc<RemoteAccountFetcher<ReqwestFederationHttpClient>>,
    delivery: Arc<ConcreteDeliveryService>,
    block_policy_registry: &BlockPolicyRegistry,
    account_ports: AccountPortsRegistry,
    accounts: Arc<AccountService<LocalFsStore, ReqwestFederationHttpClient>>,
    notifications: NotificationSinkRegistry,
) -> SocialGraphModule {
    let domain = domain.into();
    let urls = ActorUrls::new(domain.clone());

    let follow = Arc::new(ConcreteFollowService::new(
        pool.clone(),
        runtime.clone(),
        ActorDirectory::new(pool.clone()),
        ConcreteActivityBuilder::new(
            urls.clone(),
            runtime.ids.clone(),
            ActorDirectory::new(pool.clone()),
            PgRemoteActorLookup::new(pool.clone()),
        ),
        Transitions::new(pool.clone(), runtime.clone(), notifications.clone()),
        Arc::clone(&delivery),
    ));

    let follow_requests = Arc::new(ConcreteFollowRequestService::new(
        pool.clone(),
        runtime.clone(),
        ActorDirectory::new(pool.clone()),
        ConcreteActivityBuilder::new(
            urls.clone(),
            runtime.ids.clone(),
            ActorDirectory::new(pool.clone()),
            PgRemoteActorLookup::new(pool.clone()),
        ),
        Transitions::new(pool.clone(), runtime.clone(), notifications.clone()),
        Arc::clone(&delivery),
        Arc::clone(&accounts),
        domain.clone(),
    ));

    let mute = Arc::new(ConcreteMuteService::new(
        pool.clone(),
        runtime.clone(),
        ActorDirectory::new(pool.clone()),
    ));

    let block = Arc::new(ConcreteBlockService::new(
        pool.clone(),
        runtime.clone(),
        ActorDirectory::new(pool.clone()),
        ConcreteActivityBuilder::new(
            urls,
            runtime.ids.clone(),
            ActorDirectory::new(pool.clone()),
            PgRemoteActorLookup::new(pool.clone()),
        ),
        Transitions::new(pool.clone(), runtime.clone(), notifications),
        delivery,
    ));

    // Requirement 6.1: BlockPolicyImpl -> federation-core's live
    // BlockPolicyRegistry, replacing NoopBlockPolicy.
    let block_policy_actor_uris = ProdActorUriResolver::new(
        domain.clone(),
        Arc::clone(&directory),
        Arc::clone(&remote_actor_fetcher),
    );
    let block_policy_impl: ConcreteBlockPolicyImpl = BlockPolicyImpl::new(
        pool.clone(),
        domain,
        Arc::clone(&directory),
        block_policy_actor_uris,
    );
    block_policy_registry.set_policy(block_policy_impl);

    // Requirement 8.2: RelProviderImpl -> accounts-and-instance's
    // RelationshipStateProvider registry, replacing NoRelationshipProvider.
    account_ports.set_relationship_provider(Arc::new(RelProviderImpl::new(
        pool.clone(),
        runtime.clone(),
    )));

    // Boundary Commitments: the composed AccountCountsProvider (see
    // CombinedAccountCountsProvider's own doc comment) -> accounts-and-
    // instance's AccountCountsProvider registry. This call must be the last
    // registration into this particular slot (see this function's own doc
    // comment).
    account_ports.set_counts_provider(Arc::new(CombinedAccountCountsProvider {
        social: AccountCountsProviderImpl::new(pool.clone()),
        statuses: AccountCountsContribution::new(pool),
    }));

    SocialGraphModule {
        follow,
        follow_requests,
        mute,
        block,
    }
}
