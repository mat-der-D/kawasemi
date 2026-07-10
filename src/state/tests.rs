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
    ActorConfig, AppConfig, DatabaseConfig, LogConfig, LogLevel, Secret, ServerConfig,
};
use crate::runtime::{DeterministicSeed, RuntimeContext};

const LAZY_TEST_DB_URL: &str = "postgres://lazy-user:lazy-pw@127.0.0.1:5432/lazy-test-db";

/// Fixed, non-production KEK used only to construct a `KeyCipher` for these
/// tests' `ActorModule` — no real signing key is ever generated/opened
/// against a live database here (see `lazy_pool`'s own doc comment: this
/// suite never actually connects).
const TEST_KEK: [u8; 32] = [11u8; 32];

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

    let state = AppState::new(pool, runtime, config.clone(), actor);

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

    let state = AppState::new(pool, runtime, config, actor);
    assert_eq!(Arc::strong_count(&state.inner), 1);

    let cloned = state.clone();
    assert_eq!(Arc::strong_count(&state.inner), 2);

    // Both handles observe the same config value (same underlying data).
    assert_eq!(state.config(), cloned.config());

    drop(cloned);
    assert_eq!(Arc::strong_count(&state.inner), 1);
}
