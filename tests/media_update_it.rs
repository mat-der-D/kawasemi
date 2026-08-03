//! Integration test proving task 6.1's own metadata-update observable
//! completion condition (`.kiro/specs/media-pipeline/tasks.md`, "6.1 アップ
//! ロード・ポーリング・更新の統合テストを実装する", `_Depends: 5.2_`): "説明/
//! フォーカル更新の反映と範囲外 422・処理中更新・非所有 404 を検証する"
//! (Requirements 3.1, 3.2, 3.3, 3.4, 9.1, 9.2, 9.3, 9.4).
//!
//! Mirrors `tests/media_upload_it.rs`'s/`tests/media_poll_it.rs`'s own module
//! doc comments for why this is a separate `_it.rs` file (design.md's File
//! Structure Plan names it `media_update_it.rs`), why it duplicates a small
//! amount of raw-HTTP/multipart plumbing rather than sharing a module with
//! sibling `tests/*.rs` files, and why it drives the *real*, fully-composed
//! application router `spawn_test_app` serves rather than a test-local one.
//!
//! ## "処理中でも受付" (Requirement 3.4) without racing the worker
//! Every update test in this file issues its `PUT` immediately after the
//! `202` upload response, before ever polling `GET`/waiting on the resident
//! worker -- mirroring `tests/media_bootstrap_wiring_it.rs`'s own
//! `update_through_the_real_router_reflects_the_new_description_and_focus`
//! precedent. `MediaService::update_metadata` (task 4.1, `service.rs`)
//! never filters on `Media::state` at all, so this exercises the same code
//! path regardless of whether the worker has already converged the media to
//! `ready` by the time the request arrives -- the acceptance criterion this
//! task cares about ("処理中でも受付") is that update is never conditioned on
//! state, not that this test wins a race against the worker.

use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use serde_json::{Value, json};
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
// Multipart request body construction (duplicated, upload-fixture only).
// ==========================================================================

struct MultipartBuilder {
    boundary: &'static str,
    body: Vec<u8>,
}

impl MultipartBuilder {
    fn new() -> Self {
        MultipartBuilder {
            boundary: "kawasemi-media-update-it-boundary",
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

/// A small but genuine, decodable in-memory PNG -- mirrors
/// `tests/media_bootstrap_wiring_it.rs`'s own established fixture
/// convention.
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
            name: "Media Update IT Client".to_string(),
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

async fn upload_fixture(app: &TestApp, token: &str) -> String {
    let (body, content_type) = MultipartBuilder::new()
        .file_field("file", "fixture.png", "image/png", &sample_png(6, 6))
        .finish();
    let response = raw_request(
        app.address,
        "POST",
        "/api/v2/media",
        &[
            ("Content-Type".to_string(), content_type),
            auth_header(token),
        ],
        &body,
    )
    .await;
    assert_eq!(
        response.status, 202,
        "upload fixture must succeed, got: {response:?}"
    );
    body_json(&response)["id"]
        .as_str()
        .expect("id must be a decimal string")
        .to_string()
}

async fn put_update(app: &TestApp, token: &str, media_id: &str, body: &Value) -> RawResponse {
    raw_request(
        app.address,
        "PUT",
        &format!("/api/v1/media/{media_id}"),
        &[
            ("Content-Type".to_string(), "application/json".to_string()),
            auth_header(token),
        ],
        body.to_string().as_bytes(),
    )
    .await
}

// ==========================================================================
// Tests
// ==========================================================================

/// Requirements 3.1, 3.4: description/focus updates are accepted (`200`)
/// and reflected in the response even while the media has not necessarily
/// finished processing (never conditioned on state).
#[tokio::test]
async fn update_reflects_the_new_description_and_focus() {
    let app = spawn_test_app().await;
    let app_id = register_test_app(&app).await;
    let (token, _actor_id) = issue_write_media_token(&app, app_id).await;
    let media_id = upload_fixture(&app, &token).await;

    let response = put_update(
        &app,
        &token,
        &media_id,
        &json!({"description": "a good gradient", "focus": "-0.5,0.5"}),
    )
    .await;

    assert_eq!(response.status, 200, "got: {response:?}");
    let json = body_json(&response);
    assert_eq!(json["description"], "a good gradient", "got: {json}");
    assert_eq!(json["meta"]["focus"]["x"], -0.5, "got: {json}");
    assert_eq!(json["meta"]["focus"]["y"], 0.5, "got: {json}");

    app.cleanup().await;
}

/// Requirement 3.4 (explicit): the update above already exercises this
/// implicitly (it never waits for the worker), but this test additionally
/// confirms the media is still reported as `206`/processing immediately
/// before issuing the very same update, tying the "still processing" precondition
/// directly to the request that must still succeed against it.
#[tokio::test]
async fn update_while_still_processing_succeeds() {
    let app = spawn_test_app().await;
    let app_id = register_test_app(&app).await;
    let (token, _actor_id) = issue_write_media_token(&app, app_id).await;
    let media_id = upload_fixture(&app, &token).await;

    let precondition = raw_request(
        app.address,
        "GET",
        &format!("/api/v1/media/{media_id}"),
        &[auth_header(&token)],
        b"",
    )
    .await;
    assert_eq!(
        precondition.status, 206,
        "expected the fixture to still be processing at this point, got: {precondition:?}"
    );

    let response = put_update(
        &app,
        &token,
        &media_id,
        &json!({"description": "updated while processing"}),
    )
    .await;
    assert_eq!(response.status, 200, "got: {response:?}");
    let json = body_json(&response);
    assert_eq!(
        json["description"], "updated while processing",
        "got: {json}"
    );

    app.cleanup().await;
}

/// Requirement 3.2: an out-of-range focus coordinate is rejected with `422`
/// and a Mastodon-compatible error body; the update is not applied.
#[tokio::test]
async fn update_with_an_out_of_range_focus_is_422_with_a_compatible_error_body() {
    let app = spawn_test_app().await;
    let app_id = register_test_app(&app).await;
    let (token, _actor_id) = issue_write_media_token(&app, app_id).await;
    let media_id = upload_fixture(&app, &token).await;

    let response = put_update(&app, &token, &media_id, &json!({"focus": "5.0,-5.0"})).await;

    assert_eq!(response.status, 422, "got: {response:?}");
    let json = body_json(&response);
    assert!(
        json["error"].is_string(),
        "expected a compatible {{\"error\": ...}} body, got {json:?}"
    );

    app.cleanup().await;
}

/// Requirement 3.3: updating a nonexistent media is `404`.
#[tokio::test]
async fn update_a_nonexistent_media_is_404() {
    let app = spawn_test_app().await;
    let app_id = register_test_app(&app).await;
    let (token, _actor_id) = issue_write_media_token(&app, app_id).await;

    let response = put_update(
        &app,
        &token,
        "999999999999",
        &json!({"description": "ghost"}),
    )
    .await;
    assert_eq!(response.status, 404, "got: {response:?}");

    app.cleanup().await;
}

/// Requirement 3.3: updating another actor's media is `404`, never applying
/// the change or leaking the media's existence to a non-owning caller.
#[tokio::test]
async fn update_another_actors_media_is_404_not_leaked() {
    let app = spawn_test_app().await;
    let app_id = register_test_app(&app).await;
    let (owner_token, _owner_id) = issue_write_media_token(&app, app_id).await;
    let media_id = upload_fixture(&app, &owner_token).await;

    let (other_token, _other_id) = issue_write_media_token(&app, app_id).await;
    let response = put_update(
        &app,
        &other_token,
        &media_id,
        &json!({"description": "hijacked"}),
    )
    .await;
    assert_eq!(response.status, 404, "got: {response:?}");

    app.cleanup().await;
}
