//! Integration test proving task 7.1's own observable completion condition
//! for `update_credentials` (`.kiro/specs/accounts-and-instance/tasks.md`,
//! "7.1 アカウント系エンドポイントの統合テストを通す": "401/403...422...更新
//! 反映") against the real, `spawn_test_app`-booted application router
//! (Requirements 6.1, 6.3, 6.4).
//!
//! design.md's File Structure Plan names this exact filename
//! (`update_credentials_it.rs`: "プロフィール更新（項目別・アバター/ヘッダ
//! 取込・422・401/403・反映）（統合）") and its own Testing Strategy bullet
//! ("update_credentials: 項目別部分更新が verify_credentials/accounts/:id
//! に反映、avatar/header 取込、検証違反 422、スコープ不足 403（6.1, 6.2,
//! 6.3, 6.4, 6.5）").
//!
//! `tests/accounts_endpoints_wiring_it.rs` (task 6) already proved 401/403
//! wiring and a single `display_name`-reflected-via-`verify_credentials`
//! smoke case. Per task 5.4's own Implementation Note (`tasks.md`'s
//! Implementation Notes for task 5.4: "初回レビューで REJECTED（...
//! `accounts/:id`（`show_account`）経由の反映が未検証だった）"), this file's
//! own round-trip test explicitly also re-checks the update through
//! `accounts/:id`, not only `verify_credentials` — the exact gap that
//! rejection flagged. This file additionally covers:
//! 1. Every distinct 422 validation path this module's own fail-fast gate
//!    checks (`fields_attributes` over the count limit, an over-length
//!    `display_name`, an invalid `source[privacy]` literal, an empty
//!    `fields_attributes[N][name]`) — none of which perform any side
//!    effect (Requirement 6.3's "更新を行わず").
//! 2. A real avatar upload (Requirement 6.2), ingested via media-pipeline
//!    and reflected as a real, non-default `avatar`/`avatar_static` URL in
//!    both `verify_credentials` and `accounts/:id`.
//! 3. `source[language]`'s documented three-way wire mapping: absent ->
//!    unchanged, present-but-empty -> explicit clear.
//!
//! ## No HTTP client dependency: raw sockets (byte-bodied variant, mirroring
//! `tests/media_upload_it.rs`'s own established precedent, needed here
//! because a real PNG payload is not valid UTF-8).

use std::net::SocketAddr;
use std::time::Duration;

use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor};
use kawasemi::domain::Id;
use kawasemi::oauth::app_repository::{self, NewApp};
use kawasemi::oauth::model::ScopeSet as PlaceholderScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::test_harness::{TestApp, spawn_test_app};

// ---- raw HTTP plumbing (byte-bodied; mirrors `tests/media_upload_it.rs`) ----

struct RawResponse {
    status: u16,
    body: Vec<u8>,
}

impl std::fmt::Debug for RawResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RawResponse")
            .field("status", &self.status)
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

async fn raw_get(addr: SocketAddr, path: &str, headers: &[(String, String)]) -> RawResponse {
    raw_request(addr, "GET", path, headers, &[]).await
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
    RawResponse { status, body }
}

fn body_json(response: &RawResponse) -> Value {
    serde_json::from_slice(&response.body)
        .unwrap_or_else(|e| panic!("response body must be valid JSON: {e}; body: {response:?}"))
}

fn assert_error_shape(response: &RawResponse) {
    let body = body_json(response);
    assert!(
        body.get("error").and_then(Value::as_str).is_some(),
        "expected a Mastodon-compatible {{\"error\": ...}} body, got: {body}"
    );
}

// ---- multipart request body construction (duplicated per this crate's
// established `tests/*.rs` convention; see `tests/media_upload_it.rs`) ----

struct MultipartBuilder {
    boundary: &'static str,
    body: Vec<u8>,
}

impl MultipartBuilder {
    fn new() -> Self {
        MultipartBuilder {
            boundary: "kawasemi-update-credentials-it-boundary",
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

async fn patch(app: &TestApp, token: &str, body: Vec<u8>, content_type: String) -> RawResponse {
    let headers = vec![
        ("Authorization".to_string(), format!("Bearer {token}")),
        ("Content-Type".to_string(), content_type),
    ];
    raw_request(
        app.address,
        "PATCH",
        "/api/v1/accounts/update_credentials",
        &headers,
        &body,
    )
    .await
}

/// A small but genuine, decodable in-memory PNG — mirrors
/// `tests/media_upload_it.rs`'s own established fixture convention.
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

// ---- fixtures ----

async fn register_test_app(app: &TestApp) -> Id {
    let now = app.runtime.clock.now();
    let registered = app_repository::register_app(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        app.state.oauth().token_hash_key(),
        now,
        NewApp {
            name: "Update Credentials IT Client".to_string(),
            redirect_uris: vec!["https://client.example/callback".to_string()],
            scopes: PlaceholderScopeSet::new(["read", "write"]),
        },
    )
    .await
    .expect("register_app must succeed");
    registered.id
}

async fn create_owner_with_actor(app: &TestApp, handle: &str) -> Id {
    let now = app.runtime.clock.now();
    let owner_id = app.runtime.ids.next_id();
    create_owner(&app.pool, owner_id, now)
        .await
        .expect("creating the owner fixture must succeed");

    let actor = app
        .actor
        .actor_service()
        .create_actor(NewActor {
            owner_id,
            handle: Handle::new(handle).expect("valid handle"),
            actor_type: ActorType::Person,
            display_name: "Update Credentials IT Actor".to_string(),
            summary: "an update_credentials_it integration test fixture".to_string(),
        })
        .await
        .expect("creating the owner's actor fixture must succeed");

    actor.id
}

async fn issue_token(app: &TestApp, oauth_app_id: Id, actor_id: Id, scopes: &[&str]) -> String {
    let now = app.runtime.clock.now();
    let issued = token_repository::issue_token(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        app.state.oauth().token_hash_key(),
        now,
        NewAccessToken {
            app_id: oauth_app_id,
            actor_id,
            scopes: PlaceholderScopeSet::new(scopes.iter().copied()),
        },
    )
    .await
    .expect("issuing a real access token must succeed");
    issued.plaintext.expose_secret().clone()
}

fn auth_header(token: &str) -> (String, String) {
    ("Authorization".to_string(), format!("Bearer {token}"))
}

// ---- (1) scope enforcement: 401/403 (Requirement 6.4) ----

#[tokio::test]
async fn update_credentials_requires_write_accounts_scope() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let actor_id = create_owner_with_actor(&app, "updatescopeowner").await;
    let (body, content_type) = MultipartBuilder::new()
        .text_field("display_name", "Someone")
        .finish();

    let unauthenticated = raw_request(
        app.address,
        "PATCH",
        "/api/v1/accounts/update_credentials",
        &[("Content-Type".to_string(), content_type.clone())],
        &body,
    )
    .await;
    assert_eq!(unauthenticated.status, 401, "got: {unauthenticated:?}");
    assert_error_shape(&unauthenticated);

    let wrong_scope_token = issue_token(&app, oauth_app_id, actor_id, &["read:accounts"]).await;
    let forbidden = raw_request(
        app.address,
        "PATCH",
        "/api/v1/accounts/update_credentials",
        &[
            auth_header(&wrong_scope_token),
            ("Content-Type".to_string(), content_type),
        ],
        &body,
    )
    .await;
    assert_eq!(forbidden.status, 403, "got: {forbidden:?}");
    assert_error_shape(&forbidden);

    app.cleanup().await;
}

// ---- (2) 422 validation paths, none of which perform a side effect
// (Requirement 6.3) ----

#[tokio::test]
async fn update_credentials_rejects_every_distinct_validation_violation_with_422() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let actor_id = create_owner_with_actor(&app, "update422owner").await;
    let token = issue_token(
        &app,
        oauth_app_id,
        actor_id,
        &["write:accounts", "read:accounts"],
    )
    .await;

    // (a) display_name over the 30-character limit.
    let (body, ct) = MultipartBuilder::new()
        .text_field("display_name", &"x".repeat(31))
        .finish();
    let response = patch(&app, &token, body, ct).await;
    assert_eq!(response.status, 422, "got: {response:?}");
    assert_error_shape(&response);

    // (b) note over the 500-character limit.
    let (body, ct) = MultipartBuilder::new()
        .text_field("note", &"x".repeat(501))
        .finish();
    let response = patch(&app, &token, body, ct).await;
    assert_eq!(response.status, 422, "got: {response:?}");
    assert_error_shape(&response);

    // (c) more than MAX_PROFILE_FIELDS (4) fields_attributes entries.
    let mut builder = MultipartBuilder::new();
    for i in 0..5 {
        builder = builder
            .text_field(&format!("fields_attributes[{i}][name]"), "Label")
            .text_field(&format!("fields_attributes[{i}][value]"), "Value");
    }
    let (body, ct) = builder.finish();
    let response = patch(&app, &token, body, ct).await;
    assert_eq!(response.status, 422, "got: {response:?}");
    assert_error_shape(&response);

    // (d) an empty fields_attributes[N][name].
    let (body, ct) = MultipartBuilder::new()
        .text_field("fields_attributes[0][name]", "")
        .text_field("fields_attributes[0][value]", "Value")
        .finish();
    let response = patch(&app, &token, body, ct).await;
    assert_eq!(response.status, 422, "got: {response:?}");
    assert_error_shape(&response);

    // (e) an invalid source[privacy] literal.
    let (body, ct) = MultipartBuilder::new()
        .text_field("source[privacy]", "not-a-real-visibility")
        .finish();
    let response = patch(&app, &token, body, ct).await;
    assert_eq!(response.status, 422, "got: {response:?}");
    assert_error_shape(&response);

    // (f) an unparseable loosely-typed boolean.
    let (body, ct) = MultipartBuilder::new()
        .text_field("locked", "not-a-boolean")
        .finish();
    let response = patch(&app, &token, body, ct).await;
    assert_eq!(response.status, 422, "got: {response:?}");
    assert_error_shape(&response);

    // None of the above must have taken effect: verify_credentials still
    // shows the untouched, freshly-created-actor defaults.
    let verify = raw_get(
        app.address,
        "/api/v1/accounts/verify_credentials",
        &[auth_header(&token)],
    )
    .await;
    assert_eq!(verify.status, 200, "got: {verify:?}");
    let verify_body = body_json(&verify);
    assert_eq!(
        verify_body["display_name"].as_str(),
        Some(""),
        "no 422'd update must have taken effect, got: {verify_body}"
    );
    assert_eq!(verify_body["locked"].as_bool(), Some(false));
    assert_eq!(verify_body["source"]["privacy"].as_str(), Some("public"));
    assert_eq!(verify_body["fields"].as_array().map(Vec::len), Some(0));

    app.cleanup().await;
}

// ---- (3) partial update round-trip: reflected in BOTH verify_credentials
// AND accounts/:id (Requirement 6.1, 6.5 -- the exact gap task 5.4's first
// review rejected, see this file's module doc comment) ----

#[tokio::test]
async fn update_credentials_partial_update_is_reflected_by_verify_credentials_and_show_account() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let actor_id = create_owner_with_actor(&app, "updatereflectowner").await;
    let token = issue_token(
        &app,
        oauth_app_id,
        actor_id,
        &["write:accounts", "read:accounts"],
    )
    .await;

    let (body, ct) = MultipartBuilder::new()
        .text_field("display_name", "Reflected Name")
        .text_field("note", "A reflected bio.")
        .text_field("locked", "true")
        .text_field("bot", "false")
        .text_field("discoverable", "true")
        .text_field("fields_attributes[0][name]", "Pronouns")
        .text_field("fields_attributes[0][value]", "she/her")
        .text_field("source[privacy]", "unlisted")
        .text_field("source[sensitive]", "true")
        .text_field("source[language]", "en")
        .finish();
    let updated = patch(&app, &token, body, ct).await;
    assert_eq!(updated.status, 200, "got: {updated:?}");
    let updated_body = body_json(&updated);
    assert_eq!(
        updated_body["display_name"].as_str(),
        Some("Reflected Name")
    );
    assert_eq!(updated_body["note"].as_str(), Some("A reflected bio."));
    assert_eq!(updated_body["locked"].as_bool(), Some(true));
    assert_eq!(updated_body["discoverable"].as_bool(), Some(true));
    assert_eq!(updated_body["source"]["privacy"].as_str(), Some("unlisted"));
    assert_eq!(updated_body["source"]["sensitive"].as_bool(), Some(true));
    assert_eq!(updated_body["source"]["language"].as_str(), Some("en"));

    // (i) reflected in verify_credentials.
    let verify = raw_get(
        app.address,
        "/api/v1/accounts/verify_credentials",
        &[auth_header(&token)],
    )
    .await;
    assert_eq!(verify.status, 200, "got: {verify:?}");
    let verify_body = body_json(&verify);
    assert_eq!(verify_body["display_name"].as_str(), Some("Reflected Name"));
    assert_eq!(verify_body["locked"].as_bool(), Some(true));
    assert_eq!(verify_body["fields"][0]["name"].as_str(), Some("Pronouns"));
    assert_eq!(verify_body["fields"][0]["value"].as_str(), Some("she/her"));
    assert_eq!(verify_body["source"]["language"].as_str(), Some("en"));

    // (ii) reflected in accounts/:id (task 5.4's own rejected-then-fixed
    // gap -- this is the assertion that closes it).
    let show = raw_get(
        app.address,
        &format!("/api/v1/accounts/{}", actor_id.as_i64()),
        &[],
    )
    .await;
    assert_eq!(show.status, 200, "got: {show:?}");
    let show_body = body_json(&show);
    assert_eq!(
        show_body["display_name"].as_str(),
        Some("Reflected Name"),
        "update_credentials's partial update must be reflected by a subsequent \
         accounts/:id call too (Requirement 6.5), got: {show_body}"
    );
    assert_eq!(show_body["note"].as_str(), Some("A reflected bio."));
    assert_eq!(show_body["locked"].as_bool(), Some(true));
    assert_eq!(show_body["discoverable"].as_bool(), Some(true));
    assert_eq!(show_body["fields"][0]["name"].as_str(), Some("Pronouns"));
    assert_eq!(show_body["fields"][0]["value"].as_str(), Some("she/her"));

    // A subsequent update leaving `note` untouched must not clobber it
    // (partial update discipline, Requirement 6.1).
    let (body2, ct2) = MultipartBuilder::new()
        .text_field("display_name", "Second Name")
        .finish();
    let updated2 = patch(&app, &token, body2, ct2).await;
    assert_eq!(updated2.status, 200, "got: {updated2:?}");
    let updated2_body = body_json(&updated2);
    assert_eq!(updated2_body["display_name"].as_str(), Some("Second Name"));
    assert_eq!(
        updated2_body["note"].as_str(),
        Some("A reflected bio."),
        "a field absent from the request must remain unchanged, got: {updated2_body}"
    );

    app.cleanup().await;
}

// ---- (4) source[language]'s three-way wire mapping: present-but-empty
// explicitly clears a previously-set language ----

#[tokio::test]
async fn update_credentials_source_language_present_but_empty_clears_it() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let actor_id = create_owner_with_actor(&app, "updatelangclearowner").await;
    let token = issue_token(
        &app,
        oauth_app_id,
        actor_id,
        &["write:accounts", "read:accounts"],
    )
    .await;

    let (body, ct) = MultipartBuilder::new()
        .text_field("source[language]", "fr")
        .finish();
    let set = patch(&app, &token, body, ct).await;
    assert_eq!(set.status, 200, "got: {set:?}");
    assert_eq!(body_json(&set)["source"]["language"].as_str(), Some("fr"));

    let (body, ct) = MultipartBuilder::new()
        .text_field("source[language]", "")
        .finish();
    let cleared = patch(&app, &token, body, ct).await;
    assert_eq!(cleared.status, 200, "got: {cleared:?}");
    assert!(
        body_json(&cleared)["source"]["language"].is_null(),
        "a present-but-empty source[language] must explicitly clear it, got: {:?}",
        body_json(&cleared)
    );

    app.cleanup().await;
}

// ---- (5) a real avatar upload (Requirement 6.2), reflected as a
// non-default URL in both verify_credentials and accounts/:id ----

#[tokio::test]
async fn update_credentials_ingests_an_avatar_upload_and_reflects_it_everywhere() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let actor_id = create_owner_with_actor(&app, "updateavatarowner").await;
    let token = issue_token(
        &app,
        oauth_app_id,
        actor_id,
        &["write:accounts", "read:accounts"],
    )
    .await;

    let default_avatar = {
        let verify = raw_get(
            app.address,
            "/api/v1/accounts/verify_credentials",
            &[auth_header(&token)],
        )
        .await;
        body_json(&verify)["avatar"]
            .as_str()
            .expect("avatar must never be null")
            .to_string()
    };

    let png = sample_png(4, 4);
    let (body, ct) = MultipartBuilder::new()
        .file_field("avatar", "avatar.png", "image/png", &png)
        .finish();
    let updated = patch(&app, &token, body, ct).await;
    assert_eq!(updated.status, 200, "got: {updated:?}");
    let updated_body = body_json(&updated);
    let new_avatar = updated_body["avatar"]
        .as_str()
        .expect("avatar must never be null");
    assert_ne!(
        new_avatar, default_avatar,
        "a real avatar upload must replace the default missing-image URL, got: {updated_body}"
    );
    assert!(
        new_avatar.contains("/media/"),
        "the new avatar URL must be a real media-pipeline URL, got: {new_avatar}"
    );
    assert_eq!(updated_body["avatar_static"].as_str(), Some(new_avatar));

    // Reflected in verify_credentials.
    let verify = raw_get(
        app.address,
        "/api/v1/accounts/verify_credentials",
        &[auth_header(&token)],
    )
    .await;
    assert_eq!(body_json(&verify)["avatar"].as_str(), Some(new_avatar));

    // Reflected in accounts/:id too.
    let show = raw_get(
        app.address,
        &format!("/api/v1/accounts/{}", actor_id.as_i64()),
        &[],
    )
    .await;
    assert_eq!(body_json(&show)["avatar"].as_str(), Some(new_avatar));

    app.cleanup().await;
}
