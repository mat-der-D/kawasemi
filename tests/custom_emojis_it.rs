//! Integration test proving task 7.2's own observable completion condition
//! (`.kiro/specs/accounts-and-instance/tasks.md`, "7.2 instance v2 /
//! custom_emojis とリモート取得の統合テストを通す"): "custom_emojis の
//! visible 一覧" (Requirement 9.1).
//!
//! design.md's File Structure Plan names this exact filename
//! (`custom_emojis_it.rs`: "custom_emojis read（visible・category）（統
//! 合）"). `tests/accounts_endpoints_wiring_it.rs`'s own
//! `instance_v2_and_custom_emojis_are_public` test already proves the bare
//! *wiring* (200, a JSON array) — this file goes further and proves the
//! actual `visible_in_picker` filtering behavior (Requirements 9.1, 9.2):
//! only rows with `visible_in_picker = TRUE` are ever returned, and every
//! returned entry carries the full CustomEmoji contract
//! (`shortcode`/`url`/`static_url`/`visible_in_picker`/`category`).
//!
//! ## No HTTP client dependency: raw sockets, mirroring established
//! precedent (`tests/auth_scope_it.rs`/`tests/accounts_endpoints_wiring_it.rs`)
//! This crate has no HTTP client dependency (`Cargo.toml`). This file
//! duplicates the `RawResponse`/`raw_get` plumbing (each `tests/*.rs` file is
//! a separate compiled binary with no shared module).
//!
//! ## Seeding `custom_emojis` directly via SQL, not through an API
//! This spec owns `custom_emojis` read-only (Requirement 9.3: "登録・アップ
//! ロード・連合取り込み・管理は本 spec で行わない") — there is no endpoint
//! this spec mounts to create a row. This file seeds rows directly through
//! `sqlx`, the same convention `instance_v2_it.rs` uses for its own
//! read-only `instance_settings` table.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use kawasemi::test_harness::{TestApp, spawn_test_app};

// ---- raw HTTP plumbing (duplicated across this spec's `tests/*.rs` files
// by established convention) ----

#[derive(Debug)]
struct RawResponse {
    status: u16,
    #[allow(dead_code)]
    headers: HashMap<String, String>,
    body: String,
}

async fn raw_get(addr: SocketAddr, path: &str) -> RawResponse {
    let mut stream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr))
        .await
        .expect("connecting to the test listener must not time out")
        .expect("connect");

    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write request");

    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut buf))
        .await
        .expect("read must not time out")
        .expect("read response");

    parse_response(&String::from_utf8_lossy(&buf))
}

fn parse_response(raw: &str) -> RawResponse {
    let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((raw, ""));
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
        body: body.to_string(),
    }
}

fn body_json(response: &RawResponse) -> Value {
    serde_json::from_str(&response.body)
        .unwrap_or_else(|e| panic!("response body must be valid JSON: {e}; body: {response:?}"))
}

async fn get_custom_emojis(app: &TestApp) -> Value {
    let response = raw_get(app.address, "/api/v1/custom_emojis").await;
    assert_eq!(response.status, 200, "got: {response:?}");
    body_json(&response)
}

async fn seed_emoji(
    app: &TestApp,
    shortcode: &str,
    domain: &str,
    url: &str,
    static_url: &str,
    visible_in_picker: bool,
    category: Option<&str>,
) {
    let now = app.runtime.clock.now();
    sqlx::query(
        r#"
        INSERT INTO custom_emojis (
            shortcode, domain, url, static_url, visible_in_picker, category, updated_at
        ) VALUES ($1, $2, $3, $4, $5, $6, $7)
        "#,
    )
    .bind(shortcode)
    .bind(domain)
    .bind(url)
    .bind(static_url)
    .bind(visible_in_picker)
    .bind(category)
    .bind(now)
    .execute(&app.pool)
    .await
    .expect("seeding a custom_emojis row directly must succeed");
}

// ==========================================================================
// (1) With no rows at all, custom_emojis responds with an empty array, not
// an error (mirrors task 2.4's/8.3's "always a value, never an error on an
// empty table" discipline for the sibling instance_settings repository).
// ==========================================================================

#[tokio::test]
async fn custom_emojis_returns_an_empty_array_when_no_emoji_rows_exist() {
    let app = spawn_test_app().await;

    let body = get_custom_emojis(&app).await;
    assert_eq!(body.as_array().map(Vec::len), Some(0), "got: {body}");

    app.cleanup().await;
}

// ==========================================================================
// (2) Only `visible_in_picker = TRUE` rows are ever listed (Requirements
// 9.1, 9.2), with the full CustomEmoji contract on every returned entry.
// ==========================================================================

#[tokio::test]
async fn custom_emojis_lists_only_visible_in_picker_rows_with_the_full_contract() {
    let app = spawn_test_app().await;

    seed_emoji(
        &app,
        "blobcat",
        "",
        "https://kawasemi.example/emoji/blobcat.png",
        "https://kawasemi.example/emoji/blobcat_static.png",
        true,
        Some("cats"),
    )
    .await;
    seed_emoji(
        &app,
        "hidden_emoji",
        "",
        "https://kawasemi.example/emoji/hidden_emoji.png",
        "https://kawasemi.example/emoji/hidden_emoji_static.png",
        false,
        None,
    )
    .await;
    seed_emoji(
        &app,
        "another_visible",
        "",
        "https://kawasemi.example/emoji/another_visible.png",
        "https://kawasemi.example/emoji/another_visible_static.png",
        true,
        None,
    )
    .await;

    let body = get_custom_emojis(&app).await;
    let array = body.as_array().expect("custom_emojis must be a JSON array");
    assert_eq!(
        array.len(),
        2,
        "only the two visible_in_picker=TRUE rows must be listed, got: {body}"
    );

    let shortcodes: Vec<&str> = array
        .iter()
        .map(|entry| {
            entry["shortcode"]
                .as_str()
                .expect("shortcode must be a string")
        })
        .collect();
    assert!(shortcodes.contains(&"blobcat"));
    assert!(shortcodes.contains(&"another_visible"));
    assert!(
        !shortcodes.contains(&"hidden_emoji"),
        "a visible_in_picker=FALSE row must never be listed, got: {body}"
    );

    let blobcat = array
        .iter()
        .find(|entry| entry["shortcode"] == "blobcat")
        .expect("blobcat must be present");
    assert_eq!(blobcat["url"], "https://kawasemi.example/emoji/blobcat.png");
    assert_eq!(
        blobcat["static_url"],
        "https://kawasemi.example/emoji/blobcat_static.png"
    );
    assert_eq!(blobcat["visible_in_picker"], true);
    assert_eq!(blobcat["category"], "cats");

    let another_visible = array
        .iter()
        .find(|entry| entry["shortcode"] == "another_visible")
        .expect("another_visible must be present");
    assert!(
        another_visible["category"].is_null(),
        "an unset category must serialize as null, got: {another_visible}"
    );

    app.cleanup().await;
}
