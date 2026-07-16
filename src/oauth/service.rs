//! `OauthService` (design.md "OAuth Service / サービス層" -> `OauthService`;
//! Requirements 1.1, 1.2, 1.3, 2.1, 2.3, 2.5, 2.6, 3.1, 3.2, 3.3, 3.4, 4.1-4.5;
//! task 4.2): the OAuth business service aggregating app registration,
//! authorization-code issuance, token exchange, and token revocation.
//!
//! Scope: this module owns exactly the four operations design.md's Service
//! Interface sketches for this component — [`OauthService::register_app`],
//! [`OauthService::issue_authorization_code`],
//! [`OauthService::exchange_token`], [`OauthService::revoke_token`] —
//! orchestrating `app_repository`/`code_repository`/`token_repository`
//! (tasks 3.1-3.3, already implemented) and `scope`/`pkce` (tasks 2.2/2.3,
//! already implemented) against a plain `&PgPool` and an injected
//! [`RuntimeContext`]. It does not implement the consent/authorization HTML
//! screen, actor-selection UI, or CSRF handling (`AuthorizeEndpoint`, task
//! 5.2 — not built yet), does not validate that a selected `actor_id`
//! actually belongs to the authenticating owner (also task 5.2's job via
//! `ActorDirectory::list_actors_for_owner` — this service trusts the
//! `actor_id` it is given), does not implement `OwnerGate` (task 4.1,
//! already done, no dependency from here), and does not wire itself into
//! `AppState`/`bootstrap.rs`/the router (task 7.1, a later integration
//! task). It does not render a Mastodon-compatible JSON error body either
//! (`api::error`/`MastodonError`, a different, later component) — every
//! rejection here is a plain [`AppError`].
//!
//! ## Feature Flag Protocol: not applicable
//! `OauthService` is a brand-new internal service module with no existing
//! callers or previously-observable behavior to gate — nothing in the
//! running application invokes any of its four methods yet (the endpoint
//! layer that will, `AppsEndpoint`/`AuthorizeEndpoint`/`TokenEndpoint`, is
//! task 5.x, out of this task's boundary, and bootstrap/`AppState` wiring is
//! task 7.1, likewise out of boundary). This mirrors `ActorService`'s and
//! `OwnerGate`'s own identical reasoning (see `src/actor/service.rs`'s and
//! `src/oauth/owner_gate.rs`'s doc comments): a standard RED -> GREEN cycle
//! against a real Postgres instance (via `spawn_test_app`) is this crate's
//! established verification method for a component in this situation, and
//! there is no prior behavior a flag would protect.
//!
//! ## Scope bridging: real `scope::ScopeSet` in, placeholder
//! `model::ScopeSet` at the repository boundary (CONCERN — documented
//! judgment call, resolving task 2.1/2.2's deferred wiring)
//! `crate::oauth::model::ScopeSet` (the type `OauthApp::scopes`/
//! `AuthorizationCode::scopes`/`AccessToken::scopes` — and hence every
//! `*_repository.rs` function signature — actually holds) is a bare,
//! unvalidated `BTreeSet<String>` with no inclusion judgment; the real
//! Mastodon-compatible vocabulary and `is_satisfied_by` judgment live in
//! `crate::oauth::scope::ScopeSet` (task 2.2). Both `model.rs` and
//! `scope.rs`'s own doc comments explicitly defer wiring the two together to
//! "a later task" — this task, since `OauthService` is the first component
//! that needs both a validating parser (Requirement 1.3's "未知のスコープを
//! 拒否") and a repository-shaped value to persist (Requirement 3.1's
//! storage) at once. Rather than changing `model.rs`/`scope.rs`'s type
//! definitions (out of this task's boundary — `Boundary: OauthService`
//! only), this module bridges the two locally via [`to_model_scopes`]
//! (real -> placeholder, on the way into a repository call) and
//! [`to_real_scopes`] (placeholder -> real, on the way out, so
//! [`scope::ScopeSet::is_satisfied_by`] can be applied to a value read back
//! from storage). Every raw scope string this service ever hands to a
//! repository has already been validated by [`scope::ScopeSet::parse`]
//! first — no unvalidated scope string ever reaches `app_repository`/
//! `code_repository`/`token_repository` through this module.
//!
//! ## PKCE bridging: real `pkce::PkceChallenge`/`verify_pkce` in, placeholder
//! `model::PkceChallenge` at the repository boundary (CONCERN — documented
//! judgment call, resolving task 2.1/2.3's deferred wiring)
//! Symmetric to the scope bridging above: `crate::oauth::model::PkceChallenge`
//! (what `AuthorizationCode::pkce` actually holds, and what
//! `code_repository::insert_code`/`consume_code` persist/return) carries
//! only a bare `challenge: String`, no `method`. [`issue_authorization_code`]
//! stores a caller-presented raw `code_challenge` string as-is via
//! `model::PkceChallenge::new`. [`exchange_token`] reconstructs the real
//! `pkce::PkceChallenge { method: PkceMethod::S256, challenge: <stored
//! string> }` from the consumed code's placeholder value and calls
//! [`pkce::verify_pkce`] against it — `PkceMethod::S256` is always the right
//! method to pair it with because `code_repository.rs` already hardcodes
//! `PKCE_METHOD_S256` end to end (see that module's own doc comment,
//! "`pkce_method` persistence"), so this is a lossless, unambiguous
//! reconstruction, not a guess.
//!
//! ## PKCE presence mismatch: reject, do not silently accept (Requirement
//! 2.6's "Where" conditionality)
//! Requirement 2.6 makes PKCE conditional on whether the *authorization*
//! request carried a challenge ("Where 認可要求がコード交換用の検証情報を伴う
//! 場合"), not a blanket requirement for every code. [`exchange_token`]
//! therefore branches on `(code.pkce, req.code_verifier)`: both present ->
//! verify via [`pkce::verify_pkce`]; both absent -> no PKCE was ever in play,
//! proceed; exactly one present -> reject (Requirement 3.3's "不整合のときは
//! トークンを発行しない" — a code issued *with* a challenge but exchanged
//! *without* a verifier, or vice versa, is exactly the kind of inconsistency
//! that requirement exists to catch, not a case to silently wave through).
//!
//! ## `exchange_token`'s consume-then-validate ordering (CONCERN —
//! documented judgment call, forced by `code_repository`'s exposed API
//! surface)
//! `code_repository.rs` exposes only [`code_repository::insert_code`] and
//! [`code_repository::consume_code`] — no non-consuming "peek" lookup — and
//! this task's boundary does not permit adding one (`Boundary: OauthService`
//! only; `code_repository.rs` is out of scope to modify). So the only way to
//! learn a presented code's bound `app_id`/`redirect_uri`/PKCE challenge is
//! to consume it first. [`exchange_token`] therefore: (1) verifies client
//! credentials via [`app_repository::verify_app_credentials`] *before*
//! touching the code at all, so a wrong client_id/secret never burns a code
//! it was never entitled to redeem; then (2) consumes the code; then (3)
//! validates the consumed code's `app_id`/`redirect_uri`/PKCE against the
//! request, rejecting (no token issued, Requirement 3.2/3.3) on any
//! mismatch. A mismatch discovered only at step (3) still burns the code
//! (it is single-use either way, Requirement 2.5) — there is no code path in
//! the exposed repository API that could validate those fields without
//! already having consumed the row. This is flagged as a documented judgment
//! call, not a silent workaround, mirroring this crate's established
//! practice for tasks 3.1-3.3's own analogous design-doc/type mismatches.
//!
//! ## Redirect URI format validation bar (Requirement 1.2, judgment call)
//! This crate has no URI-parsing dependency (`Cargo.toml` carries no `url`/
//! `http`-uri crate), and adding one is out of this task's boundary. A
//! minimal, defensible "format invalid" bar is used instead
//! ([`validate_redirect_uri_format`]): the value must split into a
//! non-empty scheme and a non-empty remainder on the first `"://"`
//! occurrence (e.g. `https://client.example/callback` splits into
//! `"https"` / `"client.example/callback"`, both non-empty). This is not a
//! full RFC 3986 validator — it does not check that the scheme uses only
//! legal characters, or that the remainder is a well-formed authority — it
//! only rejects the obviously-malformed inputs (empty string, no `"://"` at
//! all, a bare `"://"` with nothing on either side) Requirement 1.2's "形式
//! 不正" calls for.
//!
//! ## `revoke_token`'s idempotent, non-erroring return (RFC 7009 alignment)
//! [`token_repository::revoke_token`] returns `Ok(false)` both when
//! `raw_token` matches no stored token and when it matches an
//! already-revoked one (see that module's own doc comment). This service's
//! [`OauthService::revoke_token`] discards that `bool` and always returns
//! `Ok(())` on a repository-level success, treating "already
//! revoked"/"unknown token" as an ordinary, non-erroring outcome — mirroring
//! RFC 7009 section 2.2 ("the authorization server responds with HTTP status
//! code 200 if the token has been revoked successfully or if the client
//! submitted an invalid token"). Requirement 3.4 asks only that a revocation
//! request make the token invalid going forward, not that revoking a token
//! twice (or one that never existed) be reported as an error.
//!
//! ## Authorization code entropy and lifetime (judgment calls)
//! [`issue_authorization_code`] mints the raw code plaintext itself (from
//! the injected [`crate::runtime::rng::Rng`] boundary, matching
//! `app_repository::register_app`'s/`token_repository::issue_token`'s
//! established minting convention — `code_repository::insert_code` takes an
//! already-built `AuthorizationCode`, it does not mint one) at
//! [`AUTHORIZATION_CODE_RANDOM_BYTES`] (32 bytes, matching
//! `token_repository::ACCESS_TOKEN_RANDOM_BYTES`'s entropy for another
//! single-use, security-critical bearer-shaped value) and gives it
//! [`AUTHORIZATION_CODE_TTL`] (10 minutes), mirroring
//! `owner_gate::OWNER_SESSION_TTL`'s identical "short-lived, single
//! interactive round trip" rationale and RFC 6749 section 4.1.2's
//! recommendation that authorization codes expire shortly after issuance
//! (commonly implemented as ~10 minutes).

#[cfg(test)]
mod tests;

use axum::http::StatusCode;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sqlx::postgres::PgPool;
use time::Duration;

use crate::config::Secret;
use crate::domain::Id;
use crate::error::AppError;
use crate::oauth::app_repository;
use crate::oauth::code_repository;
use crate::oauth::hash::TokenHashKey;
use crate::oauth::model::{self, AuthorizationCode, OauthApp};
use crate::oauth::pkce;
use crate::oauth::scope;
use crate::oauth::token_repository::{self, IssuedToken};
use crate::runtime::RuntimeContext;

/// Number of random bytes drawn from `runtime.rng` to mint a new
/// authorization code plaintext (before base64url encoding). See this
/// module's doc comment ("Authorization code entropy and lifetime") for why
/// this length.
const AUTHORIZATION_CODE_RANDOM_BYTES: usize = 32;

/// How long a freshly issued authorization code remains exchangeable. See
/// this module's doc comment ("Authorization code entropy and lifetime")
/// for the full rationale.
const AUTHORIZATION_CODE_TTL: Duration = Duration::minutes(10);

/// Input to [`OauthService::register_app`] (Requirements 1.1, 1.2, 1.3;
/// design.md's Service Interface names this parameter type `NewApp`).
///
/// Distinct from `app_repository::NewApp` — which this module builds
/// internally, after validation — because `scopes` here is the *raw*,
/// caller-presented, space-separated scope string (Requirement 1.3's
/// vocabulary rejection has not happened yet), not an already-validated
/// [`scope::ScopeSet`]/[`model::ScopeSet`]. See this module's doc comment
/// ("Scope bridging") for why a raw string is the right shape at this
/// boundary.
#[derive(Debug, Clone)]
pub struct NewApp {
    pub name: String,
    pub redirect_uris: Vec<String>,
    pub scopes: String,
}

/// Input to [`OauthService::issue_authorization_code`] (Requirements 2.1,
/// 2.3, 2.6; design.md's Service Interface names this parameter type
/// `AuthorizeApproval`). Represents an owner's already-made consent
/// decision (which actor, which scopes) for a client's authorization
/// request — the consent screen itself, and validating that `actor_id`
/// belongs to the authenticating owner, are `AuthorizeEndpoint`'s job (task
/// 5.2, out of this task's boundary; see this module's own doc comment).
///
/// `scopes` is the raw, space-separated *approved* scope string (mirroring
/// [`NewApp::scopes`]'s "raw, not yet validated" shape). `code_challenge` is
/// `None` when the authorization request carried no PKCE challenge
/// (Requirement 2.6 is `Where`-conditional); when `Some`, it is always
/// treated as an S256 challenge (see this module's doc comment, "PKCE
/// bridging").
#[derive(Debug, Clone)]
pub struct AuthorizeApproval {
    pub client_id: String,
    pub redirect_uri: String,
    pub scopes: String,
    pub actor_id: Id,
    pub code_challenge: Option<String>,
}

/// Result of [`OauthService::issue_authorization_code`]: the raw
/// authorization code plaintext (returned to the caller exactly once, at
/// issuance time — mirroring `token_repository::IssuedToken`'s identical
/// one-time-plaintext-return shape for a sibling security-critical bearer
/// value) alongside the persisted [`AuthorizationCode`] record.
#[derive(Debug, Clone)]
pub struct IssuedCode {
    pub plaintext: Secret<String>,
    pub code: AuthorizationCode,
}

/// Input to [`OauthService::exchange_token`] (Requirements 3.1, 3.2, 3.3;
/// design.md's Service Interface names this parameter type `TokenRequest`).
/// Carries no `grant_type` field: this method always performs the
/// authorization-code grant exchange design.md's sequence diagram
/// describes; dispatching on a raw `grant_type` string (if a future
/// requirement ever adds another grant type) is an endpoint-layer concern,
/// not this service's.
#[derive(Debug, Clone)]
pub struct TokenRequest {
    pub code: String,
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
    pub code_verifier: Option<String>,
}

/// The OAuth business service (design.md's exact Service Interface):
/// [`register_app`](Self::register_app),
/// [`issue_authorization_code`](Self::issue_authorization_code),
/// [`exchange_token`](Self::exchange_token),
/// [`revoke_token`](Self::revoke_token).
pub struct OauthService {
    pool: PgPool,
    runtime: RuntimeContext,
    token_hash_key: TokenHashKey,
}

impl OauthService {
    /// Builds a service bound to `pool` (passed through to every
    /// `*_repository` call), `runtime` (the injected clock/id/rng
    /// boundaries, so identifiers/random tokens/timestamps are never drawn
    /// directly from `OffsetDateTime::now_utc()`/`rand`/`getrandom`), and
    /// `token_hash_key` (`AppConfig.oauth.token_hash_key`, passed through to
    /// every `*_repository` call that hashes secret material).
    pub fn new(pool: PgPool, runtime: RuntimeContext, token_hash_key: TokenHashKey) -> Self {
        Self {
            pool,
            runtime,
            token_hash_key,
        }
    }

    /// Registers a new OAuth client application (Requirements 1.1, 1.2,
    /// 1.3, 1.4): rejects a missing name, a missing or malformed redirect
    /// URI, or an unknown scope token with a caller-facing [`AppError`]
    /// before ever reaching [`app_repository::register_app`]. On success,
    /// returns the registered [`OauthApp`] — whose `client_secret` holds
    /// the plaintext exactly once (Requirement 1.5's "平文は登録応答時のみ
    /// 返却", enforced by `app_repository::register_app` itself).
    pub async fn register_app(&self, input: NewApp) -> Result<OauthApp, AppError> {
        if input.name.trim().is_empty() {
            return Err(AppError::client(
                StatusCode::UNPROCESSABLE_ENTITY,
                "app registration requires a non-empty name",
            ));
        }
        if input.redirect_uris.is_empty() {
            return Err(AppError::client(
                StatusCode::UNPROCESSABLE_ENTITY,
                "app registration requires at least one redirect_uri",
            ));
        }
        for redirect_uri in &input.redirect_uris {
            validate_redirect_uri_format(redirect_uri)?;
        }
        let scopes = scope::ScopeSet::parse(&input.scopes)?;

        let now = self.runtime.clock.now();
        app_repository::register_app(
            &self.pool,
            self.runtime.ids.as_ref(),
            self.runtime.rng.as_ref(),
            &self.token_hash_key,
            now,
            app_repository::NewApp {
                name: input.name,
                redirect_uris: input.redirect_uris,
                scopes: to_model_scopes(&scopes),
            },
        )
        .await
    }

    /// Issues a short-lived authorization code bound to a single actor and
    /// its approved scopes (Requirements 2.1, 2.3, 2.6): rejects an unknown
    /// `client_id`, a `redirect_uri` that is not one of the app's
    /// registered redirect URIs (Requirement 2.1's "登録一致"), an unknown
    /// scope token, or an approved-scope set that exceeds the app's own
    /// registered scopes (Requirement 4.5's shared inclusion judgment,
    /// applied here as a narrowing check via
    /// [`scope::ScopeSet::is_satisfied_by`]) — no code is issued in any of
    /// those cases. On success, persists (via
    /// [`code_repository::insert_code`]) and returns an [`IssuedCode`]
    /// binding `req.actor_id`, the approved scopes, and the optional PKCE
    /// challenge (Requirement 2.6) to one short-lived, single-use code (see
    /// this module's doc comment, "Authorization code entropy and
    /// lifetime").
    pub async fn issue_authorization_code(
        &self,
        req: AuthorizeApproval,
    ) -> Result<IssuedCode, AppError> {
        let app = app_repository::find_app_by_client_id(&self.pool, &req.client_id)
            .await?
            .ok_or_else(|| AppError::client(StatusCode::BAD_REQUEST, "unknown OAuth client_id"))?;

        if !app
            .redirect_uris
            .iter()
            .any(|registered| registered == &req.redirect_uri)
        {
            return Err(AppError::client(
                StatusCode::BAD_REQUEST,
                "redirect_uri does not match a registered redirect URI for this client",
            ));
        }

        let approved_scopes = scope::ScopeSet::parse(&req.scopes)?;
        let registered_scopes = to_real_scopes(&app.scopes)?;
        if !approved_scopes.is_satisfied_by(&registered_scopes) {
            return Err(AppError::client(
                StatusCode::UNPROCESSABLE_ENTITY,
                "approved scopes exceed this client's registered scopes",
            ));
        }

        let now = self.runtime.clock.now();
        let raw_code =
            random_url_safe_token(self.runtime.rng.as_ref(), AUTHORIZATION_CODE_RANDOM_BYTES);
        let code = AuthorizationCode {
            code: Secret::new(raw_code.clone()),
            app_id: app.id,
            actor_id: req.actor_id,
            scopes: to_model_scopes(&approved_scopes),
            redirect_uri: req.redirect_uri,
            pkce: req.code_challenge.map(model::PkceChallenge::new),
            expires_at: now + AUTHORIZATION_CODE_TTL,
            consumed: false,
        };

        code_repository::insert_code(&self.pool, &self.token_hash_key, &code).await?;

        Ok(IssuedCode {
            plaintext: Secret::new(raw_code),
            code,
        })
    }

    /// Exchanges a single-use authorization code for an access token
    /// (Requirements 3.1, 3.2, 3.3, 2.5): verifies client credentials
    /// (Requirement 3.2's "資格情報...不一致" — before the code is ever
    /// touched, see this module's doc comment, "`exchange_token`'s
    /// consume-then-validate ordering"), atomically single-use-consumes the
    /// code (Requirement 2.5 — via
    /// [`code_repository::consume_code`]), then validates that the
    /// consumed code actually belongs to the authenticated client and
    /// carries the presented `redirect_uri` (Requirement 3.2), and finally
    /// checks PKCE consistency if either side presented it (Requirement
    /// 3.3; see this module's doc comment, "PKCE presence mismatch"). No
    /// token is issued if any check fails. On success, issues (via
    /// [`token_repository::issue_token`]) and returns an [`IssuedToken`]
    /// bound to the code's `actor_id` and approved scopes (Requirement
    /// 3.1, 3.5).
    pub async fn exchange_token(&self, req: TokenRequest) -> Result<IssuedToken, AppError> {
        let invalid_grant = || {
            AppError::client(
                StatusCode::BAD_REQUEST,
                "invalid authorization code, client credentials, or redirect_uri",
            )
        };

        let app = app_repository::verify_app_credentials(
            &self.pool,
            &self.token_hash_key,
            &req.client_id,
            &req.client_secret,
        )
        .await?
        .ok_or_else(invalid_grant)?;

        let now = self.runtime.clock.now();
        let code = code_repository::consume_code(&self.pool, &self.token_hash_key, &req.code, now)
            .await?
            .ok_or_else(invalid_grant)?;

        if code.app_id != app.id {
            return Err(invalid_grant());
        }
        if code.redirect_uri != req.redirect_uri {
            return Err(invalid_grant());
        }

        match (&code.pkce, &req.code_verifier) {
            (Some(stored_challenge), Some(verifier)) => {
                let real_challenge =
                    pkce::PkceChallenge::new(pkce::PkceMethod::S256, stored_challenge.as_str());
                pkce::verify_pkce(&real_challenge, verifier)?;
            }
            (Some(_), None) => {
                return Err(AppError::client(
                    StatusCode::BAD_REQUEST,
                    "code_verifier is required: this authorization code was issued with a PKCE challenge",
                ));
            }
            (None, Some(_)) => {
                return Err(AppError::client(
                    StatusCode::BAD_REQUEST,
                    "code_verifier was presented, but this authorization code has no PKCE challenge",
                ));
            }
            (None, None) => {}
        }

        token_repository::issue_token(
            &self.pool,
            self.runtime.ids.as_ref(),
            self.runtime.rng.as_ref(),
            &self.token_hash_key,
            now,
            token_repository::NewAccessToken {
                app_id: code.app_id,
                actor_id: code.actor_id,
                scopes: code.scopes,
            },
        )
        .await
    }

    /// Revokes `raw_token` (Requirement 3.4): always returns `Ok(())` on a
    /// successful repository call, regardless of whether `raw_token`
    /// referenced an active token, an already-revoked one, or none at all
    /// — see this module's doc comment ("`revoke_token`'s idempotent,
    /// non-erroring return") for why. Only a genuine repository/database
    /// failure propagates as an `Err`.
    pub async fn revoke_token(&self, raw_token: &str) -> Result<(), AppError> {
        token_repository::revoke_token(&self.pool, &self.token_hash_key, raw_token).await?;
        Ok(())
    }
}

/// Validates that `redirect_uri` clears the minimal "format invalid" bar
/// this module establishes for Requirement 1.2. See this module's doc
/// comment ("Redirect URI format validation bar") for exactly what this
/// does and does not check.
fn validate_redirect_uri_format(redirect_uri: &str) -> Result<(), AppError> {
    let malformed = || {
        AppError::client(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("redirect_uri has an invalid format: {redirect_uri}"),
        )
    };
    let Some((scheme, rest)) = redirect_uri.split_once("://") else {
        return Err(malformed());
    };
    if scheme.is_empty() || rest.is_empty() {
        return Err(malformed());
    }
    Ok(())
}

/// Converts a validated, real [`scope::ScopeSet`] into the placeholder
/// [`model::ScopeSet`] the `*_repository.rs` modules persist. See this
/// module's doc comment ("Scope bridging") for the full rationale.
fn to_model_scopes(real: &scope::ScopeSet) -> model::ScopeSet {
    model::ScopeSet::new(real.iter().map(|scope| scope.to_string()))
}

/// Converts a placeholder [`model::ScopeSet`] (as read back from a
/// repository) into the real [`scope::ScopeSet`] so
/// [`scope::ScopeSet::is_satisfied_by`] can be applied to it. See this
/// module's doc comment ("Scope bridging") for why a re-parse failure here
/// is a `Server` (5xx) error, not a caller-facing rejection: every string a
/// `model::ScopeSet` can hold in practice was itself produced by
/// [`to_model_scopes`] from an already-validated `scope::ScopeSet`, so a
/// failure to re-parse indicates corrupted or foreign data, not a bad
/// request.
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

/// Generates a fresh random token of `random_bytes` bytes (drawn from
/// `rng`, core-runtime's injected non-determinism boundary — never a direct
/// `rand`/`getrandom` call), base64url-encoded with no padding. Deliberately
/// near-identical to `app_repository::random_url_safe_token`/
/// `token_repository::random_url_safe_token` (both private to their own
/// modules, hence not reused directly) — see those modules' own doc
/// comments for why this small duplication follows an already-established
/// crate convention.
fn random_url_safe_token(rng: &dyn crate::runtime::rng::Rng, random_bytes: usize) -> String {
    let mut buf = vec![0u8; random_bytes];
    rng.fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}
