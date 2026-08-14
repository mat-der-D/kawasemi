//! `FederationTestHarness` (design.md `#### FederationTestHarness` -> Service
//! Interface; Requirements 10.5, 13.1, 13.2, 13.3, 13.4; task 6.4, `Boundary:
//! FederationTestHarness, federation_pair_it`): boots two genuinely separate,
//! genuinely reachable instances of this application for federation
//! verification — `A→B` signed-Activity round trips and local-vs-HTTP
//! delivery-result equivalence.
//!
//! ## Scope
//! This module owns exactly [`spawn_federation_pair`] and [`FederationPair`].
//! It deliberately reassembles the same startup steps
//! `crate::test_harness::spawn_test_app` itself reassembles
//! ([`crate::db::establish_pool`], [`crate::migrate::apply_migrations`],
//! [`crate::runtime::RuntimeContext::deterministic`],
//! [`crate::actor::build_actor_module`],
//! [`crate::bootstrap::wiring::compose_modules`],
//! [`crate::state::AppState::new`], [`crate::server::build_router`]) rather
//! than calling `spawn_test_app` itself, for the one reason this module's doc
//! comment below explains in full ("Why not `spawn_test_app`"). It reuses
//! [`crate::test_harness::TestApp`] unchanged as each paired instance's own
//! type (same `cleanup()`/`Drop` lifecycle, same
//! isolated-schema/deterministic-runtime guarantees) via `TestApp`'s
//! `pub(crate)` `from_parts` constructor — it does not duplicate `TestApp`'s
//! own struct fields or release logic.
//!
//! ## Isolated database
//! The isolated schema, its pinned pool and the migrations applied to it are
//! likewise not recomposed here: [`spawn_paired_instance`] calls
//! [`crate::test_harness::establish_isolated_db`], the same helper
//! `spawn_test_app` and `spawn_test_db` build on. A paired instance therefore
//! carries the crate-wide `kawasemi_test_harness_` schema prefix, the same
//! pool size, and the same once-per-process startup sweep. That is not
//! cosmetic: both the startup sweep and `HarnessReaper` recognize a
//! reclaimable schema by that prefix alone, so a paired instance's schema
//! abandoned by an abnormal process exit is reclaimed on a later run instead
//! of accumulating unreachable to every reclamation path.
//!
//! ## Module wiring lives elsewhere
//! The 11-stage feature-module wiring sequence itself is **not** here: this
//! module calls [`crate::bootstrap::wiring::compose_modules`] — the single
//! implementation production startup and `spawn_test_app` also run. What
//! remains below is only the genuinely per-startup-path work: the
//! deterministic [`RuntimeContext`] and actor-model wiring, the synthesized
//! [`AppConfig`], the ephemeral listener bind, and the shutdown signal.
//! Going through `compose_modules` also
//! *fixed* a pre-existing divergence: this module's own open-coded sequence
//! never called `crate::statuses::register_account_ports`, so a paired
//! instance served Account representations built from
//! `build_accounts_module`'s built-in
//! `EmptyStatusesProvider`/`ZeroCountsProvider` defaults instead of
//! statuses-core's real implementations. Observably that meant an empty `GET
//! /accounts/:id/statuses` page on both instances; the Account JSON's own
//! `statuses_count`/`last_status_at` happened to survive anyway, because
//! `social_graph::CombinedAccountCountsProvider` (wiring stage 8, which this
//! module did run) constructs its own `AccountCountsContribution` rather than
//! reading back whatever stage 7 installed. It now matches production in both
//! respects.
//!
//! ## Why not `spawn_test_app`
//! [`crate::federation::urls::ActorUrls`] hardcodes `https://{domain}/...`
//! for every URL it builds (actor/inbox/object URLs) — not configurable
//! per-request.
//! [`crate::federation::signatures::ReqwestFederationHttpClient`] (the
//! production [`crate::federation::signatures::FederationHttpClient`]
//! implementation both public-key resolution and outbound signed delivery
//! use) performs a real TLS handshake for any `https://` URL.
//! `crate::test_harness::spawn_test_app` serves its instance over plain HTTP
//! (`axum::serve`, no TLS) on a real ephemeral `127.0.0.1:PORT` — and always
//! uses the fixed placeholder domain `"test-harness.kawasemi.internal"`,
//! which resolves nowhere at all.
//!
//! If this module simply called `spawn_test_app` twice, instance B fetching
//! instance A's public key (or A delivering an Activity to B's real inbox)
//! would build a `https://test-harness.kawasemi.internal/...` URL that
//! either fails DNS resolution, or — even given a resolvable domain — would
//! attempt a real TLS handshake against a plain-HTTP server and fail before
//! any application-level federation logic ever ran. This module resolves
//! that reachability problem with two small, additive, backward-compatible
//! changes elsewhere in this same crate (both already covered by their own
//! existing tests, unaffected for every pre-existing caller):
//! - [`crate::federation::build_federation_module`] now takes an
//!   already-constructed `Arc<ReqwestFederationHttpClient>` instead of
//!   building one internally, so a caller other than `spawn_test_app`/
//!   `crate::bootstrap::bootstrap` can inject a differently-configured one.
//! - [`crate::federation::signatures::ReqwestFederationHttpClient::insecure_loopback`]
//!   is a new, narrow, explicitly-named opt-in constructor that rewrites a
//!   `https://` URL's scheme to `http://` immediately before dispatch — used
//!   *only* by this module, never by production or by `spawn_test_app`.
//!
//! Each paired instance built by [`spawn_paired_instance`] is therefore
//! configured with its own real bound address as `domain` (rather than
//! `spawn_test_app`'s fixed placeholder), so the OTHER instance's
//! `https://{that-address}/...` URLs — downgraded to `http://` by
//! `insecure_loopback` — resolve to a real, reachable, live TCP listener
//! (Requirement 13.1's "相互に到達可能にする").
//!
//! ## Dispatch-success observation (Requirement 13.3)
//! [`crate::federation::inbound::InboundActivityDispatcher`] is not
//! live-mutable after a [`crate::federation::FederationModule`] is
//! constructed (see that module's own doc comment, "Downstream registration
//! surface") and `InboxService`/`dispatcher.rs` are both outside this task's
//! boundary, so this module does not register a custom stub
//! `InboundActivityHandler` on a paired instance. `InboundActivityDispatcher::dispatch`
//! is itself a safe no-op for any outer Activity type with no registered
//! handler (see `dispatcher.rs`'s own doc comment, "Unregistered outer types
//! are a safe no-op") and runs unconditionally, strictly after signature
//! verification and deduplication succeed
//! ([`crate::federation::inbound::InboxService::process_verified`]'s own
//! documented pipeline order) — so a `received_activities` row for a given
//! Activity id is exactly as strong an "this instance verified the
//! signature and handed the Activity to the dispatch boundary successfully"
//! signal as a bespoke handler's own `Handled`/`Ignored` outcome would be,
//! without requiring `InboxService`/`dispatcher.rs` changes outside this
//! task's boundary. `tests/federation_pair_it.rs` uses exactly this signal,
//! mirroring `tests/federation_bootstrap_it.rs`'s own established
//! `received_activities`-row-existence convention.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::net::TcpListener;
use tokio::sync::oneshot;

use crate::actor::keys::cipher::{ChaCha20Poly1305KeyCipher, KeyCipher};
use crate::actor::keys::provider::DbSigningKeyProvider;
use crate::actor::{self, ActorModule};
use crate::bootstrap::wiring::{
    ComposedModules, FederationPollCadence, ModuleWiringInput, compose_modules,
};
use crate::config::{
    ActorConfig, AppConfig, FederationConfig, LogConfig, LogLevel, MediaConfig, OauthConfig,
    OwnerConfig, Secret, ServerConfig, StatusesConfig,
};
use crate::federation::signatures::ReqwestFederationHttpClient;
use crate::runtime::{DeterministicSeed, RuntimeContext};
use crate::server;
use crate::state::AppState;
use crate::test_harness::{IsolatedDb, TestApp, TestAppParts, establish_isolated_db};

/// Fixed numeric seed both paired instances build their deterministic
/// [`RuntimeContext`] from (Requirement 13.2). A single fixed seed shared by
/// both `A` and `B` mirrors `crate::test_harness::spawn_test_app`'s own
/// documented rationale for a fixed (not per-call) seed: Requirement 13.2
/// only asks for non-determinism to be replaced with a deterministic
/// implementation per instance, not for uniqueness between `A` and `B`.
const PAIR_TEST_SEED_VALUE: u64 = 424_242;

/// Fixed, non-production Key-Encryption-Key both paired instances use,
/// mirroring `crate::test_harness::TEST_KEK`'s own "why fixed" reasoning —
/// each instance's own isolated schema keeps their signing-key material
/// independent regardless of sharing this constant.
const PAIR_TEST_KEK: [u8; 32] = [0x42; 32];

/// Fixed, non-production owner passphrase both paired instances use,
/// mirroring `crate::test_harness::TEST_OWNER_PASSWORD`.
const PAIR_TEST_OWNER_PASSWORD: &str = "federation-pair-owner-passphrase";

/// Fixed, non-production OAuth token-hashing key both paired instances use,
/// mirroring `crate::test_harness::TEST_TOKEN_HASH_KEY`.
const PAIR_TEST_TOKEN_HASH_KEY: [u8; 32] = [0x24; 32];

/// Each paired instance's own delivery-worker poll interval, mirroring
/// `crate::test_harness::TEST_DELIVERY_POLL_INTERVAL`'s own "short enough
/// that an integration test observing delivery completion does not need to
/// wait production's several-second interval" reasoning — this module's own
/// `federation_pair_it.rs` caller needs A's real `DeliveryWorker` to
/// actually attempt (and succeed at) a real HTTP send to B within a test's
/// lifetime.
const PAIR_DELIVERY_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Each paired instance's own received-Activity pruning-loop interval,
/// mirroring `crate::test_harness::TEST_PRUNING_INTERVAL`.
const PAIR_PRUNING_INTERVAL: Duration = Duration::from_secs(5);

/// Generates a `LocalFsStore` root unique to this call, under the OS temp
/// directory (task 5.2). Mirrors `crate::test_harness::unique_media_storage_root`'s
/// own doc comment for why: reusing `MediaConfig::storage_root`'s fixed
/// relative production default here would accumulate never-cleaned files
/// directly inside this repository's own working directory across every
/// `spawn_federation_pair` call/`cargo test` run.
fn unique_pair_media_storage_root() -> std::path::PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "kawasemi_federation_pair_media_storage_{nanos}_{seq}"
    ))
}

/// Boots one paired instance: the same composition
/// `crate::test_harness::spawn_test_app` performs, except `domain` is set to
/// this instance's own real bound address (not a fixed placeholder) and
/// `http_client` is caller-supplied (rather than always
/// `ReqwestFederationHttpClient::new()`) — see this module's doc comment
/// ("Why not `spawn_test_app`") for why both differences are necessary.
async fn spawn_paired_instance(http_client: Arc<ReqwestFederationHttpClient>) -> TestApp {
    // The schema/pool/migrate sequence is the crate-wide one, not a third
    // copy of it — see this module's doc comment ("Isolated database") for
    // why the shared schema-name prefix in particular is what keeps an
    // abandoned paired-instance schema reclaimable.
    let IsolatedDb {
        pool,
        schema,
        db_config,
    } = establish_isolated_db().await;

    // Requirement 13.2: non-determinism boundaries replaced with
    // deterministic implementations, mirroring
    // `crate::test_harness::spawn_test_app`'s own identical `keys`-boundary
    // exception (the real, DB-backed `DbSigningKeyProvider`, not a fixed
    // stand-in) -- see that function's own doc comment for the full
    // reasoning, which applies unchanged here.
    let deterministic = RuntimeContext::deterministic(DeterministicSeed::new(PAIR_TEST_SEED_VALUE));
    let cipher: Arc<dyn KeyCipher> =
        Arc::new(ChaCha20Poly1305KeyCipher::new(Secret::new(PAIR_TEST_KEK)));
    let cache = actor::load_key_cache(&pool, cipher.as_ref())
        .await
        .expect("loading the freshly migrated (empty) signing key cache must succeed");
    let runtime = RuntimeContext {
        clock: deterministic.clock,
        ids: deterministic.ids,
        rng: deterministic.rng,
        keys: Arc::new(DbSigningKeyProvider::new(cache.clone())),
    };
    let actor_module: ActorModule =
        actor::build_actor_module(pool.clone(), runtime.clone(), cipher, cache);

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("binding an ephemeral federation-pair listener must succeed");
    let address: SocketAddr = listener
        .local_addr()
        .expect("a just-bound listener must have a local address");

    // Requirement 13.1: this instance's own domain is its own real bound
    // address, not a fixed placeholder -- see this module's doc comment
    // ("Why not `spawn_test_app`") for why this is what makes the OTHER
    // paired instance's URLs actually reachable.
    let domain = address.to_string();

    let config = AppConfig {
        server: ServerConfig {
            domain,
            bind_addr: address,
            shutdown_grace: Duration::from_secs(1),
        },
        database: db_config,
        log: LogConfig {
            level: LogLevel::Error,
            sql_diagnostic: false,
        },
        actor: ActorConfig {
            kek: Secret::new(PAIR_TEST_KEK),
        },
        owner: OwnerConfig {
            password: Secret::new(PAIR_TEST_OWNER_PASSWORD.to_string()),
        },
        oauth: OauthConfig {
            token_hash_key: Secret::new(PAIR_TEST_TOKEN_HASH_KEY),
        },
        federation: FederationConfig {
            secure_mode: false,
            public_key_cache_ttl: Duration::from_secs(24 * 60 * 60),
            received_activity_retention_days: 14,
        },
        // media-pipeline task 1.2: fixed, non-production values mirroring
        // `load_config_from`'s own defaults (`src/config.rs`) — this
        // federation-pair test harness constructs `AppConfig` directly, the
        // same way every other startup-config group above is fixed here
        // rather than loaded.
        media: MediaConfig {
            storage_root: unique_pair_media_storage_root(),
            max_upload_size_bytes: 10 * 1024 * 1024,
            thumbnail_target_width: 400,
            thumbnail_target_height: 400,
            supported_formats: vec![
                "image/jpeg".to_string(),
                "image/png".to_string(),
                "image/gif".to_string(),
                "image/webp".to_string(),
            ],
            worker_concurrency: 2,
            max_retry_attempts: 5,
            lease_duration: Duration::from_secs(5 * 60),
        },
        // statuses-core task 7.2: fixed, non-production values mirroring
        // `load_config_from`'s own defaults, the same convention every other
        // startup-config group above already follows in this harness.
        statuses: StatusesConfig {
            max_content_chars: 500,
            poll_max_options: 4,
            poll_min_expiration: Duration::from_secs(5 * 60),
            idempotency_key_retention_days: 7,
        },
    };

    // The entire 11-stage module-wiring sequence this function used to
    // open-code lives in exactly one place
    // (`crate::bootstrap::wiring::compose_modules`), shared verbatim with
    // production startup and `crate::test_harness::spawn_test_app`. What
    // stays here is only what is genuinely specific to a paired instance:
    // the isolated schema/pool/migrations, the
    // deterministic `RuntimeContext` and actor-model wiring, the synthesized
    // `AppConfig` whose `domain` is this instance's own bound address, the
    // ephemeral listener bind, and the shutdown-signal handling.
    //
    // `http_client` is still the caller-supplied
    // `ReqwestFederationHttpClient::insecure_loopback()` instance (see
    // `spawn_federation_pair`) — `compose_modules` shares that one instance
    // with every consumer it wires (federation module, accounts module, and
    // both `RemoteAccountFetcher`s), which is exactly what this function
    // already did by hand, so this instance's outbound public-key fetches,
    // signed deliveries and remote-actor/-account fetches all still reach
    // the OTHER paired instance's plain-HTTP listener.
    //
    // Behavior change, deliberate: this function previously never called
    // `statuses::register_account_ports` — the one stage of the sequence it
    // had silently dropped — so each paired instance served Account
    // representations built from `build_accounts_module`'s built-in
    // `EmptyStatusesProvider`/`ZeroCountsProvider` defaults rather than
    // statuses-core's real implementations. Going through `compose_modules`
    // restores that stage in its correct position (statuses module ->
    // `register_account_ports` -> social-graph module), so a paired
    // instance's Account representation now matches production.
    // `tests/federation_pair_it.rs`'s own
    // `federation_pair_instances_wire_statuses_account_ports_like_production`
    // pins that corrected behavior; see this module's own doc comment
    // ("Module wiring lives elsewhere") for exactly which fields were
    // observably wrong before and which happened not to be.
    let ComposedModules {
        oauth: oauth_module,
        federation: federation_module,
        media: media_module,
        accounts: accounts_module,
        statuses: statuses_module,
        social_graph: social_graph_module,
        timelines: timelines_module,
        notifications: notification_module,
        search: search_module,
        federation_background,
        media_background,
    } = compose_modules(ModuleWiringInput {
        pool: pool.clone(),
        runtime: runtime.clone(),
        actor_module: &actor_module,
        config: &config,
        http_client,
        federation_cadence: FederationPollCadence {
            delivery_poll_interval: PAIR_DELIVERY_POLL_INTERVAL,
            delivery_poll_batch_size: 20,
            pruning_interval: PAIR_PRUNING_INTERVAL,
        },
    })
    .await
    .expect("composing a paired instance's module wiring must succeed");

    federation_background.spawn();
    // Started with a shutdown signal that never resolves, mirroring
    // `crate::test_harness::spawn_test_app`'s own identical reasoning: this
    // paired instance's own listener shutdown (`shutdown_tx` below) is a
    // single-consumer `oneshot`, which cannot fan out to several worker
    // tasks.
    media_background.spawn(std::future::pending::<()>);

    let state = AppState::new(
        pool.clone(),
        runtime.clone(),
        config,
        actor_module.clone(),
        oauth_module,
        federation_module,
        media_module,
        accounts_module,
        statuses_module,
        social_graph_module,
        timelines_module,
        notification_module,
        search_module,
    );
    let router = server::build_router(state.clone());

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let server_task = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await
    });

    TestApp::from_parts(TestAppParts {
        address,
        pool,
        runtime,
        actor: actor_module,
        state,
        schema,
        shutdown_tx,
        server_task,
    })
}

/// Two genuinely separate, genuinely reachable [`TestApp`] instances
/// (design.md's exact `FederationTestHarness` Service Interface).
pub struct FederationPair {
    pub a: TestApp,
    pub b: TestApp,
}

/// Boots two isolated instances (`a`, `b`) for federation verification, each
/// with its own isolated database schema and its own real, live, plain-HTTP
/// TCP listener, and each mutually reachable from the other over real
/// loopback TCP (design.md's exact `FederationTestHarness` Service
/// Interface; task 6.4, Requirements 13.1, 13.2, 13.3, 13.4).
///
/// See this module's doc comment ("Why not `spawn_test_app`") for the full
/// reachability-problem reasoning this function's own composition
/// ([`spawn_paired_instance`]) resolves, and ("Dispatch-success
/// observation") for how a caller can observe Requirement 13.3's
/// verification/dispatch-hand-off success without a custom registered
/// handler.
///
/// Callers must call [`TestApp::cleanup`] on both `a` and `b` when done,
/// exactly as a single `spawn_test_app`-built [`TestApp`] requires
/// (Requirement 8.5, unchanged for these instances).
pub async fn spawn_federation_pair() -> FederationPair {
    let a = spawn_paired_instance(Arc::new(ReqwestFederationHttpClient::insecure_loopback())).await;
    let b = spawn_paired_instance(Arc::new(ReqwestFederationHttpClient::insecure_loopback())).await;
    FederationPair { a, b }
}
