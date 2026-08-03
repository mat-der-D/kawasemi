//! `SignatureSuite` (design.md "Crypto / 署名層" -> `SignatureSuite` Service
//! Interface; Requirements 1.4, 2.2; task 1.5, `Boundary: SignatureSuite`):
//! the format-difference-absorbing abstraction over draft-cavage ("Signing
//! HTTP Messages") and RFC 9421 ("HTTP Message Signatures") HTTP Signatures.
//!
//! ## Scope: pure string/format logic only, no cryptography
//! This module builds the "signing input" string a signer would RSA-sign,
//! assembles an already-computed signature (`&[u8]`, handed in by the
//! caller) into the correct HTTP header syntax for each format, parses
//! received signature headers back into a structured form, and detects
//! which format a set of received headers uses. It never computes an RSA
//! signature itself and never verifies one — that is `RequestSigner`
//! (design.md task numbering places `SignatureSuite` at 1.4/2.2 dependency
//! roots; `RequestSigner`/`SignatureVerifier` are later, out-of-boundary
//! tasks) and `SignatureVerifier`, both out of this task's boundary.
//!
//! ## `SignableRequest` / `SigningInput` / `RequestHeaders` / `ParsedSignature`
//! design.md's Service Interface pins only the trait signature; these four
//! types are not defined anywhere else in this spec, so they are defined
//! here. Design rationale for each:
//!
//! - [`RequestHeaders`] is a plain type alias for `axum::http::HeaderMap`
//!   (no wrapper struct) — the same header vocabulary type
//!   `src/federation/signatures/http_client.rs`'s `OutboundRequest` /
//!   `HttpResponse` already use, so a caller can pass an `OutboundRequest`'s
//!   `headers` (or an incoming request's `HeaderMap`) straight through
//!   without any conversion glue.
//! - [`SignableRequest`] intentionally mirrors `OutboundRequest`'s shape
//!   (`method: Method`, `url: String` matching `OutboundRequest::url`,
//!   `headers: RequestHeaders` matching `OutboundRequest::headers`
//!   verbatim) plus one additional field, `key_id: String` — see "Why
//!   `SignableRequest` carries `key_id`" below for why that field exists.
//!   It carries no `body`: the scope boundary (see module doc above) means
//!   the caller has already computed any body digest (via task 1.4's
//!   [`super::digest::Digest`]) and placed it in a `Digest` header *before*
//!   calling [`SignatureSuite::build_signing_input`] — this module reads
//!   that header like any other, it never hashes a body itself.
//! - [`SigningInput`] carries the exact byte string to be signed
//!   (`signing_string`) plus enough format-specific bookkeeping
//!   (`covered_components`, `signed_key_id`) for
//!   [`SignatureSuite::assemble_headers`] to render the correct header
//!   syntax without re-deriving anything from the original request.
//! - [`ParsedSignature`] is the structured result of
//!   [`SignatureSuite::parse`]: the claimed `key_id`, the components the
//!   sender says are covered, the raw (still base64-decoded, not
//!   cryptographically verified) signature bytes, and the advertised
//!   algorithm token, if any.
//!
//! ## Why `SignableRequest` carries `key_id`
//! RFC 9421's signature base is not just the covered components — its
//! final line is `"@signature-params": (<component list>);<params>`, and
//! the *signed* params (here: `keyid` and `alg`) MUST be part of the bytes
//! that get RSA-signed, or a spec-compliant peer reconstructing the base
//! from the `Signature-Input` header it received (which does carry
//! `keyid`/`alg`) will compute a different base than the one that was
//! signed, and verification will fail. design.md's pinned trait passes
//! `key_id` to [`SignatureSuite::assemble_headers`] (after the signature is
//! already computed) but *not* to
//! [`SignatureSuite::build_signing_input`] (before signing) — so the only
//! way `key_id` can reach the RFC 9421 signature base honestly is via the
//! `req: &SignableRequest` argument `build_signing_input` does receive.
//! `SignableRequest::key_id` is that channel. draft-cavage's signing string
//! never embeds `keyId` (only the `Signature` header's `keyId="..."` param
//! does, assembled after signing), so `DraftCavageSuite::build_signing_input`
//! simply ignores this field.
//!
//! Callers (the future `RequestSigner`, task 2.2, and `SignatureVerifier`,
//! task 2.3, when reconstructing a base to verify) MUST pass the same
//! `key_id` value into `SignableRequest::key_id` as they later pass to
//! `assemble_headers`'s `key_id` parameter for RFC 9421 — see
//! [`Rfc9421Suite::assemble_headers`]'s `debug_assert_eq!` documenting this
//! invariant.
//!
//! ## Why the digest component is named `digest` in both formats
//! draft-cavage's convention (Mastodon-compatible ActivityPub) covers the
//! `Digest` header (RFC 3230 style, `SHA-256=<base64>`) — exactly the form
//! task 1.4's [`super::digest::Digest::header_value`] produces. RFC 9421's
//! newer ecosystem convention is a distinct `Content-Digest` header (RFC
//! 9530) with different (structured-field byte-sequence) syntax that this
//! spec's `Digest` type does not produce. Since this task's boundary is
//! "pure string/format logic" over whatever digest primitive task 1.4
//! already built (no new digest capability is in scope here), both suite
//! implementations cover the same regular header field, `digest`, rather
//! than introducing `Content-Digest`/RFC 9530 parsing. This is a deliberate
//! subsetting choice for federation-core's own needs (per this task's
//! brief: "implement a genuinely correct subset ... not every RFC 9421
//! edge case") — flagged in this task's status report as a CONCERN for
//! whichever future task cares about interop with a strictly
//! `Content-Digest`-only RFC 9421 peer.
//!
//! ## Why RFC 9421 `created`/`expires` are omitted
//! RFC 9421 signature parameters MAY include `created`/`expires`; they are
//! OPTIONAL, not mandatory. Populating `created` correctly would require a
//! clock — but `SignatureSuite` takes only a `&SignableRequest` and must
//! stay a pure function of its input (no injected `Clock`, matching this
//! module's "pure string/format logic" scope, not a runtime service). Both
//! formats already cover the `date` header as a signed component, which
//! gives any future `SignatureVerifier` the same temporal anchor
//! draft-cavage implementations conventionally rely on (checking `Date`
//! header freshness), so `created`/`expires` add no verification capability
//! that `date` doesn't already provide for this spec's purposes. If a
//! future task needs them, design.md already lists "署名スイート
//! （`SignatureSuite`）...境界のシグネチャ変更" as an anticipated
//! Revalidation Trigger, so extending `SignableRequest`/`SigningInput` then
//! is expected, not a surprise.
//!
//! ## Why `SignatureSuite::detect` has an explicit `Self: Sized` bound
//! design.md pins `fn detect(headers: &RequestHeaders) -> Option<SignatureFormat>;`
//! with no `&self` receiver — an associated function, not a method. A trait
//! with such a function is not object-safe unless that function is
//! excluded from the vtable via `where Self: Sized`, which this trait
//! definition adds. This does not change the pinned signature's meaning;
//! `detect` was always meant to run *before* a concrete suite is chosen
//! (that's its entire purpose — to determine *which* suite applies to a
//! set of received headers), so it was never going to be called through a
//! `dyn SignatureSuite` object regardless. The trait keeps a default body
//! (delegating to the free function [`detect_signature_format`]) so
//! neither concrete suite needs to redefine it.
//!
//! Primary-source note: outbound fetches to `rfc-editor.org`,
//! `datatracker.ietf.org`, `httpwg.org`, and other mirrors of RFC 9421 /
//! draft-cavage-http-signatures were attempted while implementing this
//! module and consistently returned `403` through this environment's
//! proxy. The signing-string / signature-base construction and header
//! syntax implemented below rest on well-established, widely-documented
//! conventions (draft-cavage's `(request-target)` pseudo-header and
//! `Signature: keyId="...",algorithm="...",headers="...",signature="..."`
//! syntax; RFC 9421's `Signature-Input: sig1=(...);keyid="...";alg="...";`
//! and `Signature: sig1=:<base64>:` syntax, and its
//! `"<component>": <value>` signature-base line format terminated by a
//! `"@signature-params": (...)...` final line with no trailing newline),
//! cross-checked via search-engine-summarized excerpts of the specs during
//! implementation rather than a direct RFC fetch.

#[cfg(test)]
mod tests;

use std::collections::HashMap;

use axum::http::{HeaderMap, Method, StatusCode};

use crate::error::AppError;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;

/// The algorithm token draft-cavage suites advertise in the `Signature`
/// header's `algorithm="..."` parameter. This spec's actor keys are
/// RSA-2048 (design.md's Technology Stack row), so this is the only
/// algorithm this module ever emits or expects.
const CAVAGE_ALGORITHM: &str = "rsa-sha256";

/// The algorithm token RFC 9421 suites advertise in the `Signature-Input`
/// header's `alg="..."` parameter for RSASSA-PKCS1-v1_5 with SHA-256 (the
/// RFC 9421 IANA-registered token for this algorithm family), matching
/// design.md's `RequestSigner` section's own worked example
/// (`alg="rsa-v1_5-sha256"`).
const RFC9421_ALGORITHM: &str = "rsa-v1_5-sha256";

/// Which HTTP Signatures wire format a request uses (Requirements 1.4,
/// 2.2: "署名形式として draft-cavage 形式と RFC 9421 形式の双方を生成でき
/// る" / "draft-cavage 形式と RFC 9421 形式の双方の署名を検証できる").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SignatureFormat {
    /// "Signing HTTP Messages" (the Cavage/Cavage-Richanson IETF draft),
    /// the format Mastodon and most existing ActivityPub implementations
    /// use.
    DraftCavage,
    /// RFC 9421, "HTTP Message Signatures" — the newer, structured-field
    /// based format.
    Rfc9421,
}

/// This module's header vocabulary type: a plain alias for
/// `axum::http::HeaderMap`, matching
/// `src/federation/signatures/http_client.rs`'s `OutboundRequest` /
/// `HttpResponse` (see this module's doc comment, "`SignableRequest` /
/// `SigningInput` / `RequestHeaders` / `ParsedSignature`") rather than
/// introducing a wrapper type. `HeaderMap` lookups are already
/// case-insensitive (header names are normalized to lowercase internally),
/// so `"Signature-Input"`, `"signature-input"`, etc. all resolve the same
/// way regardless of how a caller inserted or received the header.
pub type RequestHeaders = HeaderMap;

/// A request about to be (or that was) signed — the input to
/// [`SignatureSuite::build_signing_input`]. See this module's doc comment
/// for why the fields are shaped this way and why `key_id` is here rather
/// than only on [`SignatureSuite::assemble_headers`].
#[derive(Debug, Clone)]
pub struct SignableRequest {
    pub method: Method,
    /// Full absolute request URL, e.g.
    /// `"https://mastodon.example/inbox?x=1"` — matches
    /// `OutboundRequest::url`'s shape exactly so a future `RequestSigner`
    /// can build a `SignableRequest` directly from the `OutboundRequest`
    /// it is signing.
    pub url: String,
    /// The `keyId` this request will be (RFC 9421: is already, inside the
    /// returned `SigningInput`'s signature base) signed with. See this
    /// module's doc comment, "Why `SignableRequest` carries `key_id`".
    pub key_id: String,
    /// The request's headers, already carrying anything that must be
    /// covered by the signature (`Host`, `Date`, and — only if the request
    /// has a body — `Digest`) before `build_signing_input` is called. This
    /// suite reads header values verbatim; it never sets or computes them.
    pub headers: RequestHeaders,
}

impl SignableRequest {
    /// Builds a `SignableRequest` with no headers set yet. Callers
    /// typically follow with `.headers.insert(...)` for `Host`/`Date`/
    /// `Digest` before calling `build_signing_input`.
    pub fn new(method: Method, url: impl Into<String>, key_id: impl Into<String>) -> Self {
        Self {
            method,
            url: url.into(),
            key_id: key_id.into(),
            headers: RequestHeaders::new(),
        }
    }
}

/// The result of [`SignatureSuite::build_signing_input`]: the exact byte
/// string a signer must RSA-sign, plus the bookkeeping
/// [`SignatureSuite::assemble_headers`] needs to render the matching
/// header syntax afterward.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SigningInput {
    pub format: SignatureFormat,
    /// The exact string whose UTF-8 bytes must be RSA-signed to produce
    /// the `signature` argument later passed to
    /// [`SignatureSuite::assemble_headers`].
    pub signing_string: String,
    /// The component/header names covered, in the exact order used inside
    /// `signing_string` — draft-cavage: e.g. `["(request-target)", "host",
    /// "date", "digest"]`; RFC 9421: derived/regular component identifiers
    /// without a leading `@signature-params` entry, e.g. `["@method",
    /// "@target-uri", "host", "date", "digest"]`. Used by
    /// `assemble_headers` to render the `headers="..."` (draft-cavage) or
    /// component-list (RFC 9421) parameter without re-deriving it.
    pub covered_components: Vec<String>,
    /// RFC 9421 only: echoes `SignableRequest::key_id` as it was baked
    /// into `signing_string`'s `@signature-params` line, so
    /// `Rfc9421Suite::assemble_headers` can assert the `key_id` it is
    /// asked to render matches what was actually signed. `None` for
    /// draft-cavage, whose signing string never embeds `keyId`.
    pub signed_key_id: Option<String>,
}

/// The structured result of [`SignatureSuite::parse`]: what a received
/// signature claims, not yet cryptographically verified (that is
/// `SignatureVerifier`'s job, task 2.3, out of this task's boundary).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSignature {
    pub format: SignatureFormat,
    /// The `keyId` (draft-cavage) / `keyid` (RFC 9421) parameter as
    /// received, unresolved — `PublicKeyResolver` (task 2.1) is what turns
    /// this into actual key material.
    pub key_id: String,
    /// The component/header names the sender claims are covered, in the
    /// order received (draft-cavage: the `headers="..."` param split on
    /// whitespace; RFC 9421: the `sig1=(...)` component list).
    pub covered_components: Vec<String>,
    /// The raw, base64-decoded signature bytes — not yet verified against
    /// any public key.
    pub signature: Vec<u8>,
    /// The algorithm token as advertised by the sender (draft-cavage:
    /// `algorithm="..."`; RFC 9421: `alg="..."`, absent if the sender
    /// omitted it — RFC 9421 does not require `alg`). Informational only;
    /// this module does not validate it cryptographically.
    pub algorithm: Option<String>,
}

/// Format-difference-absorbing abstraction over draft-cavage and RFC 9421
/// HTTP Signatures (design.md `#### SignatureSuite`; Requirements 1.4,
/// 2.2). See this module's doc comment for the full rationale behind each
/// method's supporting types and the `detect` associated function's
/// `Self: Sized` bound.
pub trait SignatureSuite: Send + Sync {
    /// Which format this suite implements.
    fn format(&self) -> SignatureFormat;

    /// Builds the signing input (signature target string plus bookkeeping)
    /// for `req`. Pure function of `req`'s contents — reads header values
    /// verbatim, computes no digest, calls no clock.
    fn build_signing_input(&self, req: &SignableRequest) -> SigningInput;

    /// Assembles an already-computed `signature` (the RSA signature bytes
    /// a caller produced by signing `input.signing_string`) into the
    /// `(header name, header value)` pairs this format places on an
    /// outgoing request. Returns one pair for draft-cavage (`Signature`)
    /// and two for RFC 9421 (`Signature-Input`, `Signature`).
    fn assemble_headers(
        &self,
        key_id: &str,
        signature: &[u8],
        input: &SigningInput,
    ) -> Vec<(String, String)>;

    /// Parses this format's signature header(s) out of `headers` into a
    /// [`ParsedSignature`]. Returns a caller-facing [`AppError`] (`400 Bad
    /// Request`) if the expected header(s) are missing or malformed for
    /// this format.
    fn parse(&self, headers: &RequestHeaders) -> Result<ParsedSignature, AppError>;

    /// Detects which [`SignatureFormat`] a set of received headers uses,
    /// or `None` if neither format's signature headers are present. See
    /// this module's doc comment ("Why `SignatureSuite::detect` has an
    /// explicit `Self: Sized` bound") for why this associated function
    /// cannot be called through a `dyn SignatureSuite` and why that is
    /// correct.
    fn detect(headers: &RequestHeaders) -> Option<SignatureFormat>
    where
        Self: Sized,
    {
        detect_signature_format(headers)
    }
}

/// Shared implementation backing both suites' default
/// [`SignatureSuite::detect`]: RFC 9421 requests carry a `Signature-Input`
/// header (draft-cavage never sets this header name, so its presence is
/// conclusive); draft-cavage requests carry a `Signature`-only header (the
/// `keyId=...,algorithm=...,headers=...,signature=...` comma-separated
/// syntax) or, historically, an `Authorization: Signature ...` header,
/// with no `Signature-Input` header alongside.
fn detect_signature_format(headers: &RequestHeaders) -> Option<SignatureFormat> {
    if headers.contains_key("signature-input") {
        return Some(SignatureFormat::Rfc9421);
    }
    if headers.contains_key("signature") {
        return Some(SignatureFormat::DraftCavage);
    }
    if let Some(authorization) = header_str(headers, "authorization")
        && authorization.trim_start().starts_with("Signature ")
    {
        return Some(SignatureFormat::DraftCavage);
    }
    None
}

/// Reads a header's value as `&str`, or `None` if the header is absent or
/// not valid UTF-8/ASCII (this suite treats a non-`str` header value the
/// same as an absent one, rather than erroring — the caller-supplied
/// header simply won't be covered/found).
fn header_str<'a>(headers: &'a RequestHeaders, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

/// Extracts the path-and-query portion of an absolute URL, e.g.
/// `"https://example.com/inbox?x=1"` -> `"/inbox?x=1"`, for draft-cavage's
/// `(request-target)` pseudo-header (Requirement 1.4). Falls back to `"/"`
/// for a bare-authority URL with no path.
fn path_and_query(url: &str) -> &str {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    match after_scheme.find('/') {
        Some(idx) => &after_scheme[idx..],
        None => "/",
    }
}

/// Renders a covered-component list in RFC 9421's inner-list syntax, e.g.
/// `["@method", "host"]` -> `("@method" "host")`.
fn component_list_repr(components: &[String]) -> String {
    let quoted: Vec<String> = components
        .iter()
        .map(|name| format!("\"{name}\""))
        .collect();
    format!("({})", quoted.join(" "))
}

fn malformed(what: &str) -> AppError {
    AppError::client(StatusCode::BAD_REQUEST, format!("malformed {what}"))
}

fn missing(what: &str) -> AppError {
    AppError::client(StatusCode::BAD_REQUEST, format!("missing {what}"))
}

/// draft-cavage ("Signing HTTP Messages") suite implementation.
#[derive(Debug, Clone, Copy, Default)]
pub struct DraftCavageSuite;

impl DraftCavageSuite {
    pub fn new() -> Self {
        Self
    }
}

impl SignatureSuite for DraftCavageSuite {
    fn format(&self) -> SignatureFormat {
        SignatureFormat::DraftCavage
    }

    fn build_signing_input(&self, req: &SignableRequest) -> SigningInput {
        let method_lower = req.method.as_str().to_ascii_lowercase();
        let target = path_and_query(&req.url);

        let mut covered = vec!["(request-target)".to_string()];
        let mut lines = vec![format!("(request-target): {method_lower} {target}")];

        for name in ["host", "date"] {
            if let Some(value) = header_str(&req.headers, name) {
                covered.push(name.to_string());
                lines.push(format!("{name}: {value}"));
            }
        }
        if let Some(value) = header_str(&req.headers, "digest") {
            covered.push("digest".to_string());
            lines.push(format!("digest: {value}"));
        }

        SigningInput {
            format: SignatureFormat::DraftCavage,
            signing_string: lines.join("\n"),
            covered_components: covered,
            signed_key_id: None,
        }
    }

    fn assemble_headers(
        &self,
        key_id: &str,
        signature: &[u8],
        input: &SigningInput,
    ) -> Vec<(String, String)> {
        let signature_b64 = BASE64_STANDARD.encode(signature);
        let headers_param = input.covered_components.join(" ");
        let value = format!(
            "keyId=\"{key_id}\",algorithm=\"{CAVAGE_ALGORITHM}\",headers=\"{headers_param}\",signature=\"{signature_b64}\""
        );
        vec![("Signature".to_string(), value)]
    }

    fn parse(&self, headers: &RequestHeaders) -> Result<ParsedSignature, AppError> {
        let raw = cavage_signature_header_value(headers)?;
        let params = parse_cavage_params(&raw)?;

        let key_id = params
            .get("keyid")
            .cloned()
            .ok_or_else(|| missing("draft-cavage \"keyId\" parameter"))?;
        let algorithm = params.get("algorithm").cloned();
        let headers_param = params
            .get("headers")
            .cloned()
            .ok_or_else(|| missing("draft-cavage \"headers\" parameter"))?;
        let covered_components = headers_param
            .split_whitespace()
            .map(str::to_string)
            .collect();
        let signature_b64 = params
            .get("signature")
            .cloned()
            .ok_or_else(|| missing("draft-cavage \"signature\" parameter"))?;
        let signature = BASE64_STANDARD
            .decode(signature_b64.as_bytes())
            .map_err(|source| {
                AppError::client(
                    StatusCode::BAD_REQUEST,
                    format!("invalid base64 in draft-cavage \"signature\" parameter: {source}"),
                )
            })?;

        Ok(ParsedSignature {
            format: SignatureFormat::DraftCavage,
            key_id,
            covered_components,
            signature,
            algorithm,
        })
    }
}

/// Finds the draft-cavage signature parameter string, from either the
/// canonical `Signature` header or the legacy `Authorization: Signature
/// ...` form (design.md: "`Signature` ヘッダ（または歴史的に `Authorization:
/// Signature ...`）").
fn cavage_signature_header_value(headers: &RequestHeaders) -> Result<String, AppError> {
    if let Some(value) = header_str(headers, "signature") {
        return Ok(value.to_string());
    }
    if let Some(value) = header_str(headers, "authorization")
        && let Some(rest) = value.strip_prefix("Signature ")
    {
        return Ok(rest.to_string());
    }
    Err(missing("draft-cavage \"Signature\" header"))
}

/// Parses draft-cavage's comma-separated `name="value"` parameter syntax
/// (e.g. `keyId="...",algorithm="...",headers="...",signature="..."`) into
/// a lookup map keyed by lowercased parameter name. A bare top-level
/// `split(',')` is safe here: every parameter value in this syntax is
/// either a base64 signature (no commas in its alphabet) or a
/// space-separated header-name list (no commas either), so no quoted value
/// this module needs to parse ever contains a comma.
fn parse_cavage_params(raw: &str) -> Result<HashMap<String, String>, AppError> {
    let mut map = HashMap::new();
    for segment in raw.split(',') {
        let segment = segment.trim();
        if segment.is_empty() {
            continue;
        }
        let (key, value) = segment
            .split_once('=')
            .ok_or_else(|| malformed("draft-cavage signature parameter"))?;
        let value = value.trim().trim_matches('"');
        map.insert(key.trim().to_ascii_lowercase(), value.to_string());
    }
    Ok(map)
}

/// RFC 9421 ("HTTP Message Signatures") suite implementation, restricted
/// to this spec's actual needs: RSA-SHA256 only, covering `@method`,
/// `@target-uri`, `host`, `date`, and (when the request has a body)
/// `digest`. See this module's doc comment for the deliberate subsetting
/// decisions (`digest` naming, omitted `created`/`expires`).
#[derive(Debug, Clone, Copy, Default)]
pub struct Rfc9421Suite;

impl Rfc9421Suite {
    pub fn new() -> Self {
        Self
    }
}

impl SignatureSuite for Rfc9421Suite {
    fn format(&self) -> SignatureFormat {
        SignatureFormat::Rfc9421
    }

    fn build_signing_input(&self, req: &SignableRequest) -> SigningInput {
        let mut covered = vec!["@method".to_string(), "@target-uri".to_string()];
        let mut lines = vec![
            format!("\"@method\": {}", req.method.as_str()),
            format!("\"@target-uri\": {}", req.url),
        ];

        for name in ["host", "date"] {
            if let Some(value) = header_str(&req.headers, name) {
                covered.push(name.to_string());
                lines.push(format!("\"{name}\": {value}"));
            }
        }
        if let Some(value) = header_str(&req.headers, "digest") {
            covered.push("digest".to_string());
            lines.push(format!("\"digest\": {value}"));
        }

        // See this module's doc comment, "Why `SignableRequest` carries
        // `key_id`": `keyid`/`alg` must be part of the signed base, and
        // `key_id` is only reachable here via `req.key_id`.
        let component_list = component_list_repr(&covered);
        lines.push(format!(
            "\"@signature-params\": {component_list};keyid=\"{}\";alg=\"{RFC9421_ALGORITHM}\"",
            req.key_id
        ));

        SigningInput {
            format: SignatureFormat::Rfc9421,
            signing_string: lines.join("\n"),
            covered_components: covered,
            signed_key_id: Some(req.key_id.clone()),
        }
    }

    fn assemble_headers(
        &self,
        key_id: &str,
        signature: &[u8],
        input: &SigningInput,
    ) -> Vec<(String, String)> {
        debug_assert_eq!(
            input.signed_key_id.as_deref(),
            Some(key_id),
            "assemble_headers's key_id must match the key_id already baked into the signed \
             @signature-params line (SignableRequest::key_id passed to build_signing_input); \
             otherwise the rendered Signature-Input header would misrepresent what was actually signed"
        );

        let component_list = component_list_repr(&input.covered_components);
        let signature_b64 = BASE64_STANDARD.encode(signature);
        vec![
            (
                "Signature-Input".to_string(),
                format!("sig1={component_list};keyid=\"{key_id}\";alg=\"{RFC9421_ALGORITHM}\""),
            ),
            ("Signature".to_string(), format!("sig1=:{signature_b64}:")),
        ]
    }

    fn parse(&self, headers: &RequestHeaders) -> Result<ParsedSignature, AppError> {
        let input_value = header_str(headers, "signature-input")
            .ok_or_else(|| missing("RFC 9421 \"Signature-Input\" header"))?;
        let signature_value = header_str(headers, "signature")
            .ok_or_else(|| missing("RFC 9421 \"Signature\" header"))?;

        let (label, rest) = input_value
            .split_once('=')
            .ok_or_else(|| malformed("RFC 9421 \"Signature-Input\" header"))?;
        let label = label.trim();

        let (list_part, params_part) = split_signature_input_member(rest)?;
        let covered_components = parse_component_list(list_part)?;
        let params = parse_semicolon_params(params_part)?;

        let key_id = params
            .get("keyid")
            .cloned()
            .ok_or_else(|| missing("RFC 9421 \"keyid\" parameter"))?;
        let algorithm = params.get("alg").cloned();

        let signature_prefix = format!("{label}=:");
        let signature_b64 = signature_value
            .strip_prefix(signature_prefix.as_str())
            .and_then(|rest| rest.strip_suffix(':'))
            .ok_or_else(|| malformed("RFC 9421 \"Signature\" header"))?;
        let signature = BASE64_STANDARD.decode(signature_b64).map_err(|source| {
            AppError::client(
                StatusCode::BAD_REQUEST,
                format!("invalid base64 in RFC 9421 \"Signature\" header: {source}"),
            )
        })?;

        Ok(ParsedSignature {
            format: SignatureFormat::Rfc9421,
            key_id,
            covered_components,
            signature,
            algorithm,
        })
    }
}

/// Splits an RFC 9421 dictionary member's value (everything after the
/// label and `=`, e.g. `("@method" "host");keyid="..."`) into its
/// parenthesized component list and its trailing `;param=value;...`
/// parameters string. Assumes (true for this suite's own component names
/// and parameter values) that no component name or parameter value
/// contains a literal `)`.
fn split_signature_input_member(rest: &str) -> Result<(&str, &str), AppError> {
    let rest = rest.trim();
    let inner = rest
        .strip_prefix('(')
        .ok_or_else(|| malformed("RFC 9421 \"Signature-Input\" header"))?;
    let close_idx = inner
        .find(')')
        .ok_or_else(|| malformed("RFC 9421 \"Signature-Input\" header"))?;
    Ok((&inner[..close_idx], &inner[close_idx + 1..]))
}

/// Parses `"@method" "@target-uri" "host"` into
/// `["@method", "@target-uri", "host"]`.
fn parse_component_list(list_part: &str) -> Result<Vec<String>, AppError> {
    let components: Vec<String> = list_part
        .split_whitespace()
        .map(|token| token.trim_matches('"').to_string())
        .filter(|name| !name.is_empty())
        .collect();
    if components.is_empty() {
        return Err(malformed("RFC 9421 covered-component list"));
    }
    Ok(components)
}

/// Parses `;keyid="test-key";alg="rsa-v1_5-sha256"` into a lookup map
/// keyed by lowercased parameter name.
fn parse_semicolon_params(params_part: &str) -> Result<HashMap<String, String>, AppError> {
    let mut map = HashMap::new();
    for segment in params_part.split(';') {
        let segment = segment.trim();
        if segment.is_empty() {
            continue;
        }
        let (key, value) = segment
            .split_once('=')
            .ok_or_else(|| malformed("RFC 9421 signature-input parameter"))?;
        let value = value.trim().trim_matches('"');
        map.insert(key.trim().to_ascii_lowercase(), value.to_string());
    }
    Ok(map)
}
