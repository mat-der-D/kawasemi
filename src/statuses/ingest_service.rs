//! `StatusIngestService` (design.md Boundary Commitments, line ~45: "リモー
//! ト投稿取り込みエントリポイント `StatusIngestService`（ドキュメント/URL →
//! Status）: 既存の受信 Create(Note) 正規化パスを再利用し、受信 Activity
//! ディスパッチの外（search の `RemoteResolver` 等）からも呼び出せる形で公開
//! する。"; Requirements 14.1, 14.2, 14.3; task 6.2, `Boundary:
//! StatusIngestService`; `_Depends: 6.1_`): ingests a single remote `Note`
//! document — given either its dereferenceable `uri` (fetched by this
//! service) or an already-fetched JSON-LD [`serde_json::Value`] — into a
//! local [`Status`] row, from *outside* federation-core's inbound Activity
//! dispatch path.
//!
//! ## Scope
//! Owns exactly [`StatusIngestService`] and its two public operations,
//! [`StatusIngestService::ingest_url`] / [`StatusIngestService::ingest_document`].
//! Does not reimplement HTTP transport (delegates to the already-implemented
//! [`FederationHttpClient`] port, task 1.4, mirroring
//! `crate::accounts::remote_fetcher::RemoteAccountFetcher`'s identical
//! fetch-then-normalize structure), does not reimplement JSON-LD expansion
//! (delegates to [`crate::federation::jsonld::parse_activity`]), and — the
//! whole point of this task — does not reimplement `Note`
//! normalization/persistence: it calls
//! [`crate::statuses::inbound_handlers::ingest_note_object`] verbatim, the
//! exact function `CreateNoteHandler`'s own inbound `Create(Note)` dispatch
//! path calls (task 6.1), so this entry point can never silently diverge in
//! behavior from the inbound-dispatch path for the same input (Requirement
//! 14.5's "共通コードパス" discipline; this task's own observable-completion
//! criterion, "受信ハンドラ経路と同一結果になる"). This module is not wired
//! into `AppState`/bootstrap/any live HTTP path, nor into the `search`
//! spec's `RemoteResolver` (a future spec that does not exist yet in this
//! codebase, and task 7.2's own module-wiring boundary regardless) — it is a
//! standalone, independently unit-testable service with no live caller yet,
//! mirroring `inbound_handlers.rs`'s/`activity_builder.rs`'s own identical
//! "no live caller yet" precedent.
//!
//! ## Actor resolution: the fetched document's own `attributedTo`, gated by
//! an origin/authority check (a deliberate divergence from
//! `CreateNoteHandler`)
//! [`crate::statuses::inbound_handlers::CreateNoteHandler`] resolves the
//! acting actor from `ctx.signer.actor_uri` — the HTTP-Signature-verified
//! identity federation-core's inbound dispatch already established —
//! specifically because an inbound *push* delivery's claimed `attributedTo`
//! is unauthenticated, attacker-controlled data (see that module's own doc
//! comment, "Resolving the acting remote actor: `ctx.signer.actor_uri`,
//! never the JSON body's own `actor`/`attributedTo` property"). This
//! service has no such signer at all: it is reached by an out-of-dispatch
//! caller *actively dereferencing* a URL (or handed a document from doing
//! exactly that) directly from the object's own origin server over
//! TLS-terminated HTTPS. That is weaker than, not "the identical" trust
//! model as,
//! [`crate::accounts::remote_fetcher::RemoteAccountFetcher::fetch_and_normalize`]:
//! that function resolves an identity by fetching the exact URI naming that
//! identity, so the fetched host and the resolved identity coincide by
//! construction — nothing else can claim to *be* that URI. Here, a fetched
//! document's own `id` and its claimed `attributedTo` are two independent
//! URIs on the wire; without a check, a server at `evil.example` could serve
//! a `Note` at any URL of its own choosing with `attributedTo:
//! "https://victim.example/users/victim"`, and this service would resolve
//! that URI to the real, unrelated `victim.example` actor and attribute
//! `evil.example`'s content to them. This service therefore enforces a
//! standard ActivityPub origin/authority check before ever trusting
//! `attributedTo`: the document's own `id` must share a host with its
//! `attributedTo` (see [`host_from_url`]) — i.e., only a document's own
//! origin server may vouch for who authored it. A host mismatch is rejected
//! as a malformed document (this module's own `422` convention, see
//! "Non-`Note` / malformed documents" below) *before* `attributedTo` is ever
//! resolved to an [`Id`]. Only once that check passes does this service read
//! `attributedTo` as the authoritative author, then resolve it to a stable
//! [`Id`] via the same
//! [`crate::statuses::inbound_handlers::RemoteActorResolver`] port
//! `CreateNoteHandler` uses (task 6.1's own delegation port, reused as-is —
//! not a second, narrower actor-resolution seam).
//!
//! ## `ingest_url` vs. `ingest_document`: task 6.2's own literal
//! "ドキュメント/URL" wording, as two entry points
//! [`Self::ingest_url`] fetches `url` via [`FederationHttpClient::fetch`] and
//! delegates to [`Self::ingest_document`]; [`Self::ingest_document`] accepts
//! an already-parsed [`serde_json::Value`] directly, so a caller that
//! already has the document in hand (e.g. a future search `RemoteResolver`
//! that fetched/cached it for its own reasons) need not round-trip it
//! through this service's own network call a second time. Both converge on
//! the identical [`ingest_note_object`] call.
//!
//! ## Non-`Note` / malformed documents: a `422`, mirroring this crate's own
//! established fetch-and-normalize error-shape convention
//! A document whose top-level `type` is not the literal string `"Note"`,
//! whose `attributedTo` is absent/uninterpretable, or whose own `id` host
//! does not match its `attributedTo` host (the "Actor resolution"
//! origin/authority check above), is rejected with a
//! `422 Unprocessable Entity` [`AppError`] — the same status
//! [`crate::federation::jsonld::parse_activity`] and
//! `RemoteAccountFetcher`'s own required-property checks already use for
//! "fetched a syntactically valid document that is not the shape this
//! operation needs". An unsuccessful upstream fetch in [`Self::ingest_url`]
//! maps to a caller-facing `404 Not Found`, mirroring
//! `RemoteAccountFetcher::fetch_and_upsert`'s identical non-success-status
//! mapping (that module's own doc comment, "Error mapping").

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::Value;
use sqlx::postgres::PgPool;

use crate::error::AppError;
use crate::federation::jsonld::parse_activity;
use crate::federation::signatures::FederationHttpClient;
use crate::runtime::RuntimeContext;
use crate::statuses::inbound_handlers::{
    RemoteActorResolver, ingest_note_object, object_reference_uri,
};
use crate::statuses::model::Status;

/// Builds a `422 Unprocessable Entity` [`AppError`] — this module's own
/// local copy of the identical one-line helper every sibling
/// fetch-and-normalize module in this crate defines for itself (e.g.
/// `inbound_handlers.rs::malformed`, `remote_fetcher.rs`'s own inline
/// `AppError::client(StatusCode::UNPROCESSABLE_ENTITY, ...)` call sites) —
/// mirrors that established "small, self-contained helper per module" choice
/// rather than reaching across a module boundary for a single-line utility.
fn malformed(message: impl Into<String>) -> AppError {
    AppError::client(StatusCode::UNPROCESSABLE_ENTITY, message.into())
}

/// Extracts the `host[:port]` authority portion of an absolute URL, e.g.
/// `"https://example.com/notes/1?x=1"` -> `"example.com"`, for the
/// origin/authority check in [`StatusIngestService::ingest_document`] (this
/// module's own doc comment, "Actor resolution"). `signer.rs`'s own doc
/// comment (task 1.5's Implementation Note) already flags this exact
/// string-parsing helper as duplicated once, from `suite.rs`'s
/// `path_and_query`, then a second time independently in
/// `outbound/worker.rs` (that module's own doc comment, "`host_from_url`: a
/// third duplicate..."); this is now a fourth, independent copy, for the
/// identical reason those two give — each existing copy is a private (`!pub`)
/// helper scoped to its own module, and this crate's URLs are always
/// well-formed absolute URLs it built or fetched itself, so a full
/// URL-parsing dependency is not warranted here either. If a fifth site ever
/// needs this, extracting a small shared `federation::urls`-adjacent helper
/// is worth reconsidering (flagged here, not acted on, per this task's own
/// narrow remediation boundary).
fn host_from_url(url: &str) -> &str {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    &after_scheme[..end]
}

/// Ingests a single remote `Note` (design.md's exact `StatusIngestService`;
/// Requirements 14.1, 14.2, 14.3) from either a URL this service fetches
/// itself ([`Self::ingest_url`]) or an already-fetched JSON-LD document
/// ([`Self::ingest_document`]), reusing
/// [`crate::statuses::inbound_handlers::ingest_note_object`] verbatim so
/// this entry point never diverges from the inbound `Create(Note)` dispatch
/// path's own behavior for the same input. See this module's doc comment for
/// the full fetch/actor-resolution/error-mapping contract.
///
/// Generic over `H: FederationHttpClient` (held as `Arc<H>`) and
/// `R: RemoteActorResolver` (held as `Arc<R>`), mirroring
/// `RemoteAccountFetcher<H>`'s/`CreateNoteHandler<R>`'s identical rationale:
/// both are traits of literal `async fn` methods, not object-safe as `dyn`.
pub struct StatusIngestService<H: FederationHttpClient, R: RemoteActorResolver> {
    pool: PgPool,
    http_client: Arc<H>,
    runtime: RuntimeContext,
    remote_actors: Arc<R>,
}

impl<H: FederationHttpClient, R: RemoteActorResolver> StatusIngestService<H, R> {
    /// Builds a service against `pool` (`StatusRepository`'s connection
    /// pool, shared with `CreateNoteHandler`'s own), `http_client` (the
    /// URL-fetch network boundary [`Self::ingest_url`] uses), `runtime` (the
    /// injected clock/id boundaries [`ingest_note_object`] threads through),
    /// and `remote_actors` (the `attributedTo` -> [`crate::domain::Id`]
    /// resolution port, the same [`RemoteActorResolver`] implementation a
    /// caller wires up for `CreateNoteHandler`).
    pub fn new(
        pool: PgPool,
        http_client: Arc<H>,
        runtime: RuntimeContext,
        remote_actors: Arc<R>,
    ) -> Self {
        Self {
            pool,
            http_client,
            runtime,
            remote_actors,
        }
    }

    /// Fetches `url` over the network and ingests the result as a `Note`
    /// (task 6.2's "URL... → Status"). Fails with a `404 Not Found`
    /// [`AppError`] if the upstream fetch does not return a success status,
    /// mirroring `RemoteAccountFetcher::fetch_and_upsert`'s identical
    /// mapping; propagates whatever [`AppError`] a transport-level fetch
    /// failure itself already produced otherwise.
    pub async fn ingest_url(&self, url: &str) -> Result<Status, AppError> {
        let response = self.http_client.fetch(url, None).await?;
        if !response.status.is_success() {
            return Err(AppError::client(
                StatusCode::NOT_FOUND,
                format!(
                    "remote status '{url}' could not be fetched (upstream status {})",
                    response.status
                ),
            ));
        }

        let parsed = parse_activity(&response.body)?;
        self.ingest_document(&parsed.raw).await
    }

    /// Ingests an already-fetched JSON-LD `document` as a `Note` (task 6.2's
    /// "ドキュメント... → Status"). See this module's doc comment
    /// ("Actor resolution") for why this reads `attributedTo` directly
    /// rather than a verified signer, and ("Non-`Note` / malformed
    /// documents") for this method's `422` error mapping.
    pub async fn ingest_document(&self, document: &Value) -> Result<Status, AppError> {
        let Some(object) = document.as_object() else {
            return Err(malformed("ingested document must be a JSON object"));
        };
        if object.get("type").and_then(Value::as_str) != Some("Note") {
            return Err(malformed("ingested document is not a Note"));
        }
        let Some(attributed_to) = object_reference_uri(object.get("attributedTo")) else {
            return Err(malformed(
                "Note object is missing a required 'attributedTo' property",
            ));
        };

        // Origin/authority check (this module's own doc comment, "Actor
        // resolution"): the document's own `id` must share a host with its
        // claimed `attributedTo`, so only a document's own origin server can
        // vouch for who authored it. `object`'s `id` presence/shape is itself
        // validated by `ingest_note_object` below (its own `422` on a
        // missing `id`), so a missing `id` here is simply not checked —
        // that downstream rejection covers it without duplicating it.
        if let Some(object_uri) = object.get("id").and_then(Value::as_str) {
            let document_host = host_from_url(object_uri);
            let attributed_to_host = host_from_url(attributed_to);
            if document_host != attributed_to_host {
                return Err(malformed(format!(
                    "Note object 'id' host '{document_host}' does not match \
                     its 'attributedTo' host '{attributed_to_host}' \
                     (origin/authority check failed)"
                )));
            }
        }

        let actor_id = self
            .remote_actors
            .resolve_remote_actor(attributed_to)
            .await?;

        ingest_note_object(&self.pool, &self.runtime, object, actor_id).await
    }
}
