//! OAuth module tree (design.md's File Structure Plan, `src/oauth/`).
//!
//! Scope: this file only declares the submodules that exist so far and
//! re-exports task 2.1's domain types for `crate::oauth::*` callers. `scope`
//! (task 2.2) and `pkce` (task 2.3) are not declared yet — task 2.1's
//! [`model`] module co-locates minimal placeholder stand-ins
//! (`model::ScopeSet`, `model::PkceChallenge`) for its own domain structs,
//! but the real `src/oauth/scope.rs` / `src/oauth/pkce.rs` files are those
//! later tasks' to create from scratch. Full `OauthModule` composition-root
//! assembly (bundling service/repository/middleware handles into one struct
//! `AppState` holds) is task 7.1's job per design.md's File Structure Plan
//! comment for this file (`mod.rs # OauthModule 組み立て...`) — it does not
//! exist yet because none of its dependencies (repositories, service,
//! middleware) have been implemented yet.

pub mod model;

pub use model::{AccessToken, AuthorizationCode, OauthApp, OwnerSession, RequestActorContext};
