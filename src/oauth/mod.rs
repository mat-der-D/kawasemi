//! OAuth module tree (design.md's File Structure Plan, `src/oauth/`).
//!
//! Scope: this file only declares the submodules that exist so far and
//! re-exports task 2.1's domain types for `crate::oauth::*` callers, plus
//! task 2.2's real scope model and task 2.3's real PKCE verification.
//! `model::PkceChallenge` and `model::ScopeSet` (task 2.1's placeholders)
//! are likewise still what `model.rs`'s domain structs hold — wiring them to
//! [`pkce::PkceChallenge`] / [`scope::ScopeSet`] (this module's real
//! implementations) is deferred to a later task per all three modules' doc
//! comments (hence `PkceChallenge`/`ScopeSet` below resolve to the `pkce`/
//! `scope` versions, not `model`'s, matching the pattern already
//! established for `ScopeSet`). Full `OauthModule` composition-root
//! assembly (bundling service/repository/middleware handles into one struct
//! `AppState` holds) is task 7.1's job per design.md's File Structure Plan
//! comment for this file (`mod.rs # OauthModule 組み立て...`) — it does not
//! exist yet because none of its dependencies (repositories, service,
//! middleware) have been implemented yet.

pub mod app_repository;
pub mod code_repository;
pub mod hash;
pub mod model;
pub mod pkce;
pub mod scope;
pub mod token_repository;

pub use model::{AccessToken, AuthorizationCode, OauthApp, OwnerSession, RequestActorContext};
pub use pkce::{PkceChallenge, PkceMethod, verify_pkce};
pub use scope::{Scope, ScopeSet};
