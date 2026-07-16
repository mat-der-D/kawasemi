//! `KeyCache` (design.md "Runtime / 供給層" -> "KeyCache"; Requirements 6.2,
//! 6.4; task 4.1): an in-memory, `KeyRef`-keyed store of each actor's
//! currently active [`SigningKey`], so a synchronous supply boundary (the
//! later `DbSigningKeyProvider`, task 4.2) can answer without touching the
//! database on the request path.
//!
//! Scope: this module owns exactly the cache's own read/write API —
//! construction (including pre-warming from a collection of entries),
//! [`KeyCache::get`], and [`KeyCache::upsert`]. It does not decide *when*
//! to warm the cache from the database at startup (bootstrap wiring, task
//! 6.1) or *when* to call `upsert` after a write (`SigningKeyService`, task
//! 4.1's `service` sibling module) — this type is a passive store only.
//!
//! ## State model
//! design.md: `Arc<RwLock<HashMap<KeyRef, SigningKey>>>`（内部可変性）.
//! `KeyRef` is core-runtime's canonical `KeyRef(pub Id)` (the target actor's
//! `Id`, wrapped directly; single-key-per-actor model) — this module
//! consumes it as-is and does not define its own alias type, per design.md's
//! "独自の別名型（`ActorKeyRef` 等）を重ねて定義しない". `KeyCache` itself
//! derives `Clone`: cloning only bumps the inner `Arc`'s reference count, so
//! every clone shares the same underlying map (needed so `SigningKeyService`
//! and the future `DbSigningKeyProvider` observe the same writes).
//!
//! ## Concurrency strategy
//! design.md: "読み多数・書き少数。`RwLock` で保護" (read-heavy,
//! write-rare) — `get` takes a read lock, `upsert` takes a write lock.
//! Lock poisoning (a panic while a lock was held) is treated as an
//! unrecoverable process-level invariant violation and propagates via
//! `expect`, mirroring how a poisoned lock is unrecoverable in-process
//! regardless of how it is surfaced.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::runtime::signing_key::{KeyRef, SigningKey};

/// In-memory `KeyRef` -> currently-active `SigningKey` store (design.md's
/// `KeyCache`). Cheap to clone (an `Arc` bump); every clone shares the same
/// underlying map.
#[derive(Clone)]
pub struct KeyCache {
    entries: Arc<RwLock<HashMap<KeyRef, SigningKey>>>,
}

impl KeyCache {
    /// Builds an empty cache. Useful for tests or a from-scratch instance;
    /// production startup instead pre-warms via [`KeyCache::from_entries`]
    /// with every currently active key loaded from the database (bootstrap
    /// wiring, task 6.1, out of scope here).
    pub fn new() -> Self {
        Self {
            entries: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Builds a cache pre-warmed from `entries` (design.md: "起動時に DB
    /// から温め") — e.g. every currently active signing key loaded at
    /// startup. This module only supports being warmed from an
    /// already-assembled collection of `(KeyRef, SigningKey)` pairs; the
    /// actual database read that produces that collection is bootstrap
    /// wiring (task 6.1), out of scope here.
    pub fn from_entries(entries: impl IntoIterator<Item = (KeyRef, SigningKey)>) -> Self {
        Self {
            entries: Arc::new(RwLock::new(entries.into_iter().collect())),
        }
    }

    /// Returns the currently cached active [`SigningKey`] for `key_ref`, if
    /// any. `None` means no active key is cached for that actor — the
    /// caller (`DbSigningKeyProvider`, task 4.2) is responsible for turning
    /// that into a `KeyError::NotFound` at the `SigningKeyProvider`
    /// boundary; this module itself has no notion of "not found" as an
    /// error, only as an absent map entry.
    pub fn get(&self, key_ref: KeyRef) -> Option<SigningKey> {
        let guard = self
            .entries
            .read()
            .expect("KeyCache's RwLock must not be poisoned");
        guard.get(&key_ref).cloned()
    }

    /// Inserts or overwrites the cached active key for `key_ref` with
    /// `key` (design.md: "作成／ローテーションで `SigningKeyService` のみが
    /// 更新（単一書込経路で整合）"). Called by `SigningKeyService` after
    /// every DB write that changes which key is active for an actor —
    /// this module itself does not decide when that happens.
    pub fn upsert(&self, key_ref: KeyRef, key: SigningKey) {
        let mut guard = self
            .entries
            .write()
            .expect("KeyCache's RwLock must not be poisoned");
        guard.insert(key_ref, key);
    }
}

impl Default for KeyCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Id;

    fn signing_key(byte: u8) -> SigningKey {
        SigningKey::from_pem_bytes(vec![byte; 4])
    }

    #[test]
    fn get_returns_none_for_a_key_ref_never_inserted() {
        let cache = KeyCache::new();
        let key_ref = KeyRef(Id::from_i64(1));

        assert!(cache.get(key_ref).is_none());
    }

    #[test]
    fn upsert_then_get_returns_the_inserted_key() {
        let cache = KeyCache::new();
        let key_ref = KeyRef(Id::from_i64(1));
        let key = signing_key(7);

        cache.upsert(key_ref, key.clone());

        let found = cache.get(key_ref).expect("just-upserted key must be found");
        assert_eq!(found.expose_pem_bytes(), key.expose_pem_bytes());
    }

    #[test]
    fn upsert_overwrites_a_previous_key_for_the_same_key_ref() {
        let cache = KeyCache::new();
        let key_ref = KeyRef(Id::from_i64(1));

        cache.upsert(key_ref, signing_key(1));
        cache.upsert(key_ref, signing_key(2));

        let found = cache
            .get(key_ref)
            .expect("the last-upserted key must be found");
        assert_eq!(found.expose_pem_bytes(), signing_key(2).expose_pem_bytes());
    }

    #[test]
    fn from_entries_pre_warms_the_cache_so_get_finds_every_entry() {
        let key_ref_a = KeyRef(Id::from_i64(1));
        let key_ref_b = KeyRef(Id::from_i64(2));
        let cache =
            KeyCache::from_entries([(key_ref_a, signing_key(1)), (key_ref_b, signing_key(2))]);

        assert_eq!(
            cache
                .get(key_ref_a)
                .expect("entry a must be found")
                .expose_pem_bytes(),
            signing_key(1).expose_pem_bytes()
        );
        assert_eq!(
            cache
                .get(key_ref_b)
                .expect("entry b must be found")
                .expose_pem_bytes(),
            signing_key(2).expose_pem_bytes()
        );
    }

    #[test]
    fn cloning_the_cache_shares_the_same_underlying_map() {
        let cache = KeyCache::new();
        let clone = cache.clone();
        let key_ref = KeyRef(Id::from_i64(1));

        // Write through the original...
        cache.upsert(key_ref, signing_key(9));

        // ...must be visible through the clone (shared Arc<RwLock<...>>,
        // not a deep copy).
        let found = clone
            .get(key_ref)
            .expect("write via the original must be visible via the clone");
        assert_eq!(found.expose_pem_bytes(), signing_key(9).expose_pem_bytes());
    }

    #[test]
    fn different_key_refs_do_not_collide() {
        let cache = KeyCache::new();
        let key_ref_a = KeyRef(Id::from_i64(1));
        let key_ref_b = KeyRef(Id::from_i64(2));

        cache.upsert(key_ref_a, signing_key(1));

        assert!(cache.get(key_ref_a).is_some());
        assert!(cache.get(key_ref_b).is_none());
    }
}
