//! `BearerAuthMiddleware` (design.md "API Cross-cutting" -> `BearerAuthMiddleware`;
//! Requirements 5.1, 5.2, 5.3, 5.4, 5.5, 4.2, 4.3; task 6.4): resolves a
//! presented Bearer token into a single-actor [`RequestActorContext`] +
//! approved scopes, rejects a missing/invalid/revoked token with 401 on
//! mandatory-auth endpoints, and rejects insufficient scope with 403 —
//! reusing [`token_repository::resolve_token`] (task 3.3) for resolution and
//! [`scope::ScopeSet::is_satisfied_by`] (task 2.2) for scope inclusion,
//! never reimplementing either.
//!
//! Scope: this module owns exactly the two functions design.md's Service
//! Interface sketches for `BearerAuthMiddleware` — [`authenticate`] /
//! [`require_scope`] — adapted per the reconciliation below, plus a thin,
//! genuinely reusable axum integration ([`AuthState`], [`OptionalActor`],
//! [`RequiredActor`]) proving those functions work through real axum
//! request extraction, not just as bare async functions. It does not touch
//! `AppState`/`src/server.rs`/`src/bootstrap.rs` (task 7.1's job, same
//! precedent as tasks 6.1-6.3), does not implement token resolution itself
//! (`token_repository.rs`, task 3.3, already reviewed), and does not
//! reimplement scope inclusion judgment (`scope.rs`, task 2.2, already
//! reviewed).
//!
//! ## Reconciling design.md's `authenticate(state: &AppState, ...)` sketch
//! (CONCERN — documented judgment call, same precedent as tasks 6.1-6.3)
//! design.md's illustrative Service Interface is:
//! ```ignore
//! pub async fn authenticate(state: &AppState, bearer: Option<&str>) -> Result<Option<RequestActorContext>, AppError>;
//! pub fn require_scope(ctx: &RequestActorContext, required: &ScopeSet) -> Result<(), AppError>;
//! ```
//! `crate::state::AppState` (task 7.4/6.1, already reviewed) does not carry
//! an OAuth pool/token-hash-key handle at all yet — adding one is explicitly
//! task 7.1's job (design.md's "Modified Files": "`src/state.rs`
//! ...task 7.1"). Mirroring `OauthService::new`'s (`service.rs`, task 4.2)
//! already-established shape for threading this same key material
//! explicitly, [`authenticate`] instead takes `&PgPool` and
//! `&hash::TokenHashKey` directly. `require_scope`'s signature is kept
//! exactly as sketched (see the next section for what its `ScopeSet`
//! actually resolves to).
//!
//! ## `RequestActorContext::scopes` is `model::ScopeSet` (the placeholder),
//! not `scope::ScopeSet` (CONCERN — documented judgment call correcting a
//! wrong assumption)
//! A shallow reading of `src/oauth/mod.rs`'s `pub use scope::{Scope,
//! ScopeSet};` (with no competing `model::ScopeSet` re-export) might suggest
//! `crate::oauth::RequestActorContext.scopes` resolves to the real
//! `scope::ScopeSet`. It does not: `model.rs` (task 2.1) defines and uses
//! its own local `ScopeSet` placeholder (a bare `BTreeSet<String>`, no
//! inclusion judgment) for every one of its domain structs' `scopes`
//! fields, including `RequestActorContext`, and never imports
//! `scope::ScopeSet` — confirmed by reading `model.rs`'s own `use`
//! statements and struct bodies directly (no alias trickery is possible
//! across separate files without an explicit `use`). Task 4.2's
//! Implementation Notes entry in `tasks.md` already documents this same
//! placeholder/real split for `service.rs`'s own scope bridging; this
//! module's situation is the same split applied to
//! `RequestActorContext.scopes` specifically, which task 4.2's note does not
//! itself mention (its own bridging is for `AuthorizationCode`/`AccessToken`
//! persistence, not `RequestActorContext`).
//!
//! One consequence simplifies this module: since [`model::AccessToken`]
//! (what [`token_repository::resolve_token`] returns) and
//! [`model::RequestActorContext`] (re-exported as
//! [`crate::oauth::RequestActorContext`]) both hold `model::ScopeSet`,
//! [`authenticate`] needs **no** scope bridging at all — it copies
//! `access_token.scopes` straight into the new `RequestActorContext`
//! unchanged. The bridging instead has to happen inside [`require_scope`],
//! which is handed a real `scope::ScopeSet` (the type that actually has
//! `is_satisfied_by`) as `required`, but only has a `model::ScopeSet` inside
//! `ctx.scopes` to compare it against. [`require_scope`] therefore converts
//! `ctx.scopes` via a private `to_real_scopes` helper before calling
//! `required.is_satisfied_by(&granted)` — deliberately near-identical to
//! `service.rs`'s own private `to_real_scopes` (not reused directly: that
//! function is private to `service.rs`, and task 4.2's Implementation Notes
//! entry already establishes "small helper duplication across sibling
//! modules" as this crate's convention for exactly this situation, alongside
//! `app_repository.rs`/`token_repository.rs`'s duplicated
//! `random_url_safe_token` and `code_repository.rs`'s duplicated
//! `join_scopes`/`parse_scopes`). A re-parse failure is treated as a
//! `Server` (5xx) error, matching `service.rs::to_real_scopes`'s identical
//! reasoning: every string a `model::ScopeSet` can hold in practice was
//! itself produced from an already-validated `scope::ScopeSet` (by
//! `token_repository::issue_token`'s `join_scopes`), so a failure to
//! re-parse indicates corrupted/foreign data, not a caller mistake.
//!
//! ## `resolve_token`'s collapsed "missing vs. revoked" outcome is not
//! re-split here (Requirement 5.2)
//! [`token_repository::resolve_token`] already returns `Ok(None)` uniformly
//! for "no such token" and "revoked token" (its own module doc comment,
//! "Revoked tokens are invalid on resolution"). [`authenticate`] treats that
//! `Ok(None)` as exactly one outcome — "this presented token does not
//! authenticate" — mapped to a single 401 [`AppError`], never attempting to
//! distinguish the two beneath it (there is nothing left to distinguish:
//! the repository already collapsed that decision).
//!
//! ## Optional vs. mandatory auth (Requirements 5.2, 5.4)
//! [`authenticate`] itself is the *optional*-auth primitive: `bearer: None`
//! (no `Authorization: Bearer ...` header presented at all) returns
//! `Ok(None)` and never errors — Requirement 5.4's "任意認証" continuation
//! case. A **presented** token that fails to resolve (missing/invalid/
//! revoked) is always a 401 [`AppError`], regardless of optional-vs-
//! mandatory mode: Requirement 5.4 only carves out an exception for a token
//! *not being presented*, not for a bad token being presented on an
//! optional-auth route. Endpoints that require authentication (the default,
//! unqualified case Requirement 5.2 describes) call [`authenticate`] and
//! then run its `Ok(None)` result through [`require_authenticated`], which
//! turns "no token was presented" into the same 401 an invalid/revoked
//! token already produces.
//!
//! ## Axum integration shape: `AuthState` + `FromRequestParts` extractors,
//! not `AppState`/a `tower::Layer` (judgment call)
//! Design.md's architecture diagram draws this component as
//! `AuthLayer`/"レイヤー/抽出器" but sketches no concrete interface beyond
//! `authenticate`/`require_scope` themselves. Mirroring
//! `apps_endpoint.rs`'s already-reviewed precedent of defining its own
//! small, self-contained `AppsEndpointState` (since `AppState` cannot carry
//! OAuth wiring yet, task 7.1's job), this module defines [`AuthState`] —
//! exactly the `&PgPool`/`&TokenHashKey` pair [`authenticate`] needs,
//! nothing more — as an `axum::extract::State`-compatible bundle, and
//! implements [`axum::extract::FromRequestParts<AuthState>`] for
//! [`OptionalActor`] (Requirement 5.4's optional-auth primitive) and
//! [`RequiredActor`] (built from [`OptionalActor`] plus
//! [`require_authenticated`], Requirement 5.2's mandatory-auth primitive).
//! A `tower::Layer` (mirroring `RateLimitLayer`, task 6.3) was considered
//! and rejected: a `tower::Layer` naturally protects an entire route
//! *subtree* uniformly, but Requirement 5.4's "エンドポイントが認証を任意と
//! する場合" is a **per-endpoint** choice (some routes mandatory, some
//! optional), which an axum extractor selected per-handler expresses
//! directly (`OptionalActor` vs. `RequiredActor` as a handler parameter)
//! without needing two separately-layered sub-routers. `require_scope`
//! likewise stays a plain function callers invoke inside their own handler
//! body (as `tests.rs::scoped_probe` does) rather than a third extractor
//! type, because the *required* `ScopeSet` a route demands is itself
//! per-endpoint data an extractor has no way to receive.
//!
//! `tests.rs` proves all of the above through a real, test-only axum
//! `Router` dispatched via `tower::ServiceExt::oneshot` against a real,
//! `spawn_test_app`-backed Postgres schema (per this task's brief: unlike
//! `ratelimit`'s tests, resolution genuinely needs the database, so nothing
//! here is faked) — not a `tests/*_it.rs` full production-router
//! integration test, since nothing wires this middleware into that router
//! yet (task 7.1's job, same boundary tasks 6.1-6.3 already established).

#[cfg(test)]
mod tests;

use axum::extract::{FromRef, FromRequestParts};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode, header};

use crate::error::AppError;
use crate::oauth::hash::TokenHashKey;
use crate::oauth::model::{self, RequestActorContext};
use crate::oauth::scope;
use crate::oauth::token_repository;
use sqlx::postgres::PgPool;

/// Caller-facing message for a missing/invalid/revoked bearer token
/// (Requirement 5.2). Deliberately generic — mirrors
/// `token_repository::resolve_token`'s own "don't leak existence" framing
/// by never hinting at *which* of missing/invalid/revoked applies.
const UNAUTHENTICATED_MESSAGE: &str = "the access token is missing, invalid, or has been revoked";

/// Caller-facing message for insufficient scope (Requirements 4.2, 4.3).
const INSUFFICIENT_SCOPE_MESSAGE: &str =
    "this action requires a scope the access token was not granted";

/// Converts a placeholder [`model::ScopeSet`] (as carried by
/// `RequestActorContext.scopes`) into the real [`scope::ScopeSet`] so
/// [`scope::ScopeSet::is_satisfied_by`] can be applied to it. See this
/// module's doc comment for why a re-parse failure here is a `Server` (5xx)
/// error, not a caller-facing rejection — deliberately near-identical to
/// `service.rs::to_real_scopes` (private to that module, hence not reused
/// directly; see this module's doc comment for why this small duplication
/// follows an already-established crate convention).
fn to_real_scopes(placeholder: &model::ScopeSet) -> Result<scope::ScopeSet, AppError> {
    let joined = placeholder.as_strs().collect::<Vec<_>>().join(" ");
    scope::ScopeSet::parse(&joined).map_err(|_| {
        AppError::server(
            StatusCode::INTERNAL_SERVER_ERROR,
            std::io::Error::other(
                "stored OAuth scope set failed to re-parse against the real scope vocabulary",
            ),
        )
    })
}

/// Resolves `bearer` into a single-actor [`RequestActorContext`] (Requirements
/// 5.1, 5.3), or `Ok(None)` when no token was presented at all (Requirement
/// 5.4's optional-auth continuation case). A **presented** token that fails
/// to resolve (missing/invalid/revoked, per
/// [`token_repository::resolve_token`]'s collapsed "don't leak existence"
/// outcome) is always `Err` (401), in every auth mode — see this module's
/// doc comment ("Optional vs. mandatory auth") for why that is not
/// conditional on optional-vs-mandatory. Callers that require authentication
/// should further pass this function's `Ok` value through
/// [`require_authenticated`].
pub async fn authenticate(
    pool: &PgPool,
    token_hash_key: &TokenHashKey,
    bearer: Option<&str>,
) -> Result<Option<RequestActorContext>, AppError> {
    let Some(token) = bearer else {
        return Ok(None);
    };

    let resolved = token_repository::resolve_token(pool, token_hash_key, token).await?;
    match resolved {
        None => Err(AppError::client(
            StatusCode::UNAUTHORIZED,
            UNAUTHENTICATED_MESSAGE,
        )),
        Some(access_token) => Ok(Some(RequestActorContext {
            actor_id: access_token.actor_id,
            // No bridging needed: `AccessToken.scopes` and
            // `RequestActorContext.scopes` are both `model::ScopeSet` — see
            // this module's doc comment.
            scopes: access_token.scopes,
        })),
    }
}

/// Turns [`authenticate`]'s optional-auth `Ok(None)` result into a 401
/// [`AppError`] for endpoints that require authentication (Requirement
/// 5.2's default, unqualified case) — the "None" -> 401 half of this
/// module's optional-vs-mandatory split (see module doc comment).
pub fn require_authenticated(
    ctx: Option<RequestActorContext>,
) -> Result<RequestActorContext, AppError> {
    ctx.ok_or_else(|| AppError::client(StatusCode::UNAUTHORIZED, UNAUTHENTICATED_MESSAGE))
}

/// Enforces that `ctx`'s granted scopes satisfy `required` (Requirements
/// 4.2, 4.3, 5.2's 403 case), reusing [`scope::ScopeSet::is_satisfied_by`]
/// (task 2.2) as the single shared inclusion judgment (Requirement 4.5) —
/// never reimplemented here. `required` is the *required* set (`self` in
/// `is_satisfied_by`'s own signature); `ctx.scopes` (bridged to a real
/// `scope::ScopeSet` via `to_real_scopes`) is the *granted* set.
pub fn require_scope(
    ctx: &RequestActorContext,
    required: &scope::ScopeSet,
) -> Result<(), AppError> {
    let granted = to_real_scopes(&ctx.scopes)?;
    if required.is_satisfied_by(&granted) {
        Ok(())
    } else {
        Err(AppError::client(
            StatusCode::FORBIDDEN,
            INSUFFICIENT_SCOPE_MESSAGE,
        ))
    }
}

/// Extracts a presented `Authorization: Bearer <token>` header's token
/// value, if any. Any other outcome (header absent, non-UTF-8 value, a
/// different auth scheme such as `Basic`) is treated as "no bearer token
/// presented" (`None`), not an error — mirroring
/// `apps_endpoint.rs::extract_basic_credentials`'s discipline of never
/// panicking on attacker-controlled input, but returning `None` here
/// instead of a rejection since an absent/mismatched scheme is exactly
/// [`authenticate`]'s own "no token presented" case, not a malformed-token
/// case.
fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?;
    let raw = value.to_str().ok()?;
    raw.strip_prefix("Bearer ").map(str::to_string)
}

/// The minimal state [`OptionalActor`]/[`RequiredActor`] need (see this
/// module's doc comment, "Axum integration shape") — `&PgPool` and
/// `&TokenHashKey`, nothing more.
///
/// Task 7.1 bridges this from the wider `crate::state::AppState` via
/// `impl axum::extract::FromRef<AppState> for AuthState` (`src/server.rs`),
/// which is exactly why [`OptionalActor`]/[`RequiredActor`] below implement
/// `FromRequestParts<S>` generically over any `S: AuthState: FromRef<S>`
/// rather than only `FromRequestParts<AuthState>`: `axum::extract::State<T>`
/// gets this same "derive a narrower state from a wider one via `FromRef`"
/// promotion for free from axum itself, but a hand-written extractor like
/// `OptionalActor`/`RequiredActor` (which is not `State<T>`) does not — it
/// has to ask for the promotion explicitly, which is what the `where
/// AuthState: FromRef<S>` bound on each impl below does. This is what lets
/// these two extractors be used directly inside a handler mounted on
/// `Router<AppState>` (task 7.1's production router), not only inside this
/// module's own `AuthState`-only test router (`tests.rs`'s `test_router`,
/// which keeps working unchanged: `AuthState: FromRef<AuthState>` holds via
/// axum-core's blanket reflexive `impl<T: Clone> FromRef<T> for T`).
#[derive(Clone)]
pub struct AuthState {
    pub pool: PgPool,
    pub token_hash_key: TokenHashKey,
}

/// Axum extractor for an optional-auth endpoint (Requirement 5.4): resolves
/// a presented bearer token into `Some(RequestActorContext)`, or `None` if
/// no token was presented — never rejects merely for a missing header. A
/// **presented but invalid/revoked** token still rejects with 401 (see
/// [`authenticate`]'s doc comment).
pub struct OptionalActor(pub Option<RequestActorContext>);

impl<S> FromRequestParts<S> for OptionalActor
where
    AuthState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let auth_state = AuthState::from_ref(state);
        let bearer = bearer_token(&parts.headers);
        let ctx = authenticate(&auth_state.pool, &auth_state.token_hash_key, bearer.as_deref())
            .await?;
        Ok(OptionalActor(ctx))
    }
}

/// Axum extractor for a mandatory-auth endpoint (Requirement 5.2, the
/// default case): resolves a presented bearer token into a single-actor
/// `RequestActorContext`, rejecting with 401 for a missing, invalid, or
/// revoked token alike. Built directly on top of [`OptionalActor`] +
/// [`require_authenticated`] — no separate resolution logic.
pub struct RequiredActor(pub RequestActorContext);

impl<S> FromRequestParts<S> for RequiredActor
where
    AuthState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let OptionalActor(ctx) = OptionalActor::from_request_parts(parts, state).await?;
        Ok(RequiredActor(require_authenticated(ctx)?))
    }
}
