//! `RemoteAccountFetcher` (design.md "Service / サービス層" ->
//! `RemoteAccountFetcher`; Requirements 7.1, 7.2, 7.3, 7.4, 7.5; task 4,
//! `Boundary: RemoteAccountFetcher`): fetches an ActivityPub actor document
//! for a not-yet-cached or stale `actor_uri`, safely normalizes it into a
//! [`RemoteAccount`], and upserts the result into
//! `RemoteAccountRepository`'s cache -- reusing a valid cache entry without
//! any network call while it remains fresh.
//!
//! Scope: this module owns exactly [`RemoteAccountFetcher`] and its single
//! public operation, [`RemoteAccountFetcher::fetch_and_normalize`] (design.md's
//! literal Service Interface: `pub async fn fetch_and_normalize(&self,
//! actor_uri: &str) -> Result<RemoteAccount, AppError>`). It does not
//! reimplement HTTP transport (delegates to the already-implemented
//! [`FederationHttpClient`]), does not reimplement JSON-LD expansion
//! (delegates to the already-implemented [`crate::federation::jsonld::parse_activity`]),
//! and does not touch `RemoteAccountRepository`'s own SQL (calls its already-
//! implemented [`find_remote_by_uri`]/[`upsert_remote`]/[`is_stale`] as-is,
//! task 2.2, out of this task's boundary).
//!
//! ## Structural precedent: mirrors `key_resolver.rs`'s `DbFederationPublicKeyResolver`
//! `src/federation/signatures/key_resolver.rs`'s `DbFederationPublicKeyResolver`
//! already solves the *identical* shape of problem one layer down in this
//! same crate -- cache-first, `FederationHttpClient`-fetch-on-miss/stale,
//! parse-then-upsert -- for a different cached entity (`RemotePublicKey`
//! keyed by `key_id`, vs. this module's `RemoteAccount` keyed by
//! `actor_uri`). This module deliberately mirrors that module's structure
//! (generic `<H: FederationHttpClient>` held as `Arc<H>` rather than `Arc<dyn
//! FederationHttpClient>` -- see that module's own doc comment for why a
//! trait with literal `async fn` methods is not object-safe; a TTL
//! constructor parameter with a documented `DEFAULT_*` constant; a private
//! `fetch_and_upsert` helper the public method falls through to on a cache
//! miss/stale/force-equivalent path) rather than inventing a second
//! convention for the same kind of cache-then-fetch service.
//!
//! ## TTL: this task's own cache-policy decision (Requirement 7.3)
//! Neither requirements.md nor design.md names a concrete TTL value
//! anywhere -- Requirement 7.3 only says "正規化済みリモートアカウントが
//! 有効に保持されている間" (while it remains validly held), without a
//! number -- and `remote_repository.rs`'s own doc comment (task 2.2,
//! "Reconciling `fetched_at` 陳腐化判定...") explicitly defers the concrete
//! TTL choice to this task. [`DEFAULT_REMOTE_ACCOUNT_CACHE_TTL`] reuses this
//! same crate's own already-reviewed precedent for an equivalent cache --
//! `key_resolver.rs::DEFAULT_PUBLIC_KEY_CACHE_TTL` -- verbatim (`Duration::hours(24)`,
//! design.md's own documented default for the sibling actor-document cache,
//! "既定 24 時間"). Both caches hold data derived from the same kind of
//! artifact (a remote ActivityPub actor document) and both exist for the
//! same reason (avoid re-fetching a remote actor on every request), so
//! reusing the identical, already-spec-documented value is a deliberate
//! choice, not an arbitrary new number invented for this task alone. `ttl`
//! remains a plain constructor parameter (not read from config) for the same
//! reason `key_resolver.rs` gives: wiring a `federation`/`accounts` config
//! section is a later bootstrap task's boundary, not this one's.
//!
//! ## Error mapping (Requirement 7.4)
//! - A [`FederationHttpClient::fetch`] failure (`Err`) is propagated as-is:
//!   whatever [`AppError`] the client produced (e.g. a real
//!   `ReqwestFederationHttpClient`'s `502 Bad Gateway` on a network error, or
//!   a test's queued failure) already represents "the fetch itself failed",
//!   satisfying Requirement 7.4's "取得...に失敗したとき" without this module
//!   re-wrapping it.
//! - A non-success HTTP status on an otherwise-successful fetch (e.g. the
//!   remote returns `404`/`410`/`5xx` for this actor URI) maps to a caller-facing
//!   (`ErrorKind::Client`) `404 Not Found` [`AppError`]. This is a deliberate
//!   choice distinct from `key_resolver.rs`'s own non-success mapping (a
//!   `Server` `502`): that resolver's caller (`SignatureVerifier`) treats a
//!   failed keyId fetch as an internal signature-verification failure,
//!   whereas this fetcher's caller (the future `AccountService::show_account`,
//!   task 5.1, Requirement 3.3: "未存在は404") ultimately needs to render
//!   "this remote account could not be found" to an HTTP client as a
//!   Mastodon-compatible `404`. Bundling every fetch-side failure (network
//!   error aside, which already propagates its own status) into `404` here
//!   means task 5.1 does not need a second translation step from
//!   "fetch-failed" to "not found" -- matching design.md's own flowchart,
//!   which draws exactly one `Err` box ("not found or fetch error mastodon
//!   response") for both the fetch-failure and non-success-status cases.
//! - A missing/uninterpretable required property (`type`/`id` via
//!   [`crate::federation::jsonld::parse_activity`]'s own validation, or
//!   `preferredUsername` via this module's own [`required_username`]) maps to
//!   a caller-facing `422 Unprocessable Entity` [`AppError`], mirroring
//!   `jsonld::parse::parse_activity`'s own established status choice for the
//!   identical class of failure (required-property absence). No upsert is
//!   attempted in this path (Requirement 7.4: "そのアカウントを生成せず").
//!
//! ## What counts as a "required property" (Requirement 7.4)
//! design.md's Responsibilities & Constraints prose names the required set
//! as "標準フィールドのみ正規化...必須プロパティ（type/id/preferredUsername
//! 等）欠落は失敗扱い". `type`/`id` are already enforced by
//! [`crate::federation::jsonld::parse_activity`] (non-empty string,
//! Requirement 9.3 of federation-core's own spec); this module adds exactly
//! one more required check on top -- `preferredUsername` (a non-empty
//! string) -- since `RemoteAccount::username` has no sensible default (unlike
//! `display_name`/`note`, which fall back to `""`, or `locked`, which falls
//! back to `false`: an accountless "username" would corrupt `Acct::remote`'s
//! own `user@domain` rendering discipline, Requirement 1.3). Every other
//! field this module reads (`name`, `summary`, `url`, `icon`, `image`,
//! `attachment`, `manuallyApprovesFollowers`) is genuinely optional per the
//! ActivityStreams vocabulary and is normalized with a safe default rather
//! than treated as a failure -- see each field's own extraction helper below.
//!
//! ## Unknown extension properties never fail normalization (Requirement 7.5)
//! [`crate::federation::jsonld::parse_activity`] already parses the whole
//! document into a generic [`serde_json::Value`] and never fails on a
//! property it does not itself read (its own doc comment: "any property
//! this codec does not recognize is simply never inspected, so it can never
//! cause a parse failure"). This module's own field-extraction helpers
//! ([`optional_string`], [`extract_image_url`], [`extract_fields`]) follow
//! the identical discipline -- each reads only the one named property it
//! needs (`name`/`summary`/`url`/`icon`/`image`/`attachment`/
//! `manuallyApprovesFollowers`) and silently ignores everything else on the
//! document, so a vendor-specific extension property (e.g. a custom
//! `"toot:indexable"` field some fediverse software adds) is present in the
//! parsed `Value` this module reads from but is simply never looked at --
//! it can never trigger any of this module's own error paths.
//!
//! ## `domain`: derived from the input `actor_uri`, not the parsed `id`
//! [`RemoteAccount::domain`] is derived from the exact `actor_uri` argument
//! this method was called with -- not from the fetched document's own `id`
//! property -- because [`find_remote_by_uri`]/[`upsert_remote`]'s cache key
//! *is* `actor_uri` (task 2.2's `actor_uri UNIQUE` constraint): a later
//! `fetch_and_normalize` call with the same `actor_uri` argument must always
//! find the same cached row, regardless of whether a given remote server's
//! document happens to report a `id` that differs from the URI it was
//! fetched at (e.g. a redirect, or a server misconfiguration). In the
//! well-behaved case (the overwhelming majority of real ActivityPub actors,
//! whose `id` is defined to equal their own dereferenceable IRI) the two
//! values already agree, so this choice changes nothing observable; it only
//! matters for the ill-behaved case, where keying on the request URI (not
//! the response body) keeps this fetcher's own cache-consistency invariant
//! intact. [`host_from_actor_uri`] duplicates (rather than imports)
//! `signer.rs::host_from_url`'s identical string-parsing logic -- that
//! function is private to its own module and this task must not edit
//! `federation/signatures/signer.rs` to expose it (out of this task's
//! `RemoteAccountFetcher` boundary) -- the same "small, self-contained,
//! duplicated rather than cross-module-coupled" tradeoff `remote_repository.rs`
//! itself already established for its own `fields_to_json`/`fields_from_json`
//! pair.
//!
//! ## `id` stability across re-fetches of the same `actor_uri`
//! [`upsert_remote`]'s own doc comment (task 2.2) already establishes that
//! its `ON CONFLICT (actor_uri) DO UPDATE` excludes `id` from the `SET`
//! list, so whatever `id` this module passes in a [`RemoteAccount`] value is
//! only ever *used* on that `actor_uri`'s very first insert -- every
//! subsequent re-upsert for the same `actor_uri` keeps the row's original,
//! already-persisted `id` no matter what this module supplies. This module
//! therefore always mints a fresh `id` via `self.runtime.ids.next_id()` for
//! every fetch-and-upsert call, whether the cache row already existed
//! (stale re-fetch) or not (first fetch) -- `upsert_remote`'s own guarantee
//! makes it unnecessary (and no simpler) to look up and thread through the
//! existing row's `id` first.
//!
//! ## `bot`: reuses this crate's own established `Service` == bot convention
//! `src/accounts/serializer.rs`'s own doc comment already establishes, for
//! this exact codebase, that "`crate::actor::model::ActorType` only
//! distinguishes `Person`/`Service` (BOT)" -- i.e. this codebase's own local
//! actor model already treats the ActivityStreams `Service` actor type as
//! this crate's "bot" concept. This module reuses that identical convention
//! for remote actors: `bot` is `true` exactly when the parsed document's
//! top-level `type` is the literal string `"Service"`, `false` for `"Person"`
//! and every other value -- consistent with, not a divergent policy from,
//! how this same crate already renders its own local bot actors.

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::Value;
use sqlx::postgres::PgPool;
use time::{Duration, OffsetDateTime};

use super::model::{ProfileField, RemoteAccount};
use super::remote_repository::{find_remote_by_uri, is_stale, upsert_remote};
use crate::domain::Id;
use crate::error::AppError;
use crate::federation::jsonld::{ParsedActivity, parse_activity};
use crate::federation::signatures::FederationHttpClient;
use crate::runtime::RuntimeContext;

/// Documented default cache TTL for [`RemoteAccountFetcher`]. See this
/// module's doc comment ("TTL: this task's own cache-policy decision") for
/// why this reuses `key_resolver.rs::DEFAULT_PUBLIC_KEY_CACHE_TTL`'s exact
/// value rather than inventing a new one.
pub const DEFAULT_REMOTE_ACCOUNT_CACHE_TTL: Duration = Duration::hours(24);

/// Reads `key` off `map` as a plain string, or `None` if absent/not a
/// string. Shared by every genuinely-optional field this module normalizes
/// (Requirement 7.5: unrecognized/absent properties never fail
/// normalization).
fn optional_string(map: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    map.get(key).and_then(Value::as_str).map(str::to_string)
}

/// Reads the required `preferredUsername` property (Requirement 7.4) as a
/// non-empty string, or a `422 Unprocessable Entity` [`AppError`]. See this
/// module's doc comment ("What counts as a 'required property'") for why
/// this is the one additional required check this module adds on top of
/// `parse_activity`'s own `type`/`id` validation.
fn required_username(map: &serde_json::Map<String, Value>) -> Result<String, AppError> {
    map.get("preferredUsername")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            AppError::client(
                StatusCode::UNPROCESSABLE_ENTITY,
                "ActivityPub actor document missing required 'preferredUsername' property",
            )
        })
}

/// Extracts an image URL from an ActivityStreams `icon`/`image` property
/// value, tolerating every commonly-seen shape (Requirement 7.5): a bare
/// string URL, a single `{"type":"Image","url":"..."}`-shaped object (the
/// conventional ActivityStreams `Image` object -- only its `url` member is
/// read, so any other member on that object, recognized or not, is never
/// inspected), or an array of either (the first extractable URL wins).
/// Returns `None` for an absent property or a shape this function cannot
/// interpret -- never an error, since `icon`/`image` are optional
/// (Requirement 7.2's normalized-field list marks avatar/header as
/// `Option`).
fn extract_image_url(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(url) => Some(url.clone()),
        Value::Object(object) => extract_image_url(object.get("url")),
        Value::Array(items) => items.iter().find_map(|item| extract_image_url(Some(item))),
        _ => None,
    }
}

/// Extracts `fields` from an ActivityStreams `attachment` property value
/// (Requirement 7.2's normalized `fields`). Reads only conventional
/// `PropertyValue` entries (an entry carrying a `type` other than
/// `"PropertyValue"` is skipped, not an error -- e.g. a document that
/// attaches an image via `attachment` rather than `icon`); an entry with no
/// `type` at all is still accepted if it carries string `name`/`value`
/// (lenient, since `type` itself is not universally present on every
/// real-world implementation's attachment entries). Any entry missing a
/// usable `name`/`value` pair is silently skipped rather than failing the
/// whole document -- `attachment`/`fields` is not among Requirement 7.4's
/// required properties. `verified_at` is always `None`: this document shape
/// carries no timestamp for it (Mastodon's own rel-me verification is a
/// separate out-of-band process this spec does not perform, matching
/// `RemoteAccount::fields`'s own normalized-cache nature -- it stores what
/// was fetched, not a freshly-computed verification).
fn extract_fields(value: Option<&Value>) -> Vec<ProfileField> {
    let Some(Value::Array(items)) = value else {
        return Vec::new();
    };

    items
        .iter()
        .filter_map(|item| {
            let object = item.as_object()?;
            if let Some(item_type) = object.get("type").and_then(Value::as_str)
                && item_type != "PropertyValue"
            {
                return None;
            }
            let name = object.get("name").and_then(Value::as_str)?.to_string();
            let value = object.get("value").and_then(Value::as_str)?.to_string();
            Some(ProfileField {
                name,
                value,
                verified_at: None,
            })
        })
        .collect()
}

/// Extracts the `host[:port]` authority portion of an absolute URL. See this
/// module's doc comment ("`domain`: derived from the input `actor_uri`") for
/// why this duplicates, rather than imports,
/// `federation/signatures/signer.rs::host_from_url`'s identical logic.
/// Fails with a `422 Unprocessable Entity` [`AppError`] if `actor_uri` has no
/// interpretable host at all (e.g. an empty string) -- a `RemoteAccount`
/// with an empty `domain` would corrupt `Acct::remote`'s `user@domain`
/// rendering (Requirement 1.3), so this is treated the same as any other
/// required-property failure rather than silently defaulted.
fn host_from_actor_uri(actor_uri: &str) -> Result<String, AppError> {
    let after_scheme = actor_uri
        .split_once("://")
        .map_or(actor_uri, |(_, rest)| rest);
    let end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    let host = &after_scheme[..end];

    if host.is_empty() {
        return Err(AppError::client(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("actor_uri '{actor_uri}' has no host to derive a domain from"),
        ));
    }
    Ok(host.to_string())
}

/// Normalizes a safely-parsed ActivityPub actor document into a
/// [`RemoteAccount`] (Requirement 7.2), given the already-minted `id` and
/// `fetched_at` this fetch-and-upsert call is stamping. Fails with a `422`
/// [`AppError`] if the required `preferredUsername` property is absent, or
/// if `actor_uri` itself has no interpretable host (see
/// [`required_username`]/[`host_from_actor_uri`]).
fn normalize_actor_document(
    actor_uri: &str,
    parsed: &ParsedActivity,
    id: Id,
    fetched_at: OffsetDateTime,
) -> Result<RemoteAccount, AppError> {
    let map = parsed
        .raw
        .as_object()
        .expect("parse_activity guarantees the raw document is a JSON object");

    let username = required_username(map)?;
    let domain = host_from_actor_uri(actor_uri)?;

    Ok(RemoteAccount {
        id,
        actor_uri: actor_uri.to_string(),
        username,
        domain,
        display_name: optional_string(map, "name").unwrap_or_default(),
        note: optional_string(map, "summary").unwrap_or_default(),
        url: optional_string(map, "url").unwrap_or_else(|| actor_uri.to_string()),
        avatar_url: extract_image_url(map.get("icon")),
        header_url: extract_image_url(map.get("image")),
        fields: extract_fields(map.get("attachment")),
        // See this module's doc comment ("`bot`: reuses this crate's own
        // established `Service` == bot convention").
        bot: parsed.activity_type == "Service",
        locked: map
            .get("manuallyApprovesFollowers")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        fetched_at,
    })
}

/// Fetches, safely normalizes, and caches a remote ActivityPub actor's
/// Account-contract fields (design.md's exact `RemoteAccountFetcher`
/// component; Requirements 7.1-7.5). See this module's doc comment for the
/// full cache/fetch/error-mapping contract.
///
/// Generic over `H: FederationHttpClient` (held as `Arc<H>`), matching
/// `DbFederationPublicKeyResolver`'s identical rationale in
/// `key_resolver.rs` (a trait of literal `async fn` methods is not
/// object-safe as `dyn FederationHttpClient`).
pub struct RemoteAccountFetcher<H: FederationHttpClient> {
    pool: PgPool,
    http_client: Arc<H>,
    runtime: RuntimeContext,
    ttl: Duration,
}

impl<H: FederationHttpClient> RemoteAccountFetcher<H> {
    /// Builds a fetcher against `pool` (`RemoteAccountRepository`'s
    /// connection pool), `http_client` (the fetch-on-miss/stale network
    /// boundary), `runtime` (the injected clock/id boundaries -- cache
    /// staleness judgment and fresh-`id` minting, never
    /// `OffsetDateTime::now_utc()`/an ad hoc counter directly, per steering's
    /// clock/id DI convention), and `ttl` (the cache's validity window --
    /// pass [`DEFAULT_REMOTE_ACCOUNT_CACHE_TTL`] for the documented default).
    pub fn new(pool: PgPool, http_client: Arc<H>, runtime: RuntimeContext, ttl: Duration) -> Self {
        Self {
            pool,
            http_client,
            runtime,
            ttl,
        }
    }

    /// Fetches `actor_uri` fresh over the network, safely expands and
    /// normalizes the response, and writes the cache before returning the
    /// upserted [`RemoteAccount`] -- the miss/stale tail of
    /// [`Self::fetch_and_normalize`].
    async fn fetch_and_upsert(&self, actor_uri: &str) -> Result<RemoteAccount, AppError> {
        let response = self.http_client.fetch(actor_uri, None).await?;
        if !response.status.is_success() {
            // See this module's doc comment ("Error mapping") for why this
            // is a caller-facing 404, distinct from `key_resolver.rs`'s own
            // non-success mapping.
            return Err(AppError::client(
                StatusCode::NOT_FOUND,
                format!(
                    "remote actor '{actor_uri}' could not be fetched (upstream status {})",
                    response.status
                ),
            ));
        }

        let parsed = parse_activity(&response.body)?;
        let id = self.runtime.ids.next_id();
        let fetched_at = self.runtime.clock.now();
        let normalized = normalize_actor_document(actor_uri, &parsed, id, fetched_at)?;
        upsert_remote(&self.pool, &normalized).await
    }

    /// Resolves `actor_uri` to its normalized [`RemoteAccount`] (design.md's
    /// exact Service Interface signature). Reuses a valid cache entry
    /// without any network call (Requirement 7.3); otherwise fetches,
    /// normalizes, and caches (Requirements 7.1, 7.2, 7.5), or fails with an
    /// [`AppError`] and performs no upsert (Requirement 7.4).
    pub async fn fetch_and_normalize(&self, actor_uri: &str) -> Result<RemoteAccount, AppError> {
        if let Some(cached) = find_remote_by_uri(&self.pool, actor_uri).await? {
            let now = self.runtime.clock.now();
            if !is_stale(cached.fetched_at, now, self.ttl) {
                return Ok(cached);
            }
        }
        self.fetch_and_upsert(actor_uri).await
    }
}
