//! The single implementation of this application's module-wiring sequence.
//!
//! ## Why this module exists
//! The identical 11-stage build order of the nine feature modules used to be
//! written out three separate times — once in
//! [`crate::bootstrap`]'s own `build_state` (production), once in
//! `crate::test_harness::spawn_test_app`, and once in
//! `crate::federation::test_harness::spawn_paired_instance`. Adding a new
//! spec meant copying the sequence into all three, and re-honoring by hand an
//! ordering constraint the type system does not enforce (see "The ordering
//! constraint" below). This module holds that sequence exactly once, and
//! every startup path goes through it.
//!
//! ## The ordering constraint
//! `crate::accounts::AccountPortsRegistry` and
//! `crate::statuses::RelationshipQueryRegistry` are runtime-replaceable
//! registry slots: the *last* `set_*` call for a given slot is the one
//! actually observed at request time. Two stages of this sequence write to
//! the same `AccountCountsProvider` slot:
//! - [`crate::statuses::register_account_ports`] replaces
//!   `build_accounts_module`'s built-in `ZeroCountsProvider`/
//!   `EmptyStatusesProvider` defaults with statuses-core's real ones, and
//! - [`crate::social_graph::build_social_graph_module`] then registers a
//!   *composed* counts provider (`CombinedAccountCountsProvider`) that wraps
//!   whatever statuses-core just installed.
//!
//! So the order `build_statuses_module` -> `register_account_ports` ->
//! `build_social_graph_module` is load-bearing, and swapping any two of them
//! still compiles and still boots — it silently produces an instance whose
//! Account representation reports zero counts. `tests/module_wiring_it.rs`
//! holds the runtime test that fails when this order is broken.
//!
//! ## What deliberately stays with the caller
//! Config loading/synthesis, pool creation, migrations, actor-model wiring
//! (`build_actor_wiring` / `load_key_cache` + `build_actor_module`), the HTTP
//! listener bind, and the shutdown-signal handling are all *not* here: those
//! are genuinely different per startup path (production loads real config and
//! binds `cfg.server.bind_addr`; the test harnesses synthesize fixed config
//! and bind an ephemeral `127.0.0.1:0`). Accordingly the two background-task
//! handles are **returned, never spawned** — production spawns the media
//! workers against `crate::server::os_shutdown_signal`, while both test
//! harnesses spawn them against a `std::future::pending` signal that never
//! resolves.
//!
//! ## No trait abstraction (deliberate)
//! The three call sites differ only in concrete values (which
//! `ReqwestFederationHttpClient` constructor, which background poll cadence,
//! which shutdown signal). This is a once-at-startup choice, not a
//! polymorphism problem, so [`ModuleWiringInput`] is a plain data bundle and
//! no trait is introduced — the same judgment `search/ports.rs`'s own wiring
//! notes record.
//!
//! ## Shared federation HTTP client
//! [`ModuleWiringInput::http_client`] is a single `Arc` shared by every
//! consumer this function wires (federation module, accounts module, and both
//! `RemoteAccountFetcher`s). `crate::bootstrap`/`crate::test_harness`
//! currently build four independent `ReqwestFederationHttpClient::new()`
//! instances instead, while `crate::federation::test_harness` already shares
//! one successfully — this module follows the latter. The HTTP requests
//! emitted are unchanged; only the underlying connection pool is now shared.
//! (`crate::search::build_search_module` still constructs its own client
//! internally — a known residual.)

use std::sync::Arc;
use std::time::Duration;

use sqlx::PgPool;

use crate::accounts::{
    self, AccountsModule, DEFAULT_REMOTE_ACCOUNT_CACHE_TTL, RemoteAccountFetcher,
};
use crate::actor::ActorModule;
use crate::config::AppConfig;
use crate::error::AppError;
use crate::federation::inbound::InboundActivityDispatcher;
use crate::federation::module::{
    DEFAULT_DELIVERY_POLL_BATCH_SIZE, DEFAULT_DELIVERY_POLL_INTERVAL, DEFAULT_PRUNING_INTERVAL,
};
use crate::federation::signatures::ReqwestFederationHttpClient;
use crate::federation::{
    self, FederationBackgroundTasks, FederationModule, FederationWiringConfig,
};
use crate::media::{self, MediaBackgroundWorkers, MediaModule};
use crate::notifications::{self, NotificationModule};
use crate::oauth::OauthModule;
use crate::runtime::RuntimeContext;
use crate::search::{self, SearchModule};
use crate::social_graph::{self, SocialGraphModule};
use crate::statuses::notification_sink::NotificationSinkRegistry;
use crate::statuses::{self, ProdRemoteActorResolver, StatusesModule};
use crate::timelines::{self, TimelinesModule};

/// The background-loop cadence [`compose_modules`] builds its
/// [`FederationWiringConfig`] with — the one part of that config that is
/// genuinely per-startup-path rather than derived from [`AppConfig`].
///
/// It is passed separately from the rest of [`ModuleWiringInput`] because the
/// three current call sites genuinely differ here: production uses
/// [`FederationWiringConfig::production`]'s multi-second defaults, while both
/// test harnesses use sub-second intervals so an integration test observing a
/// delivery can finish quickly. Passing the three numbers (rather than a
/// whole `FederationWiringConfig`) keeps the `domain`/`secure_mode`/TTL/
/// retention derivation from `AppConfig` in this one place.
pub(crate) struct FederationPollCadence {
    /// See [`FederationWiringConfig::delivery_poll_interval`].
    pub delivery_poll_interval: Duration,
    /// See [`FederationWiringConfig::delivery_poll_batch_size`].
    pub delivery_poll_batch_size: i64,
    /// See [`FederationWiringConfig::pruning_interval`].
    pub pruning_interval: Duration,
}

impl FederationPollCadence {
    /// The production cadence, identical to what
    /// [`FederationWiringConfig::production`] installs.
    pub(crate) fn production() -> Self {
        Self {
            delivery_poll_interval: DEFAULT_DELIVERY_POLL_INTERVAL,
            delivery_poll_batch_size: DEFAULT_DELIVERY_POLL_BATCH_SIZE,
            pruning_interval: DEFAULT_PRUNING_INTERVAL,
        }
    }
}

/// Everything [`compose_modules`] needs from its caller. A plain data
/// bundle, mirroring this crate's own
/// `FederationWiringConfig`/`TestAppParts` convention of grouping a
/// constructor's inputs into a named struct rather than a long positional
/// parameter list.
///
/// Every field is a value the caller has *already* built by its own,
/// path-specific means: `pool` is migrated, `runtime` and
/// `actor_module` are constructed, `config` is either loaded from the
/// environment or synthesized by a test harness.
pub(crate) struct ModuleWiringInput<'a> {
    /// A pool with the embedded migrations already applied.
    pub pool: PgPool,
    /// The already-built non-determinism injection boundaries.
    pub runtime: RuntimeContext,
    /// The already-built actor-model service bundle. Its
    /// [`crate::actor::ActorDirectory`] handle is shared with every module
    /// below that needs handle resolution, rather than each one constructing
    /// its own.
    pub actor_module: &'a ActorModule,
    /// Startup configuration. `server.domain`, `federation.*`, `media.*`,
    /// `oauth.token_hash_key`, and `owner` are read here.
    pub config: &'a AppConfig,
    /// The federation HTTP client, shared by every consumer this function
    /// wires. Production and the ordinary test harness pass
    /// [`ReqwestFederationHttpClient::new`]; the federation-pair harness
    /// passes [`ReqwestFederationHttpClient::insecure_loopback`]. The one
    /// genuinely differing collaborator across the three paths.
    pub http_client: Arc<ReqwestFederationHttpClient>,
    /// Background-loop cadence — see [`FederationPollCadence`].
    pub federation_cadence: FederationPollCadence,
}

/// Everything [`compose_modules`] produces: the nine feature modules
/// `crate::state::AppState::new` takes, plus the two background-task handles
/// the caller must spawn against *its own* shutdown signal.
pub(crate) struct ComposedModules {
    pub oauth: OauthModule,
    pub federation: FederationModule,
    pub media: MediaModule,
    pub accounts: AccountsModule,
    pub statuses: StatusesModule,
    pub social_graph: SocialGraphModule,
    pub timelines: TimelinesModule,
    pub notifications: NotificationModule,
    pub search: SearchModule,
    /// Not yet started. Call `.spawn()` on it — every current call site does
    /// so unconditionally right after composing, but doing it here would take
    /// the choice away from a caller that wants to inspect the wiring without
    /// running loops.
    pub federation_background: FederationBackgroundTasks,
    /// Not yet started. Call `.spawn(signal_factory)` on it with *this*
    /// startup path's own shutdown signal (`crate::server::os_shutdown_signal`
    /// in production, `std::future::pending::<()>` in the test harnesses).
    pub media_background: MediaBackgroundWorkers,
}

/// Runs the 11-stage module-wiring sequence — the single implementation
/// shared by every startup path.
///
/// Stages, in the order they must run:
/// 1. `OauthModule`
/// 2. `FederationModule` (with statuses-core's and social-graph's inbound
///    handlers registered inside its own registration point — both
///    registration closures must be built *before* this call, since
///    `InboundActivityDispatcher` is not live-mutable afterward)
/// 3. Resolve social-graph's deferred `DeliveryService` cell, now that a real
///    one exists
/// 4. `MediaModule`
/// 5. `AccountsModule` (registers its built-in safe port defaults)
/// 6. `StatusesModule`
/// 7. `statuses::register_account_ports` — **must** run after 5 and 6
/// 8. `SocialGraphModule` — **must** run after 7, so its composed
///    `AccountCountsProvider` is the last registration and therefore the
///    observed one (see this module's doc comment, "The ordering constraint")
/// 9. `TimelinesModule`
/// 10. `NotificationModule` (its `set_sink` reaches every emit call site the
///     earlier modules already hold a clone of)
/// 11. `SearchModule`
///
/// # Preconditions
/// `input.pool` has the embedded migrations applied; `input.runtime` and
/// `input.actor_module` are fully constructed.
///
/// # Postconditions
/// `accounts`' `AccountPortsRegistry` holds statuses-core's real
/// `StatusesProvider` (not `EmptyStatusesProvider`) and social-graph's
/// composed `AccountCountsProvider` (not `ZeroCountsProvider`).
///
/// # Errors
/// Returns [`AppError`] for symmetry with the rest of this crate's fallible
/// startup surface. No stage of the wiring sequence itself is currently
/// fallible (the failure-prone startup stages — config, pool, migrations, key
/// supply — all stay with the caller), so this currently always returns `Ok`;
/// the `Result` is what lets a future stage fail without changing every call
/// site.
pub(crate) async fn compose_modules(
    input: ModuleWiringInput<'_>,
) -> Result<ComposedModules, AppError> {
    let ModuleWiringInput {
        pool,
        runtime,
        actor_module,
        config,
        http_client,
        federation_cadence,
    } = input;
    let domain = config.server.domain.clone();

    // Stage 1: OAuth. `cookie_secure` is `false` because this crate's own
    // listener never terminates TLS and `ServerConfig` carries no
    // TLS-termination setting — see `crate::bootstrap`'s own documented
    // judgment call at its matching call site.
    let oauth = OauthModule::new(
        pool.clone(),
        runtime.clone(),
        config.oauth.token_hash_key.clone(),
        config.owner.clone(),
        false,
    );

    // --- Stage 2 prerequisites -------------------------------------------
    // `InboundActivityDispatcher` is only mutable inside
    // `build_federation_module`'s own registration point, so both specs'
    // downstream handlers (and everything they depend on) must be built
    // first.

    // statuses-core's `RemoteActorResolver`. Shares `actor_module`'s own
    // `ActorDirectory` handle rather than constructing a second one; the
    // directory holds nothing but a `PgPool` (`src/actor/directory.rs`), so
    // this is a shared handle, not shared state.
    let statuses_remote_actor_fetcher = Arc::new(RemoteAccountFetcher::new(
        pool.clone(),
        Arc::clone(&http_client),
        runtime.clone(),
        DEFAULT_REMOTE_ACCOUNT_CACHE_TTL,
    ));
    let statuses_remote_actor_resolver = Arc::new(ProdRemoteActorResolver::new(
        domain.clone(),
        Arc::clone(actor_module.directory()),
        statuses_remote_actor_fetcher,
    ));

    // The one `NotificationSinkRegistry` every emit call site shares —
    // local-origin (`StatusService`/`InteractionService`/social-graph) and
    // remote-origin (inbound handlers) alike — so stage 10's single
    // `set_sink` reaches all of them at once.
    let notification_sinks = NotificationSinkRegistry::new();

    // social-graph's own `RemoteAccountFetcher` (shared afterward with stage
    // 8's `BlockPolicyImpl`, so both signer-resolution call sites reuse one
    // cache) and its downstream registration closure.
    let social_graph_remote_actor_fetcher = Arc::new(RemoteAccountFetcher::new(
        pool.clone(),
        Arc::clone(&http_client),
        runtime.clone(),
        DEFAULT_REMOTE_ACCOUNT_CACHE_TTL,
    ));
    let (social_graph_register_downstream, social_graph_pending_delivery) =
        social_graph::register_downstream_handlers(
            pool.clone(),
            runtime.clone(),
            domain.clone(),
            Arc::clone(actor_module.directory()),
            Arc::clone(&social_graph_remote_actor_fetcher),
            notification_sinks.clone(),
        );

    // Stage 2: federation. Everything config-derived is computed here, once;
    // only the poll cadence comes from the caller (see
    // `FederationPollCadence`).
    let (federation, federation_background) = federation::build_federation_module(
        pool.clone(),
        runtime.clone(),
        Arc::clone(actor_module.directory()),
        FederationWiringConfig {
            domain: domain.clone(),
            secure_mode: config.federation.secure_mode,
            public_key_cache_ttl: time::Duration::seconds(
                config.federation.public_key_cache_ttl.as_secs() as i64,
            ),
            received_activity_retention: time::Duration::days(
                config.federation.received_activity_retention_days as i64,
            ),
            delivery_poll_interval: federation_cadence.delivery_poll_interval,
            delivery_poll_batch_size: federation_cadence.delivery_poll_batch_size,
            pruning_interval: federation_cadence.pruning_interval,
        },
        Arc::clone(&http_client),
        {
            // Both specs' registration closures composed into the single
            // `register_downstream` call `build_federation_module` accepts.
            let statuses_register = statuses::register_downstream_handlers(
                pool.clone(),
                runtime.clone(),
                domain.clone(),
                statuses_remote_actor_resolver,
                notification_sinks.clone(),
            );
            move |dispatcher: &mut InboundActivityDispatcher| {
                statuses_register(dispatcher);
                social_graph_register_downstream(dispatcher);
            }
        },
    );

    // Stage 3: fill in social-graph's deferred delivery cell, now that a real
    // `Arc<ConcreteDeliveryService>` exists. Strictly before any listener
    // starts serving (the caller binds one only after this function returns),
    // so no inbound request can ever observe an unresolved cell.
    social_graph_pending_delivery.resolve(Arc::clone(federation.delivery_service()));

    // Stage 4: media. Its worker pool is returned unstarted (see
    // `ComposedModules::media_background`).
    let (media, media_background) =
        media::build_media_module(pool.clone(), runtime.clone(), config.media.clone());

    // Stage 5: accounts. Installs the built-in safe port defaults
    // (`EmptyStatusesProvider`/`NoRelationshipProvider`/`ZeroCountsProvider`)
    // that stages 7 and 8 replace.
    let accounts = accounts::build_accounts_module(
        pool.clone(),
        runtime.clone(),
        domain.clone(),
        Arc::clone(actor_module.directory()),
        Arc::clone(&http_client),
        media.store().clone(),
        media.service(),
        config.media.clone(),
    );

    // Stage 6: statuses. Creates the `RelationshipQueryRegistry` stages 7, 8
    // and 11 all read back from.
    let statuses_module = statuses::build_statuses_module(
        pool.clone(),
        runtime.clone(),
        domain.clone(),
        Arc::clone(federation.delivery_service()),
        notification_sinks.clone(),
    );

    // Stage 7: ORDERING CONSTRAINT — after stages 4, 5 and 6, before stage 8.
    // Replaces accounts' `EmptyStatusesProvider`/`ZeroCountsProvider`
    // defaults with statuses-core's real implementations.
    statuses::register_account_ports(
        pool.clone(),
        runtime.clone(),
        domain.clone(),
        accounts.ports(),
        accounts.service(),
        media.store().clone(),
        statuses_module.relationship_query_registry(),
    );

    // Stage 8: ORDERING CONSTRAINT — must be the LAST writer of the
    // `AccountCountsProvider` slot, so its `CombinedAccountCountsProvider`
    // (which composes over what stage 7 installed) is the one observed.
    let social_graph = social_graph::build_social_graph_module(
        pool.clone(),
        runtime.clone(),
        domain.clone(),
        Arc::clone(actor_module.directory()),
        social_graph_remote_actor_fetcher,
        Arc::clone(federation.delivery_service()),
        federation.block_policy(),
        &statuses_module.relationship_query_registry(),
        accounts.ports(),
        accounts.service(),
        notification_sinks.clone(),
    );

    // Stage 9: timelines.
    let timelines = timelines::build_timelines_module(
        pool.clone(),
        runtime.clone(),
        accounts.service(),
        media.store().clone(),
    );

    // Stage 10: notifications. Its own `set_sink` replaces the registry's
    // built-in `NoopSink` default, reaching every emit call site stages 2, 6
    // and 8 already hold a clone of.
    let notifications = notifications::build_notification_module(
        pool.clone(),
        runtime.clone(),
        domain.clone(),
        accounts.service(),
        media.store().clone(),
        notification_sinks,
    );

    // Stage 11: search. Reads back stages 5, 4 and 6's registries/handles, so
    // it must run last among the module builders.
    let search = search::build_search_module(
        pool.clone(),
        runtime.clone(),
        domain,
        Arc::clone(actor_module.directory()),
        accounts.service(),
        accounts.ports(),
        media.store().clone(),
        statuses_module.relationship_query_registry(),
    );

    Ok(ComposedModules {
        oauth,
        federation,
        media,
        accounts,
        statuses: statuses_module,
        social_graph,
        timelines,
        notifications,
        search,
        federation_background,
        media_background,
    })
}
