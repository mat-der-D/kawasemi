//! `FederationModule` (design.md `#### FederationModule（wiring）`; task 5.4,
//! `Boundary: FederationModule, Bootstrap, AppState, Config`): the
//! composition-root wiring that constructs every federation-core port with
//! one concrete production type, bundles them behind a single handle
//! `AppState` stores, and starts the delivery-worker/pruning background
//! tasks.
//!
//! ## Scope
//! This module owns [`FederationModule`] itself, [`FederationWiringConfig`]
//! (the small config bundle [`build_federation_module`] needs beyond a
//! `PgPool`/`RuntimeContext`/`Arc<ActorDirectory>`), and
//! [`build_federation_module`]/[`FederationBackgroundTasks`] — the shared
//! composition function `src/bootstrap.rs` (production) and
//! `src/test_harness.rs` (`spawn_test_app`) both call, mirroring
//! `crate::actor::build_actor_module`'s identical "one composition function,
//! two callers" precedent. It does not implement any of the ports it wires
//! (`SignatureVerifier`, `PublicKeyResolver`, `BlockPolicy`,
//! `InboundActivityDispatcher`, `ObjectDocumentProvider`/`OutboxSource`
//! registries, `DeliveryService`/`DeliverySink`, `DeliveryQueue`/
//! `DeliveryWorker`) — all of those are already-implemented, already-tested
//! dependencies (tasks 1.x-4.3) this module only assembles.
//!
//! ## One concrete type per non-`dyn`-safe trait (the reason this task exists)
//! Every trait this spec defines that is consumed generically elsewhere in
//! this crate (`SignatureVerifier`, `PublicKeyResolver`,
//! `FederationHttpClient`, `BlockPolicy`, `ReceivedActivityStore`,
//! `DeliveryQueue`, `LocalActorLookup`, `DeliverySink`) is a literal
//! `#[allow(async_fn_in_trait)]` `async fn` trait, none `dyn`-compatible
//! (every one of tasks 1.4-4.3's own Implementation Notes documents this
//! same E0038 constraint independently). Every endpoint handler this spec
//! already implemented (tasks 5.1-5.3) is therefore generic over these
//! traits (`ApGetState<V>`, `InboxState<V, B, D>`) rather than holding
//! `Arc<dyn _>`, and axum requires a route to be mounted with one concrete,
//! monomorphized type. This module is exactly where those concrete type
//! arguments are finally chosen:
//! - `V` (`SignatureVerifier`) = [`ConcreteVerifier`] =
//!   `HttpSignatureVerifier<DbFederationPublicKeyResolver<ReqwestFederationHttpClient>>`
//!   — the production HTTP client backs both public-key resolution and
//!   outbound signing/negotiation.
//! - `B` (`BlockPolicy`) = [`ConcreteBlockPolicy`] = `NoopBlockPolicy` — this
//!   task's own text names exactly this as the port to wire
//!   ("既定ブロックポリシー"); a real block-graph-backed implementation is
//!   social-graph's (a later spec's) job.
//! - `D` (`ReceivedActivityStore`) = [`ConcreteReceivedActivityStore`] =
//!   `DbReceivedActivityStore`.
//! - `DeliveryQueue` = `DbDeliveryQueue`; the delivery-common-part's
//!   `LocalActorLookup` = `crate::actor::ActorDirectory` (implements it
//!   directly, task 3.4's own `impl LocalActorLookup for ActorDirectory`);
//!   `FederationHttpClient` = `ReqwestFederationHttpClient`.
//!
//! `src/server.rs` mounts every generic endpoint handler with exactly these
//! type arguments (e.g. `actor_get::<ConcreteVerifier>`,
//! `actor_inbox::<ConcreteVerifier, ConcreteBlockPolicy,
//! ConcreteReceivedActivityStore>`), never with a second, different
//! instantiation — there is exactly one live federation wiring per running
//! instance.
//!
//! ## Downstream registration surface (design.md's completion condition:
//! "下流がディスパッチャ・`ObjectDocumentProvider`・`OutboxSource` へ登録、
//! 配送サービスへ配送依頼できる")
//! `AppState`/`FederationModule` is immutable-after-construction (per this
//! crate's steering: "不変（起動時構築、以後共有のみ）"), yet
//! `InboundActivityDispatcher::register`/`ObjectDocumentRegistry::register`/
//! `OutboxSourceRegistry::register` all originally took `&mut self`
//! (tasks 3.2/3.5). This module resolves that tension, but not uniformly —
//! the three ports split into two different resolutions depending on what
//! this task's boundary permits touching:
//!
//! - **`ObjectDocumentRegistry` / `OutboxSourceRegistry`**: genuinely
//!   live-mutable after `AppState` is already serving. `document.rs` (task
//!   3.5/3.6's boundary file, but *not* on this task's forbidden-files list
//!   — see this task's own brief) was given a narrow, behavior-preserving
//!   interior-mutability upgrade: each registry's `Vec` now lives behind
//!   `Arc<RwLock<_>>`, `register` takes `&self`, and the registry itself is
//!   now `Clone` (cloning only bumps the inner `Arc`). This lets
//!   [`FederationModule`] hold one clone for downstream registration
//!   ([`FederationModule::object_documents`]/
//!   [`FederationModule::outbox_sources`]) while a second clone/`Arc`-wrap
//!   is what `ApGetState`/`ActivityPubDocumentBuilder` actually query — both
//!   observe the same live list. This task's own integration test
//!   (`tests/federation_bootstrap_it.rs`) proves this genuinely works: a
//!   provider/source registered *after* `spawn_test_app()` returns is
//!   observably picked up by a subsequent request through the real router.
//! - **`InboundActivityDispatcher`**: **not** live-mutable after this
//!   module is constructed, and this is a real, load-bearing limitation
//!   this task must document rather than paper over. `InboundActivityDispatcher`
//!   itself (`inbound/dispatcher.rs`) and `InboxService`
//!   (`inbound/service.rs`, which stores its dispatcher as a private, owned
//!   `InboundActivityDispatcher` field with **no accessor** at all) are both
//!   on this task's explicit forbidden-files list ("Do NOT modify
//!   `src/federation/inbound/service.rs`... `dispatcher.rs`..."). Applying
//!   the same `Arc<RwLock<_>>`/`Clone` treatment `ObjectDocumentRegistry`
//!   received is therefore structurally unreachable without violating this
//!   task's own boundary: even if `InboundActivityDispatcher::register`
//!   became `&self`-callable through a shared `Arc`, `InboxService` gives no
//!   way to reach the *specific* dispatcher instance it holds once
//!   constructed — there is no `InboxService::dispatcher()` accessor to
//!   share a clone through in the first place.
//!
//!   The resolution actually adopted: dispatcher registration happens
//!   **only** at composition-root wiring time, before `InboxService::new`
//!   ever runs — see the explicit `// DOWNSTREAM DISPATCHER REGISTRATION
//!   POINT` comment inside [`build_federation_module`]'s body. A future
//!   federation-dependent spec's own bootstrap-wiring task (statuses-core,
//!   social-graph, etc. — each will have its own `_Boundary: ..., Bootstrap,
//!   ...` task per this spec's established convention, exactly as this
//!   task's own boundary includes `Bootstrap`) is expected to extend *this*
//!   function directly, inserting its own `dispatcher.register(Arc::new(..))`
//!   calls at that marked point, mirroring how `src/bootstrap.rs`'s
//!   `build_actor_wiring`/`OauthModule::new` composition steps are
//!   themselves already extended by each spec that needs to. This is a
//!   real, narrower guarantee than "downstream can register at any time
//!   after startup" — it is "downstream can register by extending the
//!   composition root, before this instance starts serving" — and is
//!   flagged as a CONCERN in this task's own status report, not silently
//!   presented as equivalent to the other two ports' live-mutability.

use std::sync::Arc;
use std::time::Duration as StdDuration;

use sqlx::PgPool;
use time::Duration as TimeDuration;

use crate::actor::ActorDirectory;
use crate::runtime::RuntimeContext;

use super::InboxState;
use super::endpoints::{
    ActivityPubDocumentBuilder, ApGetState, NodeInfoState, ObjectDocumentRegistry,
    OutboxSourceRegistry, OutboxState, WebfingerState,
};
use super::inbound::{
    DbReceivedActivityStore, InboundActivityDispatcher, InboxService, NoopBlockPolicy,
    ReceivedActivityStore,
};
use super::outbound::{
    DbDeliveryQueue, DeliveryService, DeliveryWorker, HttpDeliverySink, LocalDeliverySink,
    RecipientTargetResolver, WorkerRunSummary,
};
use super::signatures::{
    DEFAULT_SIGNATURE_MAX_AGE, DbFederationPublicKeyResolver, HttpSignatureVerifier, RequestSigner,
    ReqwestFederationHttpClient, SignatureNegotiator,
};
use super::urls::ActorUrls;

/// The one concrete [`super::signatures::SignatureVerifier`] implementation
/// this instance mounts every verifier-generic endpoint/service with. See
/// this module's doc comment ("One concrete type per non-`dyn`-safe trait").
pub type ConcreteVerifier =
    HttpSignatureVerifier<DbFederationPublicKeyResolver<ReqwestFederationHttpClient>>;

/// The one concrete [`super::inbound::BlockPolicy`] implementation this
/// instance mounts with — the "既定ブロックポリシー" this task's own text
/// names. A real block-graph-backed implementation is social-graph's (a
/// later spec's) job.
pub type ConcreteBlockPolicy = NoopBlockPolicy;

/// The one concrete [`super::inbound::ReceivedActivityStore`] implementation
/// this instance mounts with.
pub type ConcreteReceivedActivityStore = DbReceivedActivityStore;

/// The one concrete [`super::inbound::InboxService`] instantiation this
/// instance mounts every inbox-adjacent endpoint/sink with.
pub type ConcreteInboxService =
    InboxService<ConcreteVerifier, ConcreteBlockPolicy, ConcreteReceivedActivityStore>;

/// The one concrete [`super::outbound::DeliveryService`] instantiation this
/// instance mounts with: local delivery hands off in-process to
/// [`ConcreteInboxService`]; remote delivery enqueues onto [`DbDeliveryQueue`].
pub type ConcreteDeliveryService = DeliveryService<
    ActorDirectory,
    LocalDeliverySink<ConcreteVerifier, ConcreteBlockPolicy, ConcreteReceivedActivityStore>,
    HttpDeliverySink<DbDeliveryQueue, ActorDirectory>,
>;

/// The federation-related startup settings [`build_federation_module`] needs
/// beyond a `PgPool`/`RuntimeContext`/`Arc<ActorDirectory>` (task 5.4,
/// Requirements 7.3, 10.1, 11.1, 11.2): the values `crate::config::AppConfig`
/// now carries (`server.domain`, `federation.*`), plus the background-task
/// cadence this task itself must choose (design.md names no numeric value
/// for either — see [`FederationWiringConfig::production`]'s doc comment).
#[derive(Debug, Clone, PartialEq)]
pub struct FederationWiringConfig {
    /// This instance's own public domain (`crate::config::ServerConfig::domain`),
    /// the same value [`ActorUrls`] builds every URL from.
    pub domain: String,
    /// Whether authorized fetch is required for ActivityPub GETs
    /// (`crate::config::FederationConfig::secure_mode`, Requirement 6.4).
    pub secure_mode: bool,
    /// Remote public-key cache validity window
    /// (`crate::config::FederationConfig::public_key_cache_ttl`, converted
    /// to `time::Duration` here since federation-core's own components take
    /// `time::Duration` throughout — `crate::config` stays on
    /// `std::time::Duration` for consistency with its own existing fields
    /// and to avoid pulling `time` into that foundational module's own
    /// public surface).
    pub public_key_cache_ttl: TimeDuration,
    /// Received-Activity retention window
    /// (`crate::config::FederationConfig::received_activity_retention_days`,
    /// converted to `time::Duration`).
    pub received_activity_retention: TimeDuration,
    /// How often the delivery-worker background loop polls
    /// [`super::outbound::DeliveryQueue::claim_due`] for due jobs. Not a
    /// startup-config value (design.md names no config key for it, and this
    /// task's own text only calls out secure-mode/retry-policy/TTL/retention
    /// as config surface) — a plain constructor value so
    /// `src/test_harness.rs` can use a much shorter interval than
    /// production's for fast integration tests
    /// ([`FederationWiringConfig::production`] documents the production
    /// default).
    pub delivery_poll_interval: StdDuration,
    /// How many due delivery jobs one poll iteration claims at once (passed
    /// straight through to [`DeliveryWorker::run_once`]).
    pub delivery_poll_batch_size: i64,
    /// How often the received-Activity pruning background loop calls
    /// [`super::inbound::ReceivedActivityStore::prune_expired`]. See
    /// [`Self::delivery_poll_interval`]'s doc comment for why this is a
    /// plain constructor value, not startup config.
    pub pruning_interval: StdDuration,
}

/// Production default for [`FederationWiringConfig::delivery_poll_interval`]:
/// 5 seconds. No requirement pins a numeric value; this is short enough that
/// a queued delivery is sent promptly on a single-owner deployment's
/// low-volume traffic, while not polling so aggressively that an idle
/// instance burns meaningful database load doing so.
pub const DEFAULT_DELIVERY_POLL_INTERVAL: StdDuration = StdDuration::from_secs(5);

/// Production default for [`FederationWiringConfig::delivery_poll_batch_size`]:
/// 20 jobs per poll iteration, comfortably larger than a single-owner
/// instance's typical fan-out per Activity while still bounding one poll
/// iteration's work.
pub const DEFAULT_DELIVERY_POLL_BATCH_SIZE: i64 = 20;

/// Production default for [`FederationWiringConfig::pruning_interval`]: 1
/// hour. `received_activities`' own retention window defaults to 14 days
/// (`DEFAULT_RECEIVED_ACTIVITY_RETENTION`), so hourly pruning is far more
/// frequent than strictly necessary to keep the table bounded, while still
/// cheap (a single `DELETE ... WHERE received_at < cutoff` per hour) and
/// simple to reason about.
pub const DEFAULT_PRUNING_INTERVAL: StdDuration = StdDuration::from_secs(60 * 60);

impl FederationWiringConfig {
    /// Builds a [`FederationWiringConfig`] from validated startup config
    /// (`crate::config::AppConfig`'s `server.domain`/`federation.*`) plus
    /// this module's own production background-task cadence defaults. The
    /// one constructor `src/bootstrap.rs`'s production path uses;
    /// `src/test_harness.rs` builds a [`FederationWiringConfig`] directly
    /// instead, with a much shorter `delivery_poll_interval` so integration
    /// tests observing delivery completion do not need to wait several
    /// seconds per assertion.
    pub fn production(
        domain: String,
        secure_mode: bool,
        public_key_cache_ttl: TimeDuration,
        received_activity_retention: TimeDuration,
    ) -> Self {
        Self {
            domain,
            secure_mode,
            public_key_cache_ttl,
            received_activity_retention,
            delivery_poll_interval: DEFAULT_DELIVERY_POLL_INTERVAL,
            delivery_poll_batch_size: DEFAULT_DELIVERY_POLL_BATCH_SIZE,
            pruning_interval: DEFAULT_PRUNING_INTERVAL,
        }
    }
}

/// The federation-core port bundle `AppState` shares across concurrent
/// request handlers (design.md's exact `FederationModule(wiring)` component;
/// Requirements 7.3, 10.1, 11.1, 11.2). See this module's doc comment for
/// the concrete-type choices and the downstream-registration surface each
/// field participates in.
///
/// This type does not construct its own dependencies — building the real
/// `PgPool`/`RuntimeContext`/`Arc<ActorDirectory>` and wiring every port
/// together is [`build_federation_module`]'s job (mirrors `AppState::new`'s
/// own "bundle, don't build" contract, and `ActorModule::new`'s identical
/// precedent).
pub struct FederationModule {
    domain: String,
    secure_mode: bool,
    urls: ActorUrls,
    directory: Arc<ActorDirectory>,
    verifier: Arc<ConcreteVerifier>,
    document_builder: Arc<ActivityPubDocumentBuilder>,
    object_documents: ObjectDocumentRegistry,
    outbox_sources: OutboxSourceRegistry,
    inbox: Arc<ConcreteInboxService>,
    delivery: Arc<ConcreteDeliveryService>,
}

impl FederationModule {
    /// Builds the [`WebfingerState`] `src/server.rs`'s `FromRef<AppState>`
    /// bridge derives for the mounted WebFinger route.
    pub fn webfinger_state(&self) -> WebfingerState {
        WebfingerState {
            directory: Arc::clone(&self.directory),
            urls: self.urls.clone(),
            domain: self.domain.clone(),
        }
    }

    /// Builds the [`NodeInfoState`] for the mounted NodeInfo routes.
    pub fn nodeinfo_state(&self) -> NodeInfoState {
        NodeInfoState {
            domain: self.domain.clone(),
        }
    }

    /// Builds the [`ApGetState`] for the mounted actor/object GET routes,
    /// monomorphized over [`ConcreteVerifier`] (see this module's doc
    /// comment).
    pub fn ap_get_state(&self) -> ApGetState<ConcreteVerifier> {
        ApGetState {
            directory: Arc::clone(&self.directory),
            document_builder: Arc::clone(&self.document_builder),
            object_documents: Arc::new(self.object_documents.clone()),
            domain: self.domain.clone(),
            secure_mode: self.secure_mode,
            verifier: Arc::clone(&self.verifier),
        }
    }

    /// Builds the [`OutboxState`] for the mounted outbox GET route.
    pub fn outbox_state(&self) -> OutboxState {
        OutboxState {
            document_builder: Arc::clone(&self.document_builder),
        }
    }

    /// Builds the [`InboxState`] for the mounted inbox/shared-inbox POST
    /// routes, monomorphized over [`ConcreteVerifier`]/[`ConcreteBlockPolicy`]/
    /// [`ConcreteReceivedActivityStore`] (see this module's doc comment).
    pub fn inbox_state(
        &self,
    ) -> InboxState<ConcreteVerifier, ConcreteBlockPolicy, ConcreteReceivedActivityStore> {
        InboxState {
            inbox: Arc::clone(&self.inbox),
            urls: self.urls.clone(),
        }
    }

    /// The live, downstream-registrable local-object/collection supply
    /// registry (Requirement 6.2). A provider registered here (e.g. via
    /// `federation.object_documents().register(Arc::new(MyProvider))`) is
    /// observed by every subsequent `object_get` request through the real
    /// mounted router, even though it was registered after this
    /// `FederationModule`/`AppState` was already constructed — see this
    /// module's doc comment ("Downstream registration surface") for how.
    pub fn object_documents(&self) -> &ObjectDocumentRegistry {
        &self.object_documents
    }

    /// The live, downstream-registrable outbox-contents supply registry
    /// (Requirements 8.1, 8.2, 8.3). Same live-registration guarantee as
    /// [`Self::object_documents`].
    pub fn outbox_sources(&self) -> &OutboxSourceRegistry {
        &self.outbox_sources
    }

    /// The shared delivery entry point downstream business logic (a later
    /// spec's own service layer) calls to request delivery of an Activity to
    /// local and/or remote recipients (Requirement 10.1). See this module's
    /// doc comment ("Downstream registration surface") for why this port —
    /// unlike the dispatcher — needs no special registration-timing caveat:
    /// `DeliveryService::deliver` is itself the operation downstream calls,
    /// not a registry downstream registers into, so ordinary `Arc` sharing
    /// is already sufficient.
    pub fn delivery_service(&self) -> &Arc<ConcreteDeliveryService> {
        &self.delivery
    }
}

/// Assembles every federation-core port with one concrete production type,
/// bundles them as a [`FederationModule`], and returns the (not-yet-started)
/// background tasks [`FederationBackgroundTasks::spawn`] later starts (task
/// 5.4, Requirements 7.3, 10.1, 11.1, 11.2). Shared by `src/bootstrap.rs`
/// (production) and `src/test_harness.rs` (`spawn_test_app`), mirroring
/// `crate::actor::build_actor_module`'s identical "one composition function,
/// two callers" precedent — see this module's doc comment.
///
/// `pool`/`runtime` are already-established (this task never opens its own
/// pool or builds its own `RuntimeContext` — both are threaded through from
/// whichever stage of `build_state`/`spawn_test_app` already built them).
/// `directory` is the already-built `ActorModule`'s own `ActorDirectory`
/// handle (never reconstructed independently) — federation-core needs it for
/// handle resolution across several ports (`RequestSigner`, delivery's
/// target resolution/sender lookup, `DeliveryWorker`'s `Id -> Handle` gap
/// resolution). `http_client` is the already-constructed
/// [`ReqwestFederationHttpClient`] every caller must build for itself (task
/// 6.4, `Boundary: FederationTestHarness, federation_pair_it`): production
/// (`src/bootstrap.rs`) and `crate::test_harness::spawn_test_app` both build
/// one via [`ReqwestFederationHttpClient::new`] (this function itself used
/// to do exactly that internally, before this parameter existed — behavior
/// for those callers is unchanged), while
/// [`crate::federation::test_harness::spawn_federation_pair`] passes one
/// built via [`ReqwestFederationHttpClient::insecure_loopback`] instead, so
/// its two paired, plain-HTTP-served instances can actually reach each
/// other's `https://{domain}/...` URLs (see `insecure_loopback`'s own doc
/// comment).
pub fn build_federation_module(
    pool: PgPool,
    runtime: RuntimeContext,
    directory: Arc<ActorDirectory>,
    cfg: FederationWiringConfig,
    http_client: Arc<ReqwestFederationHttpClient>,
) -> (FederationModule, FederationBackgroundTasks) {
    let urls = ActorUrls::new(cfg.domain.clone());

    let key_resolver = Arc::new(DbFederationPublicKeyResolver::new(
        pool.clone(),
        Arc::clone(&http_client),
        runtime.clock.clone(),
        cfg.public_key_cache_ttl,
    ));

    // Two independent `HttpSignatureVerifier` instances sharing the same
    // underlying `Arc<DbFederationPublicKeyResolver<_>>` (and therefore the
    // same `remote_public_keys` cache state via the shared pool) -- one
    // moved into `InboxService` below (which owns its verifier by value,
    // per design.md's own Service Interface), one kept for the
    // authorized-fetch GET path (`ApGetState::verifier`, which calls
    // `SignatureVerifier::verify_request` directly, not through
    // `InboxService` -- see `ap_get.rs`'s own documented reasoning). Both
    // instances are behaviorally identical (the verifier itself holds no
    // per-call state beyond its shared resolver/clock), so this is not a
    // divergence risk.
    let inbox_verifier = HttpSignatureVerifier::new(
        Arc::clone(&key_resolver),
        runtime.clock.clone(),
        DEFAULT_SIGNATURE_MAX_AGE,
    );
    let ap_get_verifier = Arc::new(HttpSignatureVerifier::new(
        Arc::clone(&key_resolver),
        runtime.clock.clone(),
        DEFAULT_SIGNATURE_MAX_AGE,
    ));

    let signer = RequestSigner::new(
        Arc::clone(&directory),
        Arc::clone(&runtime.keys),
        urls.clone(),
        runtime.clock.clone(),
    );
    let negotiator = SignatureNegotiator::new(
        pool.clone(),
        Arc::clone(&http_client),
        signer,
        runtime.clock.clone(),
    );

    let dedup_for_inbox = DbReceivedActivityStore::new(
        pool.clone(),
        runtime.clock.clone(),
        cfg.received_activity_retention,
    );

    // DOWNSTREAM DISPATCHER REGISTRATION POINT: a future federation-
    // dependent spec's own bootstrap-wiring task registers its
    // `InboundActivityHandler` implementation(s) here, on this exact
    // `dispatcher` value, before it is moved into `InboxService::new`
    // below. See this module's doc comment ("Downstream registration
    // surface") for why this must happen here (composition-root wiring
    // time) rather than through a live post-construction API — unlike
    // `object_documents`/`outbox_sources` below, `InboundActivityDispatcher`
    // cannot be made live-mutable without editing `inbound/dispatcher.rs`/
    // `inbound/service.rs`, both outside this task's boundary.
    let dispatcher = InboundActivityDispatcher::new();

    let inbox = Arc::new(InboxService::new(
        inbox_verifier,
        NoopBlockPolicy,
        dedup_for_inbox,
        dispatcher,
        urls.clone(),
    ));

    // Live, downstream-registrable registries (see this module's doc
    // comment, "Downstream registration surface"): each `.clone()` below
    // shares the same underlying `Arc<RwLock<Vec<_>>>` as the original, so
    // registering into `object_documents`/`outbox_sources` (kept on
    // `FederationModule`) is observed by `ap_get_state()`'s
    // `Arc<ObjectDocumentRegistry>`/`document_builder`'s own internal
    // `OutboxSourceRegistry` clone respectively.
    let object_documents = ObjectDocumentRegistry::new();
    let outbox_sources = OutboxSourceRegistry::new();

    let document_builder = Arc::new(ActivityPubDocumentBuilder::new(
        urls.clone(),
        Arc::clone(&directory),
        outbox_sources.clone(),
    ));

    let target_resolver = RecipientTargetResolver::new(ActorDirectory::new(pool.clone()));
    let local_sink = LocalDeliverySink::new(Arc::clone(&inbox), urls.clone());
    let http_sink = HttpDeliverySink::new(
        DbDeliveryQueue::new(pool.clone()),
        ActorDirectory::new(pool.clone()),
        runtime.clock.clone(),
        runtime.ids.clone(),
    );
    let delivery = Arc::new(DeliveryService::new(target_resolver, local_sink, http_sink));

    let worker = DeliveryWorker::new(
        DbDeliveryQueue::new(pool.clone()),
        negotiator,
        runtime.clock.clone(),
        Arc::clone(&directory),
    );
    let pruning_store =
        DbReceivedActivityStore::new(pool, runtime.clock.clone(), cfg.received_activity_retention);

    let module = FederationModule {
        domain: cfg.domain,
        secure_mode: cfg.secure_mode,
        urls,
        directory,
        verifier: ap_get_verifier,
        document_builder,
        object_documents,
        outbox_sources,
        inbox,
        delivery,
    };

    let background = FederationBackgroundTasks {
        worker,
        pruning_store,
        delivery_poll_interval: cfg.delivery_poll_interval,
        delivery_poll_batch_size: cfg.delivery_poll_batch_size,
        pruning_interval: cfg.pruning_interval,
    };

    (module, background)
}

/// The delivery-worker poll loop and received-Activity pruning loop, built
/// by [`build_federation_module`] but not yet started (task 5.4's own
/// "must not block bootstrap's own startup" constraint: constructing these
/// is synchronous and cheap, only [`Self::spawn`] actually starts anything).
/// A caller that has no use for live background polling (e.g. an
/// `AppState`-only unit test fixture) can simply drop this value without
/// calling [`Self::spawn`] — nothing was started yet, so there is nothing to
/// cancel.
pub struct FederationBackgroundTasks {
    worker: DeliveryWorker<DbDeliveryQueue, ReqwestFederationHttpClient>,
    pruning_store: DbReceivedActivityStore,
    delivery_poll_interval: StdDuration,
    delivery_poll_batch_size: i64,
    pruning_interval: StdDuration,
}

impl FederationBackgroundTasks {
    /// Starts both background loops as detached `tokio::spawn` tasks and
    /// returns immediately (never awaits either loop) — the caller's own
    /// startup sequence (`bootstrap()`'s listener bind, or
    /// `spawn_test_app`'s own listener bind) is never blocked on this call.
    /// Neither loop ever panics the whole process on a single transient
    /// failure: each iteration's `Result` is matched, and an `Err` is logged
    /// via `tracing::error!` before the loop simply continues to its next
    /// interval tick (this crate's established diagnostic convention,
    /// `src/error.rs`/`src/bootstrap.rs`'s own `*_with_diagnostics`
    /// functions).
    pub fn spawn(self) {
        let FederationBackgroundTasks {
            worker,
            pruning_store,
            delivery_poll_interval,
            delivery_poll_batch_size,
            pruning_interval,
        } = self;

        tokio::spawn(async move {
            loop {
                match worker.run_once(delivery_poll_batch_size).await {
                    Ok(WorkerRunSummary {
                        claimed,
                        done,
                        rescheduled,
                        failed,
                    }) if claimed > 0 => {
                        tracing::debug!(
                            claimed,
                            done,
                            rescheduled,
                            failed,
                            "delivery worker processed a batch of due jobs"
                        );
                    }
                    Ok(_zero_claimed) => {}
                    Err(err) => {
                        tracing::error!(
                            error = ?err,
                            "delivery worker poll iteration failed; will retry next interval"
                        );
                    }
                }
                tokio::time::sleep(delivery_poll_interval).await;
            }
        });

        tokio::spawn(async move {
            loop {
                match pruning_store.prune_expired().await {
                    Ok(deleted) if deleted > 0 => {
                        tracing::debug!(deleted, "received-activity pruning removed expired rows");
                    }
                    Ok(_none_deleted) => {}
                    Err(err) => {
                        tracing::error!(
                            error = ?err,
                            "received-activity pruning iteration failed; will retry next interval"
                        );
                    }
                }
                tokio::time::sleep(pruning_interval).await;
            }
        });
    }
}
