//! `AuthorizeEndpoint` (design.md "OAuth API / エンドポイント層" ->
//! "AppsEndpoint / AuthorizeEndpoint / TokenEndpoint"; Requirements 2.1,
//! 2.2, 2.3, 2.4; task 5.2): `GET`/`POST /oauth/authorize` — the
//! authorization-code-flow front door that gates the consent screen behind
//! owner authentication, lists the owner's actors as consent candidates,
//! and issues a code bound to the owner's selected actor + approved scopes
//! once CSRF-verified consent is given.
//!
//! Scope: per `tasks.md`'s own framing ("これは OAuth サービス・オーナー
//! ゲート・アクターディレクトリを束ねる横断結線であり、明示的な統合作業
//! として扱う"), this module is deliberately a thin integration layer over
//! three already-implemented, already-reviewed components —
//! [`crate::oauth::service::OauthService`] (task 4.2),
//! [`crate::oauth::owner_gate`] (task 4.1), and
//! [`crate::actor::ActorDirectory`] (actor-model task 5.2) — plus
//! [`crate::oauth::templates`] (this task, HTML rendering only). It does not
//! implement `TokenEndpoint` (task 5.3), the Mastodon-compatible error JSON
//! body renderer (`api::error`/`MastodonError`, task 6.1 — every rejection
//! here is a plain [`AppError`], exactly as `apps_endpoint.rs` already
//! established), or any router/`AppState` wiring (task 7.1).
//!
//! ## Feature Flag Protocol: not applicable
//! Mirrors `AppsEndpoint`'s/`OauthService`'s/`OwnerGate`'s own identical
//! reasoning: a brand-new module with no existing callers (the router-
//! mounting task, 7.1, has not run) and no previously-observable behavior to
//! gate. A standard RED -> GREEN cycle is this crate's established
//! verification method here.
//!
//! ## Not wired into a router: handlers tested directly, not over HTTP
//! Identical to `apps_endpoint.rs`'s own documented reasoning: [`authorize_get`]/
//! [`authorize_post`] are plain async functions with axum extractor
//! parameters (`State<AuthorizeEndpointState>`, `Query<_>`, `Form<_>`,
//! `HeaderMap`) — handler-*shaped* so task 7.1 can mount them verbatim, but
//! this module's own tests call them directly as ordinary async Rust
//! functions against a real Postgres instance via `spawn_test_app`.
//!
//! ## `AuthorizeEndpointState`: not `AppState`, and duplicates fields
//! `OauthService` holds internally
//! Mirrors `AppsEndpointState`'s own documented reasoning
//! (`apps_endpoint.rs`'s "`AppsEndpointState`: not `AppState`" /
//! "Bypassing `OauthService`..."): `crate::state::AppState` does not yet
//! hold an `OauthModule` (task 7.1's job), so this module defines its own
//! small state bundle. It holds `pool` *and* `service: Arc<OauthService>`
//! (duplicating the pool `OauthService` already holds privately) for the
//! same reason `AppsEndpointState` does — see "Bypassing `OauthService` for
//! GET-time request validation" below for exactly which operation needs the
//! bypass and why.
//!
//! ## Bypassing `OauthService` for GET-time request validation (CONCERN —
//! documented judgment call, same shape as task 5.1's own)
//! `OauthService`'s Service Interface (task 4.2) exposes no standalone
//! "validate this authorization request" method — its one relevant
//! operation, [`crate::oauth::service::OauthService::issue_authorization_code`],
//! performs client/redirect-URI/scope validation only as a side effect of
//! *issuing a code*, which cannot happen before owner authentication and
//! actor selection. But design.md's own sequence diagram places client/
//! redirect-URI/scope validation *before* "require owner session" (`Authz->
//! >Svc: validate client and redirect_uri and scope` happens first,
//! `Authz->>Gate: require owner session` second) and Requirement 2.1 itself
//! ("不一致のときは認可コードを発行せず拒否する") is about rejecting *before*
//! ever reaching the consent screen, not merely before code issuance. This
//! module therefore calls [`crate::oauth::app_repository::find_app_by_client_id`]
//! directly for this earlier validation gate — which that function's own
//! doc comment already earmarks for exactly this purpose ("used by a later
//! task's authorization endpoint to verify the registered redirect-URI exact
//! match"), so this is not a new gap being papered over but a documented
//! hand-off task 3.1 already anticipated. See [`validate_authorize_context`].
//!
//! ## Design judgment calls (HTTP mechanics design.md leaves unspecified)
//! design.md's sequence diagram says the `GET` flow is "owner authenticated
//! or show login" but sketches no separate login route (the File Structure
//! Plan lists only `GET`/`POST /oauth/authorize`, with `templates.rs`
//! rendering *both* the consent screen and "オーナーログイン" per its own
//! comment). This module's concrete, minimal design:
//!
//! 1. **Both the login form and the consent form `POST` back to
//!    `/oauth/authorize` itself** (no separate route). [`authorize_post`]
//!    disambiguates which submission it received by **owner-session-cookie
//!    validity alone** — never by inspecting which form fields are present
//!    in the body — mirroring [`authorize_get`]'s own identical branching
//!    condition, so both directions of this endpoint agree on one source of
//!    truth for "is the caller currently an authenticated owner". No valid
//!    session -> this must be a login submission (requires `password`).
//!    Valid session -> this must be a consent decision (requires
//!    `csrf_token`).
//! 2. **The original authorization request's `client_id`/`redirect_uri`/
//!    `scope`/`response_type` survive the login round trip as hidden form
//!    fields** on the rendered login form (see `templates.rs`'s
//!    `AuthorizeContext`), submitted back verbatim alongside `password`.
//! 3. **A successful login renders the consent screen directly, in the same
//!    response, instead of redirecting back to `GET`** (`Set-Cookie` header
//!    plus a `200` consent-HTML body on the login `POST`'s own response).
//!    This is a deliberate simplification over "redirect back to `GET`
//!    with the original query string": it needs no query-string
//!    re-encoding at all (this crate has no URL-encoding dependency — see
//!    `service.rs`'s own "Redirect URI format validation bar" precedent for
//!    the same "no URI crate" constraint), it is one fewer network round
//!    trip, and it is a well-established pattern (respond with the next
//!    screen directly after a `POST` that both authenticates and has
//!    everything needed to render it). The trade-off: if the just-validated
//!    hidden fields somehow fail [`validate_authorize_context`] on this
//!    path (should not happen in normal use, since they are exactly what
//!    `GET` itself just rendered), the freshly authenticated session's
//!    cookie is not attached to that particular error response — the caller
//!    would need to log in again. This is a cosmetic UX-only edge case, not
//!    a security concern (no session or code is ever issued incorrectly),
//!    and is called out here rather than silently accepted.
//! 4. **No `state` parameter.** design.md's API Contract table lists exactly
//!    `client_id, redirect_uri, scope, response_type` for `GET
//!    /oauth/authorize` and `selected_actor, approved_scopes, decision,
//!    csrf_token` for the `POST` (no `state` on either row), and neither
//!    Requirement 2.1-2.4 nor this task's own boundary mentions client-side
//!    CSRF `state` round-tripping. Supporting it would also be the one place
//!    this module would need URL query-string encoding (the client's opaque
//!    `state` value, re-embedded in a redirect back to `redirect_uri`) —
//!    avoided entirely by not implementing it. A later task can add it
//!    without breaking this module's shape.
//! 5. **No PKCE (`code_challenge`) forwarding.** Requirement 2.6 (PKCE) is
//!    not among this task's required Requirements (2.1-2.4 only — see
//!    `tasks.md`'s own `_Requirements:_` line for task 5.2), design.md's
//!    `GET /oauth/authorize` API Contract row lists no PKCE query
//!    parameters, and PKCE *verification* is token-exchange's job (task
//!    5.3) regardless. [`authorize_post`] therefore always passes
//!    `code_challenge: None` to
//!    [`crate::oauth::service::OauthService::issue_authorization_code`] —
//!    that field already exists on `AuthorizeApproval` for a later task to
//!    populate once GET-side PKCE query-parameter forwarding is in scope.
//! 6. **`cookie_secure` is an opaque `bool` state field, not derived here.**
//!    design.md's Security Considerations call for `Secure` "TLS 配信時" —
//!    but this module has no router/TLS-termination context (task 7.1's
//!    job) and `Pagination`'s `X-Forwarded-*`-respecting convention (design.md's
//!    Requirement 6.7) is a distinct, not-yet-built sibling component (task
//!    6.2) this task should not preempt by inventing its own header-sniffing
//!    heuristic. [`AuthorizeEndpointState::cookie_secure`] is therefore a
//!    plain, caller-supplied `bool` (defaulting to `false` in this module's
//!    own tests, matching `spawn_test_app`'s plain-HTTP test transport);
//!    task 7.1 decides how to derive it in production.
//! 7. **`decision` fails closed.** Any `decision` value other than exactly
//!    `"approve"` (including empty/missing) is treated as a denial
//!    (Requirement 2.4) — never as an implicit approval.
//! 8. **The owner session cookie is not rotated/cleared on a consent
//!    decision.** `OwnerSession` (design.md) is a short-lived *login*
//!    session, not a single-use *authorization code* — reusing one session
//!    to authorize multiple client applications within its ten-minute TTL
//!    (`owner_gate::OWNER_SESSION_TTL`) is the intended "one owner login,
//!    several client app approvals in one sitting" shape, not a security
//!    gap: it is still origin-scoped (`Path=/oauth/authorize`), `HttpOnly`,
//!    signed, and time-bounded exactly as it already was.
//! 9. **`GET`'s design.md-documented `401`** (API Contract table: `GET
//!    /oauth/authorize | ... | consent HTML / login HTML | 400, 401`) **is
//!    never literally emitted by this implementation.** An absent/invalid/
//!    expired owner session cookie always converts directly into a `200`
//!    login-HTML render (per judgment call 1 above and this task's own
//!    brief: "GET...with no/invalid/expired owner session cookie -> render
//!    a minimal owner-login HTML form"), never a bare 401 — only `POST`'s
//!    login-submission branch can ever propagate a genuine 401 (from
//!    [`crate::oauth::owner_gate::authenticate_owner`] itself, on a wrong
//!    password). This mirrors task 5.1's own documented design.md-table-vs-
//!    Requirement-text resolution (`apps_endpoint.rs`'s "Credential transport
//!    for `GET /api/v1/apps/verify_credentials`" section).
//! 10. **design.md's `POST /oauth/authorize` error enumeration (400, 401,
//!     403) omits 422**, but
//!     [`crate::oauth::service::OauthService::issue_authorization_code`] can
//!     itself return a `422` (an approved-scope set exceeding the client
//!     app's own registered scopes). This module propagates that error
//!     unchanged (`?`) rather than remapping its status — consistent with
//!     treating `OauthService`'s already-reviewed status choices as
//!     authoritative, and mirroring task 5.1's own acceptance of a
//!     design.md/implementation status-code gap as a documented, non-
//!     blocking observation rather than a silent workaround.
//!
//! ## CSRF token: `keyed_hash` of the session's own identity, not a
//! separate store (design.md's Security Considerations)
//! [`generate_csrf_token`]/[`verify_csrf_token`] derive a CSRF token as
//! `hex(keyed_hash(token_hash_key, "csrf:<owner_id>:<expires_at_unix>"))` —
//! bound to the specific [`crate::oauth::model::OwnerSession`] (design.md:
//! "オーナーセッションに紐づく CSRF トークン"), storage-free (no server-side
//! session/CSRF table exists in this codebase), reusing
//! `crate::oauth::hash`'s already-established keyed-hash primitive exactly
//! as `owner_gate::encode_session_cookie`/`decode_session_cookie` already do
//! for the session cookie itself. The `"csrf:"` prefix domain-separates this
//! token's payload from the session cookie's own payload
//! (`"<owner_id>:<expires_at>"`, no prefix) under the same `token_hash_key`,
//! so a session cookie value's MAC can never be replayed as a valid CSRF
//! token or vice versa, even though both are HMACed under the same key.

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use serde::Deserialize;
use sqlx::PgPool;
use time::OffsetDateTime;

use crate::actor::ActorDirectory;
use crate::domain::Id;
use crate::error::AppError;
use crate::oauth::app_repository;
use crate::oauth::hash::{TokenHashKey, keyed_hash, verify_keyed_hash};
use crate::oauth::model::{OauthApp, OwnerSession};
use crate::oauth::owner_gate::{
    self, OWNER_SESSION_COOKIE_NAME, OwnerCredential, OwnerLogin, decode_session_cookie,
    encode_session_cookie,
};
use crate::oauth::scope;
use crate::oauth::service::{AuthorizeApproval, OauthService};
use crate::oauth::templates::{self, AuthorizeContext};
use crate::runtime::RuntimeContext;

/// Everything [`authorize_get`]/[`authorize_post`] need, bundled behind one
/// `axum::extract::State`-compatible handle. See this module's doc comment
/// ("`AuthorizeEndpointState`: not `AppState`...") for why this is its own
/// small type.
#[derive(Clone)]
pub struct AuthorizeEndpointState {
    /// Issues authorization codes (Requirement 2.3). `Arc`-wrapped for the
    /// same reason `AppsEndpointState::service` is (see that type's own doc
    /// comment).
    pub service: Arc<OauthService>,
    /// Backs the GET-time client/redirect-URI validation gate that bypasses
    /// `service` — see this module's doc comment ("Bypassing
    /// `OauthService`...").
    pub pool: PgPool,
    /// The owner credential [`owner_gate::authenticate_owner`] checks a
    /// login submission against (Requirement 2.2).
    pub owner_credential: OwnerCredential,
    /// Supplies consent-screen actor candidates
    /// ([`ActorDirectory::list_actors_for_owner`], Requirement 2.2) and is
    /// threaded into [`owner_gate::authenticate_owner`] to resolve
    /// `owner_id` (Requirement 2.2, via `ActorDirectory::sole_owner`).
    pub directory: Arc<ActorDirectory>,
    /// Signs/verifies the owner-session cookie value and the CSRF token
    /// (both via `crate::oauth::hash`) — see this module's doc comment
    /// ("CSRF token...").
    pub token_hash_key: TokenHashKey,
    /// The injected clock boundary (never read directly here) used for
    /// every `now` this module needs: session-cookie decode/expiry and
    /// [`owner_gate::authenticate_owner`]'s session-issuance timestamp.
    pub runtime: RuntimeContext,
    /// Whether the owner-session `Set-Cookie` header carries the `Secure`
    /// attribute — see this module's doc comment (judgment call 6) for why
    /// this is a plain, caller-supplied flag rather than something this
    /// module derives itself.
    pub cookie_secure: bool,
}

/// Query parameters for `GET /oauth/authorize` (design.md's API Contract:
/// `client_id, redirect_uri, scope, response_type`). Every field defaults on
/// absence (mirroring `apps_endpoint.rs`'s `RegisterAppRequest`'s
/// "permissive body, validate downstream" convention) so a request missing a
/// field fails inside [`validate_authorize_context`] as an ordinary
/// [`AppError`], not a raw non-`AppError`-shaped axum extractor rejection.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AuthorizeQuery {
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub redirect_uri: String,
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub response_type: String,
}

/// Form body for `POST /oauth/authorize`: the union of design.md's stated
/// consent-decision fields (`selected_actor, approved_scopes, decision,
/// csrf_token`) and this module's login-submission field (`password`), plus
/// the four hidden authorization-request fields both rendered forms carry
/// (see `templates.rs`'s `AuthorizeContext`). Every field defaults on
/// absence, same rationale as [`AuthorizeQuery`].
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AuthorizeSubmission {
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub redirect_uri: String,
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub response_type: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub csrf_token: String,
    #[serde(default)]
    pub selected_actor: String,
    #[serde(default)]
    pub approved_scopes: String,
    #[serde(default)]
    pub decision: String,
}

/// `GET /oauth/authorize` (Requirements 2.1, 2.2): validates the
/// authorization request (client/redirect-URI/scope/response_type), then
/// renders either the consent screen (owner session present and valid) or
/// the owner-login form (absent/invalid/expired session) — see this
/// module's doc comment for the full judgment-call writeup.
pub async fn authorize_get(
    State(state): State<AuthorizeEndpointState>,
    Query(query): Query<AuthorizeQuery>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    validate_authorize_context(
        &state.pool,
        &query.client_id,
        &query.redirect_uri,
        &query.scope,
        &query.response_type,
    )
    .await?;

    let ctx = AuthorizeContext {
        client_id: query.client_id,
        redirect_uri: query.redirect_uri,
        scope: query.scope,
        response_type: query.response_type,
    };

    let now = state.runtime.clock.now();
    match resolve_owner_session(&headers, &state.token_hash_key, now) {
        Some(session) => render_consent_response(&state, &ctx, &session).await,
        None => Ok(html_response(
            StatusCode::OK,
            templates::render_login_form(&ctx),
        )),
    }
}

/// `POST /oauth/authorize` (Requirements 2.2, 2.3, 2.4): dispatches to the
/// login-submission branch (no valid owner session cookie) or the
/// consent-decision branch (valid session), per this module's doc comment's
/// judgment call 1.
pub async fn authorize_post(
    State(state): State<AuthorizeEndpointState>,
    headers: HeaderMap,
    Form(form): Form<AuthorizeSubmission>,
) -> Result<Response, AppError> {
    let now = state.runtime.clock.now();
    match resolve_owner_session(&headers, &state.token_hash_key, now) {
        None => handle_login_submission(&state, &form, now).await,
        Some(session) => handle_consent_submission(&state, &form, &session).await,
    }
}

// ---- login submission (Requirement 2.2's precondition) ----

/// Authenticates a login submission (Requirement 2.2): requires a
/// non-empty `password`, delegates to
/// [`owner_gate::authenticate_owner`] (propagating its `401` on a wrong
/// password unchanged), then — on success — re-validates the request's
/// hidden `client_id`/`redirect_uri`/`scope`/`response_type` fields exactly
/// as `GET` does, and renders the consent screen directly in this same
/// response (see this module's doc comment, judgment call 3).
async fn handle_login_submission(
    state: &AuthorizeEndpointState,
    form: &AuthorizeSubmission,
    now: OffsetDateTime,
) -> Result<Response, AppError> {
    if form.password.is_empty() {
        return Err(AppError::client(
            StatusCode::BAD_REQUEST,
            "owner login submission is missing the password field",
        ));
    }

    let login = OwnerLogin {
        password: crate::config::Secret::new(form.password.clone()),
    };
    let session =
        owner_gate::authenticate_owner(&state.owner_credential, &login, &state.directory, now)
            .await?;

    validate_authorize_context(
        &state.pool,
        &form.client_id,
        &form.redirect_uri,
        &form.scope,
        &form.response_type,
    )
    .await?;

    let ctx = AuthorizeContext {
        client_id: form.client_id.clone(),
        redirect_uri: form.redirect_uri.clone(),
        scope: form.scope.clone(),
        response_type: form.response_type.clone(),
    };

    let mut response = render_consent_response(state, &ctx, &session).await?;
    let cookie_value = encode_session_cookie(&session, &state.token_hash_key);
    let set_cookie = build_session_set_cookie_header(&cookie_value, state.cookie_secure);
    response.headers_mut().insert(
        header::SET_COOKIE,
        set_cookie
            .parse()
            .expect("a signed session cookie value contains no header-hostile characters"),
    );
    Ok(response)
}

// ---- consent decision submission (Requirements 2.3, 2.4) ----

/// Handles a consent-decision submission (Requirements 2.3, 2.4): verifies
/// the CSRF token first (before touching anything else — Requirement's
/// "検証してから認可コードを発行"), then re-validates
/// `client_id`/`redirect_uri`/`scope`/`response_type` (never trusting a
/// resubmitted form body blindly), then either issues a code bound to a
/// verified-owned actor (`decision == "approve"`) or redirects with an
/// OAuth-compliant access-denied response (anything else — fails closed,
/// judgment call 7).
async fn handle_consent_submission(
    state: &AuthorizeEndpointState,
    form: &AuthorizeSubmission,
    session: &OwnerSession,
) -> Result<Response, AppError> {
    if form.csrf_token.is_empty()
        || !verify_csrf_token(&form.csrf_token, session, &state.token_hash_key)
    {
        return Err(AppError::client(
            StatusCode::FORBIDDEN,
            "CSRF token missing or does not match the current owner session",
        ));
    }

    let app = validate_authorize_context(
        &state.pool,
        &form.client_id,
        &form.redirect_uri,
        &form.scope,
        &form.response_type,
    )
    .await?;

    if form.decision != "approve" {
        return Ok(redirect_response(format!(
            "{}{}error=access_denied",
            form.redirect_uri,
            query_separator(&form.redirect_uri)
        )));
    }

    let actor_id = resolve_and_verify_selected_actor(state, session, &form.selected_actor).await?;

    let approved_scopes = if form.approved_scopes.is_empty() {
        form.scope.clone()
    } else {
        form.approved_scopes.clone()
    };

    let issued = state
        .service
        .issue_authorization_code(AuthorizeApproval {
            client_id: app.client_id.clone(),
            redirect_uri: form.redirect_uri.clone(),
            scopes: approved_scopes,
            actor_id,
            code_challenge: None, // see this module's doc comment, judgment call 5
        })
        .await?;

    let code = issued.plaintext.expose_secret();
    Ok(redirect_response(format!(
        "{}{}code={}",
        form.redirect_uri,
        query_separator(&form.redirect_uri),
        code
    )))
}

/// Parses `raw_selected_actor` as an [`Id`] and confirms it appears in
/// `session.owner_id`'s own actor listing (Requirement 2.3's "選択された
/// アクター" must actually belong to the authenticating owner) — never
/// trusts a client-supplied `actor_id` blindly, matching this task's
/// explicit instruction and `OauthService::issue_authorization_code`'s own
/// documented assumption that its caller already did this check.
async fn resolve_and_verify_selected_actor(
    state: &AuthorizeEndpointState,
    session: &OwnerSession,
    raw_selected_actor: &str,
) -> Result<Id, AppError> {
    let selected_id = raw_selected_actor
        .trim()
        .parse::<i64>()
        .map(Id::from_i64)
        .map_err(|_| {
            AppError::client(
                StatusCode::BAD_REQUEST,
                "selected_actor is missing or not a valid actor identifier",
            )
        })?;

    let owned_actors = state
        .directory
        .list_actors_for_owner(session.owner_id)
        .await?;
    if !owned_actors.iter().any(|actor| actor.id == selected_id) {
        return Err(AppError::client(
            StatusCode::BAD_REQUEST,
            "selected_actor does not belong to the authenticated owner",
        ));
    }
    Ok(selected_id)
}

// ---- shared helpers ----

/// Validates a (candidate) authorization request's `client_id`,
/// `redirect_uri`, `scope`, and `response_type` (Requirement 2.1, and the
/// sequence diagram's "validate client and redirect_uri and scope" step,
/// which design.md places before owner-session handling) — see this
/// module's doc comment ("Bypassing `OauthService`...") for why this calls
/// `app_repository` directly. Returns the resolved [`OauthApp`] on success
/// (so callers that need it, e.g. to read `client_id` back, do not have to
/// look it up twice); every failure is a `400 Bad Request` [`AppError`]
/// (design.md's API Contract table lists only `400, 401` for `GET`, and this
/// step is never an authentication failure), consistent across the `GET`
/// and every `POST` branch that calls this (login-success, and both
/// consent-decision outcomes) so the same request is validated the same way
/// no matter which leg of the flow it arrives through.
async fn validate_authorize_context(
    pool: &PgPool,
    client_id: &str,
    redirect_uri: &str,
    scope: &str,
    response_type: &str,
) -> Result<OauthApp, AppError> {
    if response_type != "code" {
        return Err(bad_request(
            "response_type must be \"code\" (only the authorization-code grant is supported)",
        ));
    }

    let app = app_repository::find_app_by_client_id(pool, client_id)
        .await?
        .ok_or_else(|| bad_request("unknown OAuth client_id"))?;

    if !app
        .redirect_uris
        .iter()
        .any(|registered| registered == redirect_uri)
    {
        return Err(bad_request(
            "redirect_uri does not match a registered redirect URI for this client",
        ));
    }

    scope::ScopeSet::parse(scope).map_err(|err| bad_request(err.public_message))?;

    Ok(app)
}

fn bad_request(message: impl Into<String>) -> AppError {
    AppError::client(StatusCode::BAD_REQUEST, message)
}

/// Loads `session.owner_id`'s actor candidates and renders the consent
/// screen (Requirement 2.2), embedding a freshly derived CSRF token (this
/// module's doc comment, "CSRF token...").
async fn render_consent_response(
    state: &AuthorizeEndpointState,
    ctx: &AuthorizeContext,
    session: &OwnerSession,
) -> Result<Response, AppError> {
    let actors = state
        .directory
        .list_actors_for_owner(session.owner_id)
        .await?;
    let csrf_token = generate_csrf_token(session, &state.token_hash_key);
    let html = templates::render_consent_form(ctx, &actors, &csrf_token);
    Ok(html_response(StatusCode::OK, html))
}

fn html_response(status: StatusCode, body: String) -> Response {
    (status, Html(body)).into_response()
}

/// Builds a `302 Found` redirect response with `location` as the `Location`
/// header (used for both the code-issuance redirect, Requirement 2.3, and
/// the access-denied redirect, Requirement 2.4). `location` is always a
/// registered `redirect_uri` (already exact-match validated by
/// [`validate_authorize_context`]) with a safe, ASCII-only query fragment
/// appended (`code=<base64url-alphabet-only>` or the literal
/// `error=access_denied`) — axum's `TryInto<HeaderValue>` conversion for a
/// plain `String` is used directly rather than pre-parsing/`expect`-ing,
/// since a malformed `Location` value here would indicate a bug in this
/// module, not attacker-controlled input reaching the header unescaped.
fn redirect_response(location: String) -> Response {
    (StatusCode::FOUND, [(header::LOCATION, location)]).into_response()
}

/// `"&"` if `redirect_uri` already carries a query string, else `"?"` —
/// mirrors `test_harness.rs::schema_scoped_url`'s identical convention for
/// appending a query parameter to a URL that may or may not already have
/// one.
fn query_separator(redirect_uri: &str) -> char {
    if redirect_uri.contains('?') { '&' } else { '?' }
}

/// Extracts the owner-session cookie's raw value from the `Cookie` request
/// header, if present (`Cookie: name1=value1; name2=value2` syntax).
fn extract_owner_session_cookie(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    raw.split(';').find_map(|pair| {
        let (name, value) = pair.trim().split_once('=')?;
        (name == OWNER_SESSION_COOKIE_NAME).then(|| value.to_string())
    })
}

/// Resolves a currently-valid [`OwnerSession`] from `headers`'s `Cookie`
/// header, if any — `None` uniformly for an absent cookie, a
/// tampered/malformed value, a value signed under a different key, or an
/// expired session (never distinguishing which, matching
/// `owner_gate::decode_session_cookie`'s own established non-distinguishing
/// discipline).
fn resolve_owner_session(
    headers: &HeaderMap,
    key: &TokenHashKey,
    now: OffsetDateTime,
) -> Option<OwnerSession> {
    let raw = extract_owner_session_cookie(headers)?;
    decode_session_cookie(&raw, key, now).ok()
}

/// Builds the `Set-Cookie` header value carrying a signed owner-session
/// cookie value (design.md's Security Considerations: "署名付き HttpOnly
/// Cookie...SameSite=Lax...TLS 配信時は Secure"). `Path` is scoped to this
/// one endpoint (`/oauth/authorize`), the only route that ever reads or
/// writes this cookie.
fn build_session_set_cookie_header(cookie_value: &str, secure: bool) -> String {
    let secure_attr = if secure { "; Secure" } else { "" };
    format!(
        "{OWNER_SESSION_COOKIE_NAME}={cookie_value}; HttpOnly; SameSite=Lax; Path=/oauth/authorize{secure_attr}"
    )
}

/// The payload [`generate_csrf_token`]/[`verify_csrf_token`] HMAC under
/// `token_hash_key`: `"csrf:<owner_id>:<expires_at_unix_seconds>"` — see
/// this module's doc comment ("CSRF token...") for the domain-separation
/// rationale behind the `"csrf:"` prefix.
fn csrf_payload(session: &OwnerSession) -> String {
    format!(
        "csrf:{}:{}",
        session.owner_id.as_i64(),
        session.expires_at.unix_timestamp()
    )
}

/// Derives the CSRF token embedded in the consent form (design.md's
/// Security Considerations).
fn generate_csrf_token(session: &OwnerSession, key: &TokenHashKey) -> String {
    let mac = keyed_hash(key, &csrf_payload(session));
    hex_encode(&mac)
}

/// Verifies a presented CSRF token against `session` (Requirement 2.3's "CSRF
/// トークンがオーナーセッションのものと一致することを検証"): decodes
/// `presented` as hex and constant-time-compares its recomputed keyed hash
/// via [`verify_keyed_hash`], mirroring
/// `owner_gate::decode_session_cookie`'s identical "decode hex, then
/// constant-time-compare via the shared primitive" shape. Never panics on
/// malformed/attacker-controlled input.
fn verify_csrf_token(presented: &str, session: &OwnerSession, key: &TokenHashKey) -> bool {
    match hex_decode(presented) {
        Some(bytes) => verify_keyed_hash(key, &csrf_payload(session), &bytes),
        None => false,
    }
}

/// Encodes `bytes` as lowercase hex. Duplicates
/// `owner_gate.rs::hex_encode`'s identical private helper — see
/// `service.rs`'s own "near-identical duplicated helper" precedent
/// (`random_url_safe_token`) for why: the source function is private to its
/// own module, and this module's boundary does not extend to widening
/// `owner_gate.rs`'s public surface for a two-line utility.
fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(out, "{byte:02x}").expect("writing to a String never fails");
    }
    out
}

/// Decodes a lowercase-hex string into bytes, returning `None` (never
/// panicking) on an odd length or a non-hex character. Duplicates
/// `owner_gate.rs::hex_decode`'s identical private helper and discipline
/// (operates on `char`s, not raw byte indices, so malformed multi-byte UTF-8
/// input in attacker-controlled data never panics on a split byte index) —
/// see [`hex_encode`]'s doc comment for the same "private to its own
/// module" duplication rationale.
fn hex_decode(raw: &str) -> Option<Vec<u8>> {
    let chars: Vec<char> = raw.chars().collect();
    if !chars.len().is_multiple_of(2) {
        return None;
    }
    let mut bytes = Vec::with_capacity(chars.len() / 2);
    for pair in chars.chunks(2) {
        let hex_pair: String = pair.iter().collect();
        bytes.push(u8::from_str_radix(&hex_pair, 16).ok()?);
    }
    Some(bytes)
}
