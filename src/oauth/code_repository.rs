//! `AuthorizationCodeRepository` (design.md "Data / データ層" ->
//! `OauthAppRepository / AuthorizationCodeRepository / AccessTokenRepository`;
//! Requirements 2.5, 3.1, 3.2; task 3.2): short-lived OAuth authorization
//! code insertion and atomic single-use consumption against
//! `oauth_authorization_codes` (`migrations/0003_oauth.sql`).
//!
//! Scope: this module owns exactly the two operations design.md's Service
//! Interface sketches for this component — [`insert_code`],
//! [`consume_code`] — against a plain `&PgPool`. It does not implement the
//! OAuth application repository (`app_repository.rs`, task 3.1, already
//! done) or the access token repository (`token_repository.rs`, task 3.3,
//! not yet started), does not validate authorization-request input (client/
//! redirect-URI/scope checks are `OauthService`'s job, a later task), and
//! does not wire itself into `AppState`/`bootstrap.rs`/the router (a later
//! integration task).
//!
//! ## `token_hash_key` parameter (design.md sketch extension)
//! Design.md's abbreviated Service Interface sketch
//! (`insert_code(pool, code)`, `consume_code(pool, raw_code, now)`) does not
//! show a hashing-key parameter, but hashing the code plaintext is
//! unavoidable to compute `code_hash` (the table's primary key and the only
//! column the code value is ever stored under, Requirement 3.5/3.6's
//! "平文を永続化しない"). Task 3.1 already established the precedent of
//! extending a design.md sketch with the parameters actually needed
//! (`register_app`/`verify_app_credentials` both gained a `token_hash_key:
//! &TokenHashKey` parameter for the same reason) — this module follows the
//! same convention: both [`insert_code`] and [`consume_code`] take
//! `token_hash_key: &TokenHashKey` as an explicit parameter.
//!
//! ## Hashing: reused, not re-derived (`hash.rs`, task 3.1)
//! [`crate::oauth::hash::keyed_hash`] is used exactly as `app_repository.rs`
//! already uses it: `code_hash = keyed_hash(token_hash_key,
//! code_plaintext)`. Because `code_hash` is the table's `PRIMARY KEY`, both
//! [`insert_code`] (write) and [`consume_code`] (read, via `WHERE code_hash
//! = $1`) are exact-match primary-key lookups against an
//! already-known-correct-or-not digest, not a "compare two independently
//! obtained hashes" operation — so unlike `app_repository.rs`'s
//! `verify_app_credentials` (which genuinely needs
//! [`crate::oauth::hash::verify_keyed_hash`]'s constant-time comparison
//! because it compares a freshly computed hash against a *stored* hash
//! value it read out separately), this module has no second, independently
//! obtained hash value to compare against in constant time — Postgres's own
//! B-tree index equality on `code_hash` (a high-entropy 32-byte digest) is
//! the operation actually performed, and there is no application-level
//! byte-by-byte comparison path here for `verify_keyed_hash`'s
//! constant-time discipline to protect. Only [`keyed_hash`] is used.
//!
//! ## `pkce_method` persistence (CONCERN — documented judgment call)
//! `oauth_authorization_codes` has both `pkce_challenge TEXT` and
//! `pkce_method TEXT` columns (`migrations/0003_oauth.sql`), but
//! `crate::oauth::model::PkceChallenge` (task 2.1's placeholder type, which
//! is what [`crate::oauth::model::AuthorizationCode::pkce`] actually holds —
//! *not* the "real" `crate::oauth::pkce::PkceChallenge` from task 2.3, which
//! *does* carry a `method: PkceMethod` field) has only a `challenge: String`
//! field. There is therefore no method value this module can read off the
//! domain type to persist. `crate::oauth::pkce::PkceMethod` currently has
//! exactly one variant, `S256` (see that module's own doc comment for why),
//! so this module adopts the defensible, documented convention of writing
//! the fixed literal [`PKCE_METHOD_S256`] into `pkce_method` whenever a code
//! carries a PKCE challenge, and `NULL` when it does not (mirroring
//! `pkce_challenge`'s own presence/absence). This is a genuine, load-bearing
//! design-doc/domain-type mismatch — flagged here and in this task's status
//! report as a CONCERN, not silently routed around — structurally identical
//! to task 3.1's `NO_PLAINTEXT_SECRET_SENTINEL` situation
//! (`app_repository.rs`'s doc comment). The real fix (adding a `method`
//! field to `model::PkceChallenge`, mirroring `pkce::PkceChallenge`) is a
//! change to task 2.1's already-reviewed `model.rs`, out of this task's
//! boundary to make unilaterally. [`consume_code`] reads `pkce_method` back
//! off the row (so a future caller inspecting the raw column would still
//! see a consistent value) but does not feed it into the reconstructed
//! [`crate::oauth::model::PkceChallenge`], since that type has nowhere to
//! put it.
//!
//! ## Atomic single-use consumption (Requirements 2.5, 3.1, 3.2 — the core
//! acceptance criterion of this task)
//! [`consume_code`] performs the "is this code still usable" check and the
//! "mark it used" write as a *single* conditional `UPDATE ... WHERE
//! code_hash = $1 AND consumed = FALSE AND expires_at > $2 RETURNING ...`
//! statement, never a separate `SELECT` followed by an `UPDATE`. A
//! SELECT-then-UPDATE would leave a race window between the two statements
//! in which two concurrent token-exchange requests could both observe
//! "not yet consumed" before either write lands, allowing the same
//! authorization code to be redeemed twice (a double-spend). Postgres
//! evaluates an `UPDATE ... WHERE ... RETURNING` as one atomic operation
//! against a given row under any standard isolation level, so at most one
//! concurrent caller's statement can ever match a still-unconsumed row and
//! receive a non-empty `RETURNING` result; every other concurrent (or
//! subsequent) caller's `WHERE` clause fails to match (because `consumed`
//! is now `TRUE`) and gets zero rows back, mapped to `Ok(None)`.
//!
//! ## Scope/redirect-URI storage encoding
//! `oauth_authorization_codes.scopes` is a single `TEXT` column holding what
//! is logically a set of scope tokens; this module mirrors
//! `app_repository.rs`'s established space-separated join/parse convention
//! for `model::ScopeSet` (the same placeholder type
//! `AuthorizationCode::scopes` holds) rather than re-deriving a different
//! encoding. `redirect_uri` here is a single URI (unlike
//! `oauth_applications.redirect_uris`, which is a list of registered URIs)
//! so it is stored as-is, no joining needed.

#[cfg(test)]
mod tests;

use axum::http::StatusCode;
use sqlx::postgres::PgPool;
use time::OffsetDateTime;

use crate::config::Secret;
use crate::domain::Id;
use crate::error::AppError;
use crate::oauth::hash::{TokenHashKey, keyed_hash};
use crate::oauth::model::{AuthorizationCode, PkceChallenge, ScopeSet};

/// Separator joining/splitting `oauth_authorization_codes.scopes`'s single
/// `TEXT` column, matching `app_repository.rs`'s established convention for
/// the same `model::ScopeSet` placeholder type.
const SCOPE_SEPARATOR: char = ' ';

/// Fixed PKCE method literal persisted to `pkce_method` whenever a code
/// carries a PKCE challenge. See this module's doc comment ("`pkce_method`
/// persistence") for the full rationale — this is a documented CONCERN, not
/// a design.md-specified value.
const PKCE_METHOD_S256: &str = "S256";

/// Joins `scopes` into `oauth_authorization_codes.scopes`'s stored form.
/// Mirrors `app_repository.rs::join_scopes` (not reused directly — that
/// function is private to its own module — but intentionally identical in
/// behavior, since both operate on the same `model::ScopeSet` placeholder
/// under the same Mastodon-compatible space-separated convention
/// `scope::ScopeSet::parse` establishes elsewhere in this crate).
fn join_scopes(scopes: &ScopeSet) -> String {
    scopes
        .as_strs()
        .collect::<Vec<_>>()
        .join(&SCOPE_SEPARATOR.to_string())
}

/// Reverses [`join_scopes`]. Mirrors `app_repository.rs::parse_scopes`.
fn parse_scopes(raw: &str) -> ScopeSet {
    ScopeSet::new(raw.split_whitespace())
}

/// Maps a failed `INSERT INTO oauth_authorization_codes` to an [`AppError`].
/// A primary-key collision on `code_hash` should not occur in practice (the
/// code plaintext is expected to be minted from a high-entropy random source
/// by a later issuing task, mirroring `app_repository.rs::map_insert_error`'s
/// identical reasoning for `client_id`), so any failure here is treated as
/// an unexpected `Server` (5xx) error rather than a foreseeable user-facing
/// conflict.
fn map_insert_error(source: sqlx::Error) -> AppError {
    AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source)
}

/// Inserts a freshly issued, short-lived authorization code
/// (Requirements 2.5, 3.1, 3.2): the code plaintext (`code.code`) is hashed
/// via [`keyed_hash`] before it ever reaches a `sqlx` bind parameter — only
/// `code_hash` (`BYTEA`) is persisted, never the plaintext (Requirement
/// 3.5/3.6). The selected actor (`code.actor_id`), approved scopes
/// (`code.scopes`), redirect URI, and optional PKCE challenge are persisted
/// as given, unconditionally bound to this one code row (Requirements 2.3,
/// 2.6 — the caller, a later `OauthService` task, is responsible for having
/// already performed the selection/approval this data represents).
pub async fn insert_code(
    pool: &PgPool,
    token_hash_key: &TokenHashKey,
    code: &AuthorizationCode,
) -> Result<(), AppError> {
    let code_hash = keyed_hash(token_hash_key, code.code.expose_secret());
    let scopes_stored = join_scopes(&code.scopes);
    let (pkce_challenge, pkce_method) = match &code.pkce {
        Some(pkce) => (
            Some(pkce.as_str().to_string()),
            Some(PKCE_METHOD_S256.to_string()),
        ),
        None => (None, None),
    };

    sqlx::query(
        "INSERT INTO oauth_authorization_codes \
            (code_hash, app_id, actor_id, scopes, redirect_uri, pkce_challenge, \
             pkce_method, expires_at, consumed) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(&code_hash)
    .bind(code.app_id.as_i64())
    .bind(code.actor_id.as_i64())
    .bind(&scopes_stored)
    .bind(&code.redirect_uri)
    .bind(&pkce_challenge)
    .bind(&pkce_method)
    .bind(code.expires_at)
    .bind(code.consumed)
    .execute(pool)
    .await
    .map_err(map_insert_error)?;

    Ok(())
}

/// A row `RETURNING`-ed by [`consume_code`]'s conditional `UPDATE`: enough
/// to reconstruct the [`AuthorizationCode`] the caller redeemed.
type ConsumedCodeRow = (
    i64,
    i64,
    String,
    String,
    Option<String>,
    Option<String>,
    OffsetDateTime,
);

/// Atomically consumes `raw_code` if — and only if — it is currently
/// unconsumed and not yet expired as of `now` (Requirements 2.5, 3.1, 3.2):
/// a single conditional `UPDATE ... WHERE code_hash = $1 AND consumed =
/// FALSE AND expires_at > $2 RETURNING ...` flips `consumed` to `TRUE` and
/// returns the row in the same atomic statement — never a separate
/// `SELECT` followed by an `UPDATE` (see this module's doc comment,
/// "Atomic single-use consumption", for why that distinction is this task's
/// core acceptance criterion). Returns `Ok(None)` — not an error — both when
/// `raw_code` does not correspond to any stored `code_hash`, and when it
/// does but is already consumed or expired; a later `OauthService` task is
/// responsible for turning `None` into an OAuth-spec-aligned rejection.
///
/// On success, the returned [`AuthorizationCode::code`] echoes back the
/// caller's own already-known `raw_code` argument (not anything read back
/// from storage — `code_hash` is never reversible), mirroring
/// `app_repository.rs::verify_app_credentials`'s identical pattern.
/// [`AuthorizationCode::consumed`] is always `true` on a successful
/// consumption (the row this call itself just flipped).
pub async fn consume_code(
    pool: &PgPool,
    token_hash_key: &TokenHashKey,
    raw_code: &str,
    now: OffsetDateTime,
) -> Result<Option<AuthorizationCode>, AppError> {
    let code_hash = keyed_hash(token_hash_key, raw_code);

    let row: Option<ConsumedCodeRow> = sqlx::query_as(
        "UPDATE oauth_authorization_codes \
         SET consumed = TRUE \
         WHERE code_hash = $1 AND consumed = FALSE AND expires_at > $2 \
         RETURNING app_id, actor_id, scopes, redirect_uri, pkce_challenge, \
                   pkce_method, expires_at",
    )
    .bind(&code_hash)
    .bind(now)
    .fetch_optional(pool)
    .await
    .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;

    let Some((app_id, actor_id, scopes, redirect_uri, pkce_challenge, _pkce_method, expires_at)) =
        row
    else {
        return Ok(None);
    };

    Ok(Some(AuthorizationCode {
        code: Secret::new(raw_code.to_string()),
        app_id: Id::from_i64(app_id),
        actor_id: Id::from_i64(actor_id),
        scopes: parse_scopes(&scopes),
        redirect_uri,
        pkce: pkce_challenge.map(PkceChallenge::new),
        expires_at,
        consumed: true,
    }))
}
