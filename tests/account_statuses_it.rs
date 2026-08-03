//! Integration test proving task 7.1's own observable completion condition
//! for `accounts/:id/statuses` (`.kiro/specs/accounts-and-instance/tasks.md`,
//! "7.1 アカウント系エンドポイントの統合テストを通す": "ページネーション・
//! 委譲未登録時の空/既定") against the real, `spawn_test_app`-booted
//! application router (Requirements 4.1, 4.2).
//!
//! design.md's File Structure Plan names this exact filename
//! (`account_statuses_it.rs`: "accounts/:id/statuses（ページネーション・
//! provider 未登録時の空・絞り込み伝達）（統合）") and its own Testing
//! Strategy bullet ("accounts/:id/statuses: provider 未登録で空ページ・
//! `Link` 付き、ページネーション、絞り込み伝達（4.1, 4.2, 4.3, 4.4）").
//!
//! `tests/accounts_endpoints_wiring_it.rs` (task 6) already proved a single
//! `Link`-header-attaches-when-data-exists smoke case with a fixed two-item
//! provider; this file goes further:
//! 1. The unregistered-provider default returns an empty page with **no**
//!    `Link` header at all (there is nothing to link, `build_link_header`'s
//!    own documented "no cursors -> `None`" contract).
//! 2. A real cursor walk (`max_id`/limit) through a registered
//!    `AccountStatusesProvider` built on this crate's own
//!    `api::pagination::paginate`/`StatusIdCursor` — proving the query
//!    parameters actually reach and drive the provider, not just that some
//!    `Link` header exists.
//! 3. The `pinned`/`only_media`/`exclude_replies`/`exclude_reblogs` filters
//!    are propagated to the provider unchanged (Requirement 4.4).
//! 4. `accounts/:id/statuses` 404s for an id resolving to no account, the
//!    same discipline `show_account` uses (`AccountService::resolve_account_ref`
//!    is shared between both operations).
//!
//! ## No HTTP client dependency: raw sockets (see sibling `tests/*.rs` files
//! for this crate's established rationale).

use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use kawasemi::accounts::{AccountStatusesProvider, StatusesQuery};
use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor};
use kawasemi::api::pagination::{Page, StatusIdCursor};
use kawasemi::domain::Id;
use kawasemi::error::AppError;
use kawasemi::test_harness::{TestApp, spawn_test_app};

// ---- raw HTTP plumbing ----

#[derive(Debug)]
struct RawResponse {
    status: u16,
    headers: HashMap<String, String>,
    body: String,
}

async fn raw_get(addr: SocketAddr, path: &str, extra_headers: &[(&str, &str)]) -> RawResponse {
    let mut stream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr))
        .await
        .expect("connecting to the test listener must not time out")
        .expect("connect");

    let mut request = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n");
    for (name, value) in extra_headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("Content-Length: 0\r\n\r\n");

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

fn assert_error_shape(response: &RawResponse) {
    let body = body_json(response);
    assert!(
        body.get("error").and_then(Value::as_str).is_some(),
        "expected a Mastodon-compatible {{\"error\": ...}} body, got: {body}"
    );
}

// ---- fixtures ----

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
            display_name: "Account Statuses IT Actor".to_string(),
            summary: "an account_statuses_it integration test fixture".to_string(),
        })
        .await
        .expect("creating the owner's actor fixture must succeed");

    actor.id
}

// ---- (1) unregistered provider default: empty page, no Link header ----

#[tokio::test]
async fn list_statuses_returns_an_empty_page_with_no_link_header_when_no_provider_is_registered() {
    let app = spawn_test_app().await;
    let actor_id = create_owner_with_actor(&app, "statusesdefaultowner").await;

    let response = raw_get(
        app.address,
        &format!("/api/v1/accounts/{}/statuses", actor_id.as_i64()),
        &[],
    )
    .await;
    assert_eq!(response.status, 200, "got: {response:?}");
    let body = body_json(&response);
    assert_eq!(
        body.as_array().map(Vec::len),
        Some(0),
        "the built-in EmptyStatusesProvider default must return an empty page \
         (Requirement 4.3), got: {body}"
    );
    assert!(
        !response.headers.contains_key("link"),
        "an empty page has no cursor to link (`build_link_header`'s own \
         documented contract), so no Link header should be attached, got: {response:?}"
    );

    app.cleanup().await;
}

// ---- (2) accounts/:id/statuses 404s the same way show_account does ----

#[tokio::test]
async fn list_statuses_404s_for_an_id_matching_no_account() {
    let app = spawn_test_app().await;

    let missing = raw_get(app.address, "/api/v1/accounts/999999999/statuses", &[]).await;
    assert_eq!(missing.status, 404, "got: {missing:?}");
    assert_error_shape(&missing);

    app.cleanup().await;
}

// ---- (3) a real cursor walk through a registered provider ----

/// A fixed, descending-by-id in-memory pool of 10 opaque "statuses"
/// (`{"id": "<n>"}`), paginated via this crate's own already-reviewed
/// `api::pagination::paginate`/`StatusIdCursor` — proving `max_id`/`limit`
/// query parameters genuinely reach and drive whichever
/// `AccountStatusesProvider` is registered (Requirement 4.1), not merely
/// that a `Link` header is present at all.
struct FakeTimelineProvider {
    pool: Vec<(i64, Value)>,
}

impl FakeTimelineProvider {
    fn new() -> Self {
        let pool = (1..=10)
            .rev()
            .map(|id| (id, serde_json::json!({"id": id.to_string()})))
            .collect();
        FakeTimelineProvider { pool }
    }
}

impl AccountStatusesProvider for FakeTimelineProvider {
    fn list_statuses<'a>(
        &'a self,
        query: &'a StatusesQuery,
    ) -> Pin<Box<dyn Future<Output = Result<Page<Value>, AppError>> + Send + 'a>> {
        Box::pin(async move {
            let parsed = query.page.parse::<StatusIdCursor>()?;
            let page = kawasemi::api::pagination::paginate(
                &self.pool,
                |(id, _)| StatusIdCursor(*id as u64),
                &parsed,
            );
            Ok(Page {
                items: page.items.into_iter().map(|(_, value)| value).collect(),
                prev_cursor: page.prev_cursor,
                next_cursor: page.next_cursor,
            })
        })
    }
}

#[tokio::test]
async fn list_statuses_paginates_through_a_registered_provider_via_max_id_and_limit() {
    let app = spawn_test_app().await;
    let actor_id = create_owner_with_actor(&app, "statusespaginationowner").await;

    app.state
        .accounts()
        .ports()
        .set_statuses_provider(Arc::new(FakeTimelineProvider::new()));

    // First page: newest 3 of 10 (ids 10, 9, 8).
    let first = raw_get(
        app.address,
        &format!("/api/v1/accounts/{}/statuses?limit=3", actor_id.as_i64()),
        &[],
    )
    .await;
    assert_eq!(first.status, 200, "got: {first:?}");
    let first_body = body_json(&first);
    let first_ids: Vec<&str> = first_body
        .as_array()
        .expect("statuses page must be a JSON array")
        .iter()
        .map(|item| item["id"].as_str().expect("id must be a string"))
        .collect();
    assert_eq!(first_ids, vec!["10", "9", "8"], "got: {first_body}");

    let first_link = first
        .headers
        .get("link")
        .expect("a non-empty page must carry a Link header (Requirement 10.4)");
    assert!(
        first_link.contains("max_id=8"),
        "the next link must carry the last item's cursor as max_id, got: {first_link}"
    );
    assert!(
        first_link.contains("min_id=10"),
        "the prev link must carry the first item's cursor as min_id, got: {first_link}"
    );

    // Second page: continue past max_id=8 -> ids 7, 6, 5.
    let second = raw_get(
        app.address,
        &format!(
            "/api/v1/accounts/{}/statuses?limit=3&max_id=8",
            actor_id.as_i64()
        ),
        &[],
    )
    .await;
    assert_eq!(second.status, 200, "got: {second:?}");
    let second_body = body_json(&second);
    let second_ids: Vec<&str> = second_body
        .as_array()
        .expect("statuses page must be a JSON array")
        .iter()
        .map(|item| item["id"].as_str().expect("id must be a string"))
        .collect();
    assert_eq!(second_ids, vec!["7", "6", "5"], "got: {second_body}");

    app.cleanup().await;
}

// ---- (4) filter propagation (pinned/only_media/exclude_replies/
// exclude_reblogs) reaches the provider unchanged (Requirement 4.4) ----

struct CapturingProvider {
    captured: Arc<Mutex<Option<StatusesQuery>>>,
}

impl AccountStatusesProvider for CapturingProvider {
    fn list_statuses<'a>(
        &'a self,
        query: &'a StatusesQuery,
    ) -> Pin<Box<dyn Future<Output = Result<Page<Value>, AppError>> + Send + 'a>> {
        let captured = self.captured.clone();
        let query = query.clone();
        Box::pin(async move {
            *captured
                .lock()
                .expect("CapturingProvider mutex must not be poisoned") = Some(query);
            Ok(Page {
                items: Vec::new(),
                prev_cursor: None,
                next_cursor: None,
            })
        })
    }
}

#[tokio::test]
async fn list_statuses_propagates_every_filter_query_parameter_to_the_provider() {
    let app = spawn_test_app().await;
    let actor_id = create_owner_with_actor(&app, "statusesfilterowner").await;

    let captured = Arc::new(Mutex::new(None));
    app.state
        .accounts()
        .ports()
        .set_statuses_provider(Arc::new(CapturingProvider {
            captured: captured.clone(),
        }));

    let response = raw_get(
        app.address,
        &format!(
            "/api/v1/accounts/{}/statuses?limit=7&pinned=true&only_media=true&exclude_replies=true&exclude_reblogs=true",
            actor_id.as_i64()
        ),
        &[],
    )
    .await;
    assert_eq!(response.status, 200, "got: {response:?}");

    let captured_query = captured
        .lock()
        .expect("CapturingProvider mutex must not be poisoned")
        .clone()
        .expect("the provider must have been invoked exactly once");

    assert_eq!(captured_query.page.limit, Some(7));
    assert!(captured_query.pinned, "pinned=true must reach the provider");
    assert!(
        captured_query.only_media,
        "only_media=true must reach the provider"
    );
    assert!(
        captured_query.exclude_replies,
        "exclude_replies=true must reach the provider"
    );
    assert!(
        captured_query.exclude_reblogs,
        "exclude_reblogs=true must reach the provider"
    );

    app.cleanup().await;
}
