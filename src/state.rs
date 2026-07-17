//! Shared, immutable application state handle (AppState boundary,
//! Requirements 1.1, 3.3, 5.5, 5.6).
//!
//! Scope: this module owns bundling the things every downstream
//! handler/service needs a shared reference to — the database connection
//! pool (`PgPool`, established by `db::establish_pool`, task 4.1), the
//! non-determinism injection boundaries (`RuntimeContext`, task 5.6), the
//! validated startup configuration (`AppConfig`, task 2.1), the actor-model
//! service bundle (`ActorModule`, actor-model task 6.1), and (as of
//! api-foundation's task 7.1) the OAuth service bundle (`OauthModule`) —
//! behind a single handle that is cheap to clone and safe to share across
//! concurrent axum request handlers.
//!
//! Per design.md's "AppState" component ("State Management": "不変（起動時構築、
//! 以後共有のみ）" / "`Arc` 共有、内部可変性なし"), `AppState` is built once at
//! startup and never mutated afterward — there is no interior mutability
//! here beyond whatever `PgPool` and `RuntimeContext` already provide
//! internally on their own terms. Cloning `AppState` only bumps a single
//! `Arc`'s reference count; it never deep-copies the pool, runtime context,
//! or config.
//!
//! This module does not construct its own dependencies: building the real
//! `PgPool`/`RuntimeContext`/`AppConfig` values and wiring them together at
//! process startup is the Bootstrap composition root's job (task 7.4, out
//! of scope here per this task's boundary).

#[cfg(test)]
mod tests;

use std::sync::Arc;

use sqlx::PgPool;

use crate::actor::ActorModule;
use crate::config::AppConfig;
use crate::federation::FederationModule;
use crate::oauth::OauthModule;
use crate::runtime::RuntimeContext;

/// The data `AppState` bundles, held behind a single `Arc` so cloning the
/// outer handle is one atomic increment rather than a deep copy of any of
/// these fields.
struct AppStateInner {
    pool: PgPool,
    runtime: RuntimeContext,
    config: AppConfig,
    actor: ActorModule,
    oauth: OauthModule,
    /// federation-core's port bundle (task 5.4, Requirements 7.3, 10.1,
    /// 11.1, 11.2): every federation-core port constructed with one
    /// concrete production type, shared the same way `actor`/`oauth` are —
    /// see `crate::federation::FederationModule`'s own doc comment.
    federation: FederationModule,
}

/// Immutable, cheaply-cloneable shared handle bundling the database
/// connection pool, the non-determinism injection boundaries, and the
/// validated startup configuration (design.md's "AppState").
///
/// Satisfies `Clone + Send + Sync + 'static`, which is what
/// `axum::extract::State<S>` requires of its type parameter, so this can be
/// used directly as axum shared state (e.g. `Router::new().with_state(app_state)`
/// and handlers taking `State<AppState>`).
#[derive(Clone)]
pub struct AppState {
    inner: Arc<AppStateInner>,
}

impl AppState {
    /// Builds an `AppState` from an already-established pool, an
    /// already-constructed runtime context, an already-validated config, an
    /// already-assembled actor-model service bundle, and an
    /// already-assembled OAuth service bundle (task 7.1, api-foundation's
    /// `OauthModule`). Callers (the Bootstrap composition root, task 7.4/6.1
    /// and 7.1) are responsible for constructing each of these first — this
    /// constructor only bundles them.
    pub fn new(
        pool: PgPool,
        runtime: RuntimeContext,
        config: AppConfig,
        actor: ActorModule,
        oauth: OauthModule,
        federation: FederationModule,
    ) -> Self {
        Self {
            inner: Arc::new(AppStateInner {
                pool,
                runtime,
                config,
                actor,
                oauth,
                federation,
            }),
        }
    }

    /// The shared database connection pool (Requirement 3.3: downstream
    /// components retrieve the pool established by `db::establish_pool`
    /// through `AppState`, rather than each establishing their own).
    pub fn pool(&self) -> &PgPool {
        &self.inner.pool
    }

    /// The shared non-determinism injection boundaries (clock / id
    /// generator / rng / signing key), for downstream code to draw
    /// deterministic-or-production time/id/rng/key values from without
    /// depending on concrete implementations directly (Requirements 5.5,
    /// 5.6).
    pub fn runtime(&self) -> &RuntimeContext {
        &self.inner.runtime
    }

    /// The validated startup configuration this state was built with
    /// (Requirement 1.1: downstream code retrieves config values — e.g.
    /// server/database/log settings — through `AppState` rather than
    /// re-reading configuration itself).
    pub fn config(&self) -> &AppConfig {
        &self.inner.config
    }

    /// The shared actor-model service bundle (Requirements 6.1, 6.4):
    /// downstream handlers (future specs, e.g. api-foundation) retrieve
    /// `ActorService`/`SigningKeyService`/`ActorDirectory` through this one
    /// handle rather than constructing their own, so every caller observes
    /// the same `KeyCache`-backed signing-key supply this instance was
    /// booted with.
    pub fn actor(&self) -> &ActorModule {
        &self.inner.actor
    }

    /// The shared OAuth service bundle (task 7.1, Requirements 1.1, 2.1,
    /// 3.1, 5.1): downstream handlers (`src/server.rs`'s OAuth endpoint
    /// wiring, and the Bearer auth middleware via `AuthState`) retrieve the
    /// one shared `OauthService`/pool/token-hash-key/owner-credential handle
    /// through this accessor rather than each constructing their own.
    pub fn oauth(&self) -> &OauthModule {
        &self.inner.oauth
    }

    /// The shared federation-core port bundle (task 5.4, Requirements 7.3,
    /// 10.1, 11.1, 11.2): `src/server.rs`'s `FromRef<AppState>` bridges for
    /// every federation endpoint's own state type derive from this handle,
    /// and downstream code (a later spec's own service layer) retrieves the
    /// delivery service / registers into the object-document / outbox-source
    /// registries through it — see `crate::federation::FederationModule`'s
    /// own doc comment for the full downstream-registration surface.
    pub fn federation(&self) -> &FederationModule {
        &self.inner.federation
    }
}
