//! Pure-function unit coverage for `BearerAuthMiddleware`'s
//! [`super::require_scope`] (task 6.4; Requirements 4.2, 4.3): the two tests
//! below need no database and no running instance, so they stay here.
//!
//! Everything else this module used to hold — the seven tests that drive a
//! real, test-only axum router against a real, `spawn_test_app`-backed
//! Postgres schema (Requirements 5.1-5.5, 4.2, 4.3: valid-token actor
//! resolution, 401 on missing/garbage/revoked, 403 on insufficient scope,
//! top-level scope subsumption, optional-auth continuation) — now lives in
//! `tests/oauth_middleware_it.rs`, moved there by
//! `.kiro/specs/test-placement-migration` task 7.2 per steering
//! `structure.md`'s test layout rule.

use axum::http::StatusCode;

use super::*;
use crate::domain::Id;
use crate::oauth::model::ScopeSet as ModelScopeSet;
use crate::oauth::scope::ScopeSet as RealScopeSet;

// ---- Pure-function unit coverage of `require_scope` (no DB needed): would
// fail if the argument order to `is_satisfied_by` were ever flipped. ----

#[test]
fn require_scope_allows_when_the_required_scope_is_satisfied_by_the_granted_scope() {
    let ctx = RequestActorContext {
        actor_id: Id::from_i64(1),
        scopes: ModelScopeSet::new(["write"]),
    };
    let required = RealScopeSet::parse("write:media").expect("valid scope literal");
    assert!(require_scope(&ctx, &required).is_ok());
}

#[test]
fn require_scope_rejects_with_403_when_the_required_scope_is_missing() {
    let ctx = RequestActorContext {
        actor_id: Id::from_i64(1),
        scopes: ModelScopeSet::new(["read"]),
    };
    let required = RealScopeSet::parse("write:media").expect("valid scope literal");
    let err = require_scope(&ctx, &required).expect_err("missing scope must be rejected");
    assert_eq!(err.status, StatusCode::FORBIDDEN);
}
