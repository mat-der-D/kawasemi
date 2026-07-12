//! OAuth module tree (design.md's File Structure Plan, `src/oauth/`).
//!
//! Scope: this file only declares the submodules that exist so far and
//! re-exports task 2.1's domain types for `crate::oauth::*` callers, plus
//! task 2.2's real scope model. `pkce` (task 2.3) is not declared yet —
//! task 2.1's [`model`] module co-locates a minimal placeholder stand-in
//! (`model::PkceChallenge`) for its own domain structs, but the real
//! `src/oauth/pkce.rs` file is that later task's to create from scratch.
//! `model::ScopeSet` (task 2.1's placeholder) is likewise still what
//! `model.rs`'s domain structs hold — wiring them to [`scope::ScopeSet`]
//! (this module's real inclusion-judgment implementation) is deferred to a
//! later task per both modules' doc comments. Full `OauthModule`
//! composition-root assembly (bundling service/repository/middleware
//! handles into one struct `AppState` holds) is task 7.1's job per
//! design.md's File Structure Plan comment for this file (`mod.rs #
//! OauthModule 組み立て...`) — it does not exist yet because none of its
//! dependencies (repositories, service, middleware) have been implemented
//! yet.

pub mod model;
pub mod scope;

pub use model::{AccessToken, AuthorizationCode, OauthApp, OwnerSession, RequestActorContext};
pub use scope::{Scope, ScopeSet};
