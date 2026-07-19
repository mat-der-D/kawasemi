//! Integration test proving task 5.2's observable completion condition
//! (`.kiro/specs/media-pipeline/tasks.md`, "5.2 メディアモジュールをランタ
//! イムへ配線する", `_Boundary: MediaModule wiring_`): "起動後にメディアエン
//! ドポイントが到達可能で、ワーカーが稼働し、アップロード→ポーリングで派生
//! 物が反映され、`X-RateLimit-*` が付与されることを統合テストで確認できる"
//! (Requirements 1.1, 4.1, 9.5).
//!
//! `tests/media_endpoints_it.rs` (task 5.1) already proves the three
//! handlers' own HTTP-level behavior (status codes, error bodies, auth/scope
//! enforcement) through a *test-local* router built directly from
//! `MediaEndpointsState`/`upload_media`/`show_media`/`update_media` — that
//! file's own doc comment says as much: nothing wires those handlers into
//! the live application router yet, that is this task's job. This file does
//! not re-prove that per-handler status/body matrix; it proves the
//! *composition-root wiring* itself: that `crate::server::build_router`
//! (the exact router `spawn_test_app`/`bootstrap()` both serve) actually
//! mounts the three media endpoints behind the same cross-cutting layers
//! every other endpoint sits behind, that the resident `ProcessingWorker`
//! pool `crate::bootstrap`/`crate::test_harness` starts is genuinely running
//! and consuming the real DB queue (not just constructed), and that an
//! upload's processing job is picked up and completed by that worker with no
//! test-local shortcut — mirroring `tests/actor_bootstrap_wiring_it.rs`'s
//! identical "prove the composition-root wiring itself, not just the
//! already-tested individual components" precedent for a same-shaped task
//! (`actor-model`'s task 6.1, `_Boundary: ... Bootstrap ..._`).
//!
//! ## No HTTP client dependency: raw sockets, mirroring established precedent
//! Duplicates `tests/error_ratelimit_it.rs`'s/`tests/federation_bootstrap_it.rs`'s
//! own `RawResponse`/`raw_request` helpers (each `tests/*.rs` file is a
//! separate compiled binary with no shared module) — a byte-oriented variant
//! capturing response headers (needed for the `X-RateLimit-*` assertions)
//! *and* accepting a raw `&[u8]` body (needed for the binary multipart
//! upload payload), rather than either single-purpose precedent alone.
//!
//! ## Real image fixture, not the fake bytes `media_endpoints_it.rs` uses
//! `tests/media_endpoints_it.rs`'s own fixtures deliberately upload
//! non-decodable placeholder bytes (its own file never needs the worker to
//! actually run — task 5.1 predates the worker being wired in at all). This
//! file's whole point is proving the *worker* converges `processing` to
//! `ready`, which requires bytes `PureRustImageProcessor::process_image` can
//! actually decode — [`sample_png`] mirrors `src/media/worker/tests.rs`'s/
//! `src/media/image_processor.rs`'s own established in-memory PNG fixture
//! convention (a small gradient, not a solid color, encoded via the `image`
//! crate directly).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use kawasemi::domain::Id;
use kawasemi::oauth::app_repository::{self, NewApp};
use kawasemi::oauth::model::ScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::test_harness::{TestApp, spawn_test_app};

// ==========================================================================
// Raw HTTP plumbing (byte-bodied, header-capturing variant — see this
// file's module doc comment for why neither existing precedent alone fits).
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
// Multipart request body construction (duplicated from
// `tests/media_endpoints_it.rs` — no shared module between `tests/*.rs`
// binaries; see that file's own doc comment for why this is hand-rolled).
// ==========================================================================

struct MultipartBuilder {
    boundary: &'static str,
    body: Vec<u8>,
}

impl MultipartBuilder {
    fn new() -> Self {
        MultipartBuilder {
            boundary: "kawasemi-media-bootstrap-wiring-it-boundary",
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
/// color) — mirrors `src/media/worker/tests.rs`'s/`src/media/image_processor.rs`'s
/// own established fixture convention. See this file's module doc comment
/// ("Real image fixture") for why `media_endpoints_it.rs`'s own fake-byte
/// fixtures would not exercise what this file needs to prove.
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
// Fixtures (register a real OAuth app + mint a real access token bound to a
// fresh actor, hashed with this instance's own real
// `AppState::oauth().token_hash_key()` — the exact key the real, mounted
// Bearer-auth middleware verifies against, not a test-local substitute key
// the way `media_endpoints_it.rs`'s own test-local router uses).
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
            name: "Media Bootstrap Wiring Test Client".to_string(),
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

async fn upload_sample_png(app: &TestApp, token: &str, width: u32, height: u32) -> RawResponse {
    let (body, content_type) = MultipartBuilder::new()
        .file_field(
            "file",
            "fixture.png",
            "image/png",
            &sample_png(width, height),
        )
        .finish();
    raw_request(
        app.address,
        "POST",
        "/api/v2/media",
        &[
            ("Content-Type".to_string(), content_type),
            auth_header(token),
        ],
        &body,
    )
    .await
}

/// Polls `GET /api/v1/media/:id` through the real router with a bounded
/// retry loop until it reports `200` (Requirement 4.2/4.3: the resident
/// `ProcessingWorker` pool must actually be running against the real DB
/// queue, not merely constructed) — mirroring how this crate's other
/// async-worker integration tests (e.g. `federation_pair_it.rs`'s delivery
/// convergence checks) bound-poll a background worker's convergence rather
/// than sleeping a fixed arbitrary duration. `DEFAULT_POLL_INTERVAL`
/// (`src/media/worker.rs`) is 500ms; 40 attempts at 250ms apart bounds this
/// to at most 10s, comfortably above what a single tiny fixture image should
/// ever take to process.
async fn poll_until_ready(addr: SocketAddr, media_id: &str, token: &str) -> RawResponse {
    const MAX_ATTEMPTS: u32 = 40;
    const RETRY_DELAY: Duration = Duration::from_millis(250);

    for attempt in 0..MAX_ATTEMPTS {
        let response = raw_request(
            addr,
            "GET",
            &format!("/api/v1/media/{media_id}"),
            &[auth_header(token)],
            b"",
        )
        .await;
        if response.status == 200 {
            return response;
        }
        assert_eq!(
            response.status, 206,
            "expected 206 while still processing (attempt {attempt}), got: {response:?}"
        );
        tokio::time::sleep(RETRY_DELAY).await;
    }

    panic!(
        "media did not become ready within {MAX_ATTEMPTS} polls ({RETRY_DELAY:?} apart) -- is \
         the resident ProcessingWorker pool actually running against the real mounted router \
         (task 5.2's own wiring)?"
    );
}

// ==========================================================================
// Tests
// ==========================================================================

/// Requirements 1.1, 1.2, 9.1, 9.5: `POST /api/v2/media` is reachable
/// through the real, fully-composed application router (not a test-local
/// one), returns the same `202`/null-`url` acceptance contract task 5.1
/// already proved in isolation, binds the uploading actor, and — the part
/// specific to this task — carries the same `X-RateLimit-*` headers every
/// other endpoint on this router does.
#[tokio::test]
async fn upload_through_the_real_router_returns_202_with_rate_limit_headers_and_binds_the_actor() {
    let app = spawn_test_app().await;
    let app_id = register_test_app(&app).await;
    let (token, actor_id) = issue_write_media_token(&app, app_id).await;

    let response = upload_sample_png(&app, &token, 8, 8).await;

    assert_eq!(response.status, 202, "got: {response:?}");
    assert!(
        response.headers.contains_key("x-ratelimit-limit"),
        "got: {response:?}"
    );
    assert!(
        response.headers.contains_key("x-ratelimit-remaining"),
        "got: {response:?}"
    );
    assert!(
        response.headers.contains_key("x-ratelimit-reset"),
        "got: {response:?}"
    );

    let json = body_json(&response);
    assert_eq!(
        json["url"],
        Value::Null,
        "processing media must have a null url, got: {json}"
    );

    let media_id = Id::from_i64(
        json["id"]
            .as_str()
            .expect("id must be a decimal string")
            .parse()
            .expect("id must parse as i64"),
    );
    let owned = kawasemi::media::find_owned(&app.pool, media_id, actor_id)
        .await
        .expect("find_owned must succeed")
        .expect("the uploaded media must be bound to the uploading actor");
    assert_eq!(owned.actor_id, actor_id);

    app.cleanup().await;
}

/// Requirements 1.1, 1.6, 2.1, 2.2, 4.1, 4.2, 4.3, 6.1, 9.5: the full
/// upload -> enqueue -> worker-claim -> process -> store-derivative ->
/// set_ready -> poll pipeline works end to end through the real mounted
/// router and the resident worker pool `crate::test_harness::spawn_test_app`
/// starts — never a test-local shortcut that calls `ProcessingWorker::run_once`
/// directly. This is task 5.2's own central acceptance criterion.
#[tokio::test]
async fn worker_converges_an_uploaded_image_from_processing_to_ready_through_the_real_mounted_router()
 {
    let app = spawn_test_app().await;
    let app_id = register_test_app(&app).await;
    let (token, _actor_id) = issue_write_media_token(&app, app_id).await;

    let upload_response = upload_sample_png(&app, &token, 16, 12).await;
    assert_eq!(upload_response.status, 202, "got: {upload_response:?}");
    let upload_json = body_json(&upload_response);
    let media_id = upload_json["id"]
        .as_str()
        .expect("id must be a decimal string")
        .to_string();

    let ready_response = poll_until_ready(app.address, &media_id, &token).await;
    assert!(
        ready_response.headers.contains_key("x-ratelimit-limit"),
        "the ready poll response must still carry rate-limit headers, got: {ready_response:?}"
    );

    let ready_json = body_json(&ready_response);
    assert!(
        ready_json["url"].is_string(),
        "ready media must have a resolved url, got: {ready_json}"
    );
    assert!(
        ready_json["preview_url"].is_string(),
        "ready media with a small derivative must have a resolved preview_url, got: {ready_json}"
    );
    assert_eq!(
        ready_json["meta"]["original"]["width"], 16,
        "got: {ready_json}"
    );
    assert_eq!(
        ready_json["meta"]["original"]["height"], 12,
        "got: {ready_json}"
    );
    assert!(ready_json["blurhash"].is_string(), "got: {ready_json}");

    app.cleanup().await;
}

/// Requirements 3.1, 3.2, 9.5: `PUT /api/v1/media/:id` is reachable through
/// the real router and reflects a description/focus update, mirroring task
/// 5.1's own already-proven behavior but through the real composition-root
/// wiring rather than a test-local router.
#[tokio::test]
async fn update_through_the_real_router_reflects_the_new_description_and_focus() {
    let app = spawn_test_app().await;
    let app_id = register_test_app(&app).await;
    let (token, _actor_id) = issue_write_media_token(&app, app_id).await;

    let upload_response = upload_sample_png(&app, &token, 4, 4).await;
    assert_eq!(upload_response.status, 202, "got: {upload_response:?}");
    let upload_json = body_json(&upload_response);
    let media_id = upload_json["id"]
        .as_str()
        .expect("id must be a decimal string")
        .to_string();

    let put_body = json!({
        "description": "a wiring test cat",
        "focus": "0.25,-0.25",
    })
    .to_string();
    let response = raw_request(
        app.address,
        "PUT",
        &format!("/api/v1/media/{media_id}"),
        &[
            ("Content-Type".to_string(), "application/json".to_string()),
            auth_header(&token),
        ],
        put_body.as_bytes(),
    )
    .await;

    assert_eq!(response.status, 200, "got: {response:?}");
    let json = body_json(&response);
    assert_eq!(json["description"], "a wiring test cat", "got: {json}");
    assert_eq!(json["meta"]["focus"]["x"], 0.25, "got: {json}");
    assert_eq!(json["meta"]["focus"]["y"], -0.25, "got: {json}");

    app.cleanup().await;
}

/// Requirement 1.1 (task 5.1's own documented CONCERN for this task,
/// `src/media/endpoints.rs`'s "CONCERN for task 5.2"): axum's `Multipart`
/// extractor applies its own hard-coded 2MB body-limit default unless a
/// `DefaultBodyLimit` layer overrides it on the router the handler is
/// mounted on. A 3MB upload sits strictly between that unconfigured default
/// and `spawn_test_app`'s configured `max_upload_size_bytes` (10 MiB,
/// `src/test_harness.rs`) — exactly the gap that would silently reject a
/// legitimate upload if task 5.2 mounted `upload_media` without sizing a
/// `DefaultBodyLimit` layer from the real configured value. Content need not
/// be a genuine decodable image: `MediaService`'s own format validation only
/// checks the declared content-type string, so this test isolates the
/// body-limit-layer regression specifically, without conflating it with
/// decode/processing behavior already covered by the other tests in this
/// file.
#[tokio::test]
async fn upload_between_axums_builtin_2mb_default_and_the_configured_max_is_not_rejected_by_the_body_limit()
 {
    let app = spawn_test_app().await;
    let app_id = register_test_app(&app).await;
    let (token, _actor_id) = issue_write_media_token(&app, app_id).await;

    let oversized_but_within_configured_limit = vec![0u8; 3 * 1024 * 1024];
    let (body, content_type) = MultipartBuilder::new()
        .file_field(
            "file",
            "big.png",
            "image/png",
            &oversized_but_within_configured_limit,
        )
        .finish();

    let response = raw_request(
        app.address,
        "POST",
        "/api/v2/media",
        &[
            ("Content-Type".to_string(), content_type),
            auth_header(&token),
        ],
        &body,
    )
    .await;

    assert_eq!(
        response.status,
        202,
        "a {}-byte upload (over axum's built-in 2MB default, under the configured \
         max_upload_size_bytes) must be accepted, not rejected by an unconfigured \
         DefaultBodyLimit -- got: {response:?}",
        oversized_but_within_configured_limit.len()
    );

    app.cleanup().await;
}
