//! `AppsEndpoint` (design.md "OAuth API / エンドポイント層" ->
//! "AppsEndpoint / AuthorizeEndpoint / TokenEndpoint"; Requirements 1.1,
//! 1.2, 1.5; task 5.1): the HTTP-facing OAuth application-registration and
//! client-credential-verification endpoints —
//! `POST /api/v1/apps` and `GET /api/v1/apps/verify_credentials`.
//!
//! Scope: this module owns exactly the two handler functions design.md's
//! API Contract table sketches for `AppsEndpoint` —
//! [`register_app`]/[`verify_credentials`] — and the request/response JSON
//! shapes they use. It does not implement `AuthorizeEndpoint`/
//! `TokenEndpoint` (tasks 5.2/5.3), the Mastodon-compatible error JSON body
//! renderer (`api::error`/`MastodonError`, task 6.1 — every rejection here
//! is a plain [`AppError`], exactly as `OauthService` already does), or any
//! router/`AppState` wiring (`src/state.rs`/`src/bootstrap.rs`/
//! `src/server.rs`, task 7.1).
//!
//! ## Feature Flag Protocol: not applicable
//! Mirrors `OauthService`'s/`OwnerGate`'s own identical reasoning (see
//! those modules' doc comments): this is a brand-new module with no
//! existing callers or previously-observable behavior to gate — nothing in
//! the running application invokes [`register_app`]/[`verify_credentials`]
//! yet, since the router-mounting task (7.1) has not run. A standard RED ->
//! GREEN cycle is this crate's established verification method here.
//!
//! ## Not wired into a router: handlers tested directly, not over HTTP
//! Per this task's own boundary (router/`AppState` wiring is task 7.1, out
//! of scope here), [`register_app`]/[`verify_credentials`] are plain async
//! functions with axum extractor parameters (`State<AppsEndpointState>`,
//! `Json<_>`, `HeaderMap`) — they are handler-*shaped* so task 7.1 can mount
//! them onto a real `Router` with `.route(...).with_state(...)` verbatim,
//! but nothing in this crate currently does so. This module's own tests
//! (`src/oauth/apps_endpoint/tests.rs`, `tests/oauth_apps_it.rs`) therefore
//! construct axum extractor values directly (`State(state)`, `Json(body)`,
//! a manually-built `HeaderMap`) and call these functions as ordinary async
//! Rust functions, rather than driving them through a bound `Router` over
//! real HTTP — there is no production `Router` for these routes to be
//! mounted on yet, and adding one here would be task 7.1's job, not this
//! one's.
//!
//! ## `AppsEndpointState`: not `AppState` (design.md scope)
//! `crate::state::AppState` (core-runtime, task 7.1's wiring target) does
//! not yet hold an `OauthModule`/`ApiModule` handle at all — adding one is
//! explicitly task 7.1's job (design.md's "Modified Files": "`src/state.rs`
//! ...task 7.1"). This module therefore defines its own small
//! [`AppsEndpointState`] — an `axum::extract::State`-compatible bundle of
//! exactly what these two handlers need — as a local, self-contained stand-
//! in. Task 7.1 is expected to either construct one directly (if it keeps
//! this route group on its own sub-`Router`/state) or fold its fields into
//! whatever `OauthModule` it builds; either way is compatible with this
//! module's public shape, and this module does not need to guess which.
//!
//! `AppsEndpointState` holds `pool`/`token_hash_key` *alongside*
//! `service: Arc<OauthService>`, duplicating two fields `OauthService`
//! (task 4.2, already reviewed) also holds internally — see "Bypassing
//! `OauthService` for credential verification" below for why
//! `verify_credentials` cannot simply call a method on `service` instead.
//! `OauthService`'s own fields are private and it does not derive `Clone`
//! (task 4.2's boundary did not need either), and widening its already-
//! reviewed public surface to expose them is out of this task's boundary
//! (`Boundary: AppsEndpoint` only) — wrapping it in `Arc` here, and holding
//! independent clones of `pool`/`token_hash_key` for the one operation that
//! needs to bypass it, avoids modifying `service.rs` at all.
//!
//! ## Bypassing `OauthService` for credential verification (CONCERN —
//! documented judgment call)
//! Design.md's Components and Interfaces table lists `OauthService (P0)` as
//! `AppsEndpoint`'s sole key dependency, and its architecture diagram draws
//! `ApiModule --> OauthService` only (no direct `ApiModule --> AppRepo`
//! edge). However, `OauthService`'s actual, already-implemented (task 4.2)
//! Service Interface exposes exactly four methods —
//! [`crate::oauth::service::OauthService::register_app`],
//! `issue_authorization_code`, `exchange_token`, `revoke_token` — with no
//! `verify_app_credentials`/equivalent. The only existing implementation of
//! Requirement 1.5's "クライアント資格情報の検証" (client-credential
//! verification) is
//! [`crate::oauth::app_repository::verify_app_credentials`], at the
//! repository layer. Rather than widening `OauthService`'s already-reviewed
//! public surface with a new pass-through method (a change to a file
//! outside this task's `Boundary: AppsEndpoint`), [`verify_credentials`]
//! calls `app_repository::verify_app_credentials` directly — mirroring this
//! crate's established precedent (see `OauthService`'s own doc comment,
//! "Scope bridging"/"PKCE bridging") of bridging a design-doc/
//! already-implemented-code gap locally within the *new* component's own
//! file, rather than silently reaching back into an already-reviewed
//! boundary to patch it.
//!
//! ## Credential transport for `GET /api/v1/apps/verify_credentials`: HTTP
//! Basic (RFC 7617), not literally `Bearer` (CONCERN — documented judgment
//! call, requirements.md vs. design.md tension)
//! Design.md's API Contract table names this endpoint's request field
//! `Bearer` — echoing real Mastodon's actual endpoint, which verifies an
//! app-level *access token* obtained via the `client_credentials` OAuth
//! grant. But Requirement 1.5's actual acceptance criterion is narrower and
//! unambiguous: "クライアントが自身のクライアント資格情報の検証を要求した
//! とき...当該アプリケーションの公開情報を返し、無効な資格情報に対しては
//! 認証エラーを返す" — verifying the client's own *credentials*
//! (`client_id` + `client_secret`), which is exactly
//! `app_repository::verify_app_credentials`'s signature. A Bearer access
//! token cannot carry a `client_secret` at all — `AccessToken` (task 2.1)
//! is bound to a single *actor*, not to an app's registration secret — and
//! this spec's `OauthService`/`token_endpoint.rs` (task 5.3, not yet built)
//! never models a `client_credentials` grant that could have minted an
//! app-level Bearer token in the first place. Presenting `client_id`/
//! `client_secret` via HTTP Basic Authentication (RFC 7617:
//! `Authorization: Basic base64(client_id:client_secret)`) is this module's
//! resolution: it is the standard OAuth 2.0 client-authentication transport
//! (RFC 6749 section 2.3.1) for exactly this shape of credential pair, GET-
//! compatible (no request body), and does not silently invent a competing
//! wire format the way stuffing both values into query parameters would
//! (which would also leak `client_secret` into server access logs). See
//! [`extract_basic_credentials`].
//!
//! ## Permissive JSON body, validation delegated entirely to `OauthService`
//! [`RegisterAppRequest`]'s fields all carry `#[serde(default)]`, so a
//! request body omitting `client_name`/`redirect_uris`/`scopes` entirely
//! still deserializes successfully (to an empty string / empty vec / empty
//! string respectively) rather than failing inside axum's `Json` extractor
//! with a raw, non-`AppError`-shaped rejection. [`register_app`] then
//! forwards those (possibly-empty) values straight to
//! [`crate::oauth::service::OauthService::register_app`], which already
//! implements Requirement 1.2's exact rejection behavior (empty name, no
//! redirect URIs, a malformed redirect URI, an unknown scope token — all
//! `422 Unprocessable Entity` `AppError`s). This keeps "missing required /
//! malformed" validation a single implementation (Requirement 1.2's
//! "互換エラーで要求を拒否する") rather than a second, potentially-
//! diverging copy of it here.

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::domain::Id;
use crate::error::AppError;
use crate::oauth::app_repository;
use crate::oauth::hash::TokenHashKey;
use crate::oauth::model::OauthApp;
use crate::oauth::service::{NewApp, OauthService};

/// Everything [`register_app`]/[`verify_credentials`] need, bundled behind
/// one `axum::extract::State`-compatible handle. See this module's doc
/// comment ("`AppsEndpointState`: not `AppState`") for why this is its own
/// small type rather than a field on `crate::state::AppState`.
#[derive(Clone)]
pub struct AppsEndpointState {
    /// Handles `POST /api/v1/apps` registration (Requirements 1.1-1.4).
    /// `Arc`-wrapped so this state stays cheaply `Clone` without requiring
    /// `OauthService` itself to derive `Clone` (out of this task's
    /// boundary to add — see this module's doc comment).
    pub service: Arc<OauthService>,
    /// Backs `GET /api/v1/apps/verify_credentials` (Requirement 1.5) via
    /// [`app_repository::verify_app_credentials`] directly. See this
    /// module's doc comment ("Bypassing `OauthService`...") for why this
    /// duplicates a field `service` also holds internally, rather than
    /// reaching into it.
    pub pool: PgPool,
    /// Paired with `pool` for the same reason.
    pub token_hash_key: TokenHashKey,
}

/// Request body for `POST /api/v1/apps` (design.md's API Contract:
/// `client_name, redirect_uris, scopes`). Every field defaults on absence
/// — see this module's doc comment ("Permissive JSON body...") for why.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RegisterAppRequest {
    #[serde(default)]
    pub client_name: String,
    #[serde(default)]
    pub redirect_uris: Vec<String>,
    /// Mastodon-style space-separated scope string (e.g. `"read write"`),
    /// matching [`NewApp::scopes`]'s shape.
    #[serde(default)]
    pub scopes: String,
}

/// Response body for both `POST /api/v1/apps` and
/// `GET /api/v1/apps/verify_credentials` (design.md's API Contract:
/// `application(+credentials)` / `application`). `client_secret` is `Some`
/// only for the one-time registration response (Requirement 1.5's "平文は
/// 登録応答時のみ返却", already enforced by `app_repository::register_app`
/// itself) and omitted from the JSON body entirely (not `null`) for
/// `verify_credentials`'s public-info-only response.
#[derive(Debug, Clone, Serialize)]
pub struct AppResponse {
    pub id: Id,
    pub name: String,
    pub client_id: String,
    pub redirect_uris: Vec<String>,
    pub scopes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
}

/// Registers a new OAuth client application (`POST /api/v1/apps`,
/// Requirements 1.1, 1.2, 1.3, 1.4): delegates all business validation to
/// [`OauthService::register_app`] and maps the result to the wire shape,
/// including the freshly minted `client_secret` exactly once (Requirement
/// 1.1's "登録応答にクライアント資格情報を含め").
pub async fn register_app(
    State(state): State<AppsEndpointState>,
    Json(req): Json<RegisterAppRequest>,
) -> Result<Json<AppResponse>, AppError> {
    let registered = state
        .service
        .register_app(NewApp {
            name: req.client_name,
            redirect_uris: req.redirect_uris,
            scopes: req.scopes,
        })
        .await?;

    Ok(Json(to_response(&registered, true)))
}

/// Verifies a client's own credentials (`GET /api/v1/apps/verify_credentials`,
/// Requirement 1.5): extracts `client_id`/`client_secret` from HTTP Basic
/// Authentication (see this module's doc comment, "Credential transport..."
/// for why), verifies them against
/// [`app_repository::verify_app_credentials`], and returns the
/// application's public info (no `client_secret`) on success. Missing,
/// malformed, or invalid credentials all yield the same `401 Unauthorized`
/// [`AppError`] (Requirement 1.5's "無効な資格情報に対しては認証エラー"),
/// without distinguishing which — a probing caller learns nothing about
/// *why* verification failed.
pub async fn verify_credentials(
    State(state): State<AppsEndpointState>,
    headers: HeaderMap,
) -> Result<Json<AppResponse>, AppError> {
    let (client_id, client_secret) = extract_basic_credentials(&headers)?;

    let app = app_repository::verify_app_credentials(
        &state.pool,
        &state.token_hash_key,
        &client_id,
        &client_secret,
    )
    .await?
    .ok_or_else(invalid_client_credentials)?;

    Ok(Json(to_response(&app, false)))
}

/// The single `401` rejection [`verify_credentials`] returns for every
/// failure mode (missing header, malformed encoding, unknown `client_id`,
/// wrong `client_secret`) — see [`verify_credentials`]'s doc comment for
/// why these are not distinguished.
fn invalid_client_credentials() -> AppError {
    AppError::client(
        StatusCode::UNAUTHORIZED,
        "invalid or missing client credentials",
    )
}

/// Extracts `(client_id, client_secret)` from an RFC 7617 HTTP Basic
/// `Authorization` header (`Authorization: Basic
/// base64(client_id:client_secret)`). Never panics on attacker-controlled
/// input — every failure mode (missing header, non-UTF-8 header value,
/// wrong scheme, invalid base64, non-UTF-8 decoded bytes, no `:` separator)
/// maps to the same [`invalid_client_credentials`] rejection.
///
/// Splits on the *first* `:` only (via `split_once`), matching RFC 7617's
/// own convention that a password (here, `client_secret`) may itself
/// contain `:` while a username (`client_id`) may not.
fn extract_basic_credentials(headers: &HeaderMap) -> Result<(String, String), AppError> {
    let header_value = headers
        .get(header::AUTHORIZATION)
        .ok_or_else(invalid_client_credentials)?;
    let header_str = header_value
        .to_str()
        .map_err(|_| invalid_client_credentials())?;
    let encoded = header_str
        .strip_prefix("Basic ")
        .ok_or_else(invalid_client_credentials)?;
    let decoded_bytes = BASE64_STANDARD
        .decode(encoded)
        .map_err(|_| invalid_client_credentials())?;
    let decoded = String::from_utf8(decoded_bytes).map_err(|_| invalid_client_credentials())?;
    let (client_id, client_secret) = decoded
        .split_once(':')
        .ok_or_else(invalid_client_credentials)?;
    Ok((client_id.to_string(), client_secret.to_string()))
}

/// Maps a domain [`OauthApp`] to the wire [`AppResponse`] shape.
/// `include_secret` is `true` only for [`register_app`]'s one-time
/// response; `false` for [`verify_credentials`]'s public-info-only
/// response (Requirement 1.5).
fn to_response(app: &OauthApp, include_secret: bool) -> AppResponse {
    AppResponse {
        id: app.id,
        name: app.name.clone(),
        client_id: app.client_id.clone(),
        redirect_uris: app.redirect_uris.clone(),
        scopes: app.scopes.as_strs().map(str::to_string).collect(),
        client_secret: include_secret.then(|| app.client_secret.expose_secret().clone()),
    }
}
