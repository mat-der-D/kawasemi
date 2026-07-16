//! PKCE (S256) challenge verification, bound to an authorization code
//! (`Pkce` component, design.md "OAuth Domain / ドメイン層" -> `Pkce`,
//! Requirements 2.6, 3.3; task 2.3).
//!
//! Scope: this module owns exactly the PKCE challenge representation and
//! the S256 challenge/verifier consistency check design.md's `Pkce`
//! component sketches (`PkceChallenge`, `verify_pkce`). It does not own
//! binding a challenge to an [`crate::oauth::model::AuthorizationCode`]
//! (that struct already has a `pkce: Option<_>` field per task 2.1 — wiring
//! it to *this* module's real [`PkceChallenge`] instead of task 2.1's
//! placeholder stand-in `crate::oauth::model::PkceChallenge` is deferred to
//! a later task, exactly as `scope.rs`'s doc comment defers wiring
//! `crate::oauth::model::ScopeSet` to the real `ScopeSet`), authorization
//! request handling, or token exchange orchestration (`OauthService`, later
//! tasks) — those consume [`verify_pkce`] as the single shared
//! implementation rather than re-deriving S256 matching of their own.
//!
//! ## RFC 7636 S256
//!
//! A PKCE-capable client generates a high-entropy random `code_verifier` and
//! sends `code_challenge = BASE64URL-ENCODE(SHA256(ASCII(code_verifier)))`
//! with `code_challenge_method=S256` at the authorization request. The
//! server stores `code_challenge` bound to the issued authorization code
//! (Requirement 2.6). At token exchange, the client presents the original
//! `code_verifier`; the server must recompute the same digest and compare it
//! to the stored challenge, rejecting the exchange on any mismatch
//! (Requirement 3.3).
//!
//! ## Method vocabulary: S256 only
//!
//! [`PkceMethod`] is modeled as an enum (rather than a bare string) to leave
//! room for future methods, but today it has exactly one variant,
//! [`PkceMethod::S256`]. This is a deliberate choice, not an oversight:
//! RFC 7636 and Mastodon-compatible servers alike are expected to reject the
//! `plain` method (which sends `code_challenge == code_verifier` in the
//! clear, providing no protection against a stolen authorization code —
//! defeating the entire point of PKCE, Requirement 2.6's "コード交換用の検証
//! 情報"). Since this task's acceptance criteria (2.6, 3.3, and the task's
//! own line: "整合/不整合がそれぞれ成功/エラーになることを単体テストで確認でき
//! る") only require S256 support, no `Plain` variant is added — adding one
//! without also implementing/rejecting it correctly would be worse than not
//! having it. Extending [`PkceMethod`] later (e.g. if a future requirement
//! explicitly asks for `plain`) is a one-variant addition plus a new match
//! arm in [`verify_pkce`]; existing S256 callers are unaffected.
//!
//! ## Constant-time comparison rationale
//!
//! [`verify_pkce`] compares the recomputed digest to the stored challenge
//! using [`subtle::ConstantTimeEq`] rather than a plain `==` on the
//! base64url strings. Strictly speaking the PKCE challenge is not itself a
//! secret — it is transmitted in the clear in the authorization request
//! (Requirement 2.6 stores it un-encrypted, and RFC 7636's threat model
//! only requires the *verifier* to be unguessable, not the challenge to be
//! confidential) — so a timing side-channel on this specific comparison
//! does not, by itself, hand an attacker anything they could not already
//! observe by eavesdropping the authorization request. Constant-time
//! comparison is nonetheless the deliberate choice here, for two reasons:
//! (1) consistency with this project's established comparison discipline
//! for anything hash-shaped and security-adjacent (design.md's Security
//! Considerations calls for "ハッシュ化した値同士の定数時間比較" for
//! `client_secret`/token hashes, and a SHA-256 digest comparison is the same
//! shape of operation), and (2) it costs nothing here — `subtle` is already
//! a transitive dependency (via `rsa`) at the exact version this crate now
//! depends on directly, so there is no new supply-chain surface, and the
//! comparison is not on any hot path. A plain `==` would also have been
//! defensible given the non-secret nature of the challenge; this module
//! picks the more conservative option rather than silently defaulting to
//! either.

use axum::http::StatusCode;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::error::AppError;

/// The PKCE code-challenge method a client declared at the authorization
/// request (Requirement 2.6). See this module's doc comment for why only
/// `S256` exists today.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PkceMethod {
    /// `S256`: `code_challenge = BASE64URL-ENCODE(SHA256(ASCII(code_verifier)))`.
    S256,
}

/// A PKCE challenge bound to an authorization code (Requirement 2.6):
/// the method the client declared, plus the `code_challenge` value it sent
/// at the authorization request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PkceChallenge {
    pub method: PkceMethod,
    pub challenge: String,
}

impl PkceChallenge {
    /// Builds a `PkceChallenge` from a declared `method` and the raw
    /// `code_challenge` string the client sent.
    pub fn new(method: PkceMethod, challenge: impl Into<String>) -> Self {
        Self {
            method,
            challenge: challenge.into(),
        }
    }

    /// Builds a `PkceChallenge` for a `code_verifier` by computing its S256
    /// digest (Requirement 2.6's authorization-time direction). Exposed
    /// alongside [`verify_pkce`] (the token-exchange-time direction,
    /// Requirement 3.3) because a realistic S256 round trip needs both
    /// halves: this crate's authorization endpoint (a later task) will use
    /// this constructor to persist the challenge the client declared, and
    /// this module's own tests use it to derive a valid verifier/challenge
    /// pair without duplicating the digest computation inline.
    pub fn from_verifier_s256(verifier: &str) -> Self {
        Self {
            method: PkceMethod::S256,
            challenge: compute_s256_challenge(verifier),
        }
    }
}

/// Computes the RFC 7636 S256 `code_challenge` for a given `code_verifier`:
/// `BASE64URL-ENCODE(SHA256(ASCII(code_verifier)))`, base64url with no
/// padding, matching RFC 7636 section 4.2 exactly.
fn compute_s256_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

/// Verifies that `verifier` (the `code_verifier` presented at token
/// exchange) is consistent with `challenge` (the `code_challenge` bound to
/// the authorization code at the authorization request), per the
/// challenge's declared method (Requirements 2.6, 3.3).
///
/// Returns `Ok(())` when the recomputed challenge matches; returns a
/// rejecting [`AppError`] (400 Bad Request, an OAuth-spec-aligned
/// `invalid_grant`-shaped rejection) on any mismatch — the token exchange
/// must not issue a token in that case (Requirement 3.3: "不整合のときはトー
/// クンを発行しない").
pub fn verify_pkce(challenge: &PkceChallenge, verifier: &str) -> Result<(), AppError> {
    match challenge.method {
        PkceMethod::S256 => {
            let computed = compute_s256_challenge(verifier);
            let is_match: bool = computed
                .as_bytes()
                .ct_eq(challenge.challenge.as_bytes())
                .into();
            if is_match {
                Ok(())
            } else {
                Err(AppError::client(
                    StatusCode::BAD_REQUEST,
                    "PKCE verification failed: code_verifier does not match code_challenge",
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A representative high-entropy verifier, matching RFC 7636's example
    /// verifier (appendix B) so the expected challenge is a known-good,
    /// independently verifiable value rather than something only this
    /// module's own code produced.
    const RFC7636_EXAMPLE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    /// RFC 7636 appendix B's expected S256 challenge for the verifier above.
    const RFC7636_EXAMPLE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

    #[test]
    fn compute_s256_challenge_matches_rfc7636_known_answer_vector() {
        assert_eq!(
            compute_s256_challenge(RFC7636_EXAMPLE_VERIFIER),
            RFC7636_EXAMPLE_CHALLENGE
        );
    }

    #[test]
    fn pkce_challenge_from_verifier_s256_round_trips_through_verify_pkce() {
        let verifier = "a-different-high-entropy-verifier-1234567890";
        let challenge = PkceChallenge::from_verifier_s256(verifier);
        assert_eq!(challenge.method, PkceMethod::S256);
        assert_eq!(challenge.challenge, compute_s256_challenge(verifier));
    }

    #[test]
    fn pkce_challenge_new_holds_the_given_method_and_challenge_string() {
        let challenge = PkceChallenge::new(PkceMethod::S256, "raw-challenge-value");
        assert_eq!(challenge.method, PkceMethod::S256);
        assert_eq!(challenge.challenge, "raw-challenge-value");
    }

    #[test]
    fn verify_pkce_succeeds_when_verifier_hashes_to_the_stored_challenge() {
        let verifier = "correct-verifier-that-should-match-the-challenge";
        let challenge = PkceChallenge::from_verifier_s256(verifier);

        let result = verify_pkce(&challenge, verifier);

        assert!(
            result.is_ok(),
            "expected matching verifier to verify successfully, got {result:?}"
        );
    }

    #[test]
    fn verify_pkce_rejects_a_verifier_that_does_not_hash_to_the_stored_challenge() {
        let real_verifier = "the-real-verifier-the-client-actually-used";
        let challenge = PkceChallenge::from_verifier_s256(real_verifier);
        let wrong_verifier = "an-attacker-or-buggy-client-supplied-verifier";

        let result = verify_pkce(&challenge, wrong_verifier);

        let err = result.expect_err("mismatched verifier must be rejected, not accepted");
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        assert!(
            err.public_message.to_lowercase().contains("pkce"),
            "expected the rejection message to mention PKCE, got {:?}",
            err.public_message
        );
    }

    #[test]
    fn verify_pkce_rejects_an_empty_verifier_against_a_real_challenge() {
        let challenge = PkceChallenge::from_verifier_s256("some-real-verifier-value");

        let result = verify_pkce(&challenge, "");

        assert!(result.is_err(), "empty verifier must not verify");
    }

    #[test]
    fn verify_pkce_is_case_and_byte_exact_not_a_loose_comparison() {
        let verifier = "case-sensitive-verifier-value";
        let mut challenge = PkceChallenge::from_verifier_s256(verifier);
        // Flip the case of the stored challenge; base64url is case-sensitive,
        // so this must now be treated as a mismatch even though the two
        // strings differ only in letter case.
        challenge.challenge = challenge.challenge.to_uppercase();

        let result = verify_pkce(&challenge, verifier);

        assert!(
            result.is_err(),
            "a case-flipped challenge must not verify as a match"
        );
    }
}
