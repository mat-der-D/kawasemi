//! `Digest` (design.md "Components and Interfaces" -> `Digest` row: "本文
//! ダイジェスト算出・検証"; Requirements 1.3, 2.5; task 1.4, `Boundary:
//! Digest`): SHA-256 body digest computation for outgoing requests and
//! mismatch detection for incoming ones.
//!
//! Scope: this module owns only the digest primitive itself — computing a
//! SHA-256 digest over a request/response body, rendering it in the HTTP
//! `Digest` header's conventional `SHA-256=<base64>` form (RFC 3230 /
//! draft-cavage's `Digest` header convention, the shape Mastodon-style
//! ActivityPub implementations also use), parsing that form back, and
//! comparing two digests. It does not decide *when* a digest is computed or
//! where it is placed in the signing input — that is `RequestSigner`'s job
//! (task 1.5, Requirement 1.3: "本文のダイジェストを算出して署名対象に含める")
//! on the sending side, and `SignatureVerifier`'s job (task 2.3, Requirement
//! 2.5: "受信本文のダイジェストが署名対象のダイジェストと一致することを検証
//! する") on the receiving side. Both later tasks are expected to build on
//! [`Digest::compute`] / [`Digest::verify`] rather than reimplementing
//! digest math.
//!
//! ## Why SHA-256 only
//! design.md's Technology Stack row pins "SHA-256 Digest" outright (no
//! algorithm negotiation), matching Requirement 1.3/2.5's plain "ダイジェス
//! ト" (no algorithm choice mentioned) and Mastodon-style ActivityPub's
//! de facto convention. [`Digest::from_header_value`] therefore rejects any
//! `Digest` header value not prefixed `SHA-256=` rather than trying to
//! support multiple algorithms.
//!
//! ## Comparison discipline
//! [`Digest::verify`] compares digest bytes via [`subtle::ConstantTimeEq`],
//! mirroring `src/oauth/hash.rs`'s established rationale ("this crate's
//! established comparison discipline for anything hash-shaped and
//! security-adjacent"). A body digest is not itself secret (it travels in a
//! plaintext HTTP header), but it participates in the HTTP Signatures trust
//! chain, so this module holds itself to the same discipline as
//! `verify_keyed_hash` rather than special-casing a `==` comparison here.

#[cfg(test)]
mod tests;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use sha2::Digest as _;
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::error::AppError;
use axum::http::StatusCode;

/// The only digest algorithm this module computes or accepts (see this
/// module's doc comment, "Why SHA-256 only").
const ALGORITHM_PREFIX: &str = "SHA-256=";

/// A SHA-256 digest of an HTTP request/response body (Requirements 1.3,
/// 2.5). Opaque: the only ways to produce one are [`Digest::compute`] (hash
/// a body) and [`Digest::from_header_value`] (parse a received `Digest`
/// header), and the only ways to consume one are [`Digest::header_value`]
/// (render for an outgoing `Digest` header) and [`Digest::verify`] (compare
/// against another digest).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Digest(Vec<u8>);

impl Digest {
    /// Computes the SHA-256 digest of `body` (Requirement 1.3's "本文のダイ
    /// ジェストを算出"). Deterministic: the same bytes always produce the
    /// same `Digest`.
    pub fn compute(body: &[u8]) -> Self {
        Digest(Sha256::digest(body).to_vec())
    }

    /// Renders this digest in the HTTP `Digest` header's conventional form,
    /// `SHA-256=<base64>` (standard base64 with padding), suitable for
    /// `RequestSigner` (task 1.5) to place in an outgoing request's `Digest`
    /// header.
    pub fn header_value(&self) -> String {
        format!("{ALGORITHM_PREFIX}{}", BASE64_STANDARD.encode(&self.0))
    }

    /// Parses a received `Digest` header value of the form
    /// `SHA-256=<base64>` back into a [`Digest`], for `SignatureVerifier`
    /// (task 2.3) to compare against the digest it computes from the
    /// received body via [`Digest::verify`].
    ///
    /// Returns a caller-facing [`AppError`] (`400 Bad Request`) if `value`
    /// does not carry the `SHA-256=` prefix this module requires (see this
    /// module's doc comment, "Why SHA-256 only") or if the trailing base64
    /// payload does not decode.
    pub fn from_header_value(value: &str) -> Result<Self, AppError> {
        let encoded = value.strip_prefix(ALGORITHM_PREFIX).ok_or_else(|| {
            AppError::client(
                StatusCode::BAD_REQUEST,
                format!(
                    "unsupported Digest header algorithm, expected {ALGORITHM_PREFIX:?}: {value:?}"
                ),
            )
        })?;
        let bytes = BASE64_STANDARD.decode(encoded).map_err(|source| {
            AppError::client(
                StatusCode::BAD_REQUEST,
                format!("invalid base64 in Digest header value: {source}"),
            )
        })?;
        Ok(Digest(bytes))
    }

    /// Verifies this digest matches `expected`, detecting a mismatch
    /// between a locally computed digest and one asserted by a signer
    /// (Requirement 2.5's "受信本文のダイジェストが署名対象のダイジェストと
    /// 一致することを検証する"). Compares digest bytes in constant time (see
    /// this module's doc comment, "Comparison discipline").
    ///
    /// Returns a caller-facing [`AppError`] (`400 Bad Request`) on mismatch;
    /// `Ok(())` when the two digests are byte-for-byte equal.
    pub fn verify(&self, expected: &Digest) -> Result<(), AppError> {
        let matches: bool = self.0.ct_eq(&expected.0).into();
        if matches {
            Ok(())
        } else {
            Err(AppError::client(
                StatusCode::BAD_REQUEST,
                "body digest does not match the expected digest",
            ))
        }
    }
}
