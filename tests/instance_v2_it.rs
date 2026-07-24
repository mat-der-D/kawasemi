//! Integration test proving task 7.2's own observable completion condition
//! (`.kiro/specs/accounts-and-instance/tasks.md`, "7.2 instance v2 /
//! custom_emojis とリモート取得の統合テストを通す"): "instance v2 の運用設定
//! 反映と既定" (Requirements 8.1, 8.2).
//!
//! design.md's File Structure Plan names this exact filename
//! (`instance_v2_it.rs`: "instance v2（運用設定反映・既定・configuration 整
//! 合）（統合）"). `tests/accounts_endpoints_wiring_it.rs`'s own
//! `instance_v2_and_custom_emojis_are_public` test already proves the bare
//! *wiring* (200, `domain`/`configuration` keys present) — this file goes
//! further and proves the actual behavioral contract: safe defaults when
//! `instance_settings` has no row at all (task 2.4's "常に全項目埋まった値を
//! 返す" guarantee, Requirement 8.3), and every operational field actually
//! reflecting a real `instance_settings` row once one exists (Requirement
//! 8.2), plus `configuration` staying aligned with this server's real
//! `MediaConfig` (Requirement 8.4).
//!
//! ## No HTTP client dependency: raw sockets, mirroring established
//! precedent (`tests/auth_scope_it.rs`/`tests/accounts_endpoints_wiring_it.rs`)
//! This crate has no HTTP client dependency (`Cargo.toml`). This file
//! duplicates the `RawResponse`/`raw_get` plumbing (each `tests/*.rs` file is
//! a separate compiled binary with no shared module).
//!
//! ## Seeding `instance_settings` directly via SQL, not through an API
//! This spec owns `instance_settings` read-only (design.md: "運用設定の書き
//! 込み/管理画面...本 spec は読み取りと初期既定のみ"; admin-frontend owns
//! writes) — there is no endpoint this spec mounts to create/update a row.
//! This file therefore seeds the single `id = 1` row directly through
//! `sqlx`, mirroring how other integration tests in this crate seed rows
//! belonging to a read-only repository (e.g.
//! `src/accounts/remote_fetcher/tests.rs` seeding `remote_accounts` directly
//! via `upsert_remote` rather than through an HTTP call).

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

async fn get_instance_v2(app: &TestApp) -> Value {
    let response = raw_get(app.address, "/api/v2/instance").await;
    assert_eq!(response.status, 200, "got: {response:?}");
    body_json(&response)
}

// ==========================================================================
// (1) Safe defaults when `instance_settings` has no row (Requirement 8.3)
// ==========================================================================

#[tokio::test]
async fn instance_v2_returns_safe_defaults_when_instance_settings_has_no_row() {
    let app = spawn_test_app().await;

    // Sanity check: a freshly migrated schema has zero rows in
    // `instance_settings` (task 1.1's own guarantee; this spec never
    // INSERTs/UPSERTs into it, per design.md's Data Contracts).
    let row_count: (i64,) = sqlx::query_as("SELECT count(*) FROM instance_settings")
        .fetch_one(&app.pool)
        .await
        .expect("counting instance_settings rows must succeed");
    assert_eq!(
        row_count.0, 0,
        "a fresh schema must start with no instance_settings row"
    );

    let body = get_instance_v2(&app).await;

    assert_eq!(body["domain"], app.state.config().server.domain);
    assert_eq!(body["title"], "");
    assert_eq!(body["description"], "");
    assert!(body["thumbnail"].is_null());
    assert_eq!(body["languages"].as_array().map(Vec::len), Some(0));

    assert_eq!(body["registrations"]["enabled"], false);
    assert_eq!(body["registrations"]["approval_required"], false);
    assert!(body["registrations"]["message"].is_null());

    assert_eq!(body["contact"]["email"], "");
    assert!(body["contact"]["account_id"].is_null());

    assert_eq!(body["rules"].as_array().map(Vec::len), Some(0));

    // Requirement 8.1: `version`/`source_url`/`usage` are always present
    // build-time constants / a fixed MVP placeholder, decisive regardless of
    // `instance_settings`.
    assert_eq!(body["version"].as_str(), Some(env!("CARGO_PKG_VERSION")));
    assert!(body["source_url"].as_str().is_some());
    assert_eq!(body["usage"]["users"]["active_month"].as_i64(), Some(1));

    app.cleanup().await;
}

// ==========================================================================
// (2) `configuration` reflects this server's real MediaConfig (Requirement
// 8.4) -- checked alongside the defaults above since it does not depend on
// `instance_settings` at all.
// ==========================================================================

#[tokio::test]
async fn instance_v2_configuration_matches_the_real_media_config() {
    let app = spawn_test_app().await;
    let body = get_instance_v2(&app).await;

    let media_attachments = &body["configuration"]["media_attachments"];
    let supported: Vec<String> = media_attachments["supported_mime_types"]
        .as_array()
        .expect("supported_mime_types must be an array")
        .iter()
        .map(|v| {
            v.as_str()
                .expect("each mime type must be a string")
                .to_string()
        })
        .collect();
    assert_eq!(supported, app.state.config().media.supported_formats);
    assert_eq!(
        media_attachments["image_size_limit"].as_u64(),
        Some(app.state.config().media.max_upload_size_bytes)
    );

    app.cleanup().await;
}

// ==========================================================================
// (3) Operational settings from a real `instance_settings` row are reflected
// (Requirement 8.2)
// ==========================================================================

#[tokio::test]
async fn instance_v2_reflects_operational_settings_from_the_database() {
    let app = spawn_test_app().await;
    let now = app.runtime.clock.now();

    sqlx::query(
        r#"
        INSERT INTO instance_settings (
            id, title, description, contact_email, contact_account_id,
            rules, registrations_enabled, registrations_approval_required,
            registrations_message, thumbnail, languages, updated_at
        ) VALUES (
            1, $1, $2, $3, NULL,
            $4::jsonb, TRUE, TRUE,
            $5, $6, $7::jsonb, $8
        )
        "#,
    )
    .bind("Kawasemi Test Instance")
    .bind("A single-user Mastodon-compatible server, under test.")
    .bind("admin@kawasemi.example")
    .bind(serde_json::json!([
        "Be excellent to each other.",
        "No spam."
    ]))
    .bind("Approval is required before your account is activated.")
    .bind("https://kawasemi.example/thumbnail.png")
    .bind(serde_json::json!(["en", "ja"]))
    .bind(now)
    .execute(&app.pool)
    .await
    .expect("seeding the instance_settings row directly must succeed");

    let body = get_instance_v2(&app).await;

    assert_eq!(body["title"], "Kawasemi Test Instance");
    assert_eq!(
        body["description"],
        "A single-user Mastodon-compatible server, under test."
    );
    assert_eq!(
        body["thumbnail"].as_str(),
        Some("https://kawasemi.example/thumbnail.png")
    );
    assert_eq!(
        body["languages"].as_array().map(|a| a
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect::<Vec<_>>()),
        Some(vec!["en".to_string(), "ja".to_string()])
    );

    assert_eq!(body["registrations"]["enabled"], true);
    assert_eq!(body["registrations"]["approval_required"], true);
    assert_eq!(
        body["registrations"]["message"],
        "Approval is required before your account is activated."
    );

    assert_eq!(body["contact"]["email"], "admin@kawasemi.example");
    assert!(body["contact"]["account_id"].is_null());

    let rules = body["rules"].as_array().expect("rules must be an array");
    assert_eq!(rules.len(), 2);
    assert_eq!(rules[0]["id"], "1");
    assert_eq!(rules[0]["text"], "Be excellent to each other.");
    assert_eq!(rules[1]["id"], "2");
    assert_eq!(rules[1]["text"], "No spam.");

    app.cleanup().await;
}
