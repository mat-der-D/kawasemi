//! `PublicKeyResolver` (design.md "PublicKeyResolver / FederationHttpClient
//! （モック可能境界）" -> Service Interface; Requirements 2.3, 2.4; task 2.1,
//! `Boundary: PublicKeyResolver`): resolves a `keyId` to the remote public
//! key material (PEM + owning actor URI) needed to verify an inbound HTTP
//! Signature, caching the result in `remote_public_keys`
//! (`migrations/0004_federation.sql`) so repeated verifications against the
//! same signer do not re-fetch over the network every time (Requirement
//! 2.4).
//!
//! ## Cache semantics
//! - A cache hit with `force == false` and `fetched_at` within `cache_ttl`
//!   of "now" (this resolver's injected [`Clock`], never
//!   `OffsetDateTime::now_utc()`/`SystemTime::now()` directly — per
//!   steering's clock/id/rng/signing-key DI boundary,
//!   `.kiro/steering/tech.md`: "時刻・ID・乱数・署名鍵は注入可能（DI）にする")
//!   short-circuits: no [`FederationHttpClient::fetch`] call is made, and
//!   the cached row is returned as-is (Requirement 2.4: "同一の鍵識別子の
//!   公開鍵がキャッシュに有効に存在する間...ネットワーク取得を行わず
//!   キャッシュされた公開鍵を用いる").
//! - `force == true` always fetches over the network regardless of cache
//!   state, and overwrites the cache with the fresh result (design.md:
//!   "force=true で再取得").
//! - A cache miss, or a cache hit older than `cache_ttl`, is treated as
//!   requiring a fetch (design.md: "`fetched_at` から TTL を超えたキャッシュ
//!   は陳腐として扱い次回検証時に再取得する").
//!
//! ## TTL is a constructor parameter, not yet config-wired
//! design.md names the TTL's config key as `federation.public_key_cache_ttl`
//! (既定 24 時間), but wiring core-runtime's TOML+DB config layer into a
//! `federation` config section is task 5.4's boundary
//! (`_Boundary: FederationModule, Bootstrap, AppState, Config_`), not this
//! task's (`_Boundary: PublicKeyResolver_`). This module therefore accepts
//! `cache_ttl: time::Duration` as a plain constructor parameter
//! ([`DbFederationPublicKeyResolver::new`]); [`DEFAULT_PUBLIC_KEY_CACHE_TTL`]
//! captures the documented default value for task 5.4's bootstrap wiring to
//! apply once it exists, without this module reaching into config itself.
//!
//! ## What is fetched, and how the response is interpreted
//! `resolve_public_key` fetches `key_id` itself via
//! [`FederationHttpClient::fetch`] (unsigned: `signed_as: None` — no
//! `RequestSigner` exists in this spec yet, task 2.2, out of this task's
//! boundary). A `keyId` is conventionally an actor document URL with a
//! `#`-prefixed fragment (e.g.
//! `https://remote.example/users/alice#main-key`); per URL semantics the
//! fragment is never transmitted in the HTTP request line, so fetching
//! `key_id` verbatim naturally retrieves the owning actor document — the
//! exact document real-world ActivityPub actors publish their `publicKey`
//! on. The response body is parsed as a generic `serde_json::Value`
//! (mirroring `jsonld::parse::parse_activity`'s "read only what is needed,
//! never fail on unknown properties" convention) and only three members are
//! read: `publicKey.publicKeyPem` (required), and `publicKey.owner` falling
//! back to the document's own top-level `id` for the returned `actor_uri`
//! (an actor's `publicKey.owner` is expected to equal the actor's own `id`
//! in practice; the fallback tolerates a document that omits `owner`).
//! Retrieving/persisting anything else about the actor (display name,
//! inbox, etc.) is explicitly out of this spec's boundary
//! (requirements.md Boundary Context: "本 spec は署名検証に必要な公開鍵
//! 素材の取得・キャッシュのみ").
//!
//! ## Error mapping
//! A non-success HTTP status, a malformed JSON body, or a body missing the
//! fields this resolver needs are all remote-side failures the caller
//! cannot fix by retrying with different input, so each maps to a `Server`
//! (5xx) [`AppError`] with `StatusCode::BAD_GATEWAY` — the same mapping
//! `http_client.rs`'s `ReqwestFederationHttpClient` already uses for a
//! failed `send`/`fetch` call, kept consistent here rather than inventing a
//! second convention for "the remote gave us something unusable". DB
//! failures reading/writing the cache map to `StatusCode::INTERNAL_SERVER_ERROR`,
//! mirroring `actor/keys/repository.rs`'s convention for unexpected
//! database failures.

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::Value;
use sqlx::postgres::PgPool;
use time::{Duration, OffsetDateTime};

use super::http_client::FederationHttpClient;
use crate::error::AppError;
use crate::runtime::Clock;

/// Documented default for `federation.public_key_cache_ttl` (design.md:
/// "既定 24 時間"). Not applied automatically anywhere in this module — see
/// this module's doc comment ("TTL is a constructor parameter") for why;
/// exposed so task 5.4's bootstrap wiring has a single source of truth for
/// the documented default instead of re-deriving `Duration::hours(24)`
/// itself.
pub const DEFAULT_PUBLIC_KEY_CACHE_TTL: Duration = Duration::hours(24);

/// Resolved public-key material for a `keyId` (design.md's exact
/// `PublicKeyResolver` interface type; Requirement 2.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemotePublicKey {
    pub key_id: String,
    pub actor_uri: String,
    pub public_key_pem: String,
}

/// keyId -> public-key-material resolution port (design.md's exact
/// `PublicKeyResolver` Service Interface; Requirements 2.3, 2.4): caches
/// resolved material in `remote_public_keys`, skips the network fetch while
/// a cached entry is still valid, and always re-fetches when `force` is
/// set. See this module's doc comment for the full cache/fetch contract.
///
/// `#[allow(async_fn_in_trait)]`: mirrors `FederationHttpClient`'s own
/// documented rationale in `http_client.rs` (design.md pins this method as
/// literal `async fn`; boxing/`Send`-pinning concerns belong to whichever
/// later task actually needs `Arc<dyn PublicKeyResolver>` across a
/// `tokio::spawn` boundary — e.g. `SignatureVerifier`, task 2.3 — not this
/// task's `PublicKeyResolver` boundary).
#[allow(async_fn_in_trait)]
pub trait PublicKeyResolver: Send + Sync {
    /// Resolves `key_id` to its public-key material. Cache-first unless
    /// `force` is `true`. See this module's doc comment for the exact
    /// cache-validity/fetch contract.
    async fn resolve_public_key(
        &self,
        key_id: &str,
        force: bool,
    ) -> Result<RemotePublicKey, AppError>;
}

/// A `remote_public_keys` row as read directly off the wire.
type CachedPublicKeyRow = (String, String, String, OffsetDateTime);

/// Reads the current cached row for `key_id`, if any, alongside its
/// `fetched_at` (needed separately from [`RemotePublicKey`] itself to judge
/// staleness against `cache_ttl`).
async fn find_cached(
    pool: &PgPool,
    key_id: &str,
) -> Result<Option<(RemotePublicKey, OffsetDateTime)>, AppError> {
    let row: Option<CachedPublicKeyRow> = sqlx::query_as(
        "SELECT key_id, actor_uri, public_key_pem, fetched_at \
         FROM remote_public_keys WHERE key_id = $1",
    )
    .bind(key_id)
    .fetch_optional(pool)
    .await
    .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;

    Ok(row.map(|(key_id, actor_uri, public_key_pem, fetched_at)| {
        (
            RemotePublicKey {
                key_id,
                actor_uri,
                public_key_pem,
            },
            fetched_at,
        )
    }))
}

/// Inserts or overwrites the cached row for `key.key_id` with `key`'s
/// material and `fetched_at` (design.md: the cache-write side of both the
/// initial fetch and every `force`/stale re-fetch). `key_id` is the primary
/// key (`migrations/0004_federation.sql`), so a second fetch for the same
/// `key_id` updates the existing row rather than conflicting.
async fn upsert_cached(
    pool: &PgPool,
    key: &RemotePublicKey,
    fetched_at: OffsetDateTime,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO remote_public_keys (key_id, actor_uri, public_key_pem, fetched_at) \
         VALUES ($1, $2, $3, $4) \
         ON CONFLICT (key_id) DO UPDATE SET \
             actor_uri = EXCLUDED.actor_uri, \
             public_key_pem = EXCLUDED.public_key_pem, \
             fetched_at = EXCLUDED.fetched_at",
    )
    .bind(&key.key_id)
    .bind(&key.actor_uri)
    .bind(&key.public_key_pem)
    .bind(fetched_at)
    .execute(pool)
    .await
    .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;

    Ok(())
}

/// Interprets a fetched actor document's body as this resolver's
/// [`RemotePublicKey`] for `key_id`. See this module's doc comment ("What is
/// fetched, and how the response is interpreted" / "Error mapping").
fn parse_public_key_document(key_id: &str, body: &[u8]) -> Result<RemotePublicKey, AppError> {
    let document: Value = serde_json::from_slice(body).map_err(|source| {
        AppError::server(
            StatusCode::BAD_GATEWAY,
            format!("malformed actor document fetched for keyId {key_id}: {source}"),
        )
    })?;

    let public_key_pem = document
        .get("publicKey")
        .and_then(|public_key| public_key.get("publicKeyPem"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            AppError::server(
                StatusCode::BAD_GATEWAY,
                format!("actor document fetched for keyId {key_id} has no publicKey.publicKeyPem"),
            )
        })?
        .to_string();

    let actor_uri = document
        .get("publicKey")
        .and_then(|public_key| public_key.get("owner"))
        .and_then(Value::as_str)
        .or_else(|| document.get("id").and_then(Value::as_str))
        .ok_or_else(|| {
            AppError::server(
                StatusCode::BAD_GATEWAY,
                format!(
                    "actor document fetched for keyId {key_id} has neither publicKey.owner nor id"
                ),
            )
        })?
        .to_string();

    Ok(RemotePublicKey {
        key_id: key_id.to_string(),
        actor_uri,
        public_key_pem,
    })
}

/// `PublicKeyResolver` implementation backed by `remote_public_keys`
/// (cache) and a [`FederationHttpClient`] (fetch-on-miss/stale/force), with
/// cache-validity judged against an injected [`Clock`] (never wall-clock
/// time directly, per steering's non-determinism DI boundary).
///
/// Generic over `H: FederationHttpClient` (held as `Arc<H>`), rather than
/// `Arc<dyn FederationHttpClient>`: [`FederationHttpClient`]'s methods are
/// literal `async fn` (design.md's pinned signature, kept as-is per
/// `http_client.rs`'s own `#[allow(async_fn_in_trait)]` rationale), and a
/// trait with `async fn` methods is not object-safe (`dyn
/// FederationHttpClient` does not compile — there is no vtable for a method
/// whose return type is an opaque per-call future) unless every method is
/// manually boxed, which neither this task nor task 1.4 does. A generic
/// parameter avoids that entirely while still letting any
/// [`FederationHttpClient`] implementation (production
/// `ReqwestFederationHttpClient` or a deterministic
/// `MockFederationHttpClient`) be substituted at the call site
/// (Requirement 2.7's "テストでモック実装へ差し替えられるようにする",
/// satisfied here via monomorphization instead of dynamic dispatch).
/// `Arc<H>` (not a bare `H`) so a caller (e.g. a test) can keep its own
/// cloned handle to a shared mock for post-call assertions while this
/// resolver also owns a reference to the same instance.
pub struct DbFederationPublicKeyResolver<H: FederationHttpClient> {
    pool: PgPool,
    http_client: Arc<H>,
    clock: Arc<dyn Clock>,
    cache_ttl: Duration,
}

impl<H: FederationHttpClient> DbFederationPublicKeyResolver<H> {
    /// Builds a resolver against `pool` (the `remote_public_keys` table's
    /// connection pool), `http_client` (the fetch-on-miss/stale/force
    /// network boundary), `clock` (cache-validity judgment), and
    /// `cache_ttl` (the cache's validity window — see this module's doc
    /// comment, "TTL is a constructor parameter", for why this is a
    /// parameter here rather than read from config; pass
    /// [`DEFAULT_PUBLIC_KEY_CACHE_TTL`] for the documented default).
    pub fn new(
        pool: PgPool,
        http_client: Arc<H>,
        clock: Arc<dyn Clock>,
        cache_ttl: Duration,
    ) -> Self {
        Self {
            pool,
            http_client,
            clock,
            cache_ttl,
        }
    }

    /// Fetches `key_id` fresh over the network, parses the response, and
    /// writes the cache before returning the resolved material — the shared
    /// tail of both the "no valid cache" and "force" paths in
    /// [`PublicKeyResolver::resolve_public_key`].
    async fn fetch_and_cache(&self, key_id: &str) -> Result<RemotePublicKey, AppError> {
        let response = self.http_client.fetch(key_id, None).await?;
        if !response.status.is_success() {
            return Err(AppError::server(
                StatusCode::BAD_GATEWAY,
                format!(
                    "fetching keyId {key_id} returned non-success status {}",
                    response.status
                ),
            ));
        }
        let resolved = parse_public_key_document(key_id, &response.body)?;
        upsert_cached(&self.pool, &resolved, self.clock.now()).await?;
        Ok(resolved)
    }
}

impl<H: FederationHttpClient> PublicKeyResolver for DbFederationPublicKeyResolver<H> {
    /// See this module's doc comment ("Cache semantics") for the exact
    /// cache-hit/stale/force decision this implements.
    async fn resolve_public_key(
        &self,
        key_id: &str,
        force: bool,
    ) -> Result<RemotePublicKey, AppError> {
        if !force && let Some((cached, fetched_at)) = find_cached(&self.pool, key_id).await? {
            let age = self.clock.now() - fetched_at;
            if age <= self.cache_ttl {
                return Ok(cached);
            }
        }
        self.fetch_and_cache(key_id).await
    }
}
