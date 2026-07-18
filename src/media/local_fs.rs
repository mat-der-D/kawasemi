//! `LocalFsStore` (design.md "Storage / ストレージ層" -> "MediaStore（port）/
//! LocalFsStore（adapter）", Requirements 5.2, 5.3, 5.4; task 2.2, `Boundary:
//! MediaStore, LocalFsStore`).
//!
//! Scope: this module owns exactly the local-filesystem [`MediaStore`]
//! adapter — [`LocalFsStore`]. It stores each [`ObjectKey`] under a
//! configurable root directory (design.md: "起動設定の保管ルート配下"; a
//! later task 5.2 wires `LocalFsStore::new` to
//! `crate::config::MediaConfig::storage_root`, out of this task's
//! boundary), at a path deterministically derived from the key alone
//! (Requirement 5.2, 5.3): `root/{media_id}/{original|small}`. Its
//! `public_url` reuses [`ForwardedOrigin`] — the actual proxy-aware
//! scheme/host resolution primitive already implemented by api-foundation's
//! pagination module (task 6.2) — to build an absolute URL that reflects a
//! reverse proxy's externally-presented host/scheme rather than this
//! process's own bind address (Requirement 5.4). See `store.rs`'s module
//! doc comment ("`public_url`'s second parameter") for why this is
//! `&ForwardedOrigin` rather than the design-doc-sketched
//! `&RequestUriContext`.
//!
//! No database, HTTP-endpoint, or `MediaRepository`/`MediaService` code
//! lives here — this module only ever reads/writes bytes under its root and
//! renders URL strings.

use std::io::ErrorKind;
use std::path::PathBuf;

use axum::http::StatusCode;

use crate::api::pagination::ForwardedOrigin;
use crate::error::AppError;
use crate::media::store::{MediaStore, ObjectKey};

/// [`MediaStore`] adapter backed by the local filesystem (design.md's
/// "LocalFsStore（adapter）", Requirements 5.1, 5.2, 5.3, 5.4).
#[derive(Debug, Clone)]
pub struct LocalFsStore {
    root: PathBuf,
}

impl LocalFsStore {
    /// Builds a `LocalFsStore` rooted at `root`. Does not create `root`
    /// itself, or validate that it exists yet — [`MediaStore::put`] creates
    /// whatever directories a given key needs, lazily, on first write (the
    /// determinism Requirement 5.2/5.3 asks for is about the *path*, not
    /// about `root` pre-existing on disk).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        LocalFsStore { root: root.into() }
    }

    /// The deterministic on-disk path for `key`: `root/{key}` (`key`
    /// already renders as `{media_id}/{variant}`, see `store.rs`'s
    /// `ObjectKey` doc comment) — same key always resolves to the same path
    /// (Requirement 5.2, 5.3).
    fn path_for(&self, key: &ObjectKey) -> PathBuf {
        self.root.join(key.as_str())
    }
}

impl MediaStore for LocalFsStore {
    async fn put(
        &self,
        key: &ObjectKey,
        bytes: &[u8],
        _content_type: &str,
    ) -> Result<(), AppError> {
        let path = self.path_for(key);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;
        }
        tokio::fs::write(&path, bytes)
            .await
            .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;
        Ok(())
    }

    async fn get(&self, key: &ObjectKey) -> Result<Vec<u8>, AppError> {
        let path = self.path_for(key);
        match tokio::fs::read(&path).await {
            Ok(bytes) => Ok(bytes),
            Err(source) if source.kind() == ErrorKind::NotFound => Err(AppError::client(
                StatusCode::NOT_FOUND,
                "media object not found",
            )),
            Err(source) => Err(AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source)),
        }
    }

    async fn delete(&self, key: &ObjectKey) -> Result<(), AppError> {
        let path = self.path_for(key);
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            // Idempotent: deleting an already-absent key succeeds rather
            // than erroring (see `MediaStore::delete`'s doc comment).
            Err(source) if source.kind() == ErrorKind::NotFound => Ok(()),
            Err(source) => Err(AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source)),
        }
    }

    fn public_url(&self, key: &ObjectKey, origin: &ForwardedOrigin) -> String {
        format!("{}://{}/media/{}", origin.scheme, origin.host, key.as_str())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::domain::Id;

    use super::*;

    /// Builds a process-unique temp directory path, mirroring
    /// `src/contract/tests.rs`'s own `unique_temp_path` "counter + nanos"
    /// convention so concurrently-running tests never collide and no new
    /// temp-dir crate dependency is needed.
    fn unique_temp_root(label: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after the Unix epoch")
            .as_nanos();
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "kawasemi_local_fs_store_test_{label}_{nanos}_{seq}"
        ))
    }

    /// Best-effort cleanup of a temp root this test created, regardless of
    /// whether the test body panicked (mirrors `TempFileGuard` in
    /// `src/contract/tests.rs`).
    struct TempDirGuard(PathBuf);
    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn temp_store(label: &str) -> (LocalFsStore, TempDirGuard) {
        let root = unique_temp_root(label);
        (LocalFsStore::new(root.clone()), TempDirGuard(root))
    }

    #[tokio::test]
    async fn put_then_get_round_trips_identical_bytes() {
        let (store, _guard) = temp_store("round_trip");
        let key = ObjectKey::original(Id::from_i64(1));
        store
            .put(&key, b"the quick brown fox", "image/png")
            .await
            .unwrap();
        let bytes = store.get(&key).await.unwrap();
        assert_eq!(bytes, b"the quick brown fox");
    }

    #[tokio::test]
    async fn get_on_a_missing_key_returns_a_real_error_not_a_panic() {
        let (store, _guard) = temp_store("missing_get");
        let key = ObjectKey::original(Id::from_i64(2));
        let err = store
            .get(&key)
            .await
            .expect_err("expected a not-found error");
        assert_eq!(err.status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_removes_the_object_so_a_subsequent_get_fails() {
        let (store, _guard) = temp_store("delete");
        let key = ObjectKey::original(Id::from_i64(3));
        store.put(&key, b"bytes", "image/png").await.unwrap();
        store.delete(&key).await.unwrap();
        let err = store
            .get(&key)
            .await
            .expect_err("expected deletion to remove the object");
        assert_eq!(err.status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_on_an_already_missing_key_is_idempotent() {
        let (store, _guard) = temp_store("delete_missing");
        let key = ObjectKey::original(Id::from_i64(4));
        // Never put anything under `key` — deleting a key that was never
        // stored (or already deleted) must still succeed.
        store.delete(&key).await.unwrap();
    }

    #[tokio::test]
    async fn the_same_media_id_and_variant_always_resolve_to_the_same_path() {
        let (store, _guard) = temp_store("deterministic_path");
        let media_id = Id::from_i64(5);
        let key_a = ObjectKey::original(media_id);
        let key_b = ObjectKey::original(media_id);
        store
            .put(&key_a, b"first write", "image/png")
            .await
            .unwrap();
        // Overwriting via the second, independently-constructed key for the
        // same (media_id, variant) must land on the same file (Requirement
        // 5.2, 5.3: deterministic path).
        store
            .put(&key_b, b"second write", "image/png")
            .await
            .unwrap();
        let bytes = store.get(&key_a).await.unwrap();
        assert_eq!(bytes, b"second write");
    }

    #[tokio::test]
    async fn original_and_small_variants_of_the_same_media_are_stored_independently() {
        let (store, _guard) = temp_store("variant_isolation");
        let media_id = Id::from_i64(6);
        let original = ObjectKey::original(media_id);
        let small = ObjectKey::small(media_id);
        store
            .put(&original, b"original bytes", "image/png")
            .await
            .unwrap();
        store
            .put(&small, b"small bytes", "image/png")
            .await
            .unwrap();
        assert_eq!(store.get(&original).await.unwrap(), b"original bytes");
        assert_eq!(store.get(&small).await.unwrap(), b"small bytes");
        store.delete(&small).await.unwrap();
        // Deleting the small derivative must not affect the original.
        assert_eq!(store.get(&original).await.unwrap(), b"original bytes");
    }

    #[tokio::test]
    async fn put_creates_parent_directories_as_needed() {
        let root = unique_temp_root("parent_dirs");
        let _guard = TempDirGuard(root.clone());
        // `root` itself does not exist yet — `put` must create it (and any
        // intermediate directories the key implies) lazily.
        assert!(!root.exists());
        let store = LocalFsStore::new(root.clone());
        let key = ObjectKey::original(Id::from_i64(7));
        store.put(&key, b"bytes", "image/png").await.unwrap();
        assert!(root.join(key.as_str()).is_file());
    }

    #[test]
    fn public_url_reflects_the_forwarded_proxy_host_and_scheme_not_the_local_bind_address() {
        let store = LocalFsStore::new(unique_temp_root("public_url"));
        let key = ObjectKey::small(Id::from_i64(8));
        // A reverse proxy presents `example.social` over `https`, while this
        // process itself only knows about its own local bind address.
        let origin = ForwardedOrigin::resolve(
            "http",
            "127.0.0.1:9000",
            Some("https"),
            Some("example.social"),
        );
        let url = store.public_url(&key, &origin);
        assert!(url.starts_with("https://example.social/"), "got {url}");
        assert!(!url.contains("127.0.0.1"), "got {url}");
        assert!(url.contains(key.as_str()), "got {url}");
    }

    #[test]
    fn public_url_falls_back_to_the_connection_origin_when_no_proxy_headers_are_present() {
        let store = LocalFsStore::new(unique_temp_root("public_url_fallback"));
        let key = ObjectKey::original(Id::from_i64(10));
        let origin = ForwardedOrigin::resolve("http", "127.0.0.1:9000", None, None);
        let url = store.public_url(&key, &origin);
        assert!(url.starts_with("http://127.0.0.1:9000/"), "got {url}");
    }

    /// Demonstrates that a caller depending only on `MediaStore` (not the
    /// concrete `LocalFsStore` type) can still drive this adapter — the
    /// same generic helper shape as `store.rs`'s own
    /// `round_trip_through_any_media_store`, instantiated here with
    /// `LocalFsStore` specifically to prove the adapter genuinely satisfies
    /// the trait's contract end to end (Requirement 5.1, 5.5).
    async fn round_trip_through_any_media_store<S: MediaStore>(store: &S, key: &ObjectKey) {
        store
            .put(key, b"trait-only bytes", "image/png")
            .await
            .unwrap();
        assert_eq!(store.get(key).await.unwrap(), b"trait-only bytes");
        store.delete(key).await.unwrap();
        assert!(store.get(key).await.is_err());
    }

    #[tokio::test]
    async fn local_fs_store_is_usable_through_the_media_store_trait_alone() {
        let (store, _guard) = temp_store("trait_generic");
        let key = ObjectKey::original(Id::from_i64(11));
        round_trip_through_any_media_store(&store, &key).await;
    }
}
