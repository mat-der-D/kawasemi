//! `OauthAppRepository` (design.md "Data / データ層" -> `OauthAppRepository /
//! AuthorizationCodeRepository / AccessTokenRepository`; Requirements 1.1,
//! 1.4, 1.5, 3.6; task 3.1): OAuth client application registration, lookup,
//! and credential verification against `oauth_applications`
//! (`migrations/0003_oauth.sql`).
//!
//! Scope: this module owns exactly the three operations design.md's Service
//! Interface sketches for this component — [`register_app`],
//! [`find_app_by_client_id`], [`verify_app_credentials`] — against a plain
//! `&PgPool`. It does not validate registration input (missing name/
//! redirect URI, unknown scope tokens — Requirement 1.2, 1.3's rejection is
//! `OauthService`'s/`AppsEndpoint`'s job, a later task), does not implement
//! the authorization-code or access-token repositories
//! (`code_repository.rs`/`token_repository.rs`, tasks 3.2/3.3 — this module
//! only supplies the shared [`crate::oauth::hash`] primitive they are
//! expected to reuse), and does not wire itself into `AppState`/
//! `bootstrap.rs`/the router (a later integration task).
//!
//! ## `client_id`/`client_secret` minting (Requirement 1.1)
//! Both are drawn from the injected [`Rng`] boundary (never
//! `rand`/`getrandom` called directly here — see design.md's "Adjacent
//! expectations": all non-determinism flows through core-runtime's
//! injection boundaries) as fixed-length random byte strings, then
//! base64url-encoded (no padding) with the same `URL_SAFE_NO_PAD` encoding
//! `pkce.rs` already established for token-shaped values in this crate.
//! `client_id` is 24 random bytes (32 base64url characters) — it is not
//! secret, only required to be unique and unguessable enough that a client
//! app cannot be enumerated. `client_secret` is 32 random bytes (43 base64url
//! characters) — matching the entropy this crate's other secret-shaped
//! random values use (`ChaCha20Poly1305` nonces aside, which are a different
//! primitive with different length requirements).
//!
//! ## `client_secret_hash` persistence and the one-time plaintext return
//! (Requirement 1.5)
//! [`register_app`] hashes the freshly generated `client_secret` via
//! [`crate::oauth::hash::keyed_hash`] before it ever reaches a `sqlx` bind
//! parameter — only `client_secret_hash` (`BYTEA`) is written to
//! `oauth_applications`; the plaintext is never passed to `sqlx::query`'s
//! logging-adjacent machinery or to `tracing`. The plaintext is placed in
//! the *returned* [`OauthApp::client_secret`] exactly once (registration's
//! own response value) and is not re-derivable from storage afterward —
//! see the "`OauthApp::client_secret` outside `register_app`" note below.
//! [`verify_app_credentials`] hashes the *caller-presented* secret and
//! compares hashes in constant time via
//! [`crate::oauth::hash::verify_keyed_hash`] (Requirement 1.5's "ハッシュ同士
//! の定数時間比較") — the plaintext it echoes back into its `Some(OauthApp)`
//! return value on success is the caller's own already-known input, not
//! anything read back from the database.
//!
//! ## `OauthApp::client_secret` outside `register_app`/`verify_app_credentials`
//! `crate::oauth::model::OauthApp` (task 2.1, already reviewed) has exactly
//! one `client_secret: Secret<String>` field — no separate
//! `client_secret_hash` field the way `AccessToken` has `token_hash: Vec<u8>`
//! alongside no plaintext field at all. Design.md's Service Interface types
//! [`find_app_by_client_id`] as returning `Option<OauthApp>` too, but that
//! function receives no plaintext secret as an argument and (correctly,
//! Requirement 1.5) `oauth_applications` stores no plaintext to read back —
//! there is no genuine value to put in `client_secret` for that path. Rather
//! than silently fabricating a value that looks like a real secret,
//! [`find_app_by_client_id`] fills that field with
//! [`NO_PLAINTEXT_SECRET_SENTINEL`], a fixed, unmistakably-not-a-secret
//! string, and this fact is documented on the constant itself. This is a
//! genuine, load-bearing design-doc/domain-type mismatch (flagged in this
//! task's status report as a CONCERN, not silently routed around): a
//! `client_secret_hash: Vec<u8>` field on `OauthApp` itself, mirroring
//! `AccessToken::token_hash`, would remove the need for this sentinel
//! entirely, but that is a change to task 2.1's already-reviewed
//! `model.rs`, out of this task's boundary to make unilaterally.
//!
//! ## Redirect URI / scope storage encoding
//! `oauth_applications.redirect_uris`/`scopes` are single `TEXT` columns
//! (`migrations/0003_oauth.sql`) holding what is logically a list. Redirect
//! URIs are newline-joined (`\n`): a URI can legally contain spaces (percent
//! -encoded) or commas in its query string, but never a literal newline, so
//! `\n` is an unambiguous, collision-free separator (there is no re-parsing
//! ambiguity a URI-legal character could introduce). Scopes are
//! space-joined, matching the Mastodon-compatible space-separated scope
//! string convention `scope.rs::ScopeSet::parse` already parses elsewhere in
//! this crate.

#[cfg(test)]
mod tests;

use axum::http::StatusCode;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sqlx::postgres::PgPool;
use time::OffsetDateTime;

use crate::config::Secret;
use crate::domain::Id;
use crate::error::AppError;
use crate::oauth::hash::{TokenHashKey, keyed_hash, verify_keyed_hash};
use crate::oauth::model::{OauthApp, ScopeSet};
use crate::runtime::ids::IdGenerator;
use crate::runtime::rng::Rng;

/// Number of random bytes drawn from [`Rng`] to mint a new `client_id`
/// (before base64url encoding). See this module's doc comment ("`client_id`/
/// `client_secret` minting") for why this length and encoding.
const CLIENT_ID_RANDOM_BYTES: usize = 24;

/// Number of random bytes drawn from [`Rng`] to mint a new `client_secret`
/// (before base64url encoding). See this module's doc comment for why this
/// length and encoding.
const CLIENT_SECRET_RANDOM_BYTES: usize = 32;

/// Separator joining multiple redirect URIs into `oauth_applications
/// .redirect_uris`'s single `TEXT` column. See this module's doc comment
/// ("Redirect URI / scope storage encoding") for why this is safe.
const REDIRECT_URI_SEPARATOR: char = '\n';

/// Separator joining multiple scope tokens into `oauth_applications.scopes`'s
/// single `TEXT` column, matching `scope.rs`'s Mastodon-compatible
/// space-separated scope string convention.
const SCOPE_SEPARATOR: char = ' ';

/// Fixed, unmistakably-not-a-real-secret placeholder used for
/// [`OauthApp::client_secret`] when reconstructing an app from storage with
/// no plaintext available ([`find_app_by_client_id`]). See this module's doc
/// comment ("`OauthApp::client_secret` outside `register_app`/
/// `verify_app_credentials`") for the full rationale. Callers must never
/// treat this value as a real client secret — it exists only so
/// `find_app_by_client_id` can satisfy design.md's `Option<OauthApp>`
/// return type without fabricating a value that could be mistaken for a
/// genuine one.
pub const NO_PLAINTEXT_SECRET_SENTINEL: &str =
    "<oauth-app-secret-not-retrievable-after-registration>";

/// Input to [`register_app`]: everything the caller supplies about a new
/// OAuth client application. `client_id`/`client_secret` are minted by
/// [`register_app`] itself (Requirement 1.1), not supplied here.
#[derive(Debug, Clone)]
pub struct NewApp {
    pub name: String,
    pub redirect_uris: Vec<String>,
    pub scopes: ScopeSet,
}

/// Generates a fresh random token of `random_bytes` bytes (drawn from `rng`,
/// core-runtime's injected non-determinism boundary — never a direct
/// `rand`/`getrandom` call, per design.md's "Adjacent expectations"),
/// base64url-encoded with no padding (matching `pkce.rs`'s established
/// encoding convention for token-shaped values in this crate).
fn random_url_safe_token(rng: &dyn Rng, random_bytes: usize) -> String {
    let mut buf = vec![0u8; random_bytes];
    rng.fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

/// Joins `uris` into `oauth_applications.redirect_uris`'s stored form. See
/// this module's doc comment ("Redirect URI / scope storage encoding").
fn join_redirect_uris(uris: &[String]) -> String {
    uris.join(&REDIRECT_URI_SEPARATOR.to_string())
}

/// Reverses [`join_redirect_uris`]. An empty stored string parses to an
/// empty `Vec` (rather than a one-element `Vec` containing `""`) so a
/// round trip through [`join_redirect_uris`] is exact for every input this
/// repository itself ever writes, including the (validation-rejected
/// upstream, but not this layer's concern) empty-list case.
fn parse_redirect_uris(raw: &str) -> Vec<String> {
    if raw.is_empty() {
        Vec::new()
    } else {
        raw.split(REDIRECT_URI_SEPARATOR)
            .map(str::to_string)
            .collect()
    }
}

/// Joins `scopes` into `oauth_applications.scopes`'s stored form.
fn join_scopes(scopes: &ScopeSet) -> String {
    scopes
        .as_strs()
        .collect::<Vec<_>>()
        .join(&SCOPE_SEPARATOR.to_string())
}

/// Reverses [`join_scopes`] via [`ScopeSet::new`] (task 2.1's placeholder
/// `model::ScopeSet` — see that module's doc comment for why `OauthApp
/// ::scopes` still holds this placeholder type rather than `scope::ScopeSet`,
/// a wiring change this task's boundary does not own).
fn parse_scopes(raw: &str) -> ScopeSet {
    ScopeSet::new(raw.split_whitespace())
}

/// Maps a failed `INSERT INTO oauth_applications` to an [`AppError`].
/// A unique violation on `client_id` should not occur in practice (24
/// random bytes give an astronomically low collision probability), so —
/// unlike `ActorRepository::map_insert_error`'s handle-uniqueness case,
/// which maps a *foreseeable* user-driven conflict to a 409 — any failure
/// here is treated as an unexpected `Server` (5xx) error.
fn map_insert_error(source: sqlx::Error) -> AppError {
    AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source)
}

/// Registers a new OAuth client application (Requirement 1.1): mints
/// `client_id`/`client_secret` from the injected [`IdGenerator`]/[`Rng`]
/// boundaries, hashes `client_secret` via [`keyed_hash`] before persisting
/// it (Requirement 1.5 — the plaintext never reaches a bind parameter,
/// `tracing`, or any log call), and stores the redirect URIs (Requirement
/// 1.4) and requested scopes as given.
///
/// Returns the registered [`OauthApp`], whose `client_secret` field holds
/// the plaintext exactly once — this is registration's one-time response
/// value (Requirement 1.5's "平文は登録応答時のみ返却"). The caller (a
/// later task's `AppsEndpoint`) is responsible for not logging it either.
pub async fn register_app(
    pool: &PgPool,
    ids: &dyn IdGenerator,
    rng: &dyn Rng,
    token_hash_key: &TokenHashKey,
    now: OffsetDateTime,
    input: NewApp,
) -> Result<OauthApp, AppError> {
    let id = ids.next_id();
    let client_id = random_url_safe_token(rng, CLIENT_ID_RANDOM_BYTES);
    let client_secret_plaintext = random_url_safe_token(rng, CLIENT_SECRET_RANDOM_BYTES);
    let client_secret_hash = keyed_hash(token_hash_key, &client_secret_plaintext);
    let redirect_uris_stored = join_redirect_uris(&input.redirect_uris);
    let scopes_stored = join_scopes(&input.scopes);

    sqlx::query(
        "INSERT INTO oauth_applications \
            (id, client_id, client_secret_hash, name, redirect_uris, scopes, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(id.as_i64())
    .bind(&client_id)
    .bind(&client_secret_hash)
    .bind(&input.name)
    .bind(&redirect_uris_stored)
    .bind(&scopes_stored)
    .bind(now)
    .execute(pool)
    .await
    .map_err(map_insert_error)?;

    Ok(OauthApp {
        id,
        client_id,
        client_secret: Secret::new(client_secret_plaintext),
        redirect_uris: input.redirect_uris,
        scopes: input.scopes,
        name: input.name,
        created_at: now,
    })
}

/// A `oauth_applications` row as read directly off the wire when no
/// plaintext secret is available or needed ([`find_app_by_client_id`]).
type OauthAppMetaRow = (i64, String, String, String, String, OffsetDateTime);

/// Reconstructs an [`OauthApp`] from a metadata-only row, filling
/// `client_secret` with [`NO_PLAINTEXT_SECRET_SENTINEL`] — see this module's
/// doc comment for why.
fn row_to_oauth_app_meta(row: OauthAppMetaRow) -> OauthApp {
    let (id, client_id, name, redirect_uris, scopes, created_at) = row;
    OauthApp {
        id: Id::from_i64(id),
        client_id,
        client_secret: Secret::new(NO_PLAINTEXT_SECRET_SENTINEL.to_string()),
        redirect_uris: parse_redirect_uris(&redirect_uris),
        scopes: parse_scopes(&scopes),
        name,
        created_at,
    }
}

/// Looks up the [`OauthApp`] registered under `client_id`, if any
/// (Requirement 1.4's underlying data access — used by a later task's
/// authorization endpoint to verify the registered redirect-URI exact
/// match). Returns `Ok(None)` (not an error) when no row matches, mirroring
/// `ActorRepository::find_by_handle`'s "does this exist" contract at this
/// data layer.
///
/// See this module's doc comment for why the returned `OauthApp
/// ::client_secret` is [`NO_PLAINTEXT_SECRET_SENTINEL`], not a real secret.
pub async fn find_app_by_client_id(
    pool: &PgPool,
    client_id: &str,
) -> Result<Option<OauthApp>, AppError> {
    let row: Option<OauthAppMetaRow> = sqlx::query_as(
        "SELECT id, client_id, name, redirect_uris, scopes, created_at \
         FROM oauth_applications WHERE client_id = $1",
    )
    .bind(client_id)
    .fetch_optional(pool)
    .await
    .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;

    Ok(row.map(row_to_oauth_app_meta))
}

/// A `oauth_applications` row including `client_secret_hash`, used only by
/// [`verify_app_credentials`] (never returned to a caller as-is).
type OauthAppCredentialRow = (i64, String, Vec<u8>, String, String, String, OffsetDateTime);

/// Verifies that `secret` is the correct `client_secret` for the
/// application registered under `client_id` (Requirement 1.5): hashes
/// `secret` via [`keyed_hash`] and compares it to the stored
/// `client_secret_hash` in constant time via [`verify_keyed_hash`] — the
/// presented plaintext is never compared to another plaintext, and no two
/// distinct code paths exist for "hash matched" vs. "hash mismatched" timing.
///
/// Returns `Ok(None)` — not an authentication error — both when `client_id`
/// is unknown and when `secret` is wrong; the caller (a later task's
/// `AppsEndpoint`/`OauthService`) is responsible for turning `None` into the
/// Mastodon-compatible authentication error Requirement 1.5 calls for. On
/// success, the returned [`OauthApp::client_secret`] holds the caller's own
/// already-known `secret` argument echoed back (not anything read from
/// storage — `client_secret_hash` is never reversible).
pub async fn verify_app_credentials(
    pool: &PgPool,
    token_hash_key: &TokenHashKey,
    client_id: &str,
    secret: &str,
) -> Result<Option<OauthApp>, AppError> {
    let row: Option<OauthAppCredentialRow> = sqlx::query_as(
        "SELECT id, client_id, client_secret_hash, name, redirect_uris, scopes, created_at \
         FROM oauth_applications WHERE client_id = $1",
    )
    .bind(client_id)
    .fetch_optional(pool)
    .await
    .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;

    let Some((id, client_id, client_secret_hash, name, redirect_uris, scopes, created_at)) = row
    else {
        return Ok(None);
    };

    if !verify_keyed_hash(token_hash_key, secret, &client_secret_hash) {
        return Ok(None);
    }

    Ok(Some(OauthApp {
        id: Id::from_i64(id),
        client_id,
        client_secret: Secret::new(secret.to_string()),
        redirect_uris: parse_redirect_uris(&redirect_uris),
        scopes: parse_scopes(&scopes),
        name,
        created_at,
    }))
}
