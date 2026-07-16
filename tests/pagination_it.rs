//! Pagination integration tests (task 9.3, `_Depends: 7.1_`, design.md's
//! File Structure Plan: `tests/pagination_it.rs`).
//!
//! Requirements exercised: 6.1, 6.2, 6.3, 6.4, 6.5, 6.6, 6.7.
//!
//! `src/api/pagination.rs`'s own unit tests (task 6.2, `src/api/pagination/
//! tests.rs`) already prove `PageParams::parse`/`paginate`/
//! `build_link_header`'s pure-function behavior directly. This file does not
//! re-derive that logic; it proves the same, already-reviewed toolkit
//! produces correct results **through real HTTP** against a
//! `spawn_test_app`-booted instance, end to end: cursor query parameters
//! parsed off the wire, a real paginated response body, and a real `Link`
//! response header a client can literally follow.
//!
//! ## No production list endpoint exists yet to test against
//! Per `src/api/pagination.rs`'s own module doc comment, no downstream
//! feature spec has wired a list endpoint onto this toolkit yet (task 7.1
//! deliberately does not depend on 6.2). Mirroring
//! `tests/auth_scope_it.rs`'s/`tests/api_foundation_wiring_it.rs`'s own
//! precedent (itself modeled on `src/server/tests.rs`'s "merge a test-only
//! route onto `router()`, then `.with_state(state)`" technique), this file
//! mounts two tiny extra routes directly on the real, already-running
//! instance's `AppState`:
//! - `/__pagination_status_ids__`: pages a fixed, in-memory, descending
//!   `StatusIdCursor` collection (the common case: cursor == entity id).
//! - `/__pagination_category_cursor__`: pages a fixed, in-memory collection
//!   keyed by [`GroupTokenCursor`], a second, structurally different
//!   `Cursor` impl (an opaque alphanumeric token, not a `u64`) — concretely
//!   proving Requirement 6.6's "カテゴリ毎に差し替え可能" (category-swappable)
//!   claim over real HTTP, the same way `bookmarks`/`favourites`/
//!   `notifications` would plug in their own non-status-id cursor.
//!
//! Both handlers call `PageParams::parse`/`paginate`/`build_link_header`
//! directly — never reimplementing cursor interpretation or `Link`
//! generation themselves.
//!
//! ## No HTTP client dependency: raw sockets, mirroring established
//! precedent
//! This crate has no HTTP client dependency (`Cargo.toml`). This file
//! duplicates `tests/api_foundation_wiring_it.rs`'s/`tests/oauth_flow_it.rs`'s/
//! `tests/auth_scope_it.rs`'s own `RawResponse`/`raw_request`/
//! `parse_response` helpers (each `tests/*.rs` file is a separate compiled
//! binary with no shared module, and those helpers are private to their own
//! file), extended here to also capture response headers (lower-cased
//! names) since this file — unlike `tests/auth_scope_it.rs` — genuinely
//! needs to read the `Link` response header to follow `rel="next"`/
//! `rel="prev"` across pages.
//!
//! ## Requirement 6.7 (reverse-proxy-aware absolute URLs) IS concretely
//! testable at this layer
//! `src/api/pagination.rs`'s own module doc comment notes `TestApp` always
//! binds to a fixed local address, but that does not block testing 6.7 here:
//! `ForwardedOrigin::resolve` reads `X-Forwarded-Proto`/`X-Forwarded-Host`
//! *request* headers, which this file's raw HTTP client can send freely
//! regardless of what address the listener is actually bound to. This file
//! therefore does assert on forwarded-origin behavior directly (see
//! `link_header_reflects_the_forwarded_proxy_origin_not_the_raw_connection`)
//! rather than deferring entirely to the unit tests.

use std::collections::HashMap;
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::extract::Query;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use kawasemi::api::pagination::{
    Cursor, ForwardedOrigin, MAX_LIMIT, PageParams, RequestUriContext, StatusIdCursor,
    build_link_header, paginate,
};
use kawasemi::error::AppError;
use kawasemi::state::AppState;
use kawasemi::test_harness::{TestApp, spawn_test_app};

// ---- raw HTTP plumbing (duplicated from tests/auth_scope_it.rs; see this
// file's module doc comment for why, and for why `headers` is captured here
// even though tests/auth_scope_it.rs's own copy was rejected for an unused
// equivalent field) ----

#[derive(Debug)]
struct RawResponse {
    status: u16,
    /// Response headers, keyed by lower-cased header name. Actually read by
    /// every test in this file via [`RawResponse::link_targets`] — never a
    /// carried-but-unused field.
    headers: HashMap<String, String>,
    body: String,
}

impl RawResponse {
    /// Parses this response's `Link` header (if present) into its
    /// `rel="next"`/`rel="prev"` target URLs, mirroring
    /// `build_link_header`'s own rendering: `<url>; rel="next", <url>;
    /// rel="prev"`, either part optionally absent.
    fn link_targets(&self) -> (Option<String>, Option<String>) {
        let Some(raw) = self.headers.get("link") else {
            return (None, None);
        };
        let mut next = None;
        let mut prev = None;
        for part in raw.split(',') {
            let part = part.trim();
            let Some(url_end) = part.find('>') else {
                continue;
            };
            let url = part[1..url_end].to_string();
            if part.contains("rel=\"next\"") {
                next = Some(url);
            } else if part.contains("rel=\"prev\"") {
                prev = Some(url);
            }
        }
        (next, prev)
    }
}

/// Extracts the path+query portion of an absolute `Link` target URL (e.g.
/// `http://127.0.0.1/__pagination_status_ids__?limit=10&max_id=40` ->
/// `/__pagination_status_ids__?limit=10&max_id=40`), so a follow-up request
/// can be issued against the real `TestApp` socket without needing the
/// `Link` URL's own scheme/host to be independently resolvable (it is an
/// application-level convention, not necessarily a real routable name in
/// this test environment).
fn path_and_query(url: &str) -> String {
    let (_scheme, after_scheme) = url
        .split_once("://")
        .expect("Link target must be an absolute URL");
    let slash = after_scheme
        .find('/')
        .expect("Link target must carry a path after the origin");
    after_scheme[slash..].to_string()
}

async fn raw_request(
    addr: std::net::SocketAddr,
    method: &str,
    path: &str,
    extra_headers: &[(&str, &str)],
) -> RawResponse {
    let mut stream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr))
        .await
        .expect("connecting to the test listener must not time out")
        .expect("connect");

    let mut request =
        format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n");
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
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    RawResponse {
        status,
        headers,
        body: body.to_string(),
    }
}

// ---- test-only pagination routes ----

/// Raw wire-format cursor/limit query parameters, decoded by axum's `Query`
/// extractor straight into `PageParams`'s own shape (Requirements 6.2-6.5).
#[derive(Debug, Deserialize)]
struct RawPageQuery {
    max_id: Option<String>,
    since_id: Option<String>,
    min_id: Option<String>,
    limit: Option<u32>,
}

impl From<RawPageQuery> for PageParams {
    fn from(raw: RawPageQuery) -> Self {
        PageParams {
            max_id: raw.max_id,
            since_id: raw.since_id,
            min_id: raw.min_id,
            limit: raw.limit,
        }
    }
}

/// Resolves the `Link` header's origin from `X-Forwarded-Proto`/
/// `X-Forwarded-Host` (Requirement 6.7), falling back to a fixed local
/// scheme/host when absent — mirroring `ForwardedOrigin::resolve`'s own
/// documented axum-integration seam (`src/api/pagination.rs`'s module doc
/// comment: "a thin axum extractor ... is left to whichever endpoint spec
/// first wires a live router").
fn forwarded_origin(headers: &HeaderMap) -> ForwardedOrigin {
    let header_str = |name: &str| -> Option<&str> { headers.get(name)?.to_str().ok() };
    ForwardedOrigin::resolve(
        "http",
        "pagination-it.kawasemi.internal",
        header_str("x-forwarded-proto"),
        header_str("x-forwarded-host"),
    )
}

/// Attaches a `Link` header (Requirement 6.1) to `body` when `cursors`
/// carries at least one direction, otherwise returns `body` unchanged (an
/// empty result set must omit the header entirely, per
/// `build_link_header`'s own contract).
fn with_link_header(
    mut response: Response,
    ctx: &RequestUriContext,
    cursors: &kawasemi::api::pagination::PageCursors,
) -> Response {
    if let Some(link) = build_link_header(ctx, cursors) {
        response.headers_mut().insert(header::LINK, link);
    }
    response
}

/// Fixed, in-memory, descending (newest-first) `StatusIdCursor` collection:
/// ids 50 down to 1. Large enough (50 > `MAX_LIMIT` = 40) to concretely
/// prove `limit` clamping (Requirement 6.5) by item count, not just by
/// asserting a clamped `parsed.limit` value.
fn status_id_pool() -> Vec<StatusIdCursor> {
    (1..=50).rev().map(StatusIdCursor).collect()
}

/// `GET /__pagination_status_ids__`: pages [`status_id_pool`] through the
/// real `PageParams`/`paginate`/`build_link_header` pipeline.
async fn paginate_status_ids(
    headers: HeaderMap,
    Query(query): Query<RawPageQuery>,
) -> Result<Response, AppError> {
    let raw_limit = query.limit;
    let params: PageParams = query.into();
    let parsed = params.parse::<StatusIdCursor>()?;

    let pool = status_id_pool();
    let page = paginate(&pool, |c| *c, &parsed);
    let ids: Vec<u64> = page.items.iter().map(|c| c.0).collect();

    let mut ctx = RequestUriContext::new(forwarded_origin(&headers), "/__pagination_status_ids__");
    if let Some(limit) = raw_limit {
        ctx = ctx.with_query("limit", limit.to_string());
    }

    let body = Json(json!({ "ids": ids })).into_response();
    Ok(with_link_header(body, &ctx, &page.cursors()))
}

/// A second, structurally different [`Cursor`] category (Requirement 6.6):
/// an opaque, fixed-width alphanumeric token, standing in for a
/// non-status-id cursor such as a bookmark/favourite join row or a
/// notification group key — nothing like [`StatusIdCursor`]'s `u64`.
/// Lexicographic order stands in for "created_at" ordering: later letters
/// sort as newer, mirroring `src/api/pagination/tests.rs`'s own
/// `BookmarkCursor` precedent (that type is private to its own module, so
/// this file defines its own analogous type rather than reaching into it).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct GroupTokenCursor(String);

impl Cursor for GroupTokenCursor {
    fn encode(&self) -> String {
        self.0.clone()
    }

    fn decode(raw: &str) -> Result<Self, AppError> {
        if raw.len() == 8 && raw.chars().all(|c| c.is_ascii_alphanumeric()) {
            Ok(GroupTokenCursor(raw.to_string()))
        } else {
            Err(AppError::client(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("invalid group-token cursor: '{raw}'"),
            ))
        }
    }
}

/// Fixed, in-memory, descending (newest-first) [`GroupTokenCursor`]
/// collection: eight tokens, `zzzzzzz8`..`zzzzzzz1` in reverse (newest
/// first), each a valid 8-character alphanumeric token.
fn group_token_pool() -> Vec<GroupTokenCursor> {
    vec![
        "tok00008", "tok00007", "tok00006", "tok00005", "tok00004", "tok00003", "tok00002",
        "tok00001",
    ]
    .into_iter()
    .map(|s| GroupTokenCursor(s.to_string()))
    .collect()
}

/// `GET /__pagination_category_cursor__`: pages [`group_token_pool`]
/// through the identical `PageParams`/`paginate`/`build_link_header`
/// pipeline as [`paginate_status_ids`], but instantiated over
/// [`GroupTokenCursor`] instead of `StatusIdCursor` — concretely proving
/// Requirement 6.6's category-swappable abstraction over real HTTP.
async fn paginate_group_tokens(
    headers: HeaderMap,
    Query(query): Query<RawPageQuery>,
) -> Result<Response, AppError> {
    let raw_limit = query.limit;
    let params: PageParams = query.into();
    let parsed = params.parse::<GroupTokenCursor>()?;

    let pool = group_token_pool();
    let page = paginate(&pool, |c| c.clone(), &parsed);
    let tokens: Vec<String> = page.items.iter().map(|c| c.0.clone()).collect();

    let mut ctx = RequestUriContext::new(
        forwarded_origin(&headers),
        "/__pagination_category_cursor__",
    );
    if let Some(limit) = raw_limit {
        ctx = ctx.with_query("limit", limit.to_string());
    }

    let body = Json(json!({ "tokens": tokens })).into_response();
    Ok(with_link_header(body, &ctx, &page.cursors()))
}

fn pagination_test_router(state: AppState) -> Router {
    kawasemi::server::router()
        .route("/__pagination_status_ids__", get(paginate_status_ids))
        .route(
            "/__pagination_category_cursor__",
            get(paginate_group_tokens),
        )
        .with_state(state)
}

/// Serves [`pagination_test_router`] on its own freshly bound ephemeral
/// listener, mirroring `tests/auth_scope_it.rs`'s own
/// `spawn_protected_router` precedent.
async fn spawn_pagination_router(app: &TestApp) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    let router = pagination_test_router(app.state.clone());
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    addr
}

fn ids_from_body(response: &RawResponse) -> Vec<u64> {
    let body: Value = serde_json::from_str(&response.body).expect("response must be valid JSON");
    body["ids"]
        .as_array()
        .expect("response must carry an \"ids\" array")
        .iter()
        .map(|v| v.as_u64().expect("each id must be a JSON number"))
        .collect()
}

fn tokens_from_body(response: &RawResponse) -> Vec<String> {
    let body: Value = serde_json::from_str(&response.body).expect("response must be valid JSON");
    body["tokens"]
        .as_array()
        .expect("response must carry a \"tokens\" array")
        .iter()
        .map(|v| {
            v.as_str()
                .expect("each token must be a JSON string")
                .to_string()
        })
        .collect()
}

// ---- (1) max_id: strictly older side, no gaps/duplicates across
// sequential pages (Requirement 6.2) ----

#[tokio::test]
async fn max_id_returns_strictly_older_items_with_no_overlap_across_sequential_pages() {
    let app = spawn_test_app().await;
    let addr = spawn_pagination_router(&app).await;

    let page1 = raw_request(addr, "GET", "/__pagination_status_ids__?limit=10", &[]).await;
    assert_eq!(page1.status, 200, "page1: {page1:?}");
    let page1_ids = ids_from_body(&page1);
    assert_eq!(
        page1_ids,
        (41..=50).rev().collect::<Vec<u64>>(),
        "no cursor must return the newest page"
    );

    let (next_url, _prev_url) = page1.link_targets();
    let next_path = next_url.expect("page1 must carry a rel=\"next\" Link target");
    assert!(
        next_path.contains("max_id=41"),
        "next link must anchor on the oldest item of page1: {next_path}"
    );

    let page2 = raw_request(addr, "GET", &path_and_query(&next_path), &[]).await;
    assert_eq!(page2.status, 200, "page2: {page2:?}");
    let page2_ids = ids_from_body(&page2);
    assert_eq!(
        page2_ids,
        (31..=40).rev().collect::<Vec<u64>>(),
        "Requirement 6.2: max_id must select strictly older items, newest-of-the-older-set first"
    );

    let overlap: Vec<&u64> = page1_ids
        .iter()
        .filter(|id| page2_ids.contains(id))
        .collect();
    assert!(
        overlap.is_empty(),
        "sequential max_id pages must not duplicate any item, got overlap: {overlap:?}"
    );
    assert!(
        page1_ids.iter().all(|id| *id > 40) && page2_ids.iter().all(|id| *id <= 40),
        "no item may be skipped between sequential pages: page1={page1_ids:?} page2={page2_ids:?}"
    );

    app.cleanup().await;
}

// ---- (2) since_id vs min_id directional difference (Requirements 6.3,
// 6.4) ----

#[tokio::test]
async fn since_id_and_min_id_diverge_over_http_matching_their_documented_directions() {
    let app = spawn_test_app().await;
    let addr = spawn_pagination_router(&app).await;

    // Range "newer than 10" over a 50-item pool has 40 candidates (11..50),
    // far more than limit=5, so the two anchor directions must disagree.
    let since = raw_request(
        addr,
        "GET",
        "/__pagination_status_ids__?since_id=10&limit=5",
        &[],
    )
    .await;
    let min = raw_request(
        addr,
        "GET",
        "/__pagination_status_ids__?min_id=10&limit=5",
        &[],
    )
    .await;
    assert_eq!(since.status, 200, "since_id request: {since:?}");
    assert_eq!(min.status, 200, "min_id request: {min:?}");

    let since_ids = ids_from_body(&since);
    let min_ids = ids_from_body(&min);

    assert_eq!(
        since_ids,
        vec![50, 49, 48, 47, 46],
        "Requirement 6.4: since_id must anchor at the newest end of the range"
    );
    assert_eq!(
        min_ids,
        vec![15, 14, 13, 12, 11],
        "Requirement 6.3: min_id must walk forward from the oldest end of the range"
    );
    assert_ne!(
        since_ids, min_ids,
        "since_id and min_id must behave differently, not both just return \"newer\" items"
    );

    app.cleanup().await;
}

// ---- (3) limit upper-bound clamping (Requirement 6.5) ----

#[tokio::test]
async fn limit_over_the_convention_max_is_clamped_over_http() {
    let app = spawn_test_app().await;
    let addr = spawn_pagination_router(&app).await;

    let response = raw_request(addr, "GET", "/__pagination_status_ids__?limit=9999", &[]).await;
    assert_eq!(response.status, 200, "{response:?}");
    let ids = ids_from_body(&response);
    assert_eq!(
        ids.len(),
        MAX_LIMIT as usize,
        "an over-max limit must be rounded down to MAX_LIMIT ({MAX_LIMIT}), not honored verbatim \
         (pool has 50 items, more than MAX_LIMIT, so this proves real clamping): got {} items",
        ids.len()
    );
    assert_eq!(
        ids,
        (11..=50).rev().collect::<Vec<u64>>(),
        "the clamped page must still be the newest MAX_LIMIT items"
    );

    app.cleanup().await;
}

// ---- (4) full-collection traversal via rel="next": no gaps, no
// duplicates, terminates (Requirements 6.1, 6.2) ----

#[tokio::test]
async fn paging_through_rel_next_repeatedly_covers_every_item_once_and_terminates() {
    let app = spawn_test_app().await;
    let addr = spawn_pagination_router(&app).await;

    let mut collected: Vec<u64> = Vec::new();
    let mut path = "/__pagination_status_ids__?limit=10".to_string();
    let mut iterations = 0;
    loop {
        iterations += 1;
        assert!(
            iterations <= 20,
            "possible infinite loop: exceeded a generous iteration bound for a 50-item pool \
             paged at 10 per page; collected so far: {collected:?}"
        );

        let response = raw_request(addr, "GET", &path, &[]).await;
        assert_eq!(response.status, 200, "page {iterations}: {response:?}");
        collected.extend(ids_from_body(&response));

        match response.link_targets().0 {
            Some(next_url) => path = path_and_query(&next_url),
            None => break,
        }
    }

    assert_eq!(
        collected,
        (1..=50).rev().collect::<Vec<u64>>(),
        "traversing every rel=\"next\" link must cover the whole 50-item pool exactly once, in \
         descending order, with no gap and no duplicate"
    );
    // 5 full pages of 10 plus one trailing empty page confirms termination.
    assert_eq!(
        iterations, 6,
        "expected exactly 5 non-empty pages plus 1 terminating empty page, got {iterations} \
         requests"
    );

    app.cleanup().await;
}

// ---- (5) rel="prev" correctness: following prev reproduces the expected
// page (Requirement 6.1) ----

#[tokio::test]
async fn rel_prev_link_reproduces_the_previous_page() {
    let app = spawn_test_app().await;
    let addr = spawn_pagination_router(&app).await;

    let page1 = raw_request(addr, "GET", "/__pagination_status_ids__?limit=10", &[]).await;
    let page1_ids = ids_from_body(&page1);
    let (next_url, _) = page1.link_targets();
    let page2 = raw_request(addr, "GET", &path_and_query(&next_url.unwrap()), &[]).await;
    assert_eq!(page2.status, 200, "page2: {page2:?}");

    let (_, prev_url) = page2.link_targets();
    let prev_path = prev_url.expect("page2 must carry a rel=\"prev\" Link target");
    let back = raw_request(addr, "GET", &path_and_query(&prev_path), &[]).await;
    assert_eq!(back.status, 200, "following rel=\"prev\": {back:?}");

    assert_eq!(
        ids_from_body(&back),
        page1_ids,
        "following page2's rel=\"prev\" link must reproduce page1's exact item set"
    );

    app.cleanup().await;
}

// ---- (6) a non-status-id cursor category plugs into the same pipeline
// over real HTTP (Requirement 6.6) ----

#[tokio::test]
async fn a_non_status_id_cursor_category_pages_correctly_over_http() {
    let app = spawn_test_app().await;
    let addr = spawn_pagination_router(&app).await;

    // No cursor: newest-first page of the whole 8-token pool.
    let page1 = raw_request(addr, "GET", "/__pagination_category_cursor__?limit=3", &[]).await;
    assert_eq!(page1.status, 200, "{page1:?}");
    assert_eq!(
        tokens_from_body(&page1),
        vec!["tok00008", "tok00007", "tok00006"],
        "Requirement 6.6: a GroupTokenCursor category must page through the same pipeline \
         StatusIdCursor uses"
    );

    let (next_url, _) = page1.link_targets();
    let next_path = next_url.expect("page1 must carry a rel=\"next\" Link target");
    assert!(
        next_path.contains("max_id=tok00006"),
        "next link must carry the category's own opaque cursor encoding, not a numeric id: \
         {next_path}"
    );

    let page2 = raw_request(addr, "GET", &path_and_query(&next_path), &[]).await;
    assert_eq!(page2.status, 200, "{page2:?}");
    assert_eq!(
        tokens_from_body(&page2),
        vec!["tok00005", "tok00004", "tok00003"],
        "max_id semantics (older side) must hold for a non-status-id Cursor category too"
    );

    // A malformed cursor for this category is rejected the same way
    // StatusIdCursor's own decode failures are (reusing AppError, not a new
    // error type) — proven here through the real HTTP boundary.
    let malformed = raw_request(
        addr,
        "GET",
        "/__pagination_category_cursor__?max_id=nope",
        &[],
    )
    .await;
    assert_eq!(
        malformed.status, 422,
        "a malformed category cursor must be rejected as a client error over HTTP: {malformed:?}"
    );

    app.cleanup().await;
}

// ---- (7) Link header respects reverse-proxy forwarded host/scheme
// (Requirement 6.7) ----

#[tokio::test]
async fn link_header_reflects_the_forwarded_proxy_origin_not_the_raw_connection() {
    let app = spawn_test_app().await;
    let addr = spawn_pagination_router(&app).await;

    let response = raw_request(
        addr,
        "GET",
        "/__pagination_status_ids__?limit=10",
        &[
            ("X-Forwarded-Proto", "https"),
            ("X-Forwarded-Host", "kawasemi.example"),
        ],
    )
    .await;
    assert_eq!(response.status, 200, "{response:?}");

    let raw_link = response
        .headers
        .get("link")
        .expect("response must carry a Link header");
    assert!(
        raw_link.contains("https://kawasemi.example/__pagination_status_ids__"),
        "Link header must use the forwarded scheme/host, got: {raw_link}"
    );
    assert!(
        !raw_link.contains("pagination-it.kawasemi.internal") && !raw_link.contains("http://"),
        "Link header must not leak the fallback/raw connection scheme or host: {raw_link}"
    );

    app.cleanup().await;
}
