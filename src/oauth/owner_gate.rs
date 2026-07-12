//! `OwnerGate` (design.md "OAuth Service / サービス層" -> `OwnerGate`;
//! Requirement 2.2; task 4.1): a minimal, single-owner-per-instance
//! authentication gate that sits in front of the OAuth consent screen.
//!
//! Scope: this module owns exactly what design.md's OwnerGate
//! "Responsibilities & Constraints" describes:
//! - Comparing a presented credential against the startup-configured owner
//!   passphrase in constant time (`起動設定（Secret<T>）のオーナー資格情報を
//!   定数時間比較で照合`).
//! - On success, resolving `owner_id` via `ActorDirectory::sole_owner()`
//!   (see `crate::actor::directory`'s own doc comment for that method) —
//!   this module holds no multi-owner selection/matching logic of its own
//!   (`複数オーナーの選択・突合ロジックは持たない`).
//! - Issuing a short-lived [`OwnerSession`] on success.
//! - Producing the signed-cookie-value primitive that carries an
//!   `OwnerSession` (see "Scope decision: value signing here..." on
//!   [`encode_session_cookie`] below for exactly how far this goes).
//!
//! It does **not** own: the consent screen itself, actor-selection/scope
//! approval, CSRF token issuance/verification, or actual `Set-Cookie`/
//! `Cookie` HTTP header construction/parsing (all task 5.2's
//! `AuthorizeEndpoint`, out of this task's boundary) — Requirement 2.2's
//! "承認画面...を返す" belongs to that later task, not this one.
//!
//! ## Feature Flag Protocol: not applicable
//! Like `ActorDirectory`/`ActorService` (see their own doc comments for the
//! identical reasoning), this is a brand-new file with no existing callers
//! or previously observable behavior to gate — the endpoint layer that will
//! invoke [`authenticate_owner`] (task 5.2) does not exist yet. A standard
//! RED -> GREEN cycle (tests first, referencing not-yet-existing items,
//! then a real implementation verified against real Postgres via
//! `spawn_test_app`) is this crate's established verification method for
//! this situation; there is no prior behavior a flag could gate.
//!
//! ## `OwnerCredential` = `OwnerConfig` (design.md naming vs. this codebase)
//! design.md's Service Interface names the config parameter type
//! `OwnerCredential`, but `crate::config::OwnerConfig` (task 1.2, already
//! implemented/reviewed) already holds exactly the single field design.md
//! describes (`password: Secret<String>`), with no shape difference. Rather
//! than defining a second, structurally identical type (and a conversion
//! between them that would carry no behavior), [`OwnerCredential`] is a
//! plain type alias for `OwnerConfig` — callers pass `&app_config.owner`
//! directly.
//!
//! ## `OwnerLogin`: the presented plaintext credential
//! design.md names this type but does not sketch its fields. It is the
//! plaintext credential a login form submits — structurally the presented-
//! side counterpart to `OwnerCredential`'s stored-side `password`. Its
//! `password` field is wrapped in [`Secret`], mirroring every other
//! credential-shaped field in this crate (`OwnerConfig::password`,
//! `OauthApp::client_secret`, ...): a plaintext password is exactly the kind
//! of value this crate's masking discipline exists for, regardless of
//! whether it is the stored or the presented side of a comparison.
//!
//! ## Scope decision: cookie *value* signing here, HTTP header attributes
//! at the endpoint layer (task 5.2)
//! design.md marks OwnerGate's Contracts as `Service [x]` only (no
//! `API [x]`), and its Service Interface returns `Result<OwnerSession,
//! AppError>` — a domain value, not an HTTP header/cookie string — which
//! places actual `Set-Cookie` header construction (the `HttpOnly`/
//! `SameSite`/`Secure`/`Max-Age` *attributes*, all inherently HTTP-response
//! concerns) at the endpoint layer (task 5.2's `AuthorizeEndpoint`, out of
//! this task's boundary; note task 5.2's own boundary line in `tasks.md`
//! also names `OwnerGate`, consistent with it *calling into* this module
//! rather than this module reaching into HTTP response construction).
//! However, task 4.1's own completion condition (`tasks.md`: "正しい資格情報
//! でセッションが得られて署名付き Cookie が発行され...ることを単体テストで
//! 確認できる") explicitly requires a signed cookie to be produced and
//! unit-testable as part of *this* task — so this module owns the signing/
//! verification *primitive* ([`encode_session_cookie`]/
//! [`decode_session_cookie`]: turning an `OwnerSession` into a
//! tamper-evident opaque string and back), while leaving the surrounding
//! `Set-Cookie`/`Cookie` header plumbing (and the actual `HttpOnly`/
//! `SameSite`/`Secure` attribute values, per design.md's Security
//! Considerations) to task 5.2, which calls into these two functions.
//! [`OWNER_SESSION_COOKIE_NAME`] is defined here (not duplicated at the
//! endpoint layer) so both sides agree on one cookie name.
//!
//! ## Signing key: reuses `OauthConfig.token_hash_key`, no new startup secret
//! (CONCERN — documented judgment call)
//! No dedicated cookie-signing secret exists in `AppConfig`: task 1.2
//! (already implemented/reviewed) added exactly two startup secrets,
//! `owner.password` and `oauth.token_hash_key`, and design.md's Data
//! Contracts & Integration section lists exactly those two additions,
//! nothing else. Adding a third startup secret would mean editing
//! `src/config.rs`, which is not among the files task 4.1 owns.
//! `oauth.token_hash_key` is this crate's one already-provisioned,
//! already-masked 256-bit keyed-hash secret; `crate::oauth::hash`'s
//! `TokenHashKey`/`keyed_hash`/`verify_keyed_hash` primitives (task 3.1) are
//! generic over any `&str` payload in their actual implementation (only
//! their doc comments frame them around OAuth secret material
//! specifically) — reusing them here consumes an existing public API
//! exactly as `verify_keyed_hash` was designed to be used (recompute a
//! keyed hash over a payload and constant-time-compare it to a presented
//! one), rather than modifying `hash.rs`'s file or silently widening its
//! documented OAuth-secret scope. This is a deliberate, documented judgment
//! call — flagged again in this task's status report — not a silent
//! workaround.

#[cfg(test)]
mod tests;

use time::{Duration, OffsetDateTime};

use axum::http::StatusCode;
use subtle::ConstantTimeEq;

use crate::actor::ActorDirectory;
use crate::config::Secret;
use crate::domain::Id;
use crate::error::AppError;
use crate::oauth::hash::{TokenHashKey, keyed_hash, verify_keyed_hash};
use crate::oauth::model::OwnerSession;

/// The startup-configured owner credential [`authenticate_owner`] compares
/// against. A type alias for [`crate::config::OwnerConfig`] — see this
/// module's doc comment ("`OwnerCredential` = `OwnerConfig`") for why no
/// separate type is defined.
pub type OwnerCredential = crate::config::OwnerConfig;

/// A presented plaintext owner credential, e.g. from a login form submission
/// (design.md's Service Interface names this type; see this module's doc
/// comment for its field's shape rationale).
#[derive(Debug, Clone)]
pub struct OwnerLogin {
    pub password: Secret<String>,
}

/// How long an issued [`OwnerSession`] remains valid. design.md's OwnerGate
/// Responsibilities call for a "短命" (short-lived) session gating the
/// consent screen, not a long-lived login session — it only needs to
/// outlive a single human-paced GET (render consent) -> POST (submit
/// decision) round trip through `/oauth/authorize` (task 5.2, out of this
/// task's boundary), not survive across separate authorization attempts.
/// Ten minutes is comfortably longer than that interactive round trip while
/// keeping a leaked/stale session cookie's blast radius small, mirroring
/// this crate's existing "short-lived, single-purpose" precedent for
/// authorization codes (design.md's Physical Data Model:
/// `oauth_authorization_codes.expires_at`).
const OWNER_SESSION_TTL: Duration = Duration::minutes(10);

/// Name of the HttpOnly cookie an endpoint-layer caller (task 5.2's
/// `AuthorizeEndpoint`) sets/reads to carry an [`OwnerSession`] across the
/// GET (render consent) -> POST (submit decision) round trip (design.md's
/// OwnerGate Responsibilities: "署名付き HttpOnly Cookie...として運搬する").
/// Defined here so both this module's own [`encode_session_cookie`]/
/// [`decode_session_cookie`] and task 5.2's future header handling agree on
/// one name without duplicating it.
pub const OWNER_SESSION_COOKIE_NAME: &str = "kawasemi_owner_session";

/// Authenticates the single human owner of this instance and, on success,
/// issues a short-lived [`OwnerSession`] (design.md's exact Service
/// Interface signature; Requirement 2.2).
///
/// `presented`'s password is compared to `cfg`'s in constant time
/// (`subtle::ConstantTimeEq`, mirroring `crate::oauth::hash`'s established
/// comparison discipline). On a mismatch, this returns a caller-facing
/// (`Client`, 401) [`AppError`] without ever consulting `directory` — an
/// unauthenticated presenter is never told anything about whether an owner
/// even exists.
///
/// On a match, `owner_id` is resolved via [`ActorDirectory::sole_owner`]
/// (design.md: "actor-model の単一オーナー取得アクセサ...を呼び出して解決す
/// る"). If that invariant-checked lookup itself fails (an
/// un-bootstrapped/corrupted instance — see that method's own doc comment),
/// its `Server` (5xx) `AppError` propagates unchanged: a correctly-presented
/// credential against a misconfigured instance is a system problem, not an
/// authentication failure, and must not be reported as one.
///
/// `now` is the caller-supplied current time (this crate's `Clock` boundary
/// discipline — never read directly here); `OwnerSession.expires_at` is
/// `now + `[`OWNER_SESSION_TTL`].
pub async fn authenticate_owner(
    cfg: &OwnerCredential,
    presented: &OwnerLogin,
    directory: &ActorDirectory,
    now: OffsetDateTime,
) -> Result<OwnerSession, AppError> {
    if !passwords_match(cfg, presented) {
        return Err(AppError::client(
            StatusCode::UNAUTHORIZED,
            "invalid owner credentials",
        ));
    }

    let owner = directory.sole_owner().await?;
    Ok(OwnerSession {
        owner_id: owner.id,
        expires_at: now + OWNER_SESSION_TTL,
    })
}

/// Constant-time comparison of `cfg`'s stored passphrase against
/// `presented`'s plaintext (design.md: "定数時間比較で照合"). Byte-for-byte
/// via `subtle::ConstantTimeEq`, never `==`, so neither an early length
/// mismatch nor a differing byte returns in variable time relative to how
/// much of the two values matched.
fn passwords_match(cfg: &OwnerCredential, presented: &OwnerLogin) -> bool {
    let expected = cfg.password.expose_secret().as_bytes();
    let presented = presented.password.expose_secret().as_bytes();
    expected.ct_eq(presented).into()
}

/// Encodes `session` as a signed cookie *value* (design.md's OwnerGate
/// Responsibilities: "署名付き HttpOnly Cookie...として運搬する") — the
/// opaque string a caller places in a `Set-Cookie:
/// kawasemi_owner_session=<this value>; HttpOnly; ...` response header. See
/// this module's doc comment ("Scope decision...") for exactly what this
/// function does and does not own.
///
/// The payload (`owner_id:expires_at_unix_seconds`) is HMAC-SHA256-keyed
/// (via [`crate::oauth::hash::keyed_hash`]) and the resulting cookie value
/// is `"<payload>.<mac as lowercase hex>"`. Precision note: the payload
/// carries whole-second Unix time, so a session's `expires_at` round-trips
/// through this encoding only to whole-second precision — acceptable given
/// [`OWNER_SESSION_TTL`]'s minutes-scale granularity.
pub fn encode_session_cookie(session: &OwnerSession, key: &TokenHashKey) -> String {
    let payload = session_payload(session);
    let mac = keyed_hash(key, &payload);
    format!("{payload}.{}", hex_encode(&mac))
}

/// Verifies and decodes a cookie value produced by [`encode_session_cookie`]
/// under the same `key`, rejecting a tampered/malformed value, a value
/// signed under a different key, or an expired session — all as the same
/// caller-facing (`Client`, 401) [`AppError`], never distinguishing which,
/// so a probing attacker learns nothing about which failure mode occurred.
/// Never panics on malformed input: `cookie_value` originates from an
/// untrusted HTTP `Cookie` header.
///
/// `now` is the caller-supplied current time (this crate's `Clock` boundary
/// discipline), used to check `expires_at`.
pub fn decode_session_cookie(
    cookie_value: &str,
    key: &TokenHashKey,
    now: OffsetDateTime,
) -> Result<OwnerSession, AppError> {
    let invalid = || AppError::client(StatusCode::UNAUTHORIZED, "invalid or expired owner session");

    let (payload, mac_hex) = cookie_value.rsplit_once('.').ok_or_else(invalid)?;
    let presented_mac = hex_decode(mac_hex).ok_or_else(invalid)?;
    if !verify_keyed_hash(key, payload, &presented_mac) {
        return Err(invalid());
    }
    let session = parse_session_payload(payload).ok_or_else(invalid)?;
    if session.expires_at <= now {
        return Err(invalid());
    }
    Ok(session)
}

/// Canonical, deterministic string form of `session` that
/// [`encode_session_cookie`]/[`decode_session_cookie`] sign/verify:
/// `"<owner_id>:<expires_at as Unix seconds>"`.
fn session_payload(session: &OwnerSession) -> String {
    format!(
        "{}:{}",
        session.owner_id.as_i64(),
        session.expires_at.unix_timestamp()
    )
}

/// Inverse of [`session_payload`]. Returns `None` (never panics) on any
/// malformed input — `payload` is untrusted (the caller has not yet
/// verified the signature when parsing could occur, so this must not assume
/// well-formedness).
fn parse_session_payload(payload: &str) -> Option<OwnerSession> {
    let (owner_id_raw, expires_at_raw) = payload.split_once(':')?;
    let owner_id = owner_id_raw.parse::<i64>().ok()?;
    let expires_at_unix = expires_at_raw.parse::<i64>().ok()?;
    let expires_at = OffsetDateTime::from_unix_timestamp(expires_at_unix).ok()?;
    Some(OwnerSession {
        owner_id: Id::from_i64(owner_id),
        expires_at,
    })
}

/// Encodes `bytes` as lowercase hex.
fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(out, "{byte:02x}").expect("writing to a String never fails");
    }
    out
}

/// Decodes a lowercase-hex string into bytes, returning `None` (never
/// panicking) on an odd length or a non-hex character. Operates on `char`s,
/// not raw byte indices, mirroring `config.rs::validate_hex_256_bits`'s
/// exact discipline: `raw` is untrusted input from an HTTP request, so a
/// malformed value containing multi-byte UTF-8 characters must be reported
/// as "invalid", never panic on a byte index that splits one.
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
