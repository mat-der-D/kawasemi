//! Integration test proving task 6.1's own upload-side observable completion
//! condition (`.kiro/specs/media-pipeline/tasks.md`, "6.1 アップロード・ポー
//! リング・更新の統合テストを実装する", `_Depends: 5.2_`): "有効画像のアップ
//! ロードで 202（url=null）と所有アクター結びつけ、未対応形式/上限超過で
//! 422... を検証する" (Requirements 1.1, 1.2, 1.3, 1.4, 1.5, 1.6, 9.1, 9.2,
//! 9.3, 9.4).
//!
//! `tests/media_endpoints_it.rs` (task 5.1) already proved this exact
//! response-code/body matrix through a *test-local* router built directly on
//! `MediaEndpointsState`; `tests/media_bootstrap_wiring_it.rs` (task 5.2)
//! already proved the upload path once through the *real* mounted router as
//! part of proving the composition-root wiring itself. This file is task
//! 6.1's own, separate file (design.md's File Structure Plan names it
//! `media_upload_it.rs`) exercising the upload endpoint's full acceptance
//! matrix -- valid/unsupported-format/oversized -- end to end through the
//! real, fully-wired application router `spawn_test_app` serves, as real
//! HTTP requests, without re-deriving or shortcutting any of `src/media/`'s
//! already-implemented business logic.
//!
//! ## No HTTP client dependency: raw sockets, mirroring established
//! precedent (`tests/media_bootstrap_wiring_it.rs`'s/`tests/error_ratelimit_it.rs`'s
//! own `RawResponse`/`raw_request` helpers) -- each `tests/*.rs` file is a
//! separate compiled binary with no shared module, so this small amount of
//! plumbing duplication is this crate's own documented convention rather
//! than an oversight.

use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use serde_json::Value;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use kawasemi::domain::Id;
use kawasemi::oauth::app_repository::{self, NewApp};
use kawasemi::oauth::model::ScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::test_harness::{TestApp, spawn_test_app};

// ==========================================================================
// Raw HTTP plumbing (byte-bodied, header-capturing variant -- mirrors
// `tests/media_bootstrap_wiring_it.rs`'s own identical helpers).
// ==========================================================================

struct RawResponse {
    status: u16,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

impl std::fmt::Debug for RawResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RawResponse")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .field("body", &String::from_utf8_lossy(&self.body))
            .finish()
    }
}

async fn raw_request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(String, String)],
    body: &[u8],
) -> RawResponse {
    let mut stream = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(addr))
        .await
        .expect("connecting to the test listener must not time out")
        .expect("connect");

    let mut request =
        format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    let mut request_bytes = request.into_bytes();
    request_bytes.extend_from_slice(body);

    stream
        .write_all(&request_bytes)
        .await
        .expect("write request");

    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(15), stream.read_to_end(&mut buf))
        .await
        .expect("read must not time out")
        .expect("read response");

    parse_response(&buf)
}

fn parse_response(raw: &[u8]) -> RawResponse {
    let split_at = raw.windows(4).position(|w| w == b"\r\n\r\n");
    let (head_bytes, body) = match split_at {
        Some(idx) => (&raw[..idx], raw[idx + 4..].to_vec()),
        None => (raw, Vec::new()),
    };
    let head = String::from_utf8_lossy(head_bytes);
    let mut lines = head.lines();
    let status_line = lines.next().unwrap_or("");
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    let mut headers = HashMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    RawResponse {
        status,
        headers,
        body,
    }
}

fn body_json(response: &RawResponse) -> Value {
    serde_json::from_slice(&response.body)
        .unwrap_or_else(|e| panic!("response body must be valid JSON: {e}; body: {response:?}"))
}

// ==========================================================================
// Multipart request body construction (duplicated -- see this file's
// module doc comment).
// ==========================================================================

struct MultipartBuilder {
    boundary: &'static str,
    body: Vec<u8>,
}

impl MultipartBuilder {
    fn new() -> Self {
        MultipartBuilder {
            boundary: "kawasemi-media-upload-it-boundary",
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

/// A small but genuine, decodable in-memory PNG (a gradient, not a solid
/// color) -- mirrors `tests/media_bootstrap_wiring_it.rs`'s own established
/// fixture convention.
fn sample_png(width: u32, height: u32) -> Vec<u8> {
    let rgba: RgbaImage = RgbaImage::from_fn(width, height, |x, y| {
        let r = (x * 255 / width.max(1)) as u8;
        let g = (y * 255 / height.max(1)) as u8;
        Rgba([r, g, 128, 255])
    });
    let mut bytes = Vec::new();
    DynamicImage::ImageRgba8(rgba)
        .write_to(&mut std::io::Cursor::new(&mut bytes), ImageFormat::Png)
        .expect("encoding the in-memory fixture PNG must succeed");
    bytes
}

// ==========================================================================
// Fixtures.
// ==========================================================================

async fn register_test_app(app: &TestApp) -> Id {
    let key = app.state.oauth().token_hash_key();
    let now = app.runtime.clock.now();
    let registered = app_repository::register_app(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        key,
        now,
        NewApp {
            name: "Media Upload IT Client".to_string(),
            redirect_uris: vec!["https://client.example/callback".to_string()],
            scopes: ScopeSet::new(["read", "write"]),
        },
    )
    .await
    .expect("register_app must succeed");
    registered.id
}

/// A `write:media`-scoped token bound to a fresh actor id, plus that actor
/// id itself.
async fn issue_write_media_token(app: &TestApp, app_id: Id) -> (String, Id) {
    let key = app.state.oauth().token_hash_key();
    let actor_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    let issued = token_repository::issue_token(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        key,
        now,
        NewAccessToken {
            app_id,
            actor_id,
            scopes: ScopeSet::new(["write:media"]),
        },
    )
    .await
    .expect("issue_token must succeed");
    (issued.plaintext.expose_secret().to_string(), actor_id)
}

fn auth_header(token: &str) -> (String, String) {
    ("Authorization".to_string(), format!("Bearer {token}"))
}

async fn upload(
    app: &TestApp,
    token: &str,
    filename: &str,
    content_type: &str,
    bytes: &[u8],
    extra_fields: &[(&str, &str)],
) -> RawResponse {
    let mut builder = MultipartBuilder::new();
    for (name, value) in extra_fields {
        builder = builder.text_field(name, value);
    }
    let (body, multipart_content_type) = builder
        .file_field("file", filename, content_type, bytes)
        .finish();
    raw_request(
        app.address,
        "POST",
        "/api/v2/media",
        &[
            ("Content-Type".to_string(), multipart_content_type),
            auth_header(token),
        ],
        &body,
    )
    .await
}

fn media_id_of(json: &Value) -> Id {
    Id::from_i64(
        json["id"]
            .as_str()
            .expect("id must be a decimal string")
            .parse()
            .expect("id must parse as i64"),
    )
}

// ==========================================================================
// Tests
// ==========================================================================

/// Requirements 1.1, 1.2, 9.1: a valid image upload through the real router
/// is accepted (`202`), carries a `null` `url` (processing not yet
/// complete), and is bound to the uploading actor.
#[tokio::test]
async fn upload_a_valid_image_returns_202_with_a_null_url_and_binds_the_owning_actor() {
    let app = spawn_test_app().await;
    let app_id = register_test_app(&app).await;
    let (token, actor_id) = issue_write_media_token(&app, app_id).await;

    let response = upload(
        &app,
        &token,
        "fixture.png",
        "image/png",
        &sample_png(8, 8),
        &[],
    )
    .await;

    assert_eq!(response.status, 202, "got: {response:?}");
    let json = body_json(&response);
    assert_eq!(json["type"], "image", "got: {json}");
    assert_eq!(
        json["url"],
        Value::Null,
        "processing media must have a null url, got: {json}"
    );

    let media_id = media_id_of(&json);
    let owned = kawasemi::media::find_owned(&app.pool, media_id, actor_id)
        .await
        .expect("find_owned must succeed")
        .expect("the uploaded media must be bound to the uploading actor");
    assert_eq!(owned.actor_id, actor_id);

    app.cleanup().await;
}

/// Requirement 1.5: description and focus supplied at upload time are
/// recorded and reflected back immediately (even before processing
/// completes).
#[tokio::test]
async fn upload_with_description_and_focus_records_both_immediately() {
    let app = spawn_test_app().await;
    let app_id = register_test_app(&app).await;
    let (token, _actor_id) = issue_write_media_token(&app, app_id).await;

    let response = upload(
        &app,
        &token,
        "fixture.png",
        "image/png",
        &sample_png(4, 4),
        &[("description", "a small gradient"), ("focus", "0.5,-0.25")],
    )
    .await;

    assert_eq!(response.status, 202, "got: {response:?}");
    let json = body_json(&response);
    assert_eq!(json["description"], "a small gradient", "got: {json}");
    assert_eq!(json["meta"]["focus"]["x"], 0.5, "got: {json}");
    assert_eq!(json["meta"]["focus"]["y"], -0.25, "got: {json}");

    app.cleanup().await;
}

/// Requirement 1.3: an unsupported media format is rejected with `422` and
/// a Mastodon-compatible `{"error": ...}` body, never stored.
#[tokio::test]
async fn upload_an_unsupported_format_is_422_with_a_compatible_error_body() {
    let app = spawn_test_app().await;
    let app_id = register_test_app(&app).await;
    let (token, _actor_id) = issue_write_media_token(&app, app_id).await;

    let response = upload(
        &app,
        &token,
        "clip.mp4",
        "video/mp4",
        b"not actually a video",
        &[],
    )
    .await;

    assert_eq!(response.status, 422, "got: {response:?}");
    let json = body_json(&response);
    assert!(
        json["error"].is_string(),
        "expected a compatible {{\"error\": ...}} body, got {json:?}"
    );

    app.cleanup().await;
}

/// Requirement 1.4: an upload whose declared file exceeds the configured
/// maximum accepted size is rejected with `422` and a Mastodon-compatible
/// error body, never stored. `spawn_test_app`'s `MediaConfig::max_upload_size_bytes`
/// is 10 MiB (`src/test_harness.rs`); this fixture's file field alone is
/// well past that.
#[tokio::test]
async fn upload_exceeding_the_configured_size_limit_is_422_with_a_compatible_error_body() {
    let app = spawn_test_app().await;
    let app_id = register_test_app(&app).await;
    let (token, _actor_id) = issue_write_media_token(&app, app_id).await;

    let oversized = vec![0u8; 11 * 1024 * 1024];
    let response = upload(&app, &token, "big.png", "image/png", &oversized, &[]).await;

    assert_eq!(response.status, 422, "got: {response:?}");
    let json = body_json(&response);
    assert!(
        json["error"].is_string(),
        "expected a compatible {{\"error\": ...}} body for an oversized upload, got {json:?}"
    );

    app.cleanup().await;
}
