//! `RequestSigner` (design.md "Crypto / 署名層" -> `RequestSigner` Service
//! Interface; Requirements 1.1, 1.2, 1.3, 1.5; task 2.2, `Boundary:
//! RequestSigner`): attaches an HTTP Signature (draft-cavage or RFC 9421) to
//! an outbound request, using the currently valid signing key of a local
//! actor.
//!
//! ## Scope
//! This module owns exactly [`RequestSigner::sign_request`]: resolving
//! `actor: &Handle` to its signing key, setting `keyId` (Requirement 1.2),
//! including a body digest when present (Requirement 1.3), building the
//! signing input via the already-implemented [`super::suite::SignatureSuite`]
//! (task 1.5) for whichever [`SignatureFormat`] the caller asks for
//! (Requirement 1.4, already satisfied by that module — this component only
//! selects the right suite), performing the actual RSA-SHA256 signature
//! computation (not this crate's responsibility anywhere else yet), and
//! aborting with an [`AppError`] — without mutating `req` at all — if no
//! usable key is available (Requirement 1.5). It does not implement
//! `SignatureVerifier` (task 2.3) or `SignatureNegotiator` (task 3.x/3.4),
//! both out of this task's boundary.
//!
//! ## `Handle -> Id` resolution: `ActorDirectory` is a real dependency here
//! design.md's Components table lists this component's Key Dependencies as
//! only `SignatureSuite, SigningKeyProvider, ActorUrls` — no
//! [`crate::actor::ActorDirectory`]. But
//! [`crate::runtime::signing_key::SigningKeyProvider::signing_key`] takes a
//! [`crate::runtime::signing_key::KeyRef`] (an actor's numeric
//! [`crate::domain::Id`]), while design.md's own pinned
//! `sign_request(&self, actor: &Handle, ...)` signature only ever hands this
//! component a bare [`Handle`] (a local username, no `Id`). Nothing else in
//! this spec resolves `Handle -> Id`, and `ActorDirectory` (already
//! implemented, reviewed, and depended upon by this same spec's later
//! `RecipientTargetResolver`, task 3.4, whose own Key Dependencies row *does*
//! list `ActorDirectory, ActorUrls`) is exactly the owner-free reference
//! boundary requirements.md's "Adjacent expectations" section names as this
//! spec's dependency on actor-model ("actor-model が提供するハンドル解決
//! （オーナー非露出のアクター解決）...に依存し"). This component therefore
//! calls [`crate::actor::ActorDirectory::resolve_actor_by_handle`] to turn
//! `actor` into a [`crate::domain::Id`] before building a
//! [`crate::runtime::signing_key::KeyRef`] — the absence of `ActorDirectory`
//! from design.md's dependency row reads as an incomplete table entry, not a
//! deliberate exclusion, since no alternative `Handle -> Id` path exists
//! anywhere in this spec. A resolution failure (`Ok(None)`, i.e. `actor`
//! names no local actor at all) is treated identically to Requirement 1.5's
//! "有効な署名鍵が取得できない" case: signing aborts with an [`AppError`],
//! `req` is left untouched.
//!
//! ## Why `sign_request` is `async fn` (a narrow, documented deviation from design.md's literal signature)
//! design.md pins `pub fn sign_request(&self, actor: &Handle, format:
//! SignatureFormat, req: &mut OutboundRequest) -> Result<(), AppError>` —
//! synchronous. But [`ActorDirectory::resolve_actor_by_handle`] (see above)
//! is `async fn`, and there is no synchronous way to call it. Two shapes
//! were possible: (a) make this method `async fn` too, narrowly deviating
//! from design.md's literal signature while staying faithful to its
//! Responsibilities prose ("署名鍵を...取得し" — key *retrieval*, including
//! the `Handle -> Id` step it implies, is this component's own job); or (b)
//! accept an already-resolved `Id`/`KeyRef` instead of `Handle`, pushing
//! resolution onto a future caller. This module takes (a): every other
//! Crypto-layer async operation this spec defines
//! ([`super::key_resolver::PublicKeyResolver::resolve_public_key`], and the
//! future `SignatureVerifier::verify_request`) is already `async fn` per
//! design.md's own pinned interfaces, so a lone synchronous outlier here
//! would be the surprising choice, not the safe one — and this task's own
//! brief explicitly prefers (a) when uncertain, for the same reason. No
//! actual call site in this task's boundary needs a synchronous signer: the
//! only documented future caller, `DeliveryWorker` (task 11.x, out of this
//! task's boundary), is itself async-heavy throughout design.md's System
//! Flows.
//!
//! ## Algorithm: RSA-SHA256 / PKCS#1 v1.5 only, via a hand-built `Pkcs1v15Sign`
//! [`super::suite::SignatureSuite`]'s doc comment already fixes this spec's
//! algorithm choice (`rsa-sha256` / `rsa-v1_5-sha256` — RSASSA-PKCS1-v1_5
//! with SHA-256, design.md's Technology Stack row: "RSA-2048"). Producing
//! that signature means RSA-signing a **DigestInfo-prefixed** SHA-256 hash
//! (RFC 8017 §9.2's `EMSA-PKCS1-v1_5` scheme) via the `rsa` crate's
//! [`rsa::Pkcs1v15Sign`] padding scheme and [`rsa::RsaPrivateKey::sign`].
//!
//! `rsa::Pkcs1v15Sign::new::<D>()` (the crate's own convenience constructor
//! for a given digest type `D`) requires `D: digest::Digest +
//! pkcs8::AssociatedOid`, where that `digest` crate is the one `rsa` 0.9.10
//! itself depends on: **`digest` 0.10.x**. This workspace's direct `sha2 =
//! "0.11.0"` dependency (`Cargo.toml`), however, implements the *newer*
//! **`digest` 0.11.x** line's traits — `digest` 0.10 and 0.11 are separate,
//! source-incompatible major versions (mirroring
//! `src/actor/keys/material.rs`'s own documented `rand_core` 0.6-vs-0.10
//! precedent for the exact same class of transitive-version mismatch), so
//! `sha2::Sha256` (0.11.0) does not satisfy `rsa::Pkcs1v15Sign::new::<D>()`'s
//! bound and the crate does not compile if that path is used.
//!
//! Rather than adding a second, differently-versioned `sha2`/`digest`
//! dependency to `Cargo.toml` just to satisfy that one generic bound, this
//! module builds [`rsa::Pkcs1v15Sign`] directly from its two public fields
//! (`hash_len: Option<usize>`, `prefix: Box<[u8]>`) — see
//! [`sha256_pkcs1v15_padding`] — using the literal, algorithm-fixed 19-byte
//! `DigestInfo` prefix RFC 8017's `EMSA-PKCS1-v1_5` encoding defines for
//! SHA-256 (`AlgorithmIdentifier` for OID `2.16.840.1.101.3.4.2.1`, i.e.
//! `id-sha256`, plus the `OCTET STRING` header for a 32-byte digest to
//! follow). This is not a guessed/recalled constant: it was verified in
//! this environment by building a throwaway example against the real `rsa`
//! 0.9.10 + `digest`/`sha2` **0.10** (the `digest`-0.10-compatible line, with
//! `sha2`'s `oid` feature enabled) and printing
//! `rsa::Pkcs1v15Sign::new::<sha2_0_10::Sha256>().prefix` directly —
//! confirming both the exact byte sequence below and `hash_len == 32`
//! against the crate's own generator, not a recollected/external reference.
//! The hash itself is computed via the workspace's existing `sha2::Sha256`
//! (0.11, `sha2::Digest::digest`, mirroring `super::digest::Digest::compute`'s
//! own import style) — only `Pkcs1v15Sign::new::<D>()`'s *generic
//! constructor* has the digest-0.10 bound; `rsa::RsaPrivateKey::sign`'s
//! actual signing call takes a plain `&[u8]` hash and a `Pkcs1v15Sign` value,
//! neither of which cares which digest-crate lineage produced the hash
//! bytes.
//!
//! ## `req` is left untouched on any error path
//! `sign_request` performs every fallible step (actor resolution, key
//! lookup, PEM parsing, header-value construction, signing-input
//! construction, RSA signing, and format-specific header assembly) against
//! local values first, and only writes to `req.headers` once every one of
//! those steps has already succeeded — see [`sign_request`](RequestSigner::sign_request)'s
//! body. This is a strictly stronger guarantee than Requirement 1.5 asks
//! for (which only requires no mutation on the "no valid key" path
//! specifically), chosen because it costs nothing extra here and makes
//! "signing either fully succeeds or `req` is exactly as the caller left
//! it" true unconditionally, not just for that one failure mode.
//!
//! ## `Host`/`Date`/`Digest` headers: this component sets them, not the caller
//! [`super::suite::SignatureSuite::build_signing_input`] reads
//! `Host`/`Date`/`Digest` off `SignableRequest::headers` verbatim — it never
//! computes them (that module's own doc comment: "this suite reads header
//! values verbatim, it never hashes a body itself"). Per design.md's
//! Responsibilities for this component ("本文ありなら `Digest` を署名対象に
//! 含める"), `RequestSigner` is the component that actually computes and
//! sets these three headers (Requirement 1.3's digest, plus the
//! `Host`/`Date` values every HTTP Signatures peer conventionally covers),
//! rather than requiring every future caller to pre-populate them before
//! calling `sign_request`.
//!
//! ## `Date` header: HTTP-date (IMF-fixdate), from the injected `Clock`
//! Per steering's non-determinism DI boundary (`.kiro/steering/tech.md`:
//! "時刻...は注入可能（DI）にする"), the `Date` header's value comes from
//! `self.clock.now()` (a [`crate::runtime::Clock`]), never
//! `OffsetDateTime::now_utc()` directly. `time::format_description::well_known::Rfc2822`
//! is not quite HTTP-date/IMF-fixdate (RFC 9110 §5.6.7): RFC 2822 permits a
//! numeric `+0000` zone offset among other differences, while HTTP-date
//! requires the literal `GMT` and a specific weekday/month abbreviation
//! style. [`HTTP_DATE_FORMAT`] instead spells out IMF-fixdate explicitly via
//! `time::macros::format_description!`, matching RFC 9110's own worked
//! example verbatim in this module's tests (`"Sun, 06 Nov 1994 08:49:37
//! GMT"`).

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::http::header::{DATE, HOST};
use axum::http::{HeaderName, HeaderValue, StatusCode};
use rsa::pkcs8::DecodePrivateKey;
use rsa::{Pkcs1v15Sign, RsaPrivateKey};
use sha2::{Digest as _, Sha256};
use time::OffsetDateTime;
use time::macros::format_description;

use super::digest::Digest as BodyDigest;
use super::http_client::OutboundRequest;
use super::suite::{
    DraftCavageSuite, Rfc9421Suite, SignableRequest, SignatureFormat, SignatureSuite,
};
use crate::actor::{ActorDirectory, Handle};
use crate::error::AppError;
use crate::federation::urls::ActorUrls;
use crate::runtime::Clock;
use crate::runtime::signing_key::{KeyRef, SigningKeyProvider};

/// HTTP-date (RFC 9110 §5.6.7, IMF-fixdate), e.g. `"Sun, 06 Nov 1994
/// 08:49:37 GMT"`. See this module's doc comment ("`Date` header: HTTP-date
/// (IMF-fixdate)") for why this is spelled out explicitly rather than reused
/// from `time`'s bundled `Rfc2822` format.
const HTTP_DATE_FORMAT: &[time::format_description::BorrowedFormatItem<'_>] = format_description!(
    "[weekday repr:short], [day padding:zero] [month repr:short] [year] [hour]:[minute]:[second] GMT"
);

/// The fixed, algorithm-defined 19-byte `DigestInfo` prefix RFC 8017's
/// `EMSA-PKCS1-v1_5` encoding uses for SHA-256 (OID `2.16.840.1.101.3.4.2.1`,
/// `id-sha256`). See this module's doc comment ("Algorithm: RSA-SHA256 /
/// PKCS#1 v1.5 only") for how this exact byte sequence was verified against
/// the real `rsa`/`digest`-0.10/`sha2`-0.10 crates in this environment,
/// rather than recalled from memory.
const SHA256_PKCS1V15_PREFIX: [u8; 19] = [
    0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01, 0x05,
    0x00, 0x04, 0x20,
];

/// Builds the `Pkcs1v15Sign` padding scheme for RSASSA-PKCS1-v1_5 with
/// SHA-256, without going through `rsa::Pkcs1v15Sign::new::<D>()` (whose
/// `digest`-0.10 generic bound this workspace's direct `sha2 = "0.11.0"`
/// dependency cannot satisfy — see this module's doc comment). Constructed
/// directly from `Pkcs1v15Sign`'s two public fields instead.
fn sha256_pkcs1v15_padding() -> Pkcs1v15Sign {
    Pkcs1v15Sign {
        hash_len: Some(32),
        prefix: SHA256_PKCS1V15_PREFIX.to_vec().into_boxed_slice(),
    }
}

/// Renders `when` as an HTTP-date (IMF-fixdate) string, for the outgoing
/// request's `Date` header.
fn http_date(when: OffsetDateTime) -> String {
    when.to_offset(time::UtcOffset::UTC)
        .format(HTTP_DATE_FORMAT)
        .expect("HTTP-date formatting of a valid OffsetDateTime must not fail")
}

/// Extracts the `host[:port]` authority portion of an absolute URL, e.g.
/// `"https://example.com/inbox?x=1"` -> `"example.com"`, for the `Host`
/// header. Mirrors `suite.rs`'s own `path_and_query` helper's string-parsing
/// style (this spec's URLs are always well-formed absolute URLs this crate
/// itself built via `ActorUrls`/a caller's own request construction, so no
/// full URL-parsing dependency is warranted here either).
fn host_from_url(url: &str) -> &str {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    &after_scheme[..end]
}

/// Attaches an HTTP Signature (draft-cavage or RFC 9421) to an outbound
/// request, using the currently valid signing key of a local actor
/// (design.md's `RequestSigner` component; Requirements 1.1, 1.2, 1.3, 1.5).
/// See this module's doc comment for the full behavioral contract and the
/// deviations from design.md's literal (incomplete) pinned interface this
/// module documents and justifies.
pub struct RequestSigner {
    directory: Arc<ActorDirectory>,
    key_provider: Arc<dyn SigningKeyProvider>,
    urls: ActorUrls,
    clock: Arc<dyn Clock>,
}

impl RequestSigner {
    /// Builds a signer against `directory` (the `Handle -> Id` resolution
    /// boundary — see this module's doc comment), `key_provider` (the
    /// core-runtime signing-key supply boundary), `urls` (`keyId`
    /// construction), and `clock` (the `Date` header's non-determinism
    /// boundary).
    pub fn new(
        directory: Arc<ActorDirectory>,
        key_provider: Arc<dyn SigningKeyProvider>,
        urls: ActorUrls,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            directory,
            key_provider,
            urls,
            clock,
        }
    }

    /// Signs `req` as `actor`, in `format` (design.md's exact `RequestSigner`
    /// Service Interface, modulo the documented `async fn` deviation — see
    /// this module's doc comment).
    ///
    /// Resolves `actor` to its `Id` via [`ActorDirectory::resolve_actor_by_handle`],
    /// fetches its currently valid signing key via the injected
    /// [`SigningKeyProvider`], sets `keyId` via [`ActorUrls::key_id`]
    /// (Requirement 1.2), includes a `Digest` header covering `req.body`
    /// when present (Requirement 1.3), and RSA-signs the resulting signing
    /// input (Requirement 1.4's format choice, `format`), applying the
    /// signature headers [`SignatureSuite::assemble_headers`] returns onto
    /// `req.headers`.
    ///
    /// Returns a caller-facing [`AppError`] and leaves `req` entirely
    /// unmodified (see this module's doc comment, "`req` is left untouched
    /// on any error path") if: `actor` names no local actor at all, `actor`
    /// has no currently valid signing key (Requirement 1.5, both cases), the
    /// stored key's PEM material is unparsable, or the signature-header
    /// values `SignatureSuite::assemble_headers` returns are themselves
    /// malformed as HTTP header names/values.
    pub async fn sign_request(
        &self,
        actor: &Handle,
        format: SignatureFormat,
        req: &mut OutboundRequest,
    ) -> Result<(), AppError> {
        let resolved = self
            .directory
            .resolve_actor_by_handle(actor)
            .await?
            .ok_or_else(|| {
                AppError::client(
                    StatusCode::NOT_FOUND,
                    format!(
                        "cannot sign a request as {:?}: no such local actor",
                        actor.as_str()
                    ),
                )
            })?;

        let signing_key = self
            .key_provider
            .signing_key(KeyRef(resolved.id))
            .map_err(|source| {
                AppError::client(
                    StatusCode::NOT_FOUND,
                    format!(
                        "cannot sign a request as {:?}: no valid signing key available: {source}",
                        actor.as_str()
                    ),
                )
            })?;

        let pem = std::str::from_utf8(signing_key.expose_pem_bytes())
            .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;
        let private_key = RsaPrivateKey::from_pkcs8_pem(pem)
            .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;

        let key_id = self.urls.key_id(actor);

        // Every fallible step from here on is computed against local
        // values only; `req` is not touched until the very end, once
        // signing has fully succeeded (see this module's doc comment,
        // "`req` is left untouched on any error path").
        let host_value = HeaderValue::from_str(host_from_url(&req.url))
            .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;
        let date_value = HeaderValue::from_str(&http_date(self.clock.now()))
            .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;
        let digest_value = match req.body.as_deref() {
            Some(body) => Some(
                HeaderValue::from_str(&BodyDigest::compute(body).header_value()).map_err(
                    |source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source),
                )?,
            ),
            None => None,
        };
        let digest_header_name = HeaderName::from_static("digest");

        let mut signing_headers = req.headers.clone();
        signing_headers.insert(HOST, host_value.clone());
        signing_headers.insert(DATE, date_value.clone());
        if let Some(digest_value) = &digest_value {
            signing_headers.insert(digest_header_name.clone(), digest_value.clone());
        }

        let signable = SignableRequest {
            method: req.method.clone(),
            url: req.url.clone(),
            key_id: key_id.clone(),
            headers: signing_headers,
        };

        let suite: Box<dyn SignatureSuite> = match format {
            SignatureFormat::DraftCavage => Box::new(DraftCavageSuite::new()),
            SignatureFormat::Rfc9421 => Box::new(Rfc9421Suite::new()),
        };
        let signing_input = suite.build_signing_input(&signable);

        let hashed = Sha256::digest(signing_input.signing_string.as_bytes());
        let signature = private_key
            .sign(sha256_pkcs1v15_padding(), hashed.as_slice())
            .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;

        let mut assembled_headers = Vec::with_capacity(2);
        for (name, value) in suite.assemble_headers(&key_id, &signature, &signing_input) {
            let header_name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;
            let header_value = HeaderValue::from_str(&value)
                .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;
            assembled_headers.push((header_name, header_value));
        }

        // Signing has fully succeeded: apply every header at once.
        req.headers.insert(HOST, host_value);
        req.headers.insert(DATE, date_value);
        if let Some(digest_value) = digest_value {
            req.headers.insert(digest_header_name, digest_value);
        }
        for (name, value) in assembled_headers {
            req.headers.insert(name, value);
        }

        Ok(())
    }
}
