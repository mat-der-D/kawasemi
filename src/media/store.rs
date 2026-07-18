//! `MediaStore` port and `ObjectKey` (design.md "Storage / ストレージ層" ->
//! "MediaStore（port）/ LocalFsStore（adapter）", Requirements 5.1, 5.2, 5.3,
//! 5.4, 5.5; task 2.2, `Boundary: MediaStore, LocalFsStore`).
//!
//! Scope: this module owns exactly the storage abstraction boundary — the
//! [`MediaStore`] trait (put/get/delete/public_url) and the [`ObjectKey`]
//! value type identifying what a stored object is keyed by. The concrete
//! local-filesystem adapter lives in [`crate::media::local_fs`] (`LocalFsStore`);
//! this module has no filesystem, database, or HTTP-layer code at all —
//! callers (a later `MediaService`/`ProcessingWorker`/
//! `MediaAttachmentSerializer`, tasks 4.1/4.3/4.2) depend only on the
//! [`MediaStore`] trait, never on any concrete adapter (Requirement 5.1,
//! 5.5) — this module's own tests demonstrate that independence with a
//! second, trivial in-memory fake implementation exercised through nothing
//! but the trait, not just `LocalFsStore`.
//!
//! ## `ObjectKey`: media-id + variant, not an opaque blob
//! design.md's sketch (`pub struct ObjectKey(/* media id 由来の決定的キー
//! （original / small）*/);`) documents the *invariant* (deterministic,
//! derived from the media id) but not the concrete shape. The Physical Data
//! Model (`migrations/0005_media.sql`, see tasks.md's Implementation Notes
//! for why `0005` not the design doc's `0004`) persists exactly two such
//! keys per `media` row — `object_key` (the original) and `thumb_key` (the
//! small/thumbnail derivative) — so [`ObjectKey`] is modeled as a media
//! [`Id`] paired with an [`ObjectVariant`] (`Original`/`Small`), rendered to
//! a single deterministic string via [`ObjectKey::as_str`] (what a later
//! `MediaRepository`, task 3.1, would persist into those `TEXT` columns) and
//! reconstructable from a persisted string via [`ObjectKey::from_key`] (what
//! a later reader would do when loading a `media` row back). Same
//! `(media_id, variant)` always renders to the same key string (design.md's
//! Invariant: "同一 `ObjectKey` は同一実体を指す").
//!
//! ## `public_url`'s second parameter: `ForwardedOrigin`, not `RequestUriContext`
//! design.md's sketch signature is `fn public_url(&self, key: &ObjectKey,
//! req_uri: &RequestUriContext) -> String`. The actual already-implemented
//! api-foundation proxy-aware URL primitive in this codebase
//! (`crate::api::pagination`, task 6.2) is [`ForwardedOrigin`] (a public
//! `{scheme, host}` pair resolved from `X-Forwarded-Proto`/
//! `X-Forwarded-Host` with same-process fallback, Requirement 5.4's exact
//! "リバースプロキシ後段での外部ホスト名・スキームを尊重" concern) plus
//! `RequestUriContext`, which wraps a `ForwardedOrigin` together with a
//! *cursor-pagination-specific* `Link`-header URL builder (`url_with`,
//! private, hard-coded to append a `max_id`/`min_id` query parameter — see
//! `src/api/pagination.rs`). `RequestUriContext` has no public method that
//! renders a plain absolute URL for an arbitrary path, so it cannot be
//! reused as-is for a media object URL without either reaching into its
//! private internals or repurposing a cursor-shaped API for a non-cursor
//! use. `public_url` therefore takes `&ForwardedOrigin` directly — the
//! actual reusable proxy-origin-resolution primitive, not the
//! pagination-specific wrapper around it — and builds the absolute URL
//! itself. This is a deliberate, documented deviation from design.md's
//! excerpted signature (see this task's CONCERNS in its status report),
//! matching the precedent task 2.1's reviewer already accepted for `Focus`
//! not literally matching design.md's public-field sketch.

use crate::api::pagination::ForwardedOrigin;
use crate::domain::Id;
use crate::error::AppError;

/// Which derivative of a [`Media`](crate::media::Media) an [`ObjectKey`]
/// addresses: the original upload, or the generated small/thumbnail
/// derivative (design.md's Physical Data Model: `media.object_key` /
/// `media.thumb_key`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObjectVariant {
    Original,
    Small,
}

impl ObjectVariant {
    fn segment(self) -> &'static str {
        match self {
            ObjectVariant::Original => "original",
            ObjectVariant::Small => "small",
        }
    }
}

/// A deterministic key identifying one stored object: a specific
/// [`ObjectVariant`] of a specific media [`Id`] (Requirements 5.2, 5.3).
///
/// The same `(media_id, variant)` pair always renders to the same
/// [`ObjectKey::as_str`] value, and a [`MediaStore`] adapter is expected to
/// always resolve the same key to the same stored entity (design.md's
/// Invariant). See this module's doc comment ("`ObjectKey`: media-id +
/// variant") for why this is not an opaque blob.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ObjectKey(String);

impl ObjectKey {
    /// Builds the deterministic key for `variant` of `media_id`.
    pub fn new(media_id: Id, variant: ObjectVariant) -> Self {
        ObjectKey(format!("{}/{}", media_id.as_i64(), variant.segment()))
    }

    /// Shorthand for `ObjectKey::new(media_id, ObjectVariant::Original)`.
    pub fn original(media_id: Id) -> Self {
        Self::new(media_id, ObjectVariant::Original)
    }

    /// Shorthand for `ObjectKey::new(media_id, ObjectVariant::Small)`.
    pub fn small(media_id: Id) -> Self {
        Self::new(media_id, ObjectVariant::Small)
    }

    /// Reconstructs an `ObjectKey` from an already-persisted key string
    /// (e.g. a `media.object_key`/`media.thumb_key` column value read back
    /// from the database by a later `MediaRepository`, task 3.1). Does not
    /// re-derive or validate the string against any `(media_id, variant)`
    /// pair — the persisted string is already the source of truth.
    pub fn from_key(key: impl Into<String>) -> Self {
        ObjectKey(key.into())
    }

    /// The deterministic key string (what a [`MediaStore`] adapter keys
    /// storage by, and what a later `MediaRepository` persists into
    /// `object_key`/`thumb_key`).
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Storage abstraction boundary (design.md's exact `MediaStore` Service
/// Interface, Requirement 5.1): put/get/delete a media entity's bytes, and
/// resolve a proxy-aware public URL for it, all behind a trait no caller
/// depends on a concrete adapter through (Requirement 5.5).
///
/// `#[allow(async_fn_in_trait)]` mirrors this crate's established pattern
/// for a narrow async port that is consumed generically (`impl MediaStore`/
/// `S: MediaStore`), not through `dyn` — see e.g.
/// `crate::federation::inbound::dedup::ReceivedActivityStore`'s own
/// documented rationale. If a later task needs `Arc<dyn MediaStore>` across
/// a `tokio::spawn` boundary (plausibly `ProcessingWorker`, task 4.3), that
/// task is responsible for introducing the `Pin<Box<dyn Future...>>`
/// boxing/erasure this trait deliberately does not pay for here.
#[allow(async_fn_in_trait)]
pub trait MediaStore: Send + Sync {
    /// Stores `bytes` under `key`, overwriting any existing object at that
    /// key (Requirement 5.1). `content_type` is accepted for adapters that
    /// need it (e.g. a future S3-backed adapter setting a `Content-Type`
    /// object header) — `LocalFsStore` does not need it, since the owning
    /// `media` row already records `content_type` separately (design.md's
    /// Physical Data Model).
    async fn put(&self, key: &ObjectKey, bytes: &[u8], content_type: &str) -> Result<(), AppError>;

    /// Retrieves the bytes stored under `key`. Returns a real
    /// [`AppError`] (never panics) when no object exists at `key`.
    async fn get(&self, key: &ObjectKey) -> Result<Vec<u8>, AppError>;

    /// Removes the object stored under `key`. Idempotent: deleting an
    /// already-absent key succeeds rather than erroring (a later
    /// `ProcessingWorker`'s retry/reclaim paths, task 4.3, may delete the
    /// same derivative more than once).
    async fn delete(&self, key: &ObjectKey) -> Result<(), AppError>;

    /// The proxy-aware absolute URL a client should use to fetch `key`'s
    /// entity (Requirement 5.3, 5.4): reflects the external host/scheme
    /// `origin` resolved from a reverse proxy's forwarded headers, never
    /// this process's own local bind address/scheme.
    fn public_url(&self, key: &ObjectKey, origin: &ForwardedOrigin) -> String;
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use axum::http::StatusCode;

    use super::*;

    #[test]
    fn object_key_for_the_same_media_id_and_variant_is_deterministic() {
        let id = Id::from_i64(42);
        let a = ObjectKey::original(id);
        let b = ObjectKey::original(id);
        assert_eq!(a, b);
        assert_eq!(a.as_str(), b.as_str());
    }

    #[test]
    fn object_key_differs_between_original_and_small_variants_of_the_same_media() {
        let id = Id::from_i64(42);
        let original = ObjectKey::original(id);
        let small = ObjectKey::small(id);
        assert_ne!(original, small);
        assert_ne!(original.as_str(), small.as_str());
    }

    #[test]
    fn object_key_differs_between_distinct_media_ids() {
        let a = ObjectKey::original(Id::from_i64(1));
        let b = ObjectKey::original(Id::from_i64(2));
        assert_ne!(a, b);
    }

    #[test]
    fn object_key_from_key_round_trips_a_persisted_key_string() {
        let id = Id::from_i64(7);
        let original = ObjectKey::small(id);
        let key_string = original.as_str().to_string();
        let reconstructed = ObjectKey::from_key(key_string.clone());
        assert_eq!(reconstructed.as_str(), key_string);
        assert_eq!(reconstructed, original);
    }

    /// A trivial in-memory `MediaStore` fake with no filesystem, database,
    /// or HTTP involvement at all — deliberately a *second* implementation
    /// of the trait, distinct from `LocalFsStore`, so
    /// [`any_media_store_impl_supports_put_get_delete_via_the_trait_alone`]
    /// genuinely exercises "callers depend only on the trait, not a
    /// concrete adapter" (Requirement 5.1, 5.5) rather than merely calling
    /// `LocalFsStore` methods directly under a different name.
    #[derive(Default)]
    struct InMemoryStore {
        inner: Mutex<HashMap<String, Vec<u8>>>,
    }

    impl MediaStore for InMemoryStore {
        async fn put(
            &self,
            key: &ObjectKey,
            bytes: &[u8],
            _content_type: &str,
        ) -> Result<(), AppError> {
            self.inner
                .lock()
                .expect("lock poisoned")
                .insert(key.as_str().to_string(), bytes.to_vec());
            Ok(())
        }

        async fn get(&self, key: &ObjectKey) -> Result<Vec<u8>, AppError> {
            self.inner
                .lock()
                .expect("lock poisoned")
                .get(key.as_str())
                .cloned()
                .ok_or_else(|| AppError::client(StatusCode::NOT_FOUND, "media object not found"))
        }

        async fn delete(&self, key: &ObjectKey) -> Result<(), AppError> {
            self.inner
                .lock()
                .expect("lock poisoned")
                .remove(key.as_str());
            Ok(())
        }

        fn public_url(&self, key: &ObjectKey, origin: &ForwardedOrigin) -> String {
            format!("{}://{}/media/{}", origin.scheme, origin.host, key.as_str())
        }
    }

    /// Demonstrates that a caller can be written entirely in terms of `S:
    /// MediaStore` (Requirement 5.1, 5.5) — this function never names
    /// `InMemoryStore` or `LocalFsStore`.
    async fn round_trip_through_any_media_store<S: MediaStore>(store: &S, key: &ObjectKey) {
        store.put(key, b"hello world", "text/plain").await.unwrap();
        let bytes = store.get(key).await.unwrap();
        assert_eq!(bytes, b"hello world");
        store.delete(key).await.unwrap();
        let after_delete = store.get(key).await;
        assert!(after_delete.is_err());
    }

    #[tokio::test]
    async fn any_media_store_impl_supports_put_get_delete_via_the_trait_alone() {
        let store = InMemoryStore::default();
        let key = ObjectKey::original(Id::from_i64(1));
        round_trip_through_any_media_store(&store, &key).await;
    }

    #[test]
    fn public_url_reflects_the_forwarded_scheme_and_host_not_a_hardcoded_value() {
        let store = InMemoryStore::default();
        let key = ObjectKey::original(Id::from_i64(9));
        let origin = ForwardedOrigin::resolve(
            "http",
            "127.0.0.1:8080",
            Some("https"),
            Some("example.social"),
        );
        let url = store.public_url(&key, &origin);
        assert!(url.starts_with("https://example.social/"), "got {url}");
        assert!(url.contains(key.as_str()), "got {url}");
    }
}
