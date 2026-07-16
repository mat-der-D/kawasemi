//! `TokenEndpoint` (design.md "OAuth API / エンドポイント層" ->
//! "AppsEndpoint / AuthorizeEndpoint / TokenEndpoint"; Requirements 3.1,
//! 3.2, 3.4; task 5.3): `POST /oauth/token` and `POST /oauth/revoke` — the
//! authorization-code-grant token exchange and token revocation endpoints.
//!
//! Scope: this module owns exactly the two handler functions design.md's
//! API Contract table sketches for `TokenEndpoint` — [`exchange_token`]/
//! [`revoke_token`] — and the request/response wire shapes they use. It
//! delegates all OAuth business validation to
//! [`crate::oauth::service::OauthService`] (task 4.2, already implemented
//! and reviewed) and does not implement `AppsEndpoint`/`AuthorizeEndpoint`
//! (tasks 5.1/5.2, already done), the Mastodon-compatible error JSON body
//! renderer (`api::error`/`MastodonError`, task 6.1 — every rejection here
//! is a plain [`AppError`], exactly as `apps_endpoint.rs`/
//! `authorize_endpoint.rs` already established), or any router/`AppState`
//! wiring (`src/state.rs`/`src/bootstrap.rs`/`src/server.rs`, task 7.1).
//!
//! ## Feature Flag Protocol: not applicable
//! Mirrors `AppsEndpoint`'s/`AuthorizeEndpoint`'s own identical reasoning: a
//! brand-new module with no existing callers (the router-mounting task,
//! 7.1, has not run) and no previously-observable behavior to gate. A
//! standard RED -> GREEN cycle is this crate's established verification
//! method here.
//!
//! ## Not wired into a router: handlers tested directly, not over HTTP
//! Identical to `apps_endpoint.rs`'s/`authorize_endpoint.rs`'s own
//! documented reasoning: [`exchange_token`]/[`revoke_token`] are plain
//! async functions with axum extractor parameters (`State<TokenEndpointState>`,
//! `Form<_>`) — handler-*shaped* so task 7.1 can mount them onto a real
//! `Router` verbatim, but nothing in this crate currently does so. This
//! module's own tests (`src/oauth/token_endpoint/tests.rs`,
//! `tests/oauth_token_it.rs`) construct axum extractor values directly and
//! call these functions as ordinary async Rust functions.
//!
//! ## `TokenEndpointState`: not `AppState`, duplicates fields `OauthService`
//! holds internally
//! Mirrors `AppsEndpointState`'s/`AuthorizeEndpointState`'s own documented
//! reasoning: `crate::state::AppState` does not yet hold an `OauthModule`
//! (task 7.1's job), so this module defines its own small state bundle. It
//! holds `pool`/`token_hash_key` *alongside* `service: Arc<OauthService>`
//! for the same reason `AppsEndpointState` does — see "Revoke authenticates
//! the client itself" below for the one operation that needs the bypass.
//!
//! ## Wire format: `Form<_>`, not `Json<_>` (judgment call, follows task
//! 5.2's precedent rather than task 5.1's)
//! `AppsEndpoint` (task 5.1) uses `Json<_>` for `POST /api/v1/apps` because
//! that endpoint is Mastodon's own bespoke app-registration API, not part of
//! RFC 6749 itself. `POST /oauth/token` (RFC 6749 section 4.1.3) and
//! `POST /oauth/revoke` (RFC 7009 section 2.1) are, by contrast, the actual
//! OAuth 2.0 protocol endpoints, and both RFCs mandate
//! `application/x-www-form-urlencoded` request bodies — exactly the format
//! `AuthorizeEndpoint` (task 5.2) already chose for `POST /oauth/authorize`
//! via `axum::extract::Form` (see that module's own `AuthorizeSubmission`).
//! Real OAuth client libraries (and Mastodon's own server) send form-encoded
//! bodies to these two endpoints, so `Form<_>` is both spec-correct and the
//! established precedent for OAuth-protocol (as opposed to Mastodon-REST)
//! endpoints in this crate. [`TokenExchangeRequest`]/[`RevokeRequest`] give
//! every field `#[serde(default)]`, mirroring `AuthorizeSubmission`'s/
//! `RegisterAppRequest`'s identical "permissive body, validation delegated
//! downstream" rationale.
//!
//! ## `grant_type` validation (Requirement 3.1, judgment call)
//! `OauthService::exchange_token`'s `TokenRequest` (task 4.2, already
//! reviewed) carries no `grant_type` field at all — that module's own doc
//! comment explicitly defers `grant_type` dispatch to "an endpoint-layer
//! concern, not this service's". This module is that endpoint layer: an
//! absent `grant_type` (an OAuth client that only ever speaks the
//! authorization-code grant and omits the field) is treated as implicitly
//! `authorization_code` and proceeds; any *present* value other than
//! `"authorization_code"` is rejected with a `400` before `OauthService` is
//! ever touched (RFC 6749 section 5.2's `unsupported_grant_type`) — this
//! service has no other grant type implemented, so accepting the request
//! and silently ignoring an explicit `grant_type=client_credentials` (say)
//! would be wrong.
//!
//! ## Exchange-failure status codes are `OauthService`'s, propagated
//! unchanged (CONCERN — documented judgment call, deviates from this task's
//! own brief)
//! `OauthService::exchange_token` (task 4.2, already reviewed, out of this
//! task's boundary to modify) deliberately collapses *every* rejection
//! reason — unknown/wrong client credentials, an invalid/expired/already-
//! consumed code, a mismatched `redirect_uri` — into the exact same `400
//! Bad Request` `AppError` (its own doc comment: "`exchange_token`'s
//! consume-then-validate ordering"), and a PKCE mismatch is likewise a `400`
//! (`pkce::verify_pkce`). This module's execution protocol lists invalid
//! client credentials as "(401-equivalent)", but propagating
//! `OauthService`'s already-reviewed error unchanged — rather than
//! inspecting its `public_message`/some side channel to re-map wrong-
//! credentials specifically to `401` — is the correct call here: `401` vs
//! `400` is exactly the kind of "which part of the grant was wrong"
//! information `OauthService` folds together on purpose (an OAuth security
//! best practice: an attacker probing a token endpoint should not learn
//! whether their `client_secret`, their code, or their `redirect_uri` was
//! the specific thing that failed). This module's own tests therefore
//! assert `400` for every `exchange_token` rejection mode, matching
//! `service/tests.rs`'s own already-established assertions for the same
//! scenarios. Design.md's API Contract table lists `400, 401` as `/oauth/
//! token`'s possible errors; only `400` is actually reachable through the
//! currently-implemented `OauthService` surface — this module does not
//! invent a `401` path that doesn't correspond to any real rejection this
//! service can produce.
//!
//! ## Revoke authenticates the client itself (CONCERN — the central
//! judgment call for this task)
//! `OauthService::revoke_token` (task 4.2, already reviewed) does *not*
//! verify client credentials at all — per its own doc comment, it is
//! unconditionally idempotent given only a raw token string, mirroring RFC
//! 7009 section 2.2's "respond 200 whether or not the token was valid".
//! But design.md's API Contract table names `POST /oauth/revoke`'s request
//! shape as `token, client creds` (implying client authentication *is*
//! expected at the endpoint), and RFC 7009 section 2.1 itself requires a
//! confidential client's revocation request to be authenticated ("The
//! authorization server first validates the client credentials... in case
//! of an invalid client identification, the authorization server responds
//! with HTTP status code 401"). Rather than widening `OauthService`'s
//! already-reviewed public surface with a new method (out of this task's
//! `Boundary: TokenEndpoint`, and `service.rs` is explicitly listed as a
//! file this task must not modify), [`revoke_token`] verifies the
//! presented `client_id`/`client_secret` itself via
//! [`app_repository::verify_app_credentials`] — called directly, bypassing
//! `OauthService`, mirroring `AppsEndpoint::verify_credentials`'s identical
//! precedent (`apps_endpoint.rs`'s "Bypassing `OauthService` for credential
//! verification") — *before* calling
//! [`crate::oauth::service::OauthService::revoke_token`] at all. This
//! ordering is load-bearing, not incidental: it guarantees a caller
//! presenting invalid client credentials never revokes the target token as
//! a side effect (this module's own tests assert this explicitly), and a
//! failed credential check short-circuits with `401` before the
//! (unconditionally-idempotent) revocation call is ever reached. Note this
//! module does *not* verify that the token being revoked actually belongs
//! to the authenticating client (`AccessToken`/`OauthApp` are linked only
//! by `app_id`, and cross-checking it would require a new `OauthService`/
//! repository read this task's boundary does not grant) — RFC 7009 permits
//! this (section 2.1: "the authorization server SHOULD verify whether the
//! token was issued to the client making the revocation request... if this
//! validation fails... the authorization server still SHOULD return an
//! HTTP 200"), so client-credential authentication alone (without ownership
//! cross-checking) already satisfies the RFC.
//!
//! ## Revoke's success response: empty `{}` JSON body (design.md: "empty
//! 200")
//! Real Mastodon's `/oauth/revoke` returns HTTP `200` with an empty JSON
//! object body; [`revoke_token`] mirrors this exactly rather than an empty
//! (zero-byte) body, since design.md's own phrase "empty 200" is read here
//! as "the token response carries no meaningful fields", not literally "no
//! body at all" — an empty JSON object is trivially parseable by any client
//! expecting Mastodon's actual wire behavior, whereas a zero-byte body with
//! a `200` and no `Content-Type` risks tripping up a client's JSON parser.
//!
//! ## Mastodon-compatible token response shape (Requirement 3.1)
//! [`exchange_token`]'s success response follows Mastodon's real
//! `/oauth/token` shape exactly: `access_token` (from
//! `IssuedToken::plaintext`), `token_type` (always the literal `"Bearer"`),
//! `scope` (from `IssuedToken::token::scopes`, joined back into a
//! space-separated string — mirroring `apps_endpoint.rs`'s/
//! `token_repository.rs`'s own established space-separated `ScopeSet`
//! encoding), and `created_at` (a Unix timestamp in seconds, derived from
//! `IssuedToken::token::created_at` — itself already produced by
//! `OauthService`'s injected `RuntimeContext` clock inside `exchange_token`,
//! never re-read from wall-clock time here). No plaintext token, secret, or
//! authorization code is ever logged by this module — the plaintext token
//! appears exactly once, in the one-time success response body, matching
//! `AppsEndpoint`'s/`OauthService`'s identical discipline for
//! `client_secret`/authorization-code plaintexts.

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::Json;
use axum::extract::{Form, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::error::AppError;
use crate::oauth::app_repository;
use crate::oauth::hash::TokenHashKey;
use crate::oauth::service::{OauthService, TokenRequest};
use crate::oauth::token_repository::IssuedToken;

/// Everything [`exchange_token`]/[`revoke_token`] need, bundled behind one
/// `axum::extract::State`-compatible handle. See this module's doc comment
/// ("`TokenEndpointState`: not `AppState`") for why this is its own small
/// type rather than a field on `crate::state::AppState`.
#[derive(Clone)]
pub struct TokenEndpointState {
    /// Handles `POST /oauth/token` (Requirements 3.1, 3.2, 3.3) and the
    /// unconditionally-idempotent half of `POST /oauth/revoke`
    /// (Requirement 3.4). `Arc`-wrapped so this state stays cheaply `Clone`
    /// without requiring `OauthService` itself to derive `Clone` (out of
    /// this task's boundary to add — see `apps_endpoint.rs`'s identical
    /// reasoning).
    pub service: Arc<OauthService>,
    /// Backs `POST /oauth/revoke`'s client-credential authentication via
    /// [`app_repository::verify_app_credentials`] directly. See this
    /// module's doc comment ("Revoke authenticates the client itself") for
    /// why this duplicates a field `service` also holds internally.
    pub pool: PgPool,
    /// Paired with `pool` for the same reason.
    pub token_hash_key: TokenHashKey,
}

/// Form body for `POST /oauth/token` (design.md's API Contract: `grant,
/// code, client creds, redirect_uri, pkce verifier`). Every field defaults
/// on absence except `code_verifier` (already optional, matching
/// [`TokenRequest::code_verifier`]'s shape) — see this module's doc comment
/// ("Wire format" / "`grant_type` validation") for why.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TokenExchangeRequest {
    #[serde(default)]
    pub grant_type: String,
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
    #[serde(default)]
    pub redirect_uri: String,
    #[serde(default)]
    pub code_verifier: Option<String>,
}

/// Success response body for `POST /oauth/token`: Mastodon's real token
/// response shape. See this module's doc comment ("Mastodon-compatible
/// token response shape").
#[derive(Debug, Clone, Serialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub token_type: String,
    pub scope: String,
    pub created_at: i64,
}

/// Form body for `POST /oauth/revoke` (design.md's API Contract: `token,
/// client creds`). Every field defaults on absence, mirroring
/// [`TokenExchangeRequest`]'s identical rationale.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RevokeRequest {
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
}

/// Success response body for `POST /oauth/revoke`: an empty JSON object
/// (`{}`), matching real Mastodon's actual wire behavior. See this module's
/// doc comment ("Revoke's success response: empty `{}` JSON body"). Built
/// as a field-less struct (rather than `serde_json::Value`) because
/// `serde_json` is only a dev-dependency of this crate (`Cargo.toml`), not
/// available to non-test library code.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RevokeResponse {}

/// The literal grant type this endpoint supports. See this module's doc
/// comment ("`grant_type` validation").
const SUPPORTED_GRANT_TYPE: &str = "authorization_code";

/// Exchanges an authorization code for an access token (`POST /oauth/token`,
/// Requirements 3.1, 3.2, 3.3): validates `grant_type` (see this module's
/// doc comment), then delegates all other validation to
/// [`OauthService::exchange_token`], mapping success to a Mastodon-
/// compatible [`TokenResponse`]. Every rejection reason `OauthService`
/// itself distinguishes is folded into the same `400 Bad Request`
/// [`AppError`] it already returns, propagated unchanged (see this module's
/// doc comment, "Exchange-failure status codes...").
pub async fn exchange_token(
    State(state): State<TokenEndpointState>,
    Form(req): Form<TokenExchangeRequest>,
) -> Result<Json<TokenResponse>, AppError> {
    validate_grant_type(&req.grant_type)?;

    let issued = state
        .service
        .exchange_token(TokenRequest {
            code: req.code,
            client_id: req.client_id,
            client_secret: req.client_secret,
            redirect_uri: req.redirect_uri,
            code_verifier: req.code_verifier,
        })
        .await?;

    Ok(Json(to_token_response(&issued)))
}

/// Revokes an access token (`POST /oauth/revoke`, Requirement 3.4):
/// authenticates the presented client credentials itself (see this
/// module's doc comment, "Revoke authenticates the client itself") — a
/// failure here returns `401` and never reaches
/// [`OauthService::revoke_token`], so an unauthenticated caller can never
/// revoke a token as a side effect of a failed credential check. On
/// success, delegates to [`OauthService::revoke_token`] (unconditionally
/// idempotent per its own doc comment) and returns an empty `200` JSON
/// object.
pub async fn revoke_token(
    State(state): State<TokenEndpointState>,
    Form(req): Form<RevokeRequest>,
) -> Result<Json<RevokeResponse>, AppError> {
    app_repository::verify_app_credentials(
        &state.pool,
        &state.token_hash_key,
        &req.client_id,
        &req.client_secret,
    )
    .await?
    .ok_or_else(invalid_client_credentials)?;

    state.service.revoke_token(&req.token).await?;

    Ok(Json(RevokeResponse::default()))
}

/// Validates a presented `grant_type` (Requirement 3.1; see this module's
/// doc comment, "`grant_type` validation"): an absent value (empty string,
/// from [`TokenExchangeRequest::grant_type`]'s default) is treated as
/// implicitly [`SUPPORTED_GRANT_TYPE`]; any other *present* value is
/// rejected with a `400` before [`OauthService::exchange_token`] is ever
/// called.
fn validate_grant_type(grant_type: &str) -> Result<(), AppError> {
    if !grant_type.is_empty() && grant_type != SUPPORTED_GRANT_TYPE {
        return Err(AppError::client(
            StatusCode::BAD_REQUEST,
            format!(
                "unsupported_grant_type: only \"{SUPPORTED_GRANT_TYPE}\" is supported, got {grant_type:?}"
            ),
        ));
    }
    Ok(())
}

/// The `401` rejection [`revoke_token`] returns for any client-credential
/// failure (unknown `client_id`, wrong `client_secret`) — mirrors
/// `apps_endpoint.rs::invalid_client_credentials`'s identical shape and
/// "don't distinguish why" rationale.
fn invalid_client_credentials() -> AppError {
    AppError::client(
        StatusCode::UNAUTHORIZED,
        "invalid or missing client credentials",
    )
}

/// Maps an [`IssuedToken`] to the Mastodon-compatible [`TokenResponse`]
/// wire shape. See this module's doc comment ("Mastodon-compatible token
/// response shape").
fn to_token_response(issued: &IssuedToken) -> TokenResponse {
    TokenResponse {
        access_token: issued.plaintext.expose_secret().clone(),
        token_type: "Bearer".to_string(),
        scope: issued.token.scopes.as_strs().collect::<Vec<_>>().join(" "),
        created_at: issued.token.created_at.unix_timestamp(),
    }
}
