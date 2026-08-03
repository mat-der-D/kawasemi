//! `SignatureVerifier` (design.md `#### SignatureVerifier` -> Service
//! Interface; Requirements 2.1, 2.2, 2.5, 2.6, 7.1; task 2.3, `Boundary:
//! SignatureVerifier`): verifies a received HTTP Signature (draft-cavage or
//! RFC 9421) end-to-end — format detection, signing-input reconstruction,
//! public-key resolution, body-digest verification, and the actual
//! RSA-SHA256 cryptographic check — returning the verified signer's
//! identity on success.
//!
//! ## Scope
//! This module owns exactly [`SignatureVerifier::verify_request`] and its
//! concrete implementation [`HttpSignatureVerifier`]. It composes
//! already-implemented boundaries — [`super::suite::SignatureSuite`] (task
//! 1.5, format detection/signing-input reconstruction), [`super::digest::Digest`]
//! (task 1.4, body-digest comparison), and [`super::key_resolver::PublicKeyResolver`]
//! (task 2.1, keyId -> public-key resolution with cache) — rather than
//! reimplementing any of them. It does not implement `SignatureNegotiator`
//! (task 3.x) or any inbox/dispatch wiring (`InboxService`, task 4.1) — this
//! module's only postcondition is "verified signer identity, or a uniform
//! authentication failure", exactly as design.md's `Postconditions: 成功時に
//! 署名者アクター URI を返す` states.
//!
//! ## `IncomingRequest`: not defined anywhere else in this spec
//! design.md references `IncomingRequest` (this trait's own parameter, and
//! the future `InboxService::process_inbound(req: IncomingRequest)`) but
//! never defines its fields. Mirroring `http_client.rs`'s `OutboundRequest`
//! (this task's own File Structure Plan boundary is `verifier.rs`, so this
//! type belongs here — later tasks, e.g. `InboxService`, are expected to
//! reuse this definition via re-export, not redefine it), [`IncomingRequest`]
//! carries exactly what this verifier needs to read: `method`, the full
//! request URL, `headers`, and an optional `body` — no more.
//!
//! ## Covered-components mismatch: treated as verification failure
//! (tasks.md's 1.5 Implementation Note flags this exact task's open
//! question.) [`super::suite::SignatureSuite::build_signing_input`]'s
//! covered header/component set is fixed internally per suite — it is not
//! driven by whatever the received signature's `headers=`/component-list
//! parameter claims. So after parsing a received signature into a
//! [`super::suite::ParsedSignature`], this verifier independently computes
//! what the suite *would* cover for `req`'s actual headers (via
//! `build_signing_input` on a [`super::suite::SignableRequest`] built from
//! `req`) and compares that computed `covered_components` against
//! `parsed.covered_components` **as an ordered sequence** — not just as a
//! set. Order matters here because both suites embed the covered-component
//! list directly into the signing string (draft-cavage's `headers="..."`
//! line ordering is exactly the order components appear in the string;
//! RFC 9421's `@signature-params` line embeds the component list verbatim),
//! so any reordering would mean the signing string this verifier
//! reconstructs is *not* the string an honest sender actually signed. When
//! they differ, this verifier cannot faithfully reconstruct what the
//! sender claims to have signed, and treats the received signature as
//! malformed — the same failure bucket as Requirement 2.6's "不正" — rather
//! than silently accepting a partial match or crashing. In practice this
//! also generally cannot diverge from the RSA verify's own crypto failure
//! (a reordered signing string would not match a real signature either)
//! but is checked explicitly first for a clear, documented failure reason
//! rather than relying on that as an accidental side effect.
//!
//! ## Staleness window: no numeric default is specified upstream
//! requirements.md/design.md name "期限切れ" (expired) as a verification
//! failure (Requirement 2.6) but pin no numeric window. Mirroring task 2.1's
//! `DEFAULT_PUBLIC_KEY_CACHE_TTL` pattern (a named constant plus a
//! constructor parameter, not a hardwired value), [`DEFAULT_SIGNATURE_MAX_AGE`]
//! documents this module's chosen default: **1 hour**. ActivityPub delivery
//! between independently operated instances has no NTP guarantee, and
//! delivery/retry queues (this spec's own future `DeliveryWorker`, task
//! 11.x) can legitimately delay an already-signed request past the couple
//! of minutes some HTTP Signatures deployments use for pure replay
//! protection; 1 hour sits in the "several minutes to low single-digit
//! hours" range that tolerates realistic clock drift and short delivery
//! delays without turning this check into a no-op. [`HttpSignatureVerifier::new`]
//! takes this as a constructor parameter (not a hardcoded literal) so a
//! later bootstrap-wiring task (5.4, same as 2.1's TTL) can wire it to
//! config instead.
//!
//! The `Date` header itself is required unconditionally by this verifier
//! (independent of whether the received signature's declared covered
//! components happen to include `date`): with no `Date` header there is no
//! temporal anchor at all to judge "期限切れ" against, so a request with a
//! present-but-unparsable or entirely absent `Date` header is rejected the
//! same as an expired one (bucketed with "不正" per Requirement 2.6).
//!
//! ## Verification-failure retry: invalidate cache + refetch exactly once
//! Requirement 2.6 combined with design.md's "検証失敗時は公開鍵キャッシュ
//! 無効化＋再取得を一度試みる" (key-rotation tolerance): this verifier first
//! resolves the signer's public key with `force: false` (cache-preferring).
//! If the RSA-SHA256 check against that key fails — specifically a crypto
//! verify failure, not the earlier resolve call itself erroring out — this
//! verifier resolves again with `force: true` (which both invalidates and
//! refetches in one call, per [`super::key_resolver::PublicKeyResolver`]'s
//! own documented contract: "force=true always fetches over the network
//! ... and overwrites the cache") and retries the crypto check exactly once
//! against the freshly fetched key. A failure of the *first*
//! `resolve_public_key` call itself (network/DB failure resolving the key
//! at all) is a distinct failure mode — "公開鍵が取得できない" — and is
//! rejected immediately without a retry (there is nothing a repeated
//! network call under the exact same conditions would fix, and no crypto
//! verify happened yet to indicate cache staleness specifically).
//!
//! ## RSA verification: same hand-built `Pkcs1v15Sign` as `signer.rs`
//! `signer.rs`'s own doc comment ("Algorithm: RSA-SHA256 / PKCS#1 v1.5
//! only, via a hand-built `Pkcs1v15Sign`") documents why
//! `rsa::Pkcs1v15Sign::new::<Sha256>()` cannot be used in this workspace
//! (`rsa` 0.9.10 requires `digest` 0.10.x; this workspace's direct `sha2 =
//! "0.11.0"` dependency implements `digest` 0.11.x). `signer.rs`'s
//! `sha256_pkcs1v15_padding`/`SHA256_PKCS1V15_PREFIX` are not `pub` (module-
//! private), so this module cannot import them; the exact same
//! already-verified 19-byte SHA-256 `DigestInfo` prefix and padding
//! construction are duplicated here verbatim rather than re-derived, with
//! this comment as the pointer back to that precedent and its verification
//! story. `rsa::RsaPublicKey::verify` takes the identical `Pkcs1v15Sign`
//! padding-scheme value `rsa::RsaPrivateKey::sign` used to produce the
//! signature, so the same construction is correct on this, the verifying,
//! side too.
//!
//! ## Uniform 401-equivalent failure, no internal detail leaked
//! Every failure path in [`HttpSignatureVerifier::verify_request`] —
//! missing/undetectable signature, malformed signature, covered-components
//! mismatch, missing/malformed/expired `Date`, missing/mismatched `Digest`,
//! key-fetch failure, or a crypto verify failure surviving the one retry —
//! returns the same [`AppError::client`] with `StatusCode::UNAUTHORIZED`
//! and a fixed, non-specific public message (design.md's Error Handling
//! section, "認証失敗（401 相当）...本文に内部詳細を出さない"). This is a
//! deliberate security property, not an oversight: telling a caller
//! *which* check failed (bad digest vs. unknown key vs. expired signature)
//! would let an attacker use this endpoint as an oracle to probe for the
//! specific weakness to exploit next.

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use rsa::pkcs8::DecodePublicKey;
use rsa::{Pkcs1v15Sign, RsaPublicKey};
use sha2::{Digest as _, Sha256};
use time::macros::format_description;
use time::{Duration, OffsetDateTime, PrimitiveDateTime};

use super::digest::Digest as BodyDigest;
use super::key_resolver::PublicKeyResolver;
use super::suite::{
    DraftCavageSuite, RequestHeaders, Rfc9421Suite, SignableRequest, SignatureFormat,
    SignatureSuite,
};
use crate::error::AppError;
use crate::runtime::Clock;

/// Documented default staleness window for a received signature's `Date`
/// header (see this module's doc comment, "Staleness window"). Not applied
/// automatically anywhere but this module's own constructor default choice
/// — [`HttpSignatureVerifier::new`] takes `max_signature_age` as an
/// explicit parameter (mirroring task 2.1's `DEFAULT_PUBLIC_KEY_CACHE_TTL`
/// pattern) so a later bootstrap-wiring task can source it from config.
pub const DEFAULT_SIGNATURE_MAX_AGE: Duration = Duration::hours(1);

/// A received HTTP request this verifier needs to check the signature of
/// (design.md references this type via `SignatureVerifier::verify_request`
/// and the future `InboxService::process_inbound`, but never defines its
/// fields anywhere in this spec — see this module's doc comment,
/// "`IncomingRequest`: not defined anywhere else in this spec"). Mirrors
/// [`super::http_client::OutboundRequest`]'s shape for the receiving side.
#[derive(Debug, Clone)]
pub struct IncomingRequest {
    pub method: Method,
    /// Full absolute request URL as received, e.g.
    /// `"https://kawasemi.example/inbox"` — matches
    /// [`SignableRequest::url`]'s shape so it can be passed straight
    /// through when reconstructing the signing input.
    pub url: String,
    pub headers: RequestHeaders,
    /// Absent for a bodyless request; present for a request carrying a
    /// JSON-LD Activity body, whose `Digest` header this verifier checks
    /// against (Requirement 2.5) when set.
    pub body: Option<Vec<u8>>,
}

impl IncomingRequest {
    /// Builds a bodyless `IncomingRequest` with no headers set yet.
    pub fn new(method: Method, url: impl Into<String>) -> Self {
        Self {
            method,
            url: url.into(),
            headers: RequestHeaders::new(),
            body: None,
        }
    }

    /// Attaches a body to this request (builder-style).
    pub fn with_body(mut self, body: Vec<u8>) -> Self {
        self.body = Some(body);
        self
    }
}

/// The verified signer's identity (design.md's exact `VerifiedSigner`
/// interface): the `keyId` the signature claimed, and the actor URI the
/// resolved public key belongs to — the identity to hand to block-judgment
/// and dispatch (Requirement 7.1's "署名を検証してから受理する", satisfied
/// by a downstream `InboxService`, task 4.1, calling this verifier first).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedSigner {
    pub key_id: String,
    pub actor_uri: String,
}

/// Verifies a received HTTP Signature end-to-end (design.md's
/// `SignatureVerifier` Service Interface; Requirements 2.1, 2.2, 2.5, 2.6,
/// 7.1). See this module's doc comment for the full pipeline and every
/// documented design decision.
///
/// `#[allow(async_fn_in_trait)]`: mirrors `FederationHttpClient`'s and
/// `PublicKeyResolver`'s own documented rationale (`http_client.rs`,
/// `key_resolver.rs`) — design.md pins this method as literal `async fn`
/// (`verify_request` internally awaits `PublicKeyResolver::resolve_public_key`,
/// itself the same kind of native `async fn`), so this keeps that exact
/// syntax. `Arc<dyn SignatureVerifier>`/`Send`-pinning concerns belong to
/// whichever future task actually needs dynamic dispatch across this trait
/// (e.g. `InboxService`, task 4.1, or the authorized-fetch GET path,
/// Requirement 6.4) — both out of this task's boundary.
#[allow(async_fn_in_trait)]
pub trait SignatureVerifier: Send + Sync {
    /// Verifies `req`'s HTTP Signature (either format), returning the
    /// verified signer's identity on success. Every failure path returns
    /// the same [`StatusCode::UNAUTHORIZED`] [`AppError`] with no
    /// internal-detail-carrying message (see this module's doc comment,
    /// "Uniform 401-equivalent failure").
    async fn verify_request(&self, req: &IncomingRequest) -> Result<VerifiedSigner, AppError>;
}

/// The fixed, algorithm-defined 19-byte `DigestInfo` prefix RFC 8017's
/// `EMSA-PKCS1-v1_5` encoding uses for SHA-256. Duplicated verbatim from
/// `signer.rs`'s own `SHA256_PKCS1V15_PREFIX` (not `pub` there, so it
/// cannot be imported) — see this module's doc comment, "RSA verification:
/// same hand-built `Pkcs1v15Sign` as `signer.rs`", for why this exact byte
/// sequence is correct and how it was originally verified.
const SHA256_PKCS1V15_PREFIX: [u8; 19] = [
    0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01, 0x05,
    0x00, 0x04, 0x20,
];

/// Builds the `Pkcs1v15Sign` padding scheme for RSASSA-PKCS1-v1_5 with
/// SHA-256, mirroring `signer.rs`'s own `sha256_pkcs1v15_padding` (see this
/// module's doc comment for why this is duplicated rather than imported).
fn sha256_pkcs1v15_padding() -> Pkcs1v15Sign {
    Pkcs1v15Sign {
        hash_len: Some(32),
        prefix: SHA256_PKCS1V15_PREFIX.to_vec().into_boxed_slice(),
    }
}

/// HTTP-date (RFC 9110 §5.6.7, IMF-fixdate) format description, mirroring
/// `signer.rs`'s own `HTTP_DATE_FORMAT` verbatim (duplicated for the same
/// module-privacy reason as [`SHA256_PKCS1V15_PREFIX`] above) — used here
/// to *parse* a received `Date` header rather than format an outgoing one.
const HTTP_DATE_FORMAT: &[time::format_description::BorrowedFormatItem<'_>] = format_description!(
    "[weekday repr:short], [day padding:zero] [month repr:short] [year] [hour]:[minute]:[second] GMT"
);

/// Reads a header's value as `&str` (case-insensitive by `HeaderMap`
/// lookup), or `None` if absent or not valid UTF-8/ASCII. Mirrors
/// `suite.rs`'s own private `header_str` helper.
fn header_str<'a>(headers: &'a RequestHeaders, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

/// Parses an HTTP-date (IMF-fixdate) `Date` header value, e.g. `"Sun, 06
/// Nov 1994 08:49:37 GMT"`, into an [`OffsetDateTime`] (assumed UTC — the
/// format's only valid zone designator is the literal `GMT`). Returns
/// `None` on any parse failure; this verifier treats an unparsable `Date`
/// the same as a missing one (see this module's doc comment, "Staleness
/// window").
fn parse_http_date(value: &str) -> Option<OffsetDateTime> {
    PrimitiveDateTime::parse(value, HTTP_DATE_FORMAT)
        .ok()
        .map(PrimitiveDateTime::assume_utc)
}

/// Builds the uniform, no-internal-detail authentication-failure error
/// every failure path in [`HttpSignatureVerifier::verify_request`] returns
/// (see this module's doc comment, "Uniform 401-equivalent failure").
fn verification_failed() -> AppError {
    AppError::client(StatusCode::UNAUTHORIZED, "signature verification failed")
}

/// Attempts an RSA-SHA256/PKCS#1v1.5 verification of `signature` over
/// `hashed` (an already-SHA-256-hashed signing string) against the public
/// key encoded in `public_key_pem` (SPKI/PEM, [`super::key_resolver::RemotePublicKey::public_key_pem`]'s
/// shape). Returns `false` for a malformed PEM just as readily as for a
/// genuine signature mismatch — from this verifier's perspective both mean
/// "this key does not authenticate this signature", the same bucket that
/// triggers the cache-invalidate-and-retry path.
fn crypto_verify(public_key_pem: &str, hashed: &[u8], signature: &[u8]) -> bool {
    let Ok(public_key) = RsaPublicKey::from_public_key_pem(public_key_pem) else {
        return false;
    };
    public_key
        .verify(sha256_pkcs1v15_padding(), hashed, signature)
        .is_ok()
}

/// Concrete [`SignatureVerifier`] implementation, composing
/// [`super::suite::SignatureSuite`] (format detection/signing-input
/// reconstruction), [`super::digest::Digest`] (body-digest comparison),
/// and a [`PublicKeyResolver`] (keyId resolution with cache/retry) into the
/// full verification pipeline documented in this module's doc comment.
///
/// Generic over `R: PublicKeyResolver` (held as `Arc<R>`), not `Arc<dyn
/// PublicKeyResolver>`: [`PublicKeyResolver::resolve_public_key`] is a
/// literal `async fn` (its own module's documented `#[allow(async_fn_in_trait)]`
/// rationale explicitly flags this exact task as where the dyn-incompatibility
/// would otherwise resurface — `key_resolver.rs`: "`Arc<dyn
/// PublicKeyResolver>` が必要になる 2.3 側で同じ問題が再発する点に留意"), so
/// a trait object here would fail to compile with E0038 exactly as
/// `Arc<dyn FederationHttpClient>` did for task 2.1. A generic parameter
/// avoids that while still letting any [`PublicKeyResolver`] implementation
/// (production `DbFederationPublicKeyResolver` or a deterministic test
/// mock) be substituted at the call site (Requirement 2.7's mockable-
/// boundary intent, satisfied here via monomorphization).
pub struct HttpSignatureVerifier<R: PublicKeyResolver> {
    resolver: Arc<R>,
    clock: Arc<dyn Clock>,
    max_signature_age: Duration,
}

impl<R: PublicKeyResolver> HttpSignatureVerifier<R> {
    /// Builds a verifier against `resolver` (the keyId -> public-key
    /// resolution boundary, with its own cache/force semantics),
    /// `clock` (the `Date`-freshness judgment's non-determinism boundary —
    /// never wall-clock time directly, per steering's DI convention), and
    /// `max_signature_age` (the staleness window — see this module's doc
    /// comment, "Staleness window"; pass [`DEFAULT_SIGNATURE_MAX_AGE`] for
    /// the documented default).
    pub fn new(resolver: Arc<R>, clock: Arc<dyn Clock>, max_signature_age: Duration) -> Self {
        Self {
            resolver,
            clock,
            max_signature_age,
        }
    }

    /// Picks the concrete [`SignatureSuite`] for `format` (both suites are
    /// zero-sized and stateless, so this is a cheap `Box` allocation per
    /// call — mirrors `signer.rs`'s own `match format { ... }` suite
    /// selection).
    fn suite_for(format: SignatureFormat) -> Box<dyn SignatureSuite> {
        match format {
            SignatureFormat::DraftCavage => Box::new(DraftCavageSuite::new()),
            SignatureFormat::Rfc9421 => Box::new(Rfc9421Suite::new()),
        }
    }
}

impl<R: PublicKeyResolver> SignatureVerifier for HttpSignatureVerifier<R> {
    /// See this module's doc comment for the full pipeline (format
    /// detection -> parse -> covered-components validation -> `Date`
    /// freshness -> `Digest` match -> public-key resolution -> RSA verify
    /// with one invalidate-and-retry on crypto failure).
    async fn verify_request(&self, req: &IncomingRequest) -> Result<VerifiedSigner, AppError> {
        // 1. Format detection (Requirement 2.6: no detectable signature at
        // all is a "missing signature" verification failure). `detect` is
        // an associated fn, identical across both suites (both delegate to
        // the same shared implementation) -- calling it via either
        // concrete type is equivalent.
        let format = DraftCavageSuite::detect(&req.headers).ok_or_else(verification_failed)?;
        let suite = Self::suite_for(format);

        // 2. Parse (Requirement 2.1, 2.2: malformed signature headers for
        // the detected format is a verification failure).
        let parsed = suite
            .parse(&req.headers)
            .map_err(|_| verification_failed())?;

        // 3. Rebuild what the suite would actually cover for `req`'s
        // headers, and compare against what the sender declared. See this
        // module's doc comment, "Covered-components mismatch".
        let signable = SignableRequest {
            method: req.method.clone(),
            url: req.url.clone(),
            key_id: parsed.key_id.clone(),
            headers: req.headers.clone(),
        };
        let signing_input = suite.build_signing_input(&signable);
        if signing_input.covered_components != parsed.covered_components {
            return Err(verification_failed());
        }

        // 4. `Date` freshness (Requirement 2.6: "期限切れ"). Required
        // unconditionally -- see this module's doc comment.
        let date_value = header_str(&req.headers, "date").ok_or_else(verification_failed)?;
        let signed_at = parse_http_date(date_value).ok_or_else(verification_failed)?;
        let age = (self.clock.now() - signed_at).abs();
        if age > self.max_signature_age {
            return Err(verification_failed());
        }

        // 5. Body digest match (Requirement 2.5), only when a body was
        // received.
        if let Some(body) = req.body.as_deref() {
            let digest_header =
                header_str(&req.headers, "digest").ok_or_else(verification_failed)?;
            let received_digest =
                BodyDigest::from_header_value(digest_header).map_err(|_| verification_failed())?;
            BodyDigest::compute(body)
                .verify(&received_digest)
                .map_err(|_| verification_failed())?;
        }

        // 6/7. Resolve the signer's public key (cache-preferring), verify,
        // and on a genuine crypto failure invalidate + refetch once. See
        // this module's doc comment, "Verification-failure retry".
        let hashed = Sha256::digest(signing_input.signing_string.as_bytes());

        let first_key = self
            .resolver
            .resolve_public_key(&parsed.key_id, false)
            .await
            .map_err(|_| verification_failed())?;
        if crypto_verify(
            &first_key.public_key_pem,
            hashed.as_slice(),
            &parsed.signature,
        ) {
            return Ok(VerifiedSigner {
                key_id: parsed.key_id,
                actor_uri: first_key.actor_uri,
            });
        }

        let refreshed_key = self
            .resolver
            .resolve_public_key(&parsed.key_id, true)
            .await
            .map_err(|_| verification_failed())?;
        if !crypto_verify(
            &refreshed_key.public_key_pem,
            hashed.as_slice(),
            &parsed.signature,
        ) {
            return Err(verification_failed());
        }

        // 8. Success (Requirement 2.1's postcondition: the verified
        // signer's actor URI).
        Ok(VerifiedSigner {
            key_id: parsed.key_id,
            actor_uri: refreshed_key.actor_uri,
        })
    }
}
