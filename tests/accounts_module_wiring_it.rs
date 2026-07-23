//! Integration test proving task 1.4's observable completion condition
//! (`.kiro/specs/accounts-and-instance/tasks.md`, "1.4 AccountsModule の配線
//! 骨格を core-runtime Composition Root に追加する", `_Boundary:
//! AccountsModule_`): "`spawn_test_app` 起動後、`AppState` から
//! `AccountsModule` ハンドルが取得でき、ルータが空ハンドラ/プレースホルダで
//! mount される" (Requirements 10.1, 10.5).
//!
//! This file's second test originally proved task 1.4's wiring-only,
//! placeholder-`501` baseline (no real repositories/services/handlers —
//! those were later tasks). As of task 6 (`_Boundary: AccountsEndpoints,
//! AccountsModule_`), every one of those placeholders has been replaced by
//! a real handler (`crate::accounts::endpoints`), so that placeholder-`501`
//! assertion is no longer true and would be a stale regression if left
//! unchanged (task 6's own instructions are explicit about this). This file
//! therefore now proves exactly two things, mirroring
//! `tests/api_foundation_wiring_it.rs`'s/`tests/media_bootstrap_wiring_it.rs`'s
//! own "prove the composition-root wiring itself" precedent for a
//! same-shaped task:
//!
//! 1. `AppState::accounts()` returns a working `AccountsModule` handle whose
//!    delegation ports registry defaults to task 1.3's safe defaults (empty
//!    statuses page / no relationship / zero counts) — proving the module was
//!    actually constructed via `AccountPortsRegistry::new()`, not left
//!    unconstructed. (Unaffected by task 6, kept unchanged.)
//! 2. Every accounts/instance/custom_emojis route design.md's API Contract
//!    table names is mounted on the real, booted router
//!    (`crate::server::build_router`, the same one `spawn_test_app`/
//!    `bootstrap()` both serve) and now runs its real handler rather than
//!    the old `501` placeholder: the three auth-mandatory routes
//!    (`verify_credentials`/`relationships`/`update_credentials`) reject an
//!    unauthenticated request with `401` (proving a real
//!    `RequiredActor`-gated handler is mounted, not a routing artifact —
//!    `501` would have meant "not implemented", `404`/`405` would have meant
//!    "not mounted"/"wrong method", neither of which is what a real,
//!    scope-gated handler produces for a bare request), the two public
//!    single-resource routes (`accounts/:id`, `accounts/:id/statuses`)
//!    return a real `404` for an id that does not exist in a fresh test
//!    database (proving `AccountService::show_account`/`list_statuses`
//!    actually ran, resolved nothing, and reported "not found" — a `501`
//!    placeholder could never have produced this status), and the two fully
//!    public routes (`instance`, `custom_emojis`) return a real `200` with a
//!    JSON body. `PATCH /api/v1/accounts/update_credentials` (the one
//!    non-`GET` route in design.md's table) is exercised on its own `PATCH`
//!    request rather than folded into the other routes' shared `GET` loop,
//!    still contrasted with `405 Method Not Allowed` for `GET` on that same
//!    path (unaffected by task 6: the path is still `PATCH`-only) — proving
//!    the method binding itself is unchanged, only the handler behind it.
//!
//! Task-6-level scope/error-shape/full-cross-cutting-wiring coverage (every
//! endpoint's own required scope, 403 for insufficient scope, 200 with the
//! correct scope, `Link` header generation, and the Mastodon-compatible
//! error-body shape) is `tests/accounts_endpoints_wiring_it.rs`'s own,
//! separate job — this file stays scoped to what it already proved before
//! task 6 (the composition-root wiring itself), simply updated so its
//! assertions match the real handlers now mounted there instead of asserting
//! a placeholder behavior that no longer exists.

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

// ---- (2) accounts/instance/custom_emojis routes are mounted with their
// real task-6 handlers, distinct from a genuinely unmounted path's 404 and
// from task 1.4's now-retired 501 placeholder ----

#[tokio::test]
async fn accounts_instance_custom_emojis_routes_are_mounted_with_real_handlers() {
    let app = spawn_test_app().await;

    // Auth-mandatory routes (Requirement 10.1): an unauthenticated request
    // is rejected with 401 by the real `RequiredActor` extractor — never
    // 501 (that would mean "not implemented") and never 404/405 (that would
    // mean "not mounted"/"wrong method").
    for path in [
        "/api/v1/accounts/verify_credentials",
        "/api/v1/accounts/relationships",
    ] {
        let response = raw_get(app.address, path).await;
        assert_eq!(
            response.status, 401,
            "expected {path} to reject an unauthenticated request through its real, \
             RequiredActor-gated handler, got: {response:?}"
        );
    }

    // Public single-resource routes (Requirements 3.4, 10.2): a fresh test
    // database has no actor with internal id 1, so the real
    // `AccountService::show_account`/`list_statuses` resolve nothing and
    // report a genuine 404 (Requirement 3.3) — a 501 placeholder could
    // never have produced this status, only a real handler that actually
    // tried to resolve the id.
    for path in ["/api/v1/accounts/1", "/api/v1/accounts/1/statuses"] {
        let response = raw_get(app.address, path).await;
        assert_eq!(
            response.status, 404,
            "expected {path} to run its real handler and report a genuine 404 for a \
             nonexistent account, got: {response:?}"
        );
    }

    // Fully public routes (Requirements 8.1, 9.1, 10.2): real 200 JSON
    // responses, no auth involved at all.
    for path in ["/api/v2/instance", "/api/v1/custom_emojis"] {
        let response = raw_get(app.address, path).await;
        assert_eq!(
            response.status, 200,
            "expected {path} to respond 200 through its real, unauthenticated handler, \
             got: {response:?}"
        );
    }

    // `update_credentials` is the one route design.md's API Contract table
    // mounts on `PATCH`, not `GET` — exercised separately (not folded into
    // the `raw_get` loop above) so a regression that deleted, mistyped, or
    // mis-mounted that one `.route(...)` call (e.g. onto the wrong method)
    // would fail this test rather than going unnoticed. Same auth-mandatory
    // reasoning as `verify_credentials`/`relationships` above: 401, not 501.
    let update_credentials_patch =
        raw_patch(app.address, "/api/v1/accounts/update_credentials").await;
    assert_eq!(
        update_credentials_patch.status, 401,
        "expected PATCH /api/v1/accounts/update_credentials to reject an unauthenticated \
         request through its real, RequiredActor-gated handler, got: {update_credentials_patch:?}"
    );
    // `GET` on that same path is not registered at all — proves the 401
    // above is bound to `PATCH` specifically, not a fallback that would
    // accept any method on that path. Unaffected by task 6: the method
    // binding itself did not change, only the handler behind it.
    let update_credentials_get = raw_get(app.address, "/api/v1/accounts/update_credentials").await;
    assert_eq!(
        update_credentials_get.status, 405,
        "expected GET on the PATCH-only update_credentials route to be rejected at the \
         method-dispatch level, got: {update_credentials_get:?}"
    );

    // A method this route group never registers on `ACCOUNTS_SHOW_PATH`
    // (only `GET`) still gets routed (matched by path) but rejected at the
    // method-dispatch level with axum's own `405 Method Not Allowed` —
    // unaffected by task 6. (A true "path never mounted at all" 404 is not
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
