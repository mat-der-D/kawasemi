//! Integration test proving the storage half of task 6.2's observable
//! completion condition (`.kiro/specs/media-pipeline/tasks.md`, "6.2 処理・
//! キュー・ストレージの統合テストを実装する", `_Boundary: ProcessingWorker,
//! ProcessingJobQueue, MediaStore_`, `_Depends: 5.2_`): "ローカル FS の保管/
//! 取得/削除とプロキシ尊重 URL を検証する" (Requirements 5.1, 5.2, 5.3, 5.4).
//!
//! ## Relationship to `src/media/local_fs.rs`'s and `src/media/store.rs`'s
//! own inline `#[cfg(test)] mod tests`
//! Task 2.2 already added thorough `LocalFsStore`/`MediaStore` coverage
//! directly inside those two source files (real filesystem, real temp
//! directories, `ForwardedOrigin::resolve` proxy-header scenarios). This
//! file is task 6.2's own, separate top-level file (design.md's File
//! Structure Plan names it `media_store_it.rs`), addressed only through
//! this crate's `pub` surface (`kawasemi::media::*`), mirroring the same
//! phase-6-gets-its-own-verification-file precedent `media_processing_it.rs`
//! (this same task) documents relative to `worker.rs`/`job_queue.rs`'s own
//! inline coverage. See this task's implementer status report `CONCERNS`
//! for the overlap this represents.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use kawasemi::api::pagination::ForwardedOrigin;
use kawasemi::domain::Id;
use kawasemi::media::{LocalFsStore, MediaStore, ObjectKey};

fn unique_temp_root(label: &str) -> std::path::PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("kawasemi_media_store_it_{label}_{nanos}_{seq}"))
}

struct TempDirGuard(std::path::PathBuf);
impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp_store(label: &str) -> (LocalFsStore, TempDirGuard) {
    let root = unique_temp_root(label);
    (LocalFsStore::new(root.clone()), TempDirGuard(root))
}

/// Requirements 5.1, 5.2: storing a real file and retrieving it round-trips
/// identical bytes.
#[tokio::test]
async fn store_then_get_round_trips_a_real_file() {
    let (store, _guard) = temp_store("round_trip");
    let key = ObjectKey::original(Id::from_i64(1));
    store
        .put(
            &key,
            b"the quick brown fox jumps over the lazy dog",
            "image/png",
        )
        .await
        .expect("put must succeed");

    let bytes = store.get(&key).await.expect("get must succeed");
    assert_eq!(bytes, b"the quick brown fox jumps over the lazy dog");
}

/// Requirement 5.1: deleting a stored object removes it, and a subsequent
/// `get` fails rather than returning stale bytes.
#[tokio::test]
async fn delete_removes_the_object_so_it_is_no_longer_retrievable() {
    let (store, _guard) = temp_store("delete");
    let key = ObjectKey::small(Id::from_i64(2));
    store
        .put(&key, b"a thumbnail's bytes", "image/png")
        .await
        .expect("put must succeed");

    store
        .get(&key)
        .await
        .expect("get must succeed before delete");

    store.delete(&key).await.expect("delete must succeed");

    let err = store
        .get(&key)
        .await
        .expect_err("a deleted object must no longer be retrievable");
    assert_eq!(err.status, axum::http::StatusCode::NOT_FOUND);
}

/// Delete is idempotent: deleting an already-absent key still succeeds
/// (a worker's retry/reclaim path may issue the same delete more than
/// once).
#[tokio::test]
async fn delete_on_an_already_absent_key_succeeds() {
    let (store, _guard) = temp_store("delete_absent");
    let key = ObjectKey::original(Id::from_i64(3));
    store
        .delete(&key)
        .await
        .expect("deleting a never-stored key must still succeed");
}

/// The original and the small/thumbnail derivative of the same media are
/// stored independently: deleting one does not affect the other.
#[tokio::test]
async fn original_and_small_derivatives_of_the_same_media_are_independent() {
    let (store, _guard) = temp_store("variant_isolation");
    let media_id = Id::from_i64(4);
    let original = ObjectKey::original(media_id);
    let small = ObjectKey::small(media_id);

    store
        .put(&original, b"original bytes", "image/png")
        .await
        .expect("put original must succeed");
    store
        .put(&small, b"small bytes", "image/png")
        .await
        .expect("put small must succeed");

    store
        .delete(&small)
        .await
        .expect("delete small must succeed");

    assert_eq!(
        store.get(&original).await.expect("original must survive"),
        b"original bytes"
    );
    assert!(store.get(&small).await.is_err());
}

/// Requirement 5.4: `public_url` reflects `X-Forwarded-Proto`/
/// `X-Forwarded-Host`-style proxy information (via `ForwardedOrigin::
/// resolve`) rather than this process's own local bind address/scheme.
#[test]
fn public_url_reflects_forwarded_proxy_headers_via_forwarded_origin_resolve() {
    let (store, _guard) = temp_store("public_url_proxy");
    let key = ObjectKey::original(Id::from_i64(5));

    let origin = ForwardedOrigin::resolve(
        "http",
        "127.0.0.1:9000",
        Some("https"),
        Some("mastodon.example.social"),
    );
    let url = store.public_url(&key, &origin);

    assert!(
        url.starts_with("https://mastodon.example.social/"),
        "expected a proxy-reflecting absolute URL, got {url}"
    );
    assert!(!url.contains("127.0.0.1"), "got {url}");
    assert!(url.contains(key.as_str()), "got {url}");
}

/// Absent any forwarded proxy headers, `public_url` falls back to the
/// connection's own scheme/host (Requirement 5.4's fallback half).
#[test]
fn public_url_falls_back_to_the_connection_origin_without_proxy_headers() {
    let (store, _guard) = temp_store("public_url_fallback");
    let key = ObjectKey::small(Id::from_i64(6));

    let origin = ForwardedOrigin::resolve("http", "127.0.0.1:9000", None, None);
    let url = store.public_url(&key, &origin);

    assert!(url.starts_with("http://127.0.0.1:9000/"), "got {url}");
    assert!(url.contains(key.as_str()), "got {url}");
}

/// The same `(media_id, variant)` pair always resolves to the same on-disk
/// path (Requirement 5.2, 5.3): a second `put` for the same key overwrites
/// the first rather than creating a distinct object.
#[tokio::test]
async fn the_same_object_key_always_resolves_to_the_same_stored_entity() {
    let (store, _guard) = temp_store("deterministic_path");
    let media_id = Id::from_i64(7);
    let key_a = ObjectKey::original(media_id);
    let key_b = ObjectKey::original(media_id);

    store
        .put(&key_a, b"first write", "image/png")
        .await
        .expect("first put must succeed");
    store
        .put(&key_b, b"second write", "image/png")
        .await
        .expect("second put (same key) must succeed");

    let bytes = store.get(&key_a).await.expect("get must succeed");
    assert_eq!(bytes, b"second write");
}
