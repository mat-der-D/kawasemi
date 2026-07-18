//! OAuth module tree (design.md's File Structure Plan, `src/oauth.rs` + `src/oauth/`).
//!
//! Scope: this file declares the submodules and re-exports task 2.1's
//! domain types for `crate::oauth::*` callers, plus task 2.2's real scope
//! model and task 2.3's real PKCE verification.
//! `model::PkceChallenge` and `model::ScopeSet` (task 2.1's placeholders)
//! are likewise still what `model.rs`'s domain structs hold — wiring them to
//! [`pkce::PkceChallenge`] / [`scope::ScopeSet`] (this module's real
//! implementations) is deferred to a later task per all three modules' doc
//! comments (hence `PkceChallenge`/`ScopeSet` below resolve to the `pkce`/
//! `scope` versions, not `model`'s, matching the pattern already
//! established for `ScopeSet`).
//!
//! [`OauthModule`] (task 7.1, design.md's File Structure Plan comment for
//! this file: "`oauth.rs` # OauthModule 組み立て（サービス/リポジトリ/ミドル
//! ウェアのハンドル束ね）と公開") is the composition-root bundle
//! `crate::state::AppState` holds: it builds the one shared
//! [`service::OauthService`] handle plus the credential/key material every
//! OAuth HTTP endpoint (`apps_endpoint`/`authorize_endpoint`/
//! `token_endpoint`) and the Bearer auth middleware need, so
//! `src/bootstrap.rs`/`src/test_harness.rs` construct it exactly once at
//! startup and every downstream handler retrieves the same instance through
//! `AppState` rather than building its own.

pub mod app_repository;
pub mod apps_endpoint;
pub mod authorize_endpoint;
pub mod code_repository;
pub mod hash;
pub mod middleware;
pub mod model;
pub mod owner_gate;
pub mod pkce;
pub mod scope;
pub mod service;
pub mod templates;
pub mod token_endpoint;
pub mod token_repository;

use std::sync::Arc;

use sqlx::PgPool;

use crate::runtime::RuntimeContext;

pub use hash::TokenHashKey;
pub use middleware::{authenticate, require_authenticated, require_scope};
pub use model::{AccessToken, AuthorizationCode, OauthApp, OwnerSession, RequestActorContext};
pub use owner_gate::OwnerCredential;
pub use pkce::{PkceChallenge, PkceMethod, verify_pkce};
pub use scope::{Scope, ScopeSet};
pub use service::OauthService;

/// Composition-root bundle for OAuth (task 7.1): the one shared
/// [`OauthService`] handle (built from `pool`/`runtime`/`token_hash_key`,
/// mirroring `ActorModule`'s own "bundle already-constructed handles"
/// contract), plus the `pool`/`token_hash_key`/`owner_credential` material
/// every OAuth endpoint module's own `*EndpointState`/`AuthState` bundles
/// duplicate internally (see e.g. `apps_endpoint.rs`'s doc comment,
/// "Bypassing `OauthService`...", for why those duplicate fields rather
/// than reaching into `OauthService`'s private internals), and the
/// `cookie_secure` flag `AuthorizeEndpointState` needs for its owner-session
/// cookie's `Secure` attribute (see [`OauthModule::new`]'s own doc comment).
///
/// `crate::state::AppState` holds this behind one field so
/// `src/server.rs`'s `impl axum::extract::FromRef<AppState> for X` blocks
/// (one per OAuth endpoint state type) can derive each endpoint's own small
/// state bundle from it without re-deriving `OauthService` per request.
pub struct OauthModule {
    service: Arc<OauthService>,
    pool: PgPool,
    token_hash_key: TokenHashKey,
    owner_credential: OwnerCredential,
    cookie_secure: bool,
}

impl OauthModule {
    /// Builds the shared [`OauthService`] from `pool`/`runtime`/
    /// `token_hash_key`, and bundles it alongside the other handles OAuth
    /// endpoint state types need.
    ///
    /// `cookie_secure` controls whether `AuthorizeEndpointState`'s
    /// owner-session `Set-Cookie` header carries the `Secure` attribute
    /// (design.md's Security Considerations: "TLS 配信時は `Secure`").
    /// `AppConfig`/`ServerConfig` (`src/config.rs`) has no TLS-termination
    /// setting to derive this from — this codebase's HTTP listener
    /// (`src/server.rs`) never terminates TLS itself, and a typical
    /// single-owner deployment sits behind a reverse proxy that may or may
    /// not do so — so callers ([`crate::bootstrap::bootstrap`],
    /// [`crate::test_harness::spawn_test_app`]) pass this explicitly rather
    /// than this constructor guessing. See `src/bootstrap.rs`'s call site
    /// for the documented production default and the accompanying CONCERN.
    pub fn new(
        pool: PgPool,
        runtime: RuntimeContext,
        token_hash_key: TokenHashKey,
        owner_credential: OwnerCredential,
        cookie_secure: bool,
    ) -> Self {
        let service = Arc::new(OauthService::new(
            pool.clone(),
            runtime,
            token_hash_key.clone(),
        ));
        Self {
            service,
            pool,
            token_hash_key,
            owner_credential,
            cookie_secure,
        }
    }

    /// The shared [`OauthService`] handle (app registration, code issuance,
    /// token exchange/revocation).
    pub fn service(&self) -> &Arc<OauthService> {
        &self.service
    }

    /// The shared database connection pool, for OAuth endpoint state types
    /// that bypass `OauthService` for a specific read (see this module's
    /// doc comment).
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// The keyed-hashing material backing `client_secret`/code/token hashes
    /// and the owner-session cookie/CSRF signature.
    pub fn token_hash_key(&self) -> &TokenHashKey {
        &self.token_hash_key
    }

    /// The startup-configured owner credential `OwnerGate` authenticates a
    /// login submission against.
    pub fn owner_credential(&self) -> &OwnerCredential {
        &self.owner_credential
    }

    /// Whether the owner-session `Set-Cookie` header should carry the
    /// `Secure` attribute (see [`OauthModule::new`]'s doc comment).
    pub fn cookie_secure(&self) -> bool {
        self.cookie_secure
    }
}
