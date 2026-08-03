//! Integration test for the media endpoint handlers
//! (`.kiro/specs/media-pipeline/tasks.md`, task 5.1 "メディアエンドポイント
//! を実装する"; `src/media/endpoints.rs`'s own doc comment).
//!
//! This task's own observable completion condition: "各エンドポイントが規定
//! の応答コードと互換エラー本文を返すこと、処理失敗状態のメディアの取得が
//! `{"error": "..."}` 形の 422 を返すことを統合テストで確認できる"
//! (Requirements 1.1, 2.1, 2.2, 2.3, 3.1, 3.2, 6.5, 9.1, 9.2, 9.3, 9.4).
//!
//! Nothing wires `upload_media`/`show_media`/`update_media` into the live
//! application router yet (task 5.2's job) — mirroring
//! `tests/webfinger_nodeinfo_it.rs`'s already-established precedent for the
//! same "endpoint implemented, router wiring not yet landed" situation in
//! federation-core's task 5.1 (see `src/media/endpoints.rs`'s own doc
//! comment, "Test-local router"), this file builds a minimal, test-local
//! `axum::Router<MediaEndpointsState<LocalFsStore>>` mounting just these
//! three handlers and drives it through real HTTP-shaped `Request`/
//! `Response` values via `tower::ServiceExt::oneshot` against a real,
//! `spawn_test_app`-backed Postgres schema (token issuance/resolution and
//! media persistence both genuinely need the database).
//!
//! `tests/media_upload_it.rs`/`media_poll_it.rs`/`media_update_it.rs`
//! (design.md's File Structure Plan) are task 6.1's own, separate,
//! `_Depends: 5.2_` files proving the same behavior through the real
//! mounted application router once task 5.2 lands — this file does not
//! preempt those filenames.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::routing::get;
use serde_json::{Value, json};
use tower::ServiceExt;

use kawasemi::config::{MediaConfig, Secret};
use kawasemi::domain::Id;
use kawasemi::media::{
    Dimensions, LocalFsStore, MediaEndpointsState, MediaMeta, MediaService, set_failed, set_ready,
    show_media, update_media, upload_media,
};
use kawasemi::oauth::app_repository::{self, NewApp};
use kawasemi::oauth::hash::TokenHashKey;
use kawasemi::oauth::middleware::AuthState;
use kawasemi::oauth::model::ScopeSet as ModelScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::runtime::RuntimeContext;
use kawasemi::test_harness::{TestApp, spawn_test_app};

// ---- Fixture plumbing (mirrors `src/oauth/middleware/tests.rs`'s and
// `src/media/service/tests.rs`'s already-established conventions; small
// helper duplication across sibling test modules is this crate's own
// documented convention — see tasks.md's Implementation Notes for 3.2). ----

/// A fixed, non-production token-hashing key for this test module only,
/// independent of `spawn_test_app`'s own internal fixed key — `AuthState`
/// takes the key explicitly, so these tests only need *a* fixed key
/// consistently used for both issuance and resolution (mirrors
/// `oauth/middleware/tests.rs::test_token_hash_key`).
fn test_token_hash_key() -> TokenHashKey {
    Secret::new([0x51; 32])
}

async fn register_test_app(pool: &sqlx::PgPool, runtime: &RuntimeContext) -> Id {
    let key = test_token_hash_key();
    let now = runtime.clock.now();
    let registered = app_repository::register_app(
        pool,
        runtime.ids.as_ref(),
        runtime.rng.as_ref(),
        &key,
        now,
        NewApp {
            name: "Media Endpoints Test Client".to_string(),
            redirect_uris: vec!["https://client.example/callback".to_string()],
            scopes: ModelScopeSet::new(["read", "write"]),
        },
    )
    .await
    .expect("register_app must succeed");
    registered.id
}

/// Issues a real access token bound to `actor_id` with `scopes`. Never a
/// full `local_actors` row: neither `oauth_access_tokens.actor_id` nor
/// `media.actor_id` carries a real FK to actor-model (both are documented
/// logical-only references), so a bare freshly-minted `Id` is a legitimate
/// owning actor for these tests, mirroring
/// `oauth/middleware/tests.rs::issue_test_token`'s own identical choice.
async fn issue_test_token(
    pool: &sqlx::PgPool,
    runtime: &RuntimeContext,
    app_id: Id,
    actor_id: Id,
    scopes: &[&str],
) -> String {
    let key = test_token_hash_key();
    let now = runtime.clock.now();
    let issued = token_repository::issue_token(
        pool,
        runtime.ids.as_ref(),
        runtime.rng.as_ref(),
        &key,
        now,
        NewAccessToken {
            app_id,
            actor_id,
            scopes: ModelScopeSet::new(scopes.iter().copied()),
        },
    )
    .await
    .expect("issue_token must succeed");
    issued.plaintext.expose_secret().to_string()
}

/// A `write:media`-scoped token bound to a fresh actor id, plus that actor
/// id itself (most tests need both).
async fn write_media_token(app: &TestApp, app_id: Id) -> (String, Id) {
    let actor_id = app.runtime.ids.next_id();
    let token = issue_test_token(&app.pool, &app.runtime, app_id, actor_id, &["write:media"]).await;
    (token, actor_id)
}

fn unique_temp_root(label: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("kawasemi_media_endpoints_it_{label}_{nanos}_{seq}"))
}

struct TempDirGuard(std::path::PathBuf);
impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A small `max_upload_size_bytes` (1 KiB) so an oversized-upload test can
/// exercise `MediaService`'s own `422` size-validation path without
/// approaching axum's unrelated, much larger built-in 2 MB request-body cap
/// (see `src/media/endpoints.rs`'s own doc comment, "CONCERN for task 5.2",
/// for why those two limits are different things).
fn test_media_config() -> MediaConfig {
    MediaConfig {
        storage_root: std::path::PathBuf::from("media_storage"),
        max_upload_size_bytes: 1024,
        thumbnail_target_width: 400,
        thumbnail_target_height: 400,
        supported_formats: vec!["image/jpeg".to_string(), "image/png".to_string()],
        worker_concurrency: 2,
        max_retry_attempts: 5,
        lease_duration: std::time::Duration::from_secs(5 * 60),
    }
}

/// Builds a real `MediaEndpointsState<LocalFsStore>` (real Postgres pool,
/// real `LocalFsStore` rooted at a throwaway temp directory, real
/// `AuthState`) and the test-local router mounting all three handlers —
/// nothing here is faked.
fn test_state_and_router(app: &TestApp, label: &str) -> (Router, TempDirGuard) {
    let root = unique_temp_root(label);
    let guard = TempDirGuard(root.clone());
    let store = LocalFsStore::new(root);
    let media_service = Arc::new(MediaService::new(
        app.pool.clone(),
        app.runtime.clone(),
        test_media_config(),
        store.clone(),
    ));
    let state = MediaEndpointsState {
        media_service,
        store,
        auth: AuthState {
            pool: app.pool.clone(),
            token_hash_key: test_token_hash_key(),
        },
    };

    let router = Router::new()
        .route(
            "/api/v2/media",
            axum::routing::post(upload_media::<LocalFsStore>),
        )
        .route(
            "/api/v1/media/{id}",
            get(show_media::<LocalFsStore>).put(update_media::<LocalFsStore>),
        )
        .with_state(state);

    (router, guard)
}

// ---- multipart request body construction (no multipart-building crate in
// this repo's dependencies yet — this task is the first multipart
// consumer, so this small hand-rolled builder is a genuine addition, kept
// local to this test file rather than a new production dependency). ----

struct MultipartBuilder {
    boundary: &'static str,
    body: Vec<u8>,
}

impl MultipartBuilder {
    fn new() -> Self {
        MultipartBuilder {
            boundary: "kawasemi-media-endpoints-it-boundary",
            body: Vec::new(),
        }
    }

    fn text_field(mut self, name: &str, value: &str) -> Self {
        self.body
            .extend_from_slice(format!("--{}\r\n", self.boundary).as_bytes());
        self.body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
        );
        self.body.extend_from_slice(value.as_bytes());
        self.body.extend_from_slice(b"\r\n");
        self
    }

    fn file_field(mut self, name: &str, filename: &str, content_type: &str, bytes: &[u8]) -> Self {
        self.body
            .extend_from_slice(format!("--{}\r\n", self.boundary).as_bytes());
        self.body.extend_from_slice(
            format!(
                "Content-Disposition: form-data; name=\"{name}\"; filename=\"{filename}\"\r\nContent-Type: {content_type}\r\n\r\n"
            )
            .as_bytes(),
        );
        self.body.extend_from_slice(bytes);
        self.body.extend_from_slice(b"\r\n");
        self
    }

    fn finish(mut self) -> (Vec<u8>, String) {
        self.body
            .extend_from_slice(format!("--{}--\r\n", self.boundary).as_bytes());
        (
            self.body,
            format!("multipart/form-data; boundary={}", self.boundary),
        )
    }
}

fn multipart_request(
    uri: &str,
    bearer: Option<&str>,
    body: Vec<u8>,
    content_type: String,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, content_type);
    if let Some(token) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    builder.body(Body::from(body)).expect("valid test request")
}

fn get_request(uri: &str, bearer: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method("GET").uri(uri);
    if let Some(token) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    builder.body(Body::empty()).expect("valid test request")
}

fn put_json_request(uri: &str, bearer: Option<&str>, json: &Value) -> Request<Body> {
    let mut builder = Request::builder()
        .method("PUT")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(token) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    builder
        .body(Body::from(json.to_string()))
        .expect("valid test request")
}

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("test response body should be readable");
    serde_json::from_slice(&bytes).expect("test response body should be valid JSON")
}

/// Uploads a valid small "image" via a real request through the router,
/// returning the parsed `202` response body and the media id it carries.
async fn upload_fixture(router: &Router, token: &str) -> (Value, Id) {
    let (body, content_type) = MultipartBuilder::new()
        .file_field(
            "file",
            "photo.png",
            "image/png",
            b"a small fake png payload",
        )
        .finish();
    let response = router
        .clone()
        .oneshot(multipart_request(
            "/api/v2/media",
            Some(token),
            body,
            content_type,
        ))
        .await
        .expect("oneshot dispatch must succeed");
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let json = body_json(response).await;
    let id = Id::from_i64(
        json["id"]
            .as_str()
            .expect("id must be a decimal string")
            .parse()
            .expect("id must parse as i64"),
    );
    (json, id)
}

// ==== POST /api/v2/media (Requirements 1.1, 9.1, 9.2, 9.3) ====

#[tokio::test]
async fn upload_without_a_bearer_token_is_401() {
    let app = spawn_test_app().await;
    let (router, _guard) = test_state_and_router(&app, "upload_401");

    let (body, content_type) = MultipartBuilder::new()
        .file_field("file", "photo.png", "image/png", b"bytes")
        .finish();
    let response = router
        .oneshot(multipart_request("/api/v2/media", None, body, content_type))
        .await
        .expect("oneshot dispatch must succeed");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    app.cleanup().await;
}

#[tokio::test]
async fn upload_with_a_token_missing_write_media_scope_is_403() {
    let app = spawn_test_app().await;
    let app_id = register_test_app(&app.pool, &app.runtime).await;
    let actor_id = app.runtime.ids.next_id();
    let token = issue_test_token(&app.pool, &app.runtime, app_id, actor_id, &["read"]).await;
    let (router, _guard) = test_state_and_router(&app, "upload_403");

    let (body, content_type) = MultipartBuilder::new()
        .file_field("file", "photo.png", "image/png", b"bytes")
        .finish();
    let response = router
        .oneshot(multipart_request(
            "/api/v2/media",
            Some(&token),
            body,
            content_type,
        ))
        .await
        .expect("oneshot dispatch must succeed");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    app.cleanup().await;
}

#[tokio::test]
async fn upload_a_valid_image_returns_202_with_a_null_url_and_binds_the_owning_actor() {
    let app = spawn_test_app().await;
    let app_id = register_test_app(&app.pool, &app.runtime).await;
    let (token, actor_id) = write_media_token(&app, app_id).await;
    let (router, _guard) = test_state_and_router(&app, "upload_202");

    let (json, media_id) = upload_fixture(&router, &token).await;
    assert!(json["id"].is_string());
    assert_eq!(json["type"], "image");
    assert_eq!(
        json["url"],
        Value::Null,
        "processing media must have a null url"
    );

    let owned = kawasemi::media::find_owned(&app.pool, media_id, actor_id)
        .await
        .expect("find_owned must succeed")
        .expect("the uploaded media must be bound to the uploading actor");
    assert_eq!(owned.actor_id, actor_id);

    app.cleanup().await;
}

#[tokio::test]
async fn upload_with_no_file_field_is_422_with_a_compatible_error_body() {
    let app = spawn_test_app().await;
    let app_id = register_test_app(&app.pool, &app.runtime).await;
    let (token, _actor_id) = write_media_token(&app, app_id).await;
    let (router, _guard) = test_state_and_router(&app, "upload_missing_file");

    let (body, content_type) = MultipartBuilder::new()
        .text_field("description", "a cat")
        .finish();
    let response = router
        .oneshot(multipart_request(
            "/api/v2/media",
            Some(&token),
            body,
            content_type,
        ))
        .await
        .expect("oneshot dispatch must succeed");
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let json = body_json(response).await;
    assert!(
        json["error"].is_string(),
        "expected a compatible {{\"error\": ...}} body, got {json:?}"
    );

    app.cleanup().await;
}

#[tokio::test]
async fn upload_an_unsupported_format_is_422() {
    let app = spawn_test_app().await;
    let app_id = register_test_app(&app.pool, &app.runtime).await;
    let (token, _actor_id) = write_media_token(&app, app_id).await;
    let (router, _guard) = test_state_and_router(&app, "upload_unsupported_format");

    let (body, content_type) = MultipartBuilder::new()
        .file_field("file", "clip.mp4", "video/mp4", b"not actually a video")
        .finish();
    let response = router
        .oneshot(multipart_request(
            "/api/v2/media",
            Some(&token),
            body,
            content_type,
        ))
        .await
        .expect("oneshot dispatch must succeed");
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

    app.cleanup().await;
}

#[tokio::test]
async fn upload_exceeding_the_configured_size_limit_is_422() {
    let app = spawn_test_app().await;
    let app_id = register_test_app(&app.pool, &app.runtime).await;
    let (token, _actor_id) = write_media_token(&app, app_id).await;
    let (router, _guard) = test_state_and_router(&app, "upload_oversized");

    // `test_media_config()` caps uploads at 1024 bytes; this payload is
    // ~2000 bytes -- comfortably over that limit but nowhere near axum's
    // unrelated 2 MB built-in cap (see `MultipartBuilder`'s own doc
    // comment / `test_media_config`'s doc comment).
    let oversized = vec![0u8; 2000];
    let (body, content_type) = MultipartBuilder::new()
        .file_field("file", "big.png", "image/png", &oversized)
        .finish();
    let response = router
        .oneshot(multipart_request(
            "/api/v2/media",
            Some(&token),
            body,
            content_type,
        ))
        .await
        .expect("oneshot dispatch must succeed");
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

    app.cleanup().await;
}

// ==== GET /api/v1/media/:id (Requirements 2.1, 2.2, 2.3, 2.4, 6.5, 9.1-9.4) ====

#[tokio::test]
async fn show_without_a_bearer_token_is_401() {
    let app = spawn_test_app().await;
    let app_id = register_test_app(&app.pool, &app.runtime).await;
    let (token, _actor_id) = write_media_token(&app, app_id).await;
    let (router, _guard) = test_state_and_router(&app, "show_401");
    let (_json, media_id) = upload_fixture(&router, &token).await;

    let response = router
        .oneshot(get_request(
            &format!("/api/v1/media/{}", media_id.as_i64()),
            None,
        ))
        .await
        .expect("oneshot dispatch must succeed");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    app.cleanup().await;
}

#[tokio::test]
async fn show_with_insufficient_scope_is_403() {
    let app = spawn_test_app().await;
    let app_id = register_test_app(&app.pool, &app.runtime).await;
    let (token, actor_id) = write_media_token(&app, app_id).await;
    let (router, _guard) = test_state_and_router(&app, "show_403");
    let (_json, media_id) = upload_fixture(&router, &token).await;

    let read_only_token =
        issue_test_token(&app.pool, &app.runtime, app_id, actor_id, &["read"]).await;
    let response = router
        .oneshot(get_request(
            &format!("/api/v1/media/{}", media_id.as_i64()),
            Some(&read_only_token),
        ))
        .await
        .expect("oneshot dispatch must succeed");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    app.cleanup().await;
}

#[tokio::test]
async fn show_processing_media_returns_206_with_a_null_url() {
    let app = spawn_test_app().await;
    let app_id = register_test_app(&app.pool, &app.runtime).await;
    let (token, _actor_id) = write_media_token(&app, app_id).await;
    let (router, _guard) = test_state_and_router(&app, "show_206");
    let (_json, media_id) = upload_fixture(&router, &token).await;

    let response = router
        .oneshot(get_request(
            &format!("/api/v1/media/{}", media_id.as_i64()),
            Some(&token),
        ))
        .await
        .expect("oneshot dispatch must succeed");
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    let json = body_json(response).await;
    assert_eq!(json["url"], Value::Null);

    app.cleanup().await;
}

#[tokio::test]
async fn show_ready_media_returns_200_with_a_full_representation() {
    let app = spawn_test_app().await;
    let app_id = register_test_app(&app.pool, &app.runtime).await;
    let (token, _actor_id) = write_media_token(&app, app_id).await;
    let (router, _guard) = test_state_and_router(&app, "show_200");
    let (_json, media_id) = upload_fixture(&router, &token).await;

    let meta = MediaMeta {
        original: Dimensions {
            width: 800,
            height: 600,
            aspect: 800.0 / 600.0,
        },
        small: Some(Dimensions {
            width: 400,
            height: 300,
            aspect: 400.0 / 300.0,
        }),
    };
    let now = app.runtime.clock.now();
    set_ready(&app.pool, media_id, &meta, "fakeblurhash", "thumb-key", now)
        .await
        .expect("set_ready must succeed");

    let response = router
        .oneshot(get_request(
            &format!("/api/v1/media/{}", media_id.as_i64()),
            Some(&token),
        ))
        .await
        .expect("oneshot dispatch must succeed");
    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert!(
        json["url"].is_string(),
        "ready media must have a resolved url, got {json:?}"
    );
    assert!(
        json["preview_url"].is_string(),
        "ready media with a small derivative must have a resolved preview_url, got {json:?}"
    );
    assert_eq!(json["meta"]["original"]["width"], 800);
    assert_eq!(json["meta"]["small"]["width"], 400);

    app.cleanup().await;
}

#[tokio::test]
async fn show_failed_media_returns_422_with_a_compatible_error_body() {
    let app = spawn_test_app().await;
    let app_id = register_test_app(&app.pool, &app.runtime).await;
    let (token, _actor_id) = write_media_token(&app, app_id).await;
    let (router, _guard) = test_state_and_router(&app, "show_failed_422");
    let (_json, media_id) = upload_fixture(&router, &token).await;

    let now = app.runtime.clock.now();
    set_failed(&app.pool, media_id, now)
        .await
        .expect("set_failed must succeed");

    let response = router
        .oneshot(get_request(
            &format!("/api/v1/media/{}", media_id.as_i64()),
            Some(&token),
        ))
        .await
        .expect("oneshot dispatch must succeed");
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let json = body_json(response).await;
    assert!(
        json["error"].is_string(),
        "expected a compatible {{\"error\": ...}} body for a failed media, got {json:?}"
    );

    app.cleanup().await;
}

#[tokio::test]
async fn show_a_nonexistent_media_is_404() {
    let app = spawn_test_app().await;
    let app_id = register_test_app(&app.pool, &app.runtime).await;
    let (token, _actor_id) = write_media_token(&app, app_id).await;
    let (router, _guard) = test_state_and_router(&app, "show_404_missing");

    let response = router
        .oneshot(get_request("/api/v1/media/999999999", Some(&token)))
        .await
        .expect("oneshot dispatch must succeed");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    app.cleanup().await;
}

#[tokio::test]
async fn show_another_actors_media_is_404_not_leaked() {
    let app = spawn_test_app().await;
    let app_id = register_test_app(&app.pool, &app.runtime).await;
    let (owner_token, _owner_id) = write_media_token(&app, app_id).await;
    let (router, _guard) = test_state_and_router(&app, "show_404_other_actor");
    let (_json, media_id) = upload_fixture(&router, &owner_token).await;

    let (other_token, _other_id) = write_media_token(&app, app_id).await;
    let response = router
        .oneshot(get_request(
            &format!("/api/v1/media/{}", media_id.as_i64()),
            Some(&other_token),
        ))
        .await
        .expect("oneshot dispatch must succeed");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    app.cleanup().await;
}

// ==== PUT /api/v1/media/:id (Requirements 3.1, 3.2, 3.3, 3.4, 9.1-9.4) ====

#[tokio::test]
async fn update_without_a_bearer_token_is_401() {
    let app = spawn_test_app().await;
    let app_id = register_test_app(&app.pool, &app.runtime).await;
    let (token, _actor_id) = write_media_token(&app, app_id).await;
    let (router, _guard) = test_state_and_router(&app, "update_401");
    let (_json, media_id) = upload_fixture(&router, &token).await;

    let response = router
        .oneshot(put_json_request(
            &format!("/api/v1/media/{}", media_id.as_i64()),
            None,
            &json!({"description": "a cat"}),
        ))
        .await
        .expect("oneshot dispatch must succeed");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    app.cleanup().await;
}

#[tokio::test]
async fn update_a_still_processing_media_succeeds_with_200_and_reflects_the_new_values() {
    let app = spawn_test_app().await;
    let app_id = register_test_app(&app.pool, &app.runtime).await;
    let (token, _actor_id) = write_media_token(&app, app_id).await;
    let (router, _guard) = test_state_and_router(&app, "update_200");
    let (_json, media_id) = upload_fixture(&router, &token).await;

    let response = router
        .oneshot(put_json_request(
            &format!("/api/v1/media/{}", media_id.as_i64()),
            Some(&token),
            &json!({"description": "a very good cat", "focus": "0.5,-0.5"}),
        ))
        .await
        .expect("oneshot dispatch must succeed");
    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert_eq!(json["description"], "a very good cat");
    assert_eq!(json["meta"]["focus"]["x"], 0.5);
    assert_eq!(json["meta"]["focus"]["y"], -0.5);

    app.cleanup().await;
}

#[tokio::test]
async fn update_with_an_out_of_range_focus_is_422() {
    let app = spawn_test_app().await;
    let app_id = register_test_app(&app.pool, &app.runtime).await;
    let (token, _actor_id) = write_media_token(&app, app_id).await;
    let (router, _guard) = test_state_and_router(&app, "update_out_of_range_422");
    let (_json, media_id) = upload_fixture(&router, &token).await;

    let response = router
        .oneshot(put_json_request(
            &format!("/api/v1/media/{}", media_id.as_i64()),
            Some(&token),
            &json!({"focus": "5.0,-5.0"}),
        ))
        .await
        .expect("oneshot dispatch must succeed");
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

    app.cleanup().await;
}

#[tokio::test]
async fn update_with_a_malformed_focus_string_is_422() {
    let app = spawn_test_app().await;
    let app_id = register_test_app(&app.pool, &app.runtime).await;
    let (token, _actor_id) = write_media_token(&app, app_id).await;
    let (router, _guard) = test_state_and_router(&app, "update_malformed_422");
    let (_json, media_id) = upload_fixture(&router, &token).await;

    let response = router
        .oneshot(put_json_request(
            &format!("/api/v1/media/{}", media_id.as_i64()),
            Some(&token),
            &json!({"focus": "not-a-number"}),
        ))
        .await
        .expect("oneshot dispatch must succeed");
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

    app.cleanup().await;
}

#[tokio::test]
async fn update_a_nonexistent_media_is_404() {
    let app = spawn_test_app().await;
    let app_id = register_test_app(&app.pool, &app.runtime).await;
    let (token, _actor_id) = write_media_token(&app, app_id).await;
    let (router, _guard) = test_state_and_router(&app, "update_404_missing");

    let response = router
        .oneshot(put_json_request(
            "/api/v1/media/999999999",
            Some(&token),
            &json!({"description": "ghost"}),
        ))
        .await
        .expect("oneshot dispatch must succeed");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    app.cleanup().await;
}

#[tokio::test]
async fn update_another_actors_media_is_404_not_leaked() {
    let app = spawn_test_app().await;
    let app_id = register_test_app(&app.pool, &app.runtime).await;
    let (owner_token, _owner_id) = write_media_token(&app, app_id).await;
    let (router, _guard) = test_state_and_router(&app, "update_404_other_actor");
    let (_json, media_id) = upload_fixture(&router, &owner_token).await;

    let (other_token, _other_id) = write_media_token(&app, app_id).await;
    let response = router
        .oneshot(put_json_request(
            &format!("/api/v1/media/{}", media_id.as_i64()),
            Some(&other_token),
            &json!({"description": "hijacked"}),
        ))
        .await
        .expect("oneshot dispatch must succeed");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    app.cleanup().await;
}
