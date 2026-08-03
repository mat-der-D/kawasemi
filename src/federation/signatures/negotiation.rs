//! `SignatureNegotiator` (design.md `#### SignatureNegotiator` -> Service
//! Interface; Requirements 3.1, 3.2, 3.3; task 2.4, `Boundary:
//! SignatureNegotiator`): double-knocks an outbound signed request against a
//! host whose supported HTTP Signature format is not yet known, and
//! remembers whichever format actually got a signed delivery accepted so
//! subsequent sends to that host skip the guesswork.
//!
//! ## Scope
//! This module owns exactly [`SignatureNegotiator::negotiate_and_send`] and
//! its own `instance_signature_capabilities` read/write logic
//! (`migrations/0004_federation.sql`). It composes already-implemented
//! boundaries — [`super::signer::RequestSigner`] (task 2.2, actual signing)
//! and [`super::http_client::FederationHttpClient`] (task 1.4, actual
//! sending) — rather than reimplementing either. Per design.md's Key
//! Dependencies row for this component ("`RequestSigner`,
//! `FederationHttpClient`, capability store (P0)"), the "capability store"
//! is this task's own responsibility to implement directly (no separate
//! component owns `instance_signature_capabilities`), mirroring task 2.1's
//! `PublicKeyResolver` owning its own `remote_public_keys` read/write. This
//! module does not decide delivery retry/backoff policy for a persistently
//! failing host — that is the future `DeliveryWorker`'s job (task 4.3, out
//! of this task's boundary); this module returns whatever the remote
//! actually said and lets that caller apply its own policy.
//!
//! ## Struct, not a trait: a deliberate reading of design.md
//! Unlike `PublicKeyResolver`/`SignatureVerifier`/`FederationHttpClient`,
//! which design.md explicitly gives as `pub trait ... { async fn ... }`
//! blocks, design.md's `SignatureNegotiator` section gives only a single
//! bare `pub async fn negotiate_and_send(...)` line — no surrounding
//! `pub trait SignatureNegotiator { ... }` wrapper anywhere in this spec.
//! Every other component in this spec that design.md *does* want
//! mock-substitutable at a trait boundary gets an explicit trait block; the
//! absence of one here reads as intentional, not an omission, so this
//! module implements a plain concrete struct. Testability is not lost by
//! this choice: this struct is generic over `H: FederationHttpClient` (see
//! below) and takes a real `PgPool`/`RequestSigner`/`Clock`, so a test can
//! substitute [`super::http_client::MockFederationHttpClient`] for the
//! network boundary and a real, isolated-schema Postgres pool (via this
//! crate's `spawn_test_app` convention) for the capability store — exactly
//! how `key_resolver.rs`'s tests already exercise a real DB table alongside
//! a mocked network port.
//!
//! ## `H: FederationHttpClient` held as `Arc<H>`, not `Arc<dyn ...>`
//! Same dyn-incompatibility this spec has hit at every prior task that holds
//! a `FederationHttpClient` (tasks.md's Implementation Notes, 2.1: `#[allow(async_fn_in_trait)]`
//! `async fn` methods are not object-safe, so `Arc<dyn FederationHttpClient>`
//! does not compile). This module follows the same precedent
//! `key_resolver.rs` set: a generic type parameter `H: FederationHttpClient`
//! held as `Arc<H>`, letting either the production `ReqwestFederationHttpClient`
//! or the deterministic `MockFederationHttpClient` be substituted at the
//! call site via monomorphization.
//!
//! ## Signature-related-rejection heuristic: `401` triggers a retry, nothing else does
//! design.md names "署名関連拒否と一般失敗を区別する" as this component's
//! responsibility but does not spell out a status-code table. This
//! codebase's own inbox error-mapping convention (design.md's Error Handling
//! section: "認証失敗（401 相当）: 署名欠落/不正/期限切れ/公開鍵取得失敗" vs.
//! "拒否（403 相当）: ブロック対象署名者") already draws exactly this line for
//! the *receiving* side of this same protocol, and is the only
//! self-consistent reading available for the *sending* side too: a `401`
//! response means "the signature itself is why you were rejected" (worth
//! retrying with the other format), while `403` means "we know who signed
//! this and deliberately don't want it" (retrying with a different signature
//! format cannot change who the signer is, so it cannot help) and any other
//! non-2xx status is a general failure unrelated to signature format choice.
//! A transport-level `Err` from [`FederationHttpClient::send`] (connection
//! failure, not even a parsed response) is treated the same as "general
//! failure, not signature-related" and is propagated immediately without
//! attempting the second format.
//!
//! ## One retry, never a loop
//! Requirement 3.1's "他方の署名形式で再送する" is exactly one retry with the
//! other format, not a loop between the two formats: [`SignatureFormat`] has
//! exactly two variants, so "the other format" is well-defined and a second
//! `401` on the retry is simply returned as-is (Requirement 3.2's recording
//! only fires on an actual successful delivery, never merely because both
//! formats were attempted).
//!
//! ## `format` column values: exact strings pinned by the migration's own doc comment
//! `migrations/0004_federation.sql`'s `instance_signature_capabilities.format`
//! column comment pins the exact on-disk strings this module reads/writes:
//! `'draft_cavage' | 'rfc9421'` — see [`format_to_db`]/[`format_from_db`].

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::http::StatusCode;
use sqlx::postgres::PgPool;

use super::http_client::{FederationHttpClient, HttpResponse, OutboundRequest};
use super::signer::RequestSigner;
use super::suite::SignatureFormat;
use crate::actor::Handle;
use crate::error::AppError;
use crate::runtime::Clock;

/// The exact on-disk string `migrations/0004_federation.sql` pins for
/// [`SignatureFormat::DraftCavage`].
const FORMAT_DRAFT_CAVAGE: &str = "draft_cavage";
/// The exact on-disk string `migrations/0004_federation.sql` pins for
/// [`SignatureFormat::Rfc9421`].
const FORMAT_RFC9421: &str = "rfc9421";

/// Maps a [`SignatureFormat`] to the exact string
/// `instance_signature_capabilities.format` stores for it.
fn format_to_db(format: SignatureFormat) -> &'static str {
    match format {
        SignatureFormat::DraftCavage => FORMAT_DRAFT_CAVAGE,
        SignatureFormat::Rfc9421 => FORMAT_RFC9421,
    }
}

/// Parses a stored `instance_signature_capabilities.format` value back into
/// a [`SignatureFormat`]. `None` for any value other than the two the
/// migration's doc comment pins — this module never writes anything else,
/// so this only guards against out-of-band/manual data.
fn format_from_db(value: &str) -> Option<SignatureFormat> {
    match value {
        FORMAT_DRAFT_CAVAGE => Some(SignatureFormat::DraftCavage),
        FORMAT_RFC9421 => Some(SignatureFormat::Rfc9421),
        _ => None,
    }
}

/// The other [`SignatureFormat`] from `format` — well-defined since the enum
/// has exactly two variants (see this module's doc comment, "One retry,
/// never a loop").
fn other_format(format: SignatureFormat) -> SignatureFormat {
    match format {
        SignatureFormat::DraftCavage => SignatureFormat::Rfc9421,
        SignatureFormat::Rfc9421 => SignatureFormat::DraftCavage,
    }
}

/// Double-knocks a signed outbound request against a host whose supported
/// HTTP Signature format may not yet be known, and remembers whichever
/// format a successful delivery used (design.md's `SignatureNegotiator`
/// component; Requirements 3.1, 3.2, 3.3). See this module's doc comment for
/// the full behavioral contract, the struct-vs-trait choice, and the
/// resolved signature-related-rejection heuristic.
pub struct SignatureNegotiator<H: FederationHttpClient> {
    pool: PgPool,
    http_client: Arc<H>,
    signer: RequestSigner,
    clock: Arc<dyn Clock>,
}

impl<H: FederationHttpClient> SignatureNegotiator<H> {
    /// Builds a negotiator against `pool` (the `instance_signature_capabilities`
    /// capability store this module owns), `http_client` (the actual send
    /// boundary), `signer` (attaches the chosen format's signature to each
    /// attempt), and `clock` (the capability row's `updated_at`, never
    /// wall-clock time directly, per steering's non-determinism DI
    /// boundary).
    pub fn new(
        pool: PgPool,
        http_client: Arc<H>,
        signer: RequestSigner,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            pool,
            http_client,
            signer,
            clock,
        }
    }

    /// Reads the recorded successful format for `host`, if any (Requirement
    /// 3.3's memory). `None` for a host with no row yet, or with a stored
    /// value this module does not recognize (see [`format_from_db`]) — the
    /// latter is treated the same as "unknown", so negotiation still
    /// proceeds via the default-first-attempt path rather than failing
    /// outright.
    async fn recorded_format(&self, host: &str) -> Result<Option<SignatureFormat>, AppError> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT format FROM instance_signature_capabilities WHERE host = $1")
                .bind(host)
                .fetch_optional(&self.pool)
                .await
                .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;

        Ok(row.and_then(|(format,)| format_from_db(&format)))
    }

    /// Records `format` as the successful format for `host` (Requirement
    /// 3.2), upserting so a host whose previously recorded format has
    /// stopped working — and now succeeds with the other format instead —
    /// gets its recorded format overwritten rather than left stale
    /// (`host` is the table's primary key, `migrations/0004_federation.sql`).
    async fn record_format(&self, host: &str, format: SignatureFormat) -> Result<(), AppError> {
        sqlx::query(
            "INSERT INTO instance_signature_capabilities (host, format, updated_at) \
             VALUES ($1, $2, $3) \
             ON CONFLICT (host) DO UPDATE SET \
                 format = EXCLUDED.format, \
                 updated_at = EXCLUDED.updated_at",
        )
        .bind(host)
        .bind(format_to_db(format))
        .bind(self.clock.now())
        .execute(&self.pool)
        .await
        .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;

        Ok(())
    }

    /// Signs a fresh clone of `req` as `actor` in `format`, then sends it.
    /// Cloning per attempt (rather than mutating a single shared request)
    /// matters because the same *logical* unsigned request may need to be
    /// signed twice with genuinely different formats during one
    /// double-knock (Requirement 3.1) — signing the already-signed first
    /// attempt again would layer a second format's headers onto the first's
    /// instead of producing an independent, correctly-formatted second
    /// attempt.
    async fn send_with_format(
        &self,
        actor: &Handle,
        format: SignatureFormat,
        req: &OutboundRequest,
    ) -> Result<HttpResponse, AppError> {
        let mut signed = req.clone();
        self.signer.sign_request(actor, format, &mut signed).await?;
        self.http_client.send(signed).await
    }

    /// Sends `req` to `host` as `actor`, negotiating the HTTP Signature
    /// format (design.md's exact `SignatureNegotiator` Service Interface).
    ///
    /// Uses `host`'s recorded format first if one exists (Requirement 3.3),
    /// otherwise [`SignatureFormat::DraftCavage`] as the default first
    /// attempt (Requirement 3.1's "既定" — draft-cavage is the format "most
    /// existing ActivityPub implementations" use, per `suite.rs`'s own doc
    /// comment). If that attempt's response is `401 Unauthorized`
    /// (signature-related rejection per this module's resolved heuristic —
    /// see this module's doc comment), resends a fresh copy of `req` signed
    /// with the other format exactly once (Requirement 3.1). Whichever
    /// attempt (if either) receives a `2xx` response has its format recorded
    /// for `host` (Requirement 3.2) before that response is returned. If
    /// neither attempt succeeds, the last response is returned as-is with no
    /// capability recorded, letting the caller apply its own retry/backoff
    /// policy. A transport-level `Err` from either attempt is propagated
    /// immediately — it is not "signature-related", so it never triggers the
    /// second-format retry (this module's doc comment, "Signature-related-
    /// rejection heuristic").
    pub async fn negotiate_and_send(
        &self,
        actor: &Handle,
        host: &str,
        req: OutboundRequest,
    ) -> Result<HttpResponse, AppError> {
        let first_format = self
            .recorded_format(host)
            .await?
            .unwrap_or(SignatureFormat::DraftCavage);
        let first_response = self.send_with_format(actor, first_format, &req).await?;

        if first_response.status.is_success() {
            self.record_format(host, first_format).await?;
            return Ok(first_response);
        }

        if first_response.status != StatusCode::UNAUTHORIZED {
            // General failure (e.g. 403 blocked, 4xx/5xx otherwise): not
            // signature-related, no retry, no capability change.
            return Ok(first_response);
        }

        let retry_format = other_format(first_format);
        let retry_response = self.send_with_format(actor, retry_format, &req).await?;

        if retry_response.status.is_success() {
            self.record_format(host, retry_format).await?;
        }
        Ok(retry_response)
    }
}
