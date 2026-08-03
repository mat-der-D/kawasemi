//! `RemoteResolver` (design.md "Service / サービス層" -> "#### RemoteResolver";
//! Requirements 6.1, 6.2, 6.3, 6.4, 6.5; task 4.3, `Boundary: RemoteResolver`):
//! resolves a [`ParsedQuery::Acct`]/[`ParsedQuery::Url`] to a remote
//! [`AccountRef`]/[`Id`] by orchestrating an outbound WebFinger lookup (for
//! `acct:`) or a federation fetch + JSON-LD safe expansion (for a URL),
//! delegating the actual normalization to accounts-and-instance's
//! [`RemoteAccountFetcher::fetch_and_normalize`] and statuses-core's
//! [`StatusIngestService::ingest_document`] — never reimplementing either
//! (design.md's own Components table: "acct:/URL 解決のオーケストレーショ
//! ン・上流委譲・失敗除外").
//!
//! ## Scope
//! Owns exactly [`Resolved`] and [`RemoteResolver`]/[`RemoteResolver::
//! resolve_remote`] (design.md's literal Service Interface, line ~405-406),
//! plus the small private helpers `resolve_remote` is built from
//! ([`build_webfinger_url`], [`JrdDocument`]/[`JrdLink`]/[`find_self_actor_href`],
//! [`is_actor_type`]). It does not touch `SearchService`/`SearchEndpoint`
//! (task 5.1/5.2, which call `resolve_remote` once `resolve=true` and
//! authenticated — Requirement 6.3, 6.5 — a gate this module does not itself
//! enforce, see "Authentication/`resolve` gating" below), and does not touch
//! `hydrator.rs`/`tag_serializer.rs`/`result_serializer.rs` or any earlier
//! task's boundary.
//!
//! ## `resolve_remote` never returns `Err` (Requirement 6.4)
//! Every failure mode this module's own network/parse/normalization/
//! ingestion calls can produce — a `FederationHttpClient::fetch` error, a
//! non-success upstream status, a malformed/incomplete WebFinger JRD, a
//! JSON-LD parse failure, or a `RemoteAccountFetcher`/`StatusIngestService`
//! normalization/ingestion failure — is caught here and normalized to
//! `Ok(Resolved::None)`, exactly design.md's own inline comment on the
//! pinned signature ("失敗は `Resolved::None` に正規化"). The `Result`
//! return type is kept only because design.md pins it verbatim as part of
//! this component's Service Interface (matching every other `async fn ..
//! -> Result<_, AppError>` port in this crate); as written, this
//! implementation's own `Err` arm is unreachable — `resolve_acct`/
//! `resolve_url` are themselves infallible (`-> Resolved`, not `-> Result<..>`),
//! and [`Self::resolve_remote`] only ever wraps their output in `Ok`.
//!
//! ## `ParsedQuery::Plain` is normalized to `Resolved::None`, not an error
//! design.md's flow diagram (`### type 判定と結果組み立て`) only ever routes
//! `Acct`/`Url` kinds into `resolve_remote` (`resolve=true` gated); a `Plain`
//! query never reaches this component in the intended call graph.
//! `resolve_remote` still accepts the whole [`ParsedQuery`] enum (design.md's
//! own pinned signature, not a narrower `enum RemoteTarget { Acct, Url }`),
//! so a `Plain` argument is representable at the type level even though no
//! documented caller ever passes one. Rather than `unreachable!()` (which
//! would turn a caller-boundary mistake into a panic — a worse failure mode
//! than this task needs to introduce for an input this function's own type
//! signature permits) this module treats `Plain` identically to any other
//! "nothing to resolve" outcome: `Resolved::None`, with no network call at
//! all. This is a deliberate decision for a case design.md does not pin —
//! flagged as a CONCERN for reviewer confirmation, per this task's own
//! instructions.
//!
//! ## Authentication/`resolve` gating is the caller's job, not this module's
//! Requirement 6.3 ("`resolve` を伴わない...間はリモート取得を行わない")
//! and 6.5 ("...認証済みリクエストに限定") are both phrased as constraints on
//! *whether `resolve_remote` is ever called at all* — design.md's flow
//! diagram draws the `resolve=true and authenticated` branch entirely
//! outside this component, at the `SearchService` orchestration layer (task
//! 5.1, not yet implemented). This module has no `resolve`/authentication
//! parameter to gate on and performs no such check itself; it trusts that a
//! caller only invokes it when both conditions already hold.
//!
//! ## WebFinger JRD parsing: `self` link, `activity+json`-family `type`
//! (Requirement 6.1)
//! [`JrdDocument`]/[`JrdLink`] are this module's own minimal `Deserialize`
//! shape for the WebFinger response body — narrower than
//! `federation::endpoints::webfinger`'s own (private, `Serialize`-only,
//! server-side) `JrdDocument`/`JrdLink`, which this module cannot import
//! (private to that module) and would not fit anyway (this module needs to
//! *deserialize* a JRD it fetched, not serialize one it is producing).
//! [`find_self_actor_href`] locates the first `links` entry whose `rel` is
//! literally `"self"` and whose `type` is judged to name an ActivityPub
//! representation by federation-core's own [`crate::federation::jsonld::
//! accepts_activitypub`] predicate (already-reviewed, handles both
//! `application/activity+json` and `application/ld+json`, and ignores
//! `;`-delimited parameters) — reusing that predicate rather than a second,
//! narrower string-equality check keeps this module's own JRD interpretation
//! consistent with how this same crate already judges "is this an
//! ActivityPub representation" everywhere else it asks that question. A
//! missing/empty `links` array, or no entry satisfying both conditions,
//! yields `None` (Requirement 6.4's "取得...失敗").
//!
//! ## Actor URL: a deliberate double fetch (Requirement 6.2)
//! For [`ParsedQuery::Url`], this module always fetches `url` itself first
//! (via its own `http_client`) to run [`crate::federation::jsonld::
//! parse_activity`]'s safe expansion and read the document's own `type` —
//! the only way to know, without any out-of-band hint, whether the resource
//! is an Actor or a `Note`. When the type is one of [`is_actor_type`]'s
//! recognized set, this module then calls `self.account_fetcher.
//! fetch_and_normalize(url)` — task instructions' own explicit direction
//! ("Actor → normalize via the same `RemoteAccountFetcher` path"), which
//! internally issues its *own* fetch of the same `url` (cache-miss path,
//! `remote_fetcher.rs`'s own `fetch_and_upsert`) rather than reusing the
//! already-fetched+parsed document this module holds. This is therefore a
//! genuine, observable double network fetch for every actor-URL resolution —
//! not reusing the first fetch's body would be strictly cheaper, but
//! `RemoteAccountFetcher::fetch_and_normalize`'s own public surface (task 4,
//! already implemented/reviewed, out of this task's boundary to change) only
//! ever accepts an `actor_uri: &str` to fetch itself, with no "I already
//! have the body" entry point (unlike `StatusIngestService`, which exposes
//! both `ingest_url`/`ingest_document` for exactly this reason). Flagged as
//! a CONCERN for reviewer confirmation rather than silently working around
//! `RemoteAccountFetcher`'s boundary (e.g. by duplicating its normalization
//! logic here, which this task's instructions explicitly forbid — "do not
//! reinvent it"). The `acct:` path has no equivalent double fetch: WebFinger
//! only ever returns a JRD (never an actor document itself), so the
//! `fetch_and_normalize(actor_uri)` call there is always the *first* fetch
//! of that actor document.
//!
//! ## `is_actor_type`: a fixed AS2 actor-type whitelist (not spec-pinned)
//! Neither requirements.md nor design.md enumerates which ActivityStreams
//! `type` values count as "an actor" for this module's own type-dispatch
//! (design.md's flow diagram only draws two outcomes, "actor" and "note", as
//! abstract labels). [`is_actor_type`] recognizes the five conventional
//! ActivityStreams/ActivityPub actor types (`Person`, `Service`,
//! `Application`, `Group`, `Organization` — the same set most ActivityPub
//! implementations, including Mastodon, treat as actor kinds); any other
//! `type` (e.g. `Tombstone`, `Question`, `Article`, or a `Note` that fell
//! through to this branch by construction-impossible ordering) is neither an
//! actor nor a `Note` and normalizes to `Resolved::None` — this module
//! cannot resolve a document it does not recognize as either shape. Flagged
//! as a CONCERN for reviewer confirmation, mirroring `remote_fetcher.rs`'s
//! own precedent of documenting an undocumented-but-necessary classification
//! choice inline rather than silently picking one.
//!
//! ## `viewer: Id`: threaded into structured failure diagnostics only
//! (Requirement 9.5)
//! design.md pins `viewer: Id` on [`Self::resolve_remote`]'s signature, but
//! no requirement in this task's boundary (6.1-6.5) has this module *use*
//! `viewer` to change which resource is fetched or how it is normalized —
//! WebFinger/URL resolution targets are identified entirely by the parsed
//! query itself, not by who is asking. This module's own use for `viewer` is
//! Requirement 9.5's cross-cutting diagnostics obligation ("失敗の原因特定に
//! 十分な構造化診断...秘匿値を除く"): every failure path emits a
//! `tracing::warn!` carrying `viewer` alongside the query kind/target and the
//! failure point, via `AppError::status`/`public_message` only (never
//! `source`, which may carry a `Server`-error's internal detail — see
//! `crate::error::AppError`'s own doc comment on what is safe to surface).

#[cfg(test)]
mod tests;

use std::sync::Arc;

use serde::Deserialize;

use crate::accounts::RemoteAccountFetcher;
use crate::domain::{AccountRef, Id};
use crate::error::AppError;
use crate::federation::jsonld::{accepts_activitypub, parse_activity};
use crate::federation::signatures::FederationHttpClient;
use crate::search::model::ParsedQuery;
use crate::statuses::inbound_handlers::{LocalMentionResolver, RemoteActorResolver};
use crate::statuses::ingest_service::StatusIngestService;

/// The conventional ActivityStreams/ActivityPub actor `type` values this
/// module recognizes for a URL resolution (Requirement 6.2). See this
/// module's doc comment ("`is_actor_type`: a fixed AS2 actor-type
/// whitelist") for why this set and not a spec-pinned one.
const ACTOR_TYPES: [&str; 5] = ["Person", "Service", "Application", "Group", "Organization"];

/// Returns whether `activity_type` (an ActivityStreams `type` string, e.g.
/// from [`crate::federation::jsonld::ParsedActivity::activity_type`]) names
/// one of this module's recognized actor types.
fn is_actor_type(activity_type: &str) -> bool {
    ACTOR_TYPES.contains(&activity_type)
}

/// Extracts the `host[:port]` authority portion of an absolute URL, e.g.
/// `"https://example.com/notes/1?x=1"` -> `"example.com"`. This module's own
/// copy of the identical string-parsing helper already duplicated four times
/// across this crate (`crate::federation::signatures::signer::host_from_url`,
/// `crate::federation::outbound::worker::host_from_url`,
/// `crate::statuses::ingest_service::host_from_url` — see that last module's
/// own doc comment, "Actor resolution", for why: each copy is a private
/// helper scoped to its own module, and this crate's URLs are always
/// well-formed absolute URLs it built or fetched itself, so a full
/// URL-parsing dependency is not warranted here either). This is that
/// duplicate's fifth instance, for the identical reason — no punycode/IDN
/// normalization, no separate port handling: `host[:port]` is compared
/// as-is, matching every existing copy's exact semantics.
fn host_from_url(url: &str) -> &str {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    &after_scheme[..end]
}

/// Replicates
/// [`crate::statuses::ingest_service::StatusIngestService::ingest_url`]'s own
/// `check_fetched_host` origin/authority anchor (`src/statuses/
/// ingest_service.rs`, that module's own doc comment "Actor resolution"):
/// verifies that `fetched_url` — the address this resolver itself actually
/// dereferenced, the one thing an attacker-controlled answering server
/// cannot forge — shares a host with `document`'s own claimed `id` field.
///
/// This check exists because `StatusIngestService::ingest_document` alone
/// (which [`RemoteResolver::resolve_url`] must call, not `ingest_url` — see
/// that method's own doc comment for why a third network fetch is
/// unnecessary here) only verifies that a fetched document's `id` and
/// `attributedTo` agree with *each other*, both attacker-supplied: a server
/// at `evil.example` could serve a `Note` at any URL of its own choosing
/// whose `id`/`attributedTo` already agree with each other on some other,
/// real host (e.g. `victim.example`) that `evil.example` never actually
/// controls — internally self-consistent, but laundering content through
/// that host's real actor identity. Anchoring to `fetched_url`, the one
/// value the answering server cannot forge, closes that gap (the same
/// vulnerability class this codebase already found and fixed once for
/// `ingest_url` itself, commit `03dab77`).
///
/// Returns `Ok(())` when the hosts match, or when `document` has no `id` at
/// all — mirrors `check_fetched_host`'s own identical "a missing `id` is not
/// checked here" precedent: `id` presence/shape is validated by
/// `ingest_note_object` downstream regardless of which entry point ingests
/// the document, so a missing `id` is simply left to that downstream
/// rejection rather than duplicating it here. Returns
/// `Err((fetched_host, document_host))` on a mismatch, for the caller's own
/// structured-diagnostics logging. Host comparison is ASCII-case-insensitive
/// (`eq_ignore_ascii_case`), matching every other host comparison in this
/// crate.
fn check_fetched_host<'a>(
    fetched_url: &'a str,
    document: &'a serde_json::Value,
) -> Result<(), (&'a str, &'a str)> {
    let Some(object_uri) = document
        .as_object()
        .and_then(|object| object.get("id"))
        .and_then(serde_json::Value::as_str)
    else {
        return Ok(());
    };
    let fetched_host = host_from_url(fetched_url);
    let document_host = host_from_url(object_uri);
    if fetched_host.eq_ignore_ascii_case(document_host) {
        Ok(())
    } else {
        Err((fetched_host, document_host))
    }
}

/// One WebFinger JRD `links` entry this module reads (RFC 7033 section
/// 4.4.4.1) — a `Deserialize`-only counterpart to
/// `federation::endpoints::webfinger`'s own private, `Serialize`-only
/// `JrdLink` (see this module's doc comment, "WebFinger JRD parsing").
/// `#[serde(default)]` on every field: an unexpected/absent field must never
/// fail deserialization of an otherwise-parseable JRD (mirrors
/// `crate::federation::jsonld::parse_activity`'s "unknown properties never
/// fail parsing" discipline) — a link this module cannot fully interpret
/// simply fails [`find_self_actor_href`]'s match instead.
#[derive(Debug, Deserialize)]
struct JrdLink {
    #[serde(default)]
    rel: String,
    #[serde(default, rename = "type")]
    media_type: String,
    #[serde(default)]
    href: String,
}

/// The WebFinger JRD document this module fetches for an `acct:` resolution
/// (Requirement 6.1). Only `links` is read; `subject`/any other top-level
/// property is never inspected (never fails deserialization either).
#[derive(Debug, Deserialize)]
struct JrdDocument {
    #[serde(default)]
    links: Vec<JrdLink>,
}

/// Locates the first `self` (`rel == "self"`) link in `document` whose
/// `type` names an ActivityPub representation (via
/// [`crate::federation::jsonld::accepts_activitypub`]) and carries a
/// non-empty `href`, returning that `href`. `None` if no such link exists
/// (Requirement 6.4's "取得...失敗" — an unusable JRD is a resolution
/// failure, not a panic).
fn find_self_actor_href(document: &JrdDocument) -> Option<&str> {
    document
        .links
        .iter()
        .find(|link| {
            link.rel == "self" && !link.href.is_empty() && accepts_activitypub(&link.media_type)
        })
        .map(|link| link.href.as_str())
}

/// Builds the outbound WebFinger query URL for `user@domain`
/// (`https://{domain}/.well-known/webfinger?resource=acct:{user}@{domain}`,
/// design.md's exact URL shape, Requirement 6.1), percent-encoding the
/// `resource` query value via [`reqwest::Url`]'s own query-pair encoding
/// (the same URL-parsing dependency `query_parser.rs` already reuses,
/// Requirement 6.1/6.2). Returns `None` if `domain` cannot form a valid URL
/// host — defensive: `QueryParser::parse_query`'s `split_user_domain`
/// already rejects an empty domain segment before a `ParsedQuery::Acct`
/// value can exist, but this helper does not assume that invariant holds
/// forever (Requirement 6.4: an unresolvable/malformed target normalizes to
/// `None`, never a panic).
fn build_webfinger_url(user: &str, domain: &str) -> Option<String> {
    if domain.is_empty() {
        // `reqwest::Url::parse` happily accepts an empty authority
        // (`"https:///..."` parses with an empty, not absent, host), so this
        // must be checked explicitly rather than relying on the parse
        // itself to fail.
        return None;
    }
    let mut url = reqwest::Url::parse(&format!("https://{domain}/.well-known/webfinger")).ok()?;
    url.query_pairs_mut()
        .append_pair("resource", &format!("acct:{user}@{domain}"));
    Some(url.to_string())
}

/// design.md's exact `Resolved` enum (Service Interface, line ~405): the
/// outcome of [`RemoteResolver::resolve_remote`], normalized so a caller
/// never needs to special-case a resolution *failure* separately from
/// "nothing to resolve" (Requirement 6.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    Account(AccountRef),
    Status(Id),
    None,
}

/// Orchestrates `acct:`/URL remote resolution (Requirements 6.1, 6.2, 6.4).
/// See this module's doc comment for the full reasoning behind every
/// constructor dependency and design.md deviation.
///
/// Generic over `H: FederationHttpClient`, `R: RemoteActorResolver`, `M:
/// LocalMentionResolver` — mirrors `RemoteAccountFetcher<H>`'s/
/// `StatusIngestService<H, R, M>`'s identical rationale (both are traits of
/// literal `async fn` methods, not object-safe as `dyn`), and this module
/// wraps both of those generic collaborators directly rather than
/// re-narrowing their type parameters.
pub struct RemoteResolver<H, R, M>
where
    H: FederationHttpClient,
    R: RemoteActorResolver,
    M: LocalMentionResolver,
{
    http_client: Arc<H>,
    account_fetcher: Arc<RemoteAccountFetcher<H>>,
    status_ingest: Arc<StatusIngestService<H, R, M>>,
}

impl<H, R, M> RemoteResolver<H, R, M>
where
    H: FederationHttpClient,
    R: RemoteActorResolver,
    M: LocalMentionResolver,
{
    /// Builds a `RemoteResolver` from already-constructed collaborators
    /// (mirrors this crate's established "bundle, don't build" business-
    /// layer constructor convention, e.g. `SearchHydrator::new`): `http_client`
    /// is this module's own WebFinger/URL-classification fetch boundary
    /// (Requirements 6.1, 6.2); `account_fetcher`/`status_ingest` are the
    /// upstream normalization/ingestion delegates this module never
    /// reimplements (design.md's own Components dependency list).
    pub fn new(
        http_client: Arc<H>,
        account_fetcher: Arc<RemoteAccountFetcher<H>>,
        status_ingest: Arc<StatusIngestService<H, R, M>>,
    ) -> Self {
        Self {
            http_client,
            account_fetcher,
            status_ingest,
        }
    }

    /// design.md's exact Service Interface signature (line ~406). See this
    /// module's doc comment ("`resolve_remote` never returns `Err`") for why
    /// this always returns `Ok`.
    pub async fn resolve_remote(
        &self,
        parsed: &ParsedQuery,
        viewer: Id,
    ) -> Result<Resolved, AppError> {
        let resolved = match parsed {
            ParsedQuery::Acct { user, domain } => self.resolve_acct(user, domain, viewer).await,
            ParsedQuery::Url(url) => self.resolve_url(url, viewer).await,
            // See this module's doc comment ("`ParsedQuery::Plain` is
            // normalized to `Resolved::None`, not an error").
            ParsedQuery::Plain(_) => Resolved::None,
        };
        Ok(resolved)
    }

    /// Requirement 6.1: outbound WebFinger (`self` link -> actor_uri), then
    /// `RemoteAccountFetcher::fetch_and_normalize` -> `Resolved::Account`.
    /// Any failure at any step normalizes to `Resolved::None` (Requirement
    /// 6.4), logged via `tracing::warn!` (Requirement 9.5).
    async fn resolve_acct(&self, user: &str, domain: &str, viewer: Id) -> Resolved {
        let Some(webfinger_url) = build_webfinger_url(user, domain) else {
            tracing::warn!(
                query_kind = "acct",
                domain,
                viewer = viewer.as_i64(),
                "acct domain does not form a valid webfinger URL; excluding from search results"
            );
            return Resolved::None;
        };

        let response = match self.http_client.fetch(&webfinger_url, None).await {
            Ok(response) => response,
            Err(err) => {
                tracing::warn!(
                    query_kind = "acct",
                    domain,
                    viewer = viewer.as_i64(),
                    failure_point = "webfinger_fetch",
                    status = %err.status,
                    error = %err.public_message,
                    "webfinger fetch failed; excluding from search results"
                );
                return Resolved::None;
            }
        };
        if !response.status.is_success() {
            tracing::warn!(
                query_kind = "acct",
                domain,
                viewer = viewer.as_i64(),
                failure_point = "webfinger_status",
                status = %response.status,
                "webfinger fetch returned a non-success status; excluding from search results"
            );
            return Resolved::None;
        }

        let jrd: JrdDocument = match serde_json::from_slice(&response.body) {
            Ok(jrd) => jrd,
            Err(err) => {
                tracing::warn!(
                    query_kind = "acct",
                    domain,
                    viewer = viewer.as_i64(),
                    failure_point = "webfinger_jrd_parse",
                    error = %err,
                    "webfinger response body is not a valid JRD; excluding from search results"
                );
                return Resolved::None;
            }
        };
        let Some(actor_uri) = find_self_actor_href(&jrd) else {
            tracing::warn!(
                query_kind = "acct",
                domain,
                viewer = viewer.as_i64(),
                failure_point = "webfinger_self_link",
                "webfinger JRD has no matching self link; excluding from search results"
            );
            return Resolved::None;
        };

        match self.account_fetcher.fetch_and_normalize(actor_uri).await {
            Ok(account) => Resolved::Account(AccountRef::Remote(account.id)),
            Err(err) => {
                tracing::warn!(
                    query_kind = "acct",
                    domain,
                    viewer = viewer.as_i64(),
                    failure_point = "remote_account_normalize",
                    status = %err.status,
                    error = %err.public_message,
                    "remote account normalization failed; excluding from search results"
                );
                Resolved::None
            }
        }
    }

    /// Requirement 6.2: federation fetch + JSON-LD safe expansion to
    /// classify `url` as an actor or a `Note`, then delegate to the matching
    /// upstream normalizer/ingester. See this module's doc comment ("Actor
    /// URL: a deliberate double fetch") for the actor branch's own network
    /// cost. Any failure at any step normalizes to `Resolved::None`
    /// (Requirement 6.4), logged via `tracing::warn!` (Requirement 9.5).
    async fn resolve_url(&self, url: &str, viewer: Id) -> Resolved {
        let response = match self.http_client.fetch(url, None).await {
            Ok(response) => response,
            Err(err) => {
                tracing::warn!(
                    query_kind = "url",
                    url,
                    viewer = viewer.as_i64(),
                    failure_point = "url_fetch",
                    status = %err.status,
                    error = %err.public_message,
                    "federation fetch failed; excluding from search results"
                );
                return Resolved::None;
            }
        };
        if !response.status.is_success() {
            tracing::warn!(
                query_kind = "url",
                url,
                viewer = viewer.as_i64(),
                failure_point = "url_status",
                status = %response.status,
                "federation fetch returned a non-success status; excluding from search results"
            );
            return Resolved::None;
        }

        let parsed = match parse_activity(&response.body) {
            Ok(parsed) => parsed,
            Err(err) => {
                tracing::warn!(
                    query_kind = "url",
                    url,
                    viewer = viewer.as_i64(),
                    failure_point = "url_jsonld_parse",
                    status = %err.status,
                    error = %err.public_message,
                    "fetched document failed JSON-LD safe expansion; excluding from search results"
                );
                return Resolved::None;
            }
        };

        if parsed.activity_type == "Note" {
            // Origin/authority anchor (see this module's doc comment on
            // `check_fetched_host`): `ingest_document` alone only checks the
            // document's own `id` against its own `attributedTo` — both
            // attacker-supplied — never against the URL this resolver
            // itself actually dereferenced. Reject before ever calling
            // `ingest_document` on a host mismatch, rather than trusting a
            // self-consistent-but-wrong-origin document.
            if let Err((fetched_host, document_host)) = check_fetched_host(url, &parsed.raw) {
                tracing::warn!(
                    query_kind = "url",
                    url,
                    viewer = viewer.as_i64(),
                    failure_point = "url_origin_authority_check",
                    fetched_host,
                    document_host,
                    "fetched document's own id host does not match the fetched url's host (origin/authority check failed); excluding from search results"
                );
                return Resolved::None;
            }

            match self.status_ingest.ingest_document(&parsed.raw).await {
                Ok(status) => Resolved::Status(status.id),
                Err(err) => {
                    tracing::warn!(
                        query_kind = "url",
                        url,
                        viewer = viewer.as_i64(),
                        failure_point = "status_ingest",
                        status = %err.status,
                        error = %err.public_message,
                        "remote Note ingestion failed; excluding from search results"
                    );
                    Resolved::None
                }
            }
        } else if is_actor_type(&parsed.activity_type) {
            match self.account_fetcher.fetch_and_normalize(url).await {
                Ok(account) => Resolved::Account(AccountRef::Remote(account.id)),
                Err(err) => {
                    tracing::warn!(
                        query_kind = "url",
                        url,
                        viewer = viewer.as_i64(),
                        failure_point = "remote_account_normalize",
                        status = %err.status,
                        error = %err.public_message,
                        "remote account normalization failed; excluding from search results"
                    );
                    Resolved::None
                }
            }
        } else {
            tracing::warn!(
                query_kind = "url",
                url,
                viewer = viewer.as_i64(),
                failure_point = "url_unrecognized_type",
                activity_type = %parsed.activity_type,
                "fetched document is neither a Note nor a recognized actor type; excluding from search results"
            );
            Resolved::None
        }
    }
}
