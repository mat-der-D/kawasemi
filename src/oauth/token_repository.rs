//! `AccessTokenRepository` (design.md "Data / データ層" -> `OauthAppRepository
//! / AuthorizationCodeRepository / AccessTokenRepository`; Requirements 3.1,
//! 3.4, 3.6, 5.1, 5.2; task 3.3): access token issuance, hash-based
//! resolution, and revocation against `oauth_access_tokens`
//! (`migrations/0003_oauth.sql`).
//!
//! Scope: this module owns exactly the three operations design.md's Service
//! Interface sketches for this component (adapted — see [`IssuedToken`]'s
//! doc comment for why) — [`issue_token`], [`resolve_token`],
//! [`revoke_token`] — against a plain `&PgPool`. It does not implement the
//! OAuth application repository (`app_repository.rs`, task 3.1) or the
//! authorization code repository (`code_repository.rs`, task 3.2), both
//! already done, does not validate token-exchange input (client/redirect-URI/
//! PKCE checks are `OauthService`'s job, a later task), and does not wire
//! itself into `AppState`/`bootstrap.rs`/the router (a later integration
//! task).
//!
//! ## Token minting (Requirement 3.1) and the `issue_token` return shape
//! (CONCERN — documented judgment call)
//! Design.md's abbreviated Service Interface sketch is `issue_token(pool,
//! token: &AccessToken) -> Result<(), AppError>` — a pre-built `AccessToken`
//! handed in, nothing returned. That shape cannot work here:
//! `crate::oauth::model::AccessToken` (task 2.1, already reviewed) holds only
//! `token_hash: Vec<u8>` — unlike `OauthApp::client_secret: Secret<String>`
//! (task 3.1) or `AuthorizationCode::code: Secret<String>` (task 3.2), it has
//! *no* plaintext field at all (see `model.rs`'s own doc comment: "computing
//! that hash is a later repository task's job (3.3), not this module's").
//! There is therefore nothing to read a bearer token back out of an
//! `&AccessToken` — the entire point of issuing one. Tasks 3.1
//! (`register_app`) and 3.2 (`insert_code`) each already had to extend this
//! same abbreviated design.md sketch for analogous reasons (see their own
//! module doc comments); this module follows that established precedent by
//! having [`issue_token`] mint the raw bearer token itself — from the
//! injected [`Rng`] boundary, never `rand`/`getrandom` directly, matching
//! `app_repository.rs::random_url_safe_token`'s convention (duplicated here
//! as a private, near-identical local helper, since that function is private
//! to its own module; `code_repository.rs`'s duplicated `join_scopes`/
//! `parse_scopes` already establish this "small helper duplication across
//! sibling repository modules" convention in this crate) — and returning it
//! alongside the persisted [`AccessToken`] record via [`IssuedToken`], rather
//! than accepting an already-hashed `&AccessToken` with no path to a
//! plaintext at all. This is a genuine, load-bearing design-doc/domain-type
//! mismatch, flagged here and in this task's status report as a CONCERN, not
//! silently routed around, structurally identical to task 3.1's
//! `NO_PLAINTEXT_SECRET_SENTINEL` situation and task 3.2's `pkce_method`
//! situation (see those modules' doc comments).
//!
//! ## Hashing: reused, not re-derived (`hash.rs`, task 3.1)
//! [`crate::oauth::hash::keyed_hash`] is used exactly as
//! `app_repository.rs`/`code_repository.rs` already use it:
//! `token_hash = keyed_hash(token_hash_key, token_plaintext)`.
//!
//! ## Why `resolve_token`/`revoke_token` do not need `verify_keyed_hash`
//! (CONCERN — documented judgment call, per this task's Implementation Notes
//! precedent)
//! Unlike `app_repository.rs::verify_app_credentials` (which genuinely needs
//! [`crate::oauth::hash::verify_keyed_hash`]'s constant-time comparison
//! because it compares a freshly computed hash against a *stored* hash value
//! it read out separately, in a second, independent step), neither
//! [`resolve_token`] nor [`revoke_token`] ever reads a stored `token_hash`
//! back into application code to compare it byte-by-byte against anything.
//! Both instead bind the freshly computed `keyed_hash(token_hash_key,
//! raw_token)` straight into a single SQL `WHERE token_hash = $1 [AND
//! revoked = FALSE]` clause and let Postgres's own index equality check
//! decide the match — structurally the same shape `code_repository.rs`
//! already reasoned through for `consume_code`'s `WHERE code_hash = $1`
//! primary-key lookup. `oauth_access_tokens.token_hash` is declared `UNIQUE`
//! rather than `PRIMARY KEY` (unlike `code_hash`), but a `UNIQUE` constraint
//! is backed by the same kind of B-tree index Postgres uses for a primary
//! key, and `=` against it is the same single exact-match index operation —
//! the primary-key-vs-unique-index distinction does not change *how* the
//! comparison is performed (there is still no separately-fetched hash value
//! for an application-level byte comparison to leak timing information
//! about). The additional `AND revoked = FALSE` in [`resolve_token`], and
//! the conditional `UPDATE ... WHERE token_hash = $1 AND revoked = FALSE` in
//! [`revoke_token`], are extra conditions evaluated as part of that same
//! single indexed lookup/atomic statement, not a second, separate hash
//! comparison step — so this module's judgment (matching `code_repository
//! .rs`'s) is that only [`keyed_hash`] is needed here; [`verify_keyed_hash`]
//! is not.
//!
//! ## Revoked tokens are invalid on resolution (Requirements 3.4, 5.1, 5.2 —
//! the core acceptance criterion of this task)
//! [`resolve_token`] queries `WHERE token_hash = $1 AND revoked = FALSE`, not
//! a query that ignores `revoked` followed by an application-level `if
//! revoked { None }` check — so a revoked token's row genuinely fails to
//! match the query itself and [`resolve_token`] returns `Ok(None)`, the same
//! outcome as "no such token", never distinguishing the two to the caller
//! (mirroring `verify_app_credentials`'s/`consume_code`'s established
//! "don't leak existence" pattern from tasks 3.1/3.2). The row itself is left
//! in place (`revoked = TRUE`, not deleted), so this is genuinely a
//! query-level exclusion, not merely "the field got set somewhere and nobody
//! checks it".
//!
//! ## `revoke_token`'s `bool` return and the already-revoked case (CONCERN —
//! documented judgment call)
//! [`revoke_token`] performs the "is this token still active" check and the
//! "mark it revoked" write as a single conditional `UPDATE ... WHERE
//! token_hash = $1 AND revoked = FALSE` statement (never a separate `SELECT`
//! then `UPDATE`), mirroring `consume_code`'s atomicity rationale
//! (`code_repository.rs`'s doc comment) — this closes the same race window a
//! SELECT-then-UPDATE would otherwise open between two concurrent revoke
//! calls for the same token. It returns `true` only when its own call
//! actually flipped a row from `revoked = FALSE` to `revoked = TRUE`
//! (`rows_affected() > 0`), and `false` both when `raw_token` matches no row
//! at all *and* when it matches a row that was already revoked (by an
//! earlier call). This module's judgment is that "already revoked" should
//! report `false`, not an error or a `true`: revocation is idempotent from
//! the caller's perspective (the token ends up invalid either way), and
//! `false` in the "already revoked" case cannot be distinguished from `false`
//! in the "never existed" case — which mirrors this same module's
//! [`resolve_token`] deliberately not distinguishing "no such token" from
//! "revoked token" either, for the same "don't leak existence" reasoning.
//!
//! ## Scope storage encoding
//! `oauth_access_tokens.scopes` is a single `TEXT` column holding what is
//! logically a set of scope tokens; this module mirrors
//! `app_repository.rs`'s/`code_repository.rs`'s established space-separated
//! join/parse convention for `model::ScopeSet` (the same placeholder type
//! `AccessToken::scopes` holds) rather than re-deriving a different encoding.

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
use crate::oauth::hash::{TokenHashKey, keyed_hash};
use crate::oauth::model::{AccessToken, ScopeSet};
use crate::runtime::ids::IdGenerator;
use crate::runtime::rng::Rng;

/// Number of random bytes drawn from [`Rng`] to mint a new bearer access
/// token (before base64url encoding) — matching
/// `app_repository.rs::CLIENT_SECRET_RANDOM_BYTES`'s entropy for other
/// secret-shaped random values in this crate.
const ACCESS_TOKEN_RANDOM_BYTES: usize = 32;

/// Separator joining/splitting `oauth_access_tokens.scopes`'s single `TEXT`
/// column, matching `app_repository.rs`'s/`code_repository.rs`'s established
/// convention for the same `model::ScopeSet` placeholder type.
const SCOPE_SEPARATOR: char = ' ';

/// Input to [`issue_token`]: everything the caller supplies about a token to
/// mint. The plaintext bearer token itself is minted by [`issue_token`]
/// (mirroring `app_repository.rs::register_app`'s `client_id`/
/// `client_secret` minting), not supplied here — see this module's doc
/// comment ("Token minting ... `issue_token` return shape").
#[derive(Debug, Clone)]
pub struct NewAccessToken {
    pub app_id: Id,
    pub actor_id: Id,
    pub scopes: ScopeSet,
}

/// The result of issuing a fresh access token: the plaintext bearer token
/// (returned to the caller exactly once, at issuance time — mirroring
/// `OauthApp::client_secret`'s one-time-return convention in
/// `app_repository.rs::register_app`), alongside the persisted [`AccessToken`]
/// record (whose own `token_hash` field never carries the plaintext, per
/// `model.rs`'s doc comment). A later `OauthService`/`TokenEndpoint` task
/// consumes `plaintext` to hand the bearer token to the OAuth client in the
/// Mastodon-compatible token response; `token` is the durable record this
/// repository persisted. See this module's doc comment for why design.md's
/// `issue_token(pool, token: &AccessToken) -> Result<(), AppError>` sketch
/// does not fit `AccessToken`'s real shape and had to be extended this way.
#[derive(Debug, Clone)]
pub struct IssuedToken {
    pub plaintext: Secret<String>,
    pub token: AccessToken,
}

/// Generates a fresh random bearer token of `random_bytes` bytes (drawn from
/// `rng`, core-runtime's injected non-determinism boundary — never a direct
/// `rand`/`getrandom` call, per design.md's "Adjacent expectations"),
/// base64url-encoded with no padding. Deliberately near-identical to
/// `app_repository.rs::random_url_safe_token` (private to that module, hence
/// not reused directly) — see this module's doc comment for why this small
/// duplication follows an already-established crate convention.
fn random_url_safe_token(rng: &dyn Rng, random_bytes: usize) -> String {
    let mut buf = vec![0u8; random_bytes];
    rng.fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

/// Joins `scopes` into `oauth_access_tokens.scopes`'s stored form. Mirrors
/// `app_repository.rs::join_scopes`/`code_repository.rs::join_scopes` (not
/// reused directly — both are private to their own modules — but
/// intentionally identical in behavior; see this module's doc comment).
fn join_scopes(scopes: &ScopeSet) -> String {
    scopes
        .as_strs()
        .collect::<Vec<_>>()
        .join(&SCOPE_SEPARATOR.to_string())
}

/// Reverses [`join_scopes`]. Mirrors `app_repository.rs::parse_scopes`/
/// `code_repository.rs::parse_scopes`.
fn parse_scopes(raw: &str) -> ScopeSet {
    ScopeSet::new(raw.split_whitespace())
}

/// Maps a failed `INSERT INTO oauth_access_tokens` to an [`AppError`]. A
/// unique violation on `token_hash` should not occur in practice (the token
/// plaintext is minted from a high-entropy random source, mirroring
/// `app_repository.rs::map_insert_error`'s/`code_repository.rs
/// ::map_insert_error`'s identical reasoning for `client_id`/`code_hash`), so
/// any failure here is treated as an unexpected `Server` (5xx) error rather
/// than a foreseeable user-facing conflict.
fn map_insert_error(source: sqlx::Error) -> AppError {
    AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source)
}

/// Issues a fresh access token bound to a single actor and its approved
/// scopes (Requirements 3.1, 3.5): mints the raw bearer token from the
/// injected [`Rng`] boundary and an `id` from the injected [`IdGenerator`]
/// boundary, hashes the token via [`keyed_hash`] before it ever reaches a
/// `sqlx` bind parameter — only `token_hash` (`BYTEA`) is persisted, never
/// the plaintext (Requirement 3.6) — and stores `app_id`/`actor_id`/`scopes`
/// as given, with `revoked = FALSE`.
///
/// Returns an [`IssuedToken`] whose `plaintext` field holds the bearer token
/// exactly once — this is issuance's one-time response value, mirroring
/// `register_app`'s `client_secret` return. The caller (a later task's
/// `OauthService`/`TokenEndpoint`) is responsible for not logging it either.
pub async fn issue_token(
    pool: &PgPool,
    ids: &dyn IdGenerator,
    rng: &dyn Rng,
    token_hash_key: &TokenHashKey,
    now: OffsetDateTime,
    input: NewAccessToken,
) -> Result<IssuedToken, AppError> {
    let id = ids.next_id();
    let plaintext = random_url_safe_token(rng, ACCESS_TOKEN_RANDOM_BYTES);
    let token_hash = keyed_hash(token_hash_key, &plaintext);
    let scopes_stored = join_scopes(&input.scopes);

    sqlx::query(
        "INSERT INTO oauth_access_tokens \
            (id, token_hash, app_id, actor_id, scopes, created_at, revoked) \
         VALUES ($1, $2, $3, $4, $5, $6, FALSE)",
    )
    .bind(id.as_i64())
    .bind(&token_hash)
    .bind(input.app_id.as_i64())
    .bind(input.actor_id.as_i64())
    .bind(&scopes_stored)
    .bind(now)
    .execute(pool)
    .await
    .map_err(map_insert_error)?;

    Ok(IssuedToken {
        plaintext: Secret::new(plaintext),
        token: AccessToken {
            id,
            token_hash,
            app_id: input.app_id,
            actor_id: input.actor_id,
            scopes: input.scopes,
            created_at: now,
            revoked: false,
        },
    })
}

/// A row `SELECT`-ed by [`resolve_token`]'s lookup: enough to reconstruct the
/// [`AccessToken`] a valid, non-revoked bearer token resolves to.
type ResolvedTokenRow = (i64, i64, i64, String, OffsetDateTime);

/// Resolves `raw_token` to the [`AccessToken`] it was issued as (Requirements
/// 3.1, 3.5, 5.1): hashes `raw_token` via [`keyed_hash`] and looks it up via
/// `WHERE token_hash = $1 AND revoked = FALSE` — a single indexed equality
/// lookup against the `UNIQUE` `token_hash` column (see this module's doc
/// comment for why [`crate::oauth::hash::verify_keyed_hash`]'s constant-time
/// comparison is not needed here).
///
/// Returns `Ok(None)` — not an error — both when `raw_token` matches no
/// stored `token_hash` at all, and when it matches a row that has been
/// revoked (Requirement 3.4, 5.2's "失効済みは None 相当"): the revoked-token
/// case is excluded by the query's own `WHERE` clause, not by an
/// application-level check performed after the fact, and the caller cannot
/// distinguish the two outcomes (mirroring `verify_app_credentials`'s/
/// `consume_code`'s established "don't leak existence" pattern). On success,
/// the returned [`AccessToken`] carries the actor identifier and approved
/// scopes bound at issuance time, ready for a later Bearer-auth middleware
/// task to build a `RequestActorContext` from.
pub async fn resolve_token(
    pool: &PgPool,
    token_hash_key: &TokenHashKey,
    raw_token: &str,
) -> Result<Option<AccessToken>, AppError> {
    let token_hash = keyed_hash(token_hash_key, raw_token);

    let row: Option<ResolvedTokenRow> = sqlx::query_as(
        "SELECT id, app_id, actor_id, scopes, created_at \
         FROM oauth_access_tokens WHERE token_hash = $1 AND revoked = FALSE",
    )
    .bind(&token_hash)
    .fetch_optional(pool)
    .await
    .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;

    let Some((id, app_id, actor_id, scopes, created_at)) = row else {
        return Ok(None);
    };

    Ok(Some(AccessToken {
        id: Id::from_i64(id),
        token_hash,
        app_id: Id::from_i64(app_id),
        actor_id: Id::from_i64(actor_id),
        scopes: parse_scopes(&scopes),
        created_at,
        revoked: false,
    }))
}

/// Revokes `raw_token` (Requirement 3.4): a single conditional `UPDATE
/// oauth_access_tokens SET revoked = TRUE WHERE token_hash = $1 AND revoked =
/// FALSE` — never a separate `SELECT` followed by an `UPDATE` — atomically
/// flips the row's `revoked` flag, mirroring `consume_code`'s atomicity
/// rationale (`code_repository.rs`'s doc comment) for the analogous "check
/// then mark, without a race window" shape.
///
/// Returns `Ok(true)` only when this call's own `UPDATE` actually matched and
/// flipped a row (`rows_affected() > 0`); `Ok(false)` both when `raw_token`
/// matches no stored `token_hash` at all, and when it matches a row that was
/// already revoked by an earlier call. See this module's doc comment
/// ("`revoke_token`'s `bool` return and the already-revoked case") for why
/// this module treats those two outcomes alike rather than distinguishing
/// them or erroring.
pub async fn revoke_token(
    pool: &PgPool,
    token_hash_key: &TokenHashKey,
    raw_token: &str,
) -> Result<bool, AppError> {
    let token_hash = keyed_hash(token_hash_key, raw_token);

    let result = sqlx::query(
        "UPDATE oauth_access_tokens SET revoked = TRUE \
         WHERE token_hash = $1 AND revoked = FALSE",
    )
    .bind(&token_hash)
    .execute(pool)
    .await
    .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;

    Ok(result.rows_affected() > 0)
}
