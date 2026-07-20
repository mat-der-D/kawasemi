//! Integration test proving task 1.4's observable completion condition
//! (`.kiro/specs/accounts-and-instance/tasks.md`, "1.4 AccountsModule の配線
//! 骨格を core-runtime Composition Root に追加する", `_Boundary:
//! AccountsModule_`): "`spawn_test_app` 起動後、`AppState` から
//! `AccountsModule` ハンドルが取得でき、ルータが空ハンドラ/プレースホルダで
//! mount される" (Requirements 10.1, 10.5).
//!
//! This task is wiring-only (no real repositories/services/handlers — those
//! are later tasks, `_Boundary: AccountsEndpoints, AccountsModule_` at task
//! group 6). This file therefore proves exactly two things, mirroring
//! `tests/api_foundation_wiring_it.rs`'s/`tests/media_bootstrap_wiring_it.rs`'s
//! own "prove the composition-root wiring itself" precedent for a
//! same-shaped task:
//!
//! 1. `AppState::accounts()` returns a working `AccountsModule` handle whose
//!    delegation ports registry defaults to task 1.3's safe defaults (empty
//!    statuses page / no relationship / zero counts) — proving the module was
//!    actually constructed via `AccountPortsRegistry::new()`, not left
//!    unconstructed.
//! 2. Every accounts/instance/custom_emojis route design.md's API Contract
//!    table names is mounted on the real, booted router
//!    (`crate::server::build_router`, the same one `spawn_test_app`/
//!    `bootstrap()` both serve) and returns an explicit `501 Not Implemented`
//!    placeholder — including `PATCH /api/v1/accounts/update_credentials`,
//!    the one non-`GET` route in that table, exercised on its own `PATCH`
//!    request rather than folded into the other routes' shared `GET` loop —
//!    contrasted with `405 Method Not Allowed` for a method the same
//!    mounted path never registers (both for `update_credentials`'s own
//!    path and for a `GET`-only route), proving `501` is this task's own
//!    deliberate "mounted, right method, real handler pending" choice, not
//!    axum's routing default for an unmatched method/path.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use kawasemi::domain::{AccountRef, Id};
use kawasemi::test_harness::spawn_test_app;

// ---- raw HTTP plumbing (mirrors tests/api_foundation_wiring_it.rs's own
// RawResponse/raw_request precedent — each tests/*.rs file is a separate
// compiled binary with no shared module, so this is deliberately duplicated,
// not imported) ----

#[derive(Debug)]
struct RawResponse {
    status: u16,
    #[allow(dead_code)]
    headers: HashMap<String, String>,
    #[allow(dead_code)]
    body: String,
}

async fn raw_get(addr: SocketAddr, path: &str) -> RawResponse {
    raw_request(addr, "GET", path).await
}

async fn raw_delete(addr: SocketAddr, path: &str) -> RawResponse {
    raw_request(addr, "DELETE", path).await
}

async fn raw_patch(addr: SocketAddr, path: &str) -> RawResponse {
    raw_request(addr, "PATCH", path).await
}

async fn raw_request(addr: SocketAddr, method: &str, path: &str) -> RawResponse {
    let mut stream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr))
        .await
        .expect("connecting to the test listener must not time out")
        .expect("connect");

    let request =
        format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
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

// ---- (1) AppState::accounts() is a working handle defaulting to task 1.3's
// safe defaults ----

#[tokio::test]
async fn app_state_accounts_handle_defaults_to_task_1_3s_safe_defaults() {
    let app = spawn_test_app().await;

    let accounts = app.state.accounts();
    let target = AccountRef::Local(Id::from_i64(1));

    let counts = accounts
        .ports()
        .counts(&target)
        .await
        .expect("the default ZeroCountsProvider must never fail");
    assert_eq!(counts.followers, 0);
    assert_eq!(counts.following, 0);
    assert_eq!(counts.statuses, 0);
    assert!(counts.last_status_at.is_none());

    let relationships = accounts
        .ports()
        .relationships(Id::from_i64(99), &[target])
        .await
        .expect("the default NoRelationshipProvider must never fail");
    assert_eq!(relationships.len(), 1);
    assert!(!relationships[0].following);
    assert!(!relationships[0].followed_by);

    app.cleanup().await;
}

// ---- (2) accounts/instance/custom_emojis routes are mounted with an
// explicit 501 placeholder, distinct from a genuinely unmounted path's 404
// ----

#[tokio::test]
async fn accounts_instance_custom_emojis_routes_are_mounted_as_placeholders() {
    let app = spawn_test_app().await;

    for path in [
        "/api/v1/accounts/verify_credentials",
        "/api/v1/accounts/1",
        "/api/v1/accounts/1/statuses",
        "/api/v1/accounts/relationships",
        "/api/v2/instance",
        "/api/v1/custom_emojis",
    ] {
        let response = raw_get(app.address, path).await;
        assert_eq!(
            response.status, 501,
            "expected {path} to be mounted as an explicit 501 placeholder \
             through the real router, got: {response:?}"
        );
    }

    // `update_credentials` is the one route design.md's API Contract table
    // mounts on `PATCH`, not `GET` — exercised separately (not folded into
    // the `raw_get` loop above) so a regression that deleted, mistyped, or
    // mis-mounted that one `.route(...)` call (e.g. onto the wrong method)
    // would fail this test rather than going unnoticed.
    let update_credentials_patch =
        raw_patch(app.address, "/api/v1/accounts/update_credentials").await;
    assert_eq!(
        update_credentials_patch.status, 501,
        "expected PATCH /api/v1/accounts/update_credentials to be mounted as an explicit 501 \
         placeholder through the real router, got: {update_credentials_patch:?}"
    );
    // `GET` on that same path is not registered at all — proves the 501
    // above is bound to `PATCH` specifically, not a fallback that would
    // accept any method on that path.
    let update_credentials_get = raw_get(app.address, "/api/v1/accounts/update_credentials").await;
    assert_eq!(
        update_credentials_get.status, 405,
        "expected GET on the PATCH-only update_credentials route to be rejected at the \
         method-dispatch level, got: {update_credentials_get:?}"
    );

    // A method this placeholder group never registers on
    // `ACCOUNTS_SHOW_PATH` (only `GET`) still gets routed (matched by path)
    // but rejected at the method-dispatch level with axum's own `405
    // Method Not Allowed` — distinct from the deliberate `501` placeholder
    // above, proving `501` is this task's own explicit choice for "mounted,
    // right method, real handler pending" rather than an accidental
    // catch-all default. (A true "path never mounted at all" 404 is not
    // separately observable through this app's *GET* router: federation-core's
    // own `OBJECT_CATCH_ALL_PATH` (`/{*path}`) already matches every
    // otherwise-unmatched `GET` request — see `src/server.rs`'s own doc
    // comment on that route — so this file does not assert on it.)
    let wrong_method = raw_delete(app.address, "/api/v1/accounts/1").await;
    assert_eq!(
        wrong_method.status, 405,
        "expected DELETE on a GET-only mounted route to be rejected at the method-dispatch \
         level, got: {wrong_method:?}"
    );

    app.cleanup().await;
}
