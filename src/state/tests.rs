//! Unit tests for the `AppState` shared handle (task 7.1, Requirements 1.1,
//! 3.3, 5.5, 5.6).
//!
//! These tests never touch a real database: `AppState::new` only needs to
//! *hold* a `PgPool` value, not connect one, so every test here builds its
//! pool with `PgPoolOptions::connect_lazy`, which parses the connection
//! string and configures the pool without opening any network connection
//! (sqlx only actually connects lazily, on first use). This keeps the suite
//! fast and independent of local PostgreSQL availability, unlike
//! `src/db/tests.rs`'s "does `establish_pool` really connect" tests, which is
//! not this task's concern. `connect_lazy` still spawns the pool's idle-reaper
//! maintenance task on construction (even though it never dials the
//! database), which requires a Tokio runtime context to be current — hence
//! `#[tokio::test]` rather than a plain `#[test]` for the tests that call it.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use sqlx::postgres::PgPoolOptions;

use super::*;
use crate::actor::keys::cache::KeyCache;
use crate::actor::keys::cipher::{ChaCha20Poly1305KeyCipher, KeyCipher};
use crate::actor::{ActorModule, build_actor_module};
use crate::config::{
    ActorConfig, AppConfig, DatabaseConfig, FederationConfig, LogConfig, LogLevel, MediaConfig,
    OauthConfig, OwnerConfig, Secret, ServerConfig,
};
use crate::federation::signatures::ReqwestFederationHttpClient;
use crate::federation::{FederationModule, FederationWiringConfig, build_federation_module};
use crate::oauth::OauthModule;
use crate::runtime::{DeterministicSeed, RuntimeContext};

const LAZY_TEST_DB_URL: &str = "postgres://lazy-user:lazy-pw@127.0.0.1:5432/lazy-test-db";

/// Fixed, non-production KEK used only to construct a `KeyCipher` for these
/// tests' `ActorModule` — no real signing key is ever generated/opened
/// against a live database here (see `lazy_pool`'s own doc comment: this
/// suite never actually connects).
const TEST_KEK: [u8; 32] = [11u8; 32];

/// Fixed, non-production owner passphrase for [`sample_config`]
/// (api-foundation task 1.2, Requirement 2.2). Mirrors [`TEST_KEK`]'s "why
/// fixed" reasoning.
const TEST_OWNER_PASSWORD: &str = "state-test-owner-passphrase";

/// Fixed, non-production OAuth token-hashing key for [`sample_config`]
/// (api-foundation task 1.2, Requirement 3.6). Mirrors [`TEST_KEK`]'s "why
/// fixed" reasoning.
const TEST_TOKEN_HASH_KEY: [u8; 32] = [13u8; 32];

fn sample_config(domain: &str, max_connections: u32) -> AppConfig {
    AppConfig {
        server: ServerConfig {
            domain: domain.to_string(),
            bind_addr: "0.0.0.0:3000".parse::<SocketAddr>().expect("valid addr"),
            shutdown_grace: Duration::from_secs(30),
        },
        database: DatabaseConfig {
            url: Secret::new(LAZY_TEST_DB_URL.to_string()),
            max_connections,
            acquire_timeout: Duration::from_secs(5),
        },
        log: LogConfig {
            level: LogLevel::Info,
            sql_diagnostic: false,
        },
        actor: ActorConfig {
            kek: Secret::new(TEST_KEK),
        },
        owner: OwnerConfig {
            password: Secret::new(TEST_OWNER_PASSWORD.to_string()),
        },
        oauth: OauthConfig {
            token_hash_key: Secret::new(TEST_TOKEN_HASH_KEY),
        },
        federation: FederationConfig {
            secure_mode: false,
            public_key_cache_ttl: Duration::from_secs(24 * 60 * 60),
            received_activity_retention_days: 14,
        },
        // media-pipeline task 1.2: fixed, non-production values mirroring
        // `load_config_from`'s own defaults (`src/config.rs`) — this test
        // harness constructs `AppConfig` directly rather than through TOML/
        // env parsing, the same way every other startup-config group above
        // is fixed here rather than loaded.
        media: MediaConfig {
            storage_root: std::path::PathBuf::from("media_storage"),
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
    }
}

fn lazy_pool(max_connections: u32) -> sqlx::PgPool {
    PgPoolOptions::new()
        .max_connections(max_connections)
        .connect_lazy(LAZY_TEST_DB_URL)
        .expect("connect_lazy only parses the URL; it never opens a connection")
}

/// Builds an `ActorModule` sharing `pool`/`runtime`, mirroring
/// `src/actor.rs`'s own `build_actor_module` (which these tests reuse
/// directly rather than hand-rolling the same wiring) with a fresh, empty
/// `KeyCache` — these tests never provision/rotate a real key, only assert
/// on `AppState`'s own bundling/cloning behavior, so an empty cache is
/// sufficient.
fn sample_actor_module(pool: sqlx::PgPool, runtime: RuntimeContext) -> ActorModule {
    let cipher: Arc<dyn KeyCipher> =
        Arc::new(ChaCha20Poly1305KeyCipher::new(Secret::new(TEST_KEK)));
    build_actor_module(pool, runtime, cipher, KeyCache::new())
}

/// Builds an `OauthModule` from `config`'s own `oauth.token_hash_key`/
/// `owner.password` (task 7.1) — these tests never touch a real database
/// (see this module's own doc comment), and `OauthModule::new` only stores
/// `pool`/`runtime`/the secret material, matching `sample_actor_module`'s
/// identical "no real I/O" property.
fn sample_oauth_module(
    pool: sqlx::PgPool,
    runtime: RuntimeContext,
    config: &AppConfig,
) -> OauthModule {
    OauthModule::new(
        pool,
        runtime,
        config.oauth.token_hash_key.clone(),
        config.owner.clone(),
        false,
    )
}

/// Builds a `FederationModule` sharing `pool`/`runtime`/`directory` (task
/// 5.4), mirroring `sample_actor_module`/`sample_oauth_module`'s own "no
/// real I/O beyond what construction itself needs" property:
/// `build_federation_module`'s own constructors only ever store `pool`
/// (never dial it), so this is safe against the same `connect_lazy` pool
/// this suite's other fixtures use. The returned background-tasks handle is
/// deliberately dropped without calling `.spawn()` — these tests assert only
/// on `AppState`'s own bundling/cloning behavior, not on federation-core's
/// live delivery/pruning loops, so there is nothing for a background task to
/// usefully do here (and spawning one against a `connect_lazy` pool pointed
/// at a fake URL would just be a source of unrelated background log noise).
fn sample_federation_module(
    pool: sqlx::PgPool,
    runtime: RuntimeContext,
    directory: Arc<crate::actor::ActorDirectory>,
) -> FederationModule {
    let (federation, _background_tasks_not_spawned) = build_federation_module(
        pool,
        runtime,
        directory,
        FederationWiringConfig::production(
            "state-test.federation.internal".to_string(),
            false,
            time::Duration::hours(24),
            time::Duration::days(14),
        ),
        Arc::new(ReqwestFederationHttpClient::new()),
    );
    federation
}

/// Requirements 1.1, 3.3, 5.5, 5.6: downstream code must be able to retrieve
/// the pool, the injection boundaries (via `RuntimeContext`), and the
/// validated config values from `AppState`, unchanged from what was passed
/// to the constructor.
#[tokio::test]
async fn app_state_exposes_the_pool_runtime_context_and_config_it_was_built_with() {
    let seed = DeterministicSeed::new(99);
    let runtime = RuntimeContext::deterministic(seed);
    let config = sample_config("state.example.test", 7);
    let pool = lazy_pool(config.database.max_connections);
    let actor = sample_actor_module(pool.clone(), runtime.clone());
    let oauth = sample_oauth_module(pool.clone(), runtime.clone(), &config);
    let federation =
        sample_federation_module(pool.clone(), runtime.clone(), Arc::clone(actor.directory()));

    let state = AppState::new(pool, runtime, config.clone(), actor, oauth, federation);

    // Config values are retrievable and match what was supplied.
    assert_eq!(state.config().server.domain, "state.example.test");
    assert_eq!(state.config(), &config);

    // The pool is retrievable and carries through the configured pool size
    // (proving it's the same pool handed to the constructor, not rebuilt).
    assert_eq!(
        state.pool().options().get_max_connections(),
        config.database.max_connections
    );

    // The runtime context (and its injection boundaries) is retrievable:
    // comparing against an independently-built context from the same seed
    // proves the *values* line up (RuntimeContext has no PartialEq, so we
    // compare through its boundaries, mirroring runtime.rs's own tests).
    let expected_runtime = RuntimeContext::deterministic(seed);
    assert_eq!(state.runtime().clock.now(), expected_runtime.clock.now());
    assert_eq!(
        state.runtime().ids.next_id(),
        expected_runtime.ids.next_id()
    );
}

/// Requirement: `AppState` must be usable as axum shared state, which
/// requires `Clone + Send + Sync + 'static` (`axum::extract::State<S>`'s
/// bound on `S`). This is a compile-time proof: if `AppState` stopped
/// satisfying these bounds, this test would fail to compile rather than
/// fail at runtime.
#[test]
fn app_state_satisfies_axum_shared_state_bounds() {
    fn assert_axum_state_bounds<S: Clone + Send + Sync + 'static>() {}
    assert_axum_state_bounds::<AppState>();
}

/// design.md's AppState "State Management" section: "起動時構築、以後共有のみ"
/// (built once, shared thereafter) via `Arc` sharing with no interior
/// mutability — cloning `AppState` must be cheap (a single `Arc` bump), not
/// a deep copy of the pool/runtime/config. Proven here by checking the
/// shared inner `Arc`'s strong count increases with each clone, which would
/// not happen if `Clone` instead deep-copied the state.
#[tokio::test]
async fn cloning_app_state_shares_the_same_inner_handle_instead_of_deep_copying() {
    let runtime = RuntimeContext::deterministic(DeterministicSeed::new(1));
    let config = sample_config("clone.example.test", 3);
    let pool = lazy_pool(config.database.max_connections);
    let actor = sample_actor_module(pool.clone(), runtime.clone());
    let oauth = sample_oauth_module(pool.clone(), runtime.clone(), &config);
    let federation =
        sample_federation_module(pool.clone(), runtime.clone(), Arc::clone(actor.directory()));

    let state = AppState::new(pool, runtime, config, actor, oauth, federation);
    assert_eq!(Arc::strong_count(&state.inner), 1);

    let cloned = state.clone();
    assert_eq!(Arc::strong_count(&state.inner), 2);

    // Both handles observe the same config value (same underlying data).
    assert_eq!(state.config(), cloned.config());

    drop(cloned);
    assert_eq!(Arc::strong_count(&state.inner), 1);
}
