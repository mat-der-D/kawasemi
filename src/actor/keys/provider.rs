//! `DbSigningKeyProvider` (design.md "Runtime / 供給層" -> "DbSigningKeyProvider";
//! Requirements 6.1, 6.2, 6.3; task 4.2): core-runtime's `SigningKeyProvider`
//! trait's production implementation — the synchronous supply boundary
//! federation consumers call to obtain an actor's currently active signing
//! key.
//!
//! Scope: this module owns exactly the `impl SigningKeyProvider for
//! DbSigningKeyProvider` block and its constructor. Per design.md's
//! "署名鍵供給（同期境界）" sequence diagram, it is a thin, direct
//! pass-through against [`KeyCache`] — it performs no I/O of its own on the
//! request path; the whole point of task 4.1's `KeyCache` existing is so
//! this boundary never touches the database while answering a request. It
//! does not decide when the cache is warmed at startup (bootstrap wiring,
//! task 6.1) or when the cache is updated after a write (`SigningKeyService`,
//! task 4.1's `service` sibling module) — this type only reads.
//!
//! ## `KeyRef` interpretation
//! Per design.md: "`KeyRef(pub Id)`（core-runtime 定義）の `Id` を対象アクター
//! の `Id` そのものとして解釈し、`KeyRef` をそのまま `KeyCache` のキーとして
//! 有効鍵を取得する" — core-runtime's `KeyRef(pub Id)` already wraps the
//! target actor's `Id` directly (single-key-per-actor model; no independent
//! key-version identifier), so `signing_key` uses `key_ref` as-is as the
//! `KeyCache` lookup key with no further interpretation step.

use crate::actor::keys::cache::KeyCache;
use crate::runtime::signing_key::{KeyError, KeyRef, SigningKey, SigningKeyProvider};

/// core-runtime `SigningKeyProvider`'s production implementation
/// (design.md's `DbSigningKeyProvider`): answers `signing_key` requests
/// synchronously from a pre-warmed, kept-in-sync [`KeyCache`] handle,
/// without ever touching the database on the request path.
#[derive(Clone)]
pub struct DbSigningKeyProvider {
    cache: KeyCache,
}

impl DbSigningKeyProvider {
    /// Builds a provider backed by `cache` — the same shared [`KeyCache`]
    /// handle `SigningKeyService` (task 4.1) writes through, so this
    /// provider's answers always reflect the latest provisioned/rotated key
    /// (Requirement 6.4, satisfied by `KeyCache`'s shared-`Arc` design; see
    /// `src/actor/keys/cache.rs`).
    pub fn new(cache: KeyCache) -> Self {
        Self { cache }
    }
}

impl SigningKeyProvider for DbSigningKeyProvider {
    /// Looks `key_ref` up directly in the [`KeyCache`] (Requirement 6.2):
    /// returns the cached active key if present, or
    /// `KeyError::NotFound(key_ref)` if no active key is cached for the
    /// referenced actor (Requirement 6.3). Purely synchronous, in-memory —
    /// no I/O.
    fn signing_key(&self, key_ref: KeyRef) -> Result<SigningKey, KeyError> {
        self.cache.get(key_ref).ok_or(KeyError::NotFound(key_ref))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Id;

    const REGISTERED_PEM: &[u8] =
        b"-----BEGIN PRIVATE KEY-----\nregistered-actor-key\n-----END PRIVATE KEY-----\n";

    #[test]
    fn signing_key_returns_the_cached_active_key_for_a_registered_actor() {
        let key_ref = KeyRef(Id::from_i64(101));
        let cache = KeyCache::from_entries([(
            key_ref,
            SigningKey::from_pem_bytes(REGISTERED_PEM.to_vec()),
        )]);
        let provider = DbSigningKeyProvider::new(cache);

        let found = provider
            .signing_key(key_ref)
            .expect("registered actor's key_ref must resolve to its cached key");

        assert_eq!(found.expose_pem_bytes(), REGISTERED_PEM);
    }

    #[test]
    fn signing_key_returns_not_found_for_an_unregistered_key_ref() {
        let registered = KeyRef(Id::from_i64(101));
        let unregistered = KeyRef(Id::from_i64(202));
        let cache = KeyCache::from_entries([(
            registered,
            SigningKey::from_pem_bytes(REGISTERED_PEM.to_vec()),
        )]);
        let provider = DbSigningKeyProvider::new(cache);

        let error = provider
            .signing_key(unregistered)
            .expect_err("unregistered key_ref must not resolve to a key");

        assert_eq!(error, KeyError::NotFound(unregistered));
    }

    #[test]
    fn signing_key_reflects_a_key_upserted_into_the_cache_after_construction() {
        // Requirement 6.4 (reflected here for completeness even though
        // task 4.1's SigningKeyService is the actual writer in production):
        // this provider must observe writes made through any handle that
        // shares the same underlying KeyCache, since KeyCache::clone only
        // bumps an Arc reference count.
        let cache = KeyCache::new();
        let provider = DbSigningKeyProvider::new(cache.clone());
        let key_ref = KeyRef(Id::from_i64(303));

        assert_eq!(
            provider.signing_key(key_ref).unwrap_err(),
            KeyError::NotFound(key_ref)
        );

        cache.upsert(key_ref, SigningKey::from_pem_bytes(REGISTERED_PEM.to_vec()));

        let found = provider
            .signing_key(key_ref)
            .expect("provider must observe the write made through the shared cache handle");
        assert_eq!(found.expose_pem_bytes(), REGISTERED_PEM);
    }
}
