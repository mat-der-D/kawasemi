//! Integration test proving task 6.1's own polling-side observable
//! completion condition (`.kiro/specs/media-pipeline/tasks.md`, "6.1 アップ
//! ロード・ポーリング・更新の統合テストを実装する", `_Depends: 5.2_`): "処理
//! 後のポーリングで 206→200、処理失敗状態のメディアの取得で互換エラー本文
//! の 422、未存在で 404、他アクターからの不可視...を検証する" (Requirements
//! 2.1, 2.2, 2.3, 2.4, 6.5, 9.1, 9.2, 9.3, 9.4).
//!
//! Mirrors `tests/media_upload_it.rs`'s own module doc comment for why this
//! is a separate `_it.rs` file (design.md's File Structure Plan names it
//! `media_poll_it.rs`), why it duplicates a small amount of raw-HTTP/
//! multipart plumbing rather than sharing a module with sibling `tests/*.rs`
//! files, and why it drives the *real*, fully-composed application router
//! `spawn_test_app` serves rather than a test-local one.
//!
//! ## Reaching a `failed` media without bypassing the real pipeline
//! Unlike `tests/media_endpoints_it.rs` (task 5.1), which could call
//! `set_failed` directly against its test-local router's own service since
//! nothing there ever ran a real worker, this file drives a genuine
//! decode failure through the *actual* resident `ProcessingWorker` pool
//! `spawn_test_app` starts: an upload whose declared `content-type` is
//! `image/png` (passing `MediaService`'s own format check) but whose bytes
//! are not a decodable image at all. `src/media/worker.rs`'s own documented
//! failure classification treats `MediaProcessor::process_image` returning
//! `Err` as an immediate terminal failure (`fail_or_retry` called with
//! `max_attempts = 0`), bypassing the retry budget entirely -- so polling
//! converges to `422` just as quickly as a valid upload converges to `200`
//! in `worker_converges_..._through_the_real_mounted_router`
//! (`tests/media_bootstrap_wiring_it.rs`, task 5.2).

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
// Raw HTTP plumbing (duplicated -- see this file's module doc comment).
// ==========================================================================

struct RawResponse {
    status: u16,
    #[allow(dead_code)]
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
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut buf))
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
// Multipart request body construction (duplicated).
// ==========================================================================

struct MultipartBuilder {
    boundary: &'static str,
    body: Vec<u8>,
}

impl MultipartBuilder {
    fn new() -> Self {
        MultipartBuilder {
            boundary: "kawasemi-media-poll-it-boundary",
            body: Vec::new(),
        }
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
            name: "Media Poll IT Client".to_string(),
            redirect_uris: vec!["https://client.example/callback".to_string()],
            scopes: ScopeSet::new(["read", "write"]),
        },
    )
    .await
    .expect("register_app must succeed");
    registered.id
}

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

async fn upload_bytes(
    app: &TestApp,
    token: &str,
    filename: &str,
    content_type: &str,
    bytes: &[u8],
) -> RawResponse {
    let (body, multipart_content_type) = MultipartBuilder::new()
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

fn media_id_of(json: &Value) -> String {
    json["id"]
        .as_str()
        .expect("id must be a decimal string")
        .to_string()
}

async fn get_media(addr: SocketAddr, media_id: &str, token: &str) -> RawResponse {
    raw_request(
        addr,
        "GET",
        &format!("/api/v1/media/{media_id}"),
        &[auth_header(token)],
        b"",
    )
    .await
}

/// Polls `GET /api/v1/media/:id` through the real router with a bounded
/// retry loop until it stops reporting `206` (Requirement 2.1) -- mirroring
/// `tests/media_bootstrap_wiring_it.rs`'s own `poll_until_ready` bound-poll
/// convention (`DEFAULT_POLL_INTERVAL` is 500ms; 40 attempts at 250ms apart
/// bounds this to at most 10s).
async fn poll_until_settled(addr: SocketAddr, media_id: &str, token: &str) -> RawResponse {
    const MAX_ATTEMPTS: u32 = 40;
    const RETRY_DELAY: Duration = Duration::from_millis(250);

    for attempt in 0..MAX_ATTEMPTS {
        let response = get_media(addr, media_id, token).await;
        if response.status != 206 {
            return response;
        }
        let _ = attempt;
        tokio::time::sleep(RETRY_DELAY).await;
    }

    panic!(
        "media did not leave the `processing` (206) state within {MAX_ATTEMPTS} polls \
         ({RETRY_DELAY:?} apart) -- is the resident ProcessingWorker pool actually running?"
    );
}

// ==========================================================================
// Tests
// ==========================================================================

/// Requirements 2.1, 2.2: polling a freshly uploaded (still processing)
/// media reports `206`; once the resident worker converges it to `ready`,
/// polling reports `200` with the completed representation.
#[tokio::test]
async fn poll_a_processing_media_reports_206_then_200_once_ready() {
    let app = spawn_test_app().await;
    let app_id = register_test_app(&app).await;
    let (token, _actor_id) = issue_write_media_token(&app, app_id).await;

    let upload_response =
        upload_bytes(&app, &token, "fixture.png", "image/png", &sample_png(10, 6)).await;
    assert_eq!(upload_response.status, 202, "got: {upload_response:?}");
    let media_id = media_id_of(&body_json(&upload_response));

    // Confirm the immediate post-upload poll really is 206 before entering
    // the bounded convergence loop (a media that were somehow already ready
    // this fast would defeat the point of this assertion).
    let immediate = get_media(app.address, &media_id, &token).await;
    assert_eq!(
        immediate.status, 206,
        "expected 206 immediately after upload, got: {immediate:?}"
    );
    assert_eq!(body_json(&immediate)["url"], Value::Null);

    let settled = poll_until_settled(app.address, &media_id, &token).await;
    assert_eq!(settled.status, 200, "got: {settled:?}");
    let json = body_json(&settled);
    assert!(json["url"].is_string(), "got: {json}");
    assert!(json["blurhash"].is_string(), "got: {json}");
    assert!(json["meta"]["original"]["width"].is_number(), "got: {json}");

    app.cleanup().await;
}

/// Requirement 6.5: a media whose processing genuinely fails (an
/// undecodable "image") is reported as `422` with a Mastodon-compatible
/// `{"error": ...}` body once the resident worker converges it to `failed`
/// -- never a `200`/`206`.
#[tokio::test]
async fn poll_a_media_that_fails_processing_reports_422_with_a_compatible_error_body() {
    let app = spawn_test_app().await;
    let app_id = register_test_app(&app).await;
    let (token, _actor_id) = issue_write_media_token(&app, app_id).await;

    // A declared `image/png` upload (passes `MediaService`'s format check)
    // whose bytes are not a decodable image at all -- `PureRustImageProcessor::
    // process_image` must fail, driving the worker's immediate terminal
    // failure path (see this file's module doc comment).
    let upload_response = upload_bytes(
        &app,
        &token,
        "not-really-a.png",
        "image/png",
        b"this is not a valid png payload at all",
    )
    .await;
    assert_eq!(upload_response.status, 202, "got: {upload_response:?}");
    let media_id = media_id_of(&body_json(&upload_response));

    let settled = poll_until_settled(app.address, &media_id, &token).await;
    assert_eq!(settled.status, 422, "got: {settled:?}");
    let json = body_json(&settled);
    assert!(
        json["error"].is_string(),
        "expected a compatible {{\"error\": ...}} body for a failed media, got {json:?}"
    );

    app.cleanup().await;
}

/// Requirement 2.3: polling a nonexistent media id is `404`.
#[tokio::test]
async fn poll_a_nonexistent_media_is_404() {
    let app = spawn_test_app().await;
    let app_id = register_test_app(&app).await;
    let (token, _actor_id) = issue_write_media_token(&app, app_id).await;

    let response = get_media(app.address, "999999999999", &token).await;
    assert_eq!(response.status, 404, "got: {response:?}");

    app.cleanup().await;
}

/// Requirement 2.4: polling another actor's media is `404`, never leaking
/// its existence or contents to a non-owning caller.
#[tokio::test]
async fn poll_another_actors_media_is_404_not_visible() {
    let app = spawn_test_app().await;
    let app_id = register_test_app(&app).await;
    let (owner_token, _owner_id) = issue_write_media_token(&app, app_id).await;

    let upload_response = upload_bytes(
        &app,
        &owner_token,
        "fixture.png",
        "image/png",
        &sample_png(4, 4),
    )
    .await;
    assert_eq!(upload_response.status, 202, "got: {upload_response:?}");
    let media_id = media_id_of(&body_json(&upload_response));

    let (other_token, _other_id) = issue_write_media_token(&app, app_id).await;
    let response = get_media(app.address, &media_id, &other_token).await;
    assert_eq!(response.status, 404, "got: {response:?}");

    app.cleanup().await;
}
