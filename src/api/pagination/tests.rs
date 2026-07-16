//! Unit tests for the pagination toolkit (`Pagination` boundary, task 6.2).
//!
//! Requirements exercised:
//! - 6.1: `Link` header carries `rel="next"`/`rel="prev"` when cursors
//!   exist, and is entirely absent (not an empty header) when there is
//!   nothing to link.
//! - 6.2: `max_id` returns the older (smaller) side.
//! - 6.3 / 6.4: `min_id` (walking from the old side) and `since_id`
//!   (newest fixed) select a *different subset* of the same "newer than X"
//!   candidate set when there are more matches than `limit` — proven by
//!   `since_id_and_min_id_select_different_subsets_of_the_same_range`, not
//!   just that both happen to return "newer" items.
//! - 6.5: default limit when unspecified, and clamping when the requested
//!   limit exceeds [`MAX_LIMIT`].
//! - 6.6: a second, structurally different (non-status-id) [`Cursor`] impl
//!   is instantiated and driven through the same `parse`/`paginate`/
//!   `build_link_header` pipeline as [`StatusIdCursor`].
//! - 6.7: `build_link_header` produces absolute URLs using the
//!   reverse-proxy-forwarded host/scheme, not the raw connection's own.

use super::*;
use crate::error::ErrorKind;

// ---------------------------------------------------------------------
// limit resolution (6.5)
// ---------------------------------------------------------------------

#[test]
fn unspecified_limit_resolves_to_default() {
    let params = PageParams::default();
    let parsed = params.parse::<StatusIdCursor>().unwrap();
    assert_eq!(parsed.limit, DEFAULT_LIMIT);
}

#[test]
fn within_bound_limit_is_honored_verbatim() {
    let params = PageParams {
        limit: Some(5),
        ..Default::default()
    };
    let parsed = params.parse::<StatusIdCursor>().unwrap();
    assert_eq!(parsed.limit, 5);
}

#[test]
fn over_max_limit_is_clamped_down_to_the_convention_max() {
    let params = PageParams {
        limit: Some(MAX_LIMIT + 500),
        ..Default::default()
    };
    let parsed = params.parse::<StatusIdCursor>().unwrap();
    assert_eq!(
        parsed.limit, MAX_LIMIT,
        "an over-max request must round down to MAX_LIMIT, not pass through or error"
    );
}

// ---------------------------------------------------------------------
// cursor decode failure (reuse AppError, no new error type)
// ---------------------------------------------------------------------

#[test]
fn malformed_cursor_is_rejected_as_a_client_app_error() {
    let params = PageParams {
        max_id: Some("not-a-number".to_string()),
        ..Default::default()
    };
    let err = params
        .parse::<StatusIdCursor>()
        .expect_err("non-numeric max_id must fail to decode as a StatusIdCursor");
    assert_eq!(err.kind, ErrorKind::Client);
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
}

// ---------------------------------------------------------------------
// direction semantics (6.2, 6.3, 6.4) over an in-memory pool sorted
// descending by cursor (newest first), mirroring a typical
// `ORDER BY id DESC` repository query result.
// ---------------------------------------------------------------------

/// Newest-first pool: 50, 40, 30, 20, 10.
fn sample_pool() -> Vec<StatusIdCursor> {
    vec![50, 40, 30, 20, 10]
        .into_iter()
        .map(StatusIdCursor)
        .collect()
}

fn ids(page: &Page<StatusIdCursor>) -> Vec<u64> {
    page.items.iter().map(|c| c.0).collect()
}

#[test]
fn max_id_returns_the_older_side_newest_first() {
    let pool = sample_pool();
    let params = PageParams {
        max_id: Some("35".to_string()),
        limit: Some(2),
        ..Default::default()
    };
    let parsed = params.parse::<StatusIdCursor>().unwrap();

    let page = paginate(&pool, |c| *c, &parsed);

    // Items older (smaller) than 35: 30, 20, 10 — newest-of-that-set first.
    assert_eq!(
        ids(&page),
        vec![30, 20],
        "Requirement 6.2: max_id=35 must select the older side, newest-of-the-older-set first"
    );
}

#[test]
fn since_id_and_min_id_select_different_subsets_of_the_same_range() {
    let pool = sample_pool();
    // Both parameters describe the same logical range: "newer than 15",
    // which is {50, 40, 30, 20}. With limit=2 there are more candidates
    // than fit on a page, so the anchor direction determines which two
    // survive.
    let since_params = PageParams {
        since_id: Some("15".to_string()),
        limit: Some(2),
        ..Default::default()
    }
    .parse::<StatusIdCursor>()
    .unwrap();
    let min_params = PageParams {
        min_id: Some("15".to_string()),
        limit: Some(2),
        ..Default::default()
    }
    .parse::<StatusIdCursor>()
    .unwrap();

    let since_page = paginate(&pool, |c| *c, &since_params);
    let min_page = paginate(&pool, |c| *c, &min_params);

    assert_eq!(
        ids(&since_page),
        vec![50, 40],
        "Requirement 6.4: since_id anchors at the newest end of the range"
    );
    assert_eq!(
        ids(&min_page),
        vec![30, 20],
        "Requirement 6.3: min_id walks from the old side of the range forward"
    );
    assert_ne!(
        ids(&since_page),
        ids(&min_page),
        "since_id and min_id must diverge when the range exceeds the limit, not just both return \"newer\" items"
    );
}

#[test]
fn since_id_and_min_id_agree_when_the_whole_range_fits_in_one_page() {
    // When the range is smaller than the limit, there's nothing to anchor
    // away from — both directions return the identical full set. This
    // guards against a test that only "passes" because of an accidental
    // off-by-one rather than genuine anchor-direction logic.
    let pool = sample_pool();
    let since_params = PageParams {
        since_id: Some("15".to_string()),
        limit: Some(10),
        ..Default::default()
    }
    .parse::<StatusIdCursor>()
    .unwrap();
    let min_params = PageParams {
        min_id: Some("15".to_string()),
        limit: Some(10),
        ..Default::default()
    }
    .parse::<StatusIdCursor>()
    .unwrap();

    let since_page = paginate(&pool, |c| *c, &since_params);
    let min_page = paginate(&pool, |c| *c, &min_params);

    assert_eq!(ids(&since_page), vec![50, 40, 30, 20]);
    assert_eq!(ids(&min_page), vec![50, 40, 30, 20]);
}

#[test]
fn since_id_takes_precedence_over_min_id_when_both_are_present() {
    let pool = sample_pool();
    let params = PageParams {
        since_id: Some("15".to_string()),
        min_id: Some("15".to_string()),
        limit: Some(2),
        ..Default::default()
    }
    .parse::<StatusIdCursor>()
    .unwrap();

    let page = paginate(&pool, |c| *c, &params);

    assert_eq!(
        ids(&page),
        vec![50, 40],
        "documented precedence: since_id wins when both since_id and min_id are supplied"
    );
}

#[test]
fn no_cursor_params_returns_the_newest_page() {
    let pool = sample_pool();
    let params = PageParams {
        limit: Some(3),
        ..Default::default()
    }
    .parse::<StatusIdCursor>()
    .unwrap();

    let page = paginate(&pool, |c| *c, &params);

    assert_eq!(ids(&page), vec![50, 40, 30]);
}

#[test]
fn max_id_and_min_id_can_combine_into_a_bounded_window() {
    let pool = sample_pool();
    let params = PageParams {
        max_id: Some("45".to_string()),
        min_id: Some("15".to_string()),
        limit: Some(10),
        ..Default::default()
    }
    .parse::<StatusIdCursor>()
    .unwrap();

    let page = paginate(&pool, |c| *c, &params);

    // 15 < cursor < 45: {40, 30, 20}. min_id anchors from the old side, but
    // the whole window fits under the limit so nothing is dropped.
    assert_eq!(ids(&page), vec![40, 30, 20]);
}

#[test]
fn page_cursors_reflect_the_newest_and_oldest_items_in_the_page() {
    let pool = sample_pool();
    let params = PageParams {
        limit: Some(2),
        ..Default::default()
    }
    .parse::<StatusIdCursor>()
    .unwrap();

    let page = paginate(&pool, |c| *c, &params);

    assert_eq!(
        page.items.iter().map(|c| c.0).collect::<Vec<_>>(),
        vec![50, 40]
    );
    assert_eq!(page.prev_cursor.as_deref(), Some("50"));
    assert_eq!(page.next_cursor.as_deref(), Some("40"));
}

#[test]
fn empty_pool_yields_an_empty_page_with_no_cursors() {
    let pool: Vec<StatusIdCursor> = Vec::new();
    let params = PageParams::default().parse::<StatusIdCursor>().unwrap();

    let page = paginate(&pool, |c| *c, &params);

    assert!(page.items.is_empty());
    assert_eq!(page.prev_cursor, None);
    assert_eq!(page.next_cursor, None);
}

// ---------------------------------------------------------------------
// non-status-id cursor category (6.6): a bookmark-style cursor whose
// underlying representation is a lexicographically-ordered opaque string
// token, structurally nothing like StatusIdCursor's u64.
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct BookmarkCursor(String);

impl Cursor for BookmarkCursor {
    fn encode(&self) -> String {
        self.0.clone()
    }

    fn decode(raw: &str) -> Result<Self, AppError> {
        if raw.len() == 6 && raw.chars().all(|c| c.is_ascii_alphanumeric()) {
            Ok(BookmarkCursor(raw.to_string()))
        } else {
            Err(AppError::client(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("invalid bookmark cursor: '{raw}'"),
            ))
        }
    }
}

#[test]
fn non_status_id_cursor_category_plugs_into_the_same_pipeline() {
    // Lexicographic order stands in for "bookmarked_at" ordering: later
    // letters sort as "newer" bookmarks, exactly like StatusIdCursor's
    // numeric order stands in for time.
    let pool: Vec<BookmarkCursor> = ["aaaaaa", "bbbbbb", "cccccc", "dddddd"]
        .into_iter()
        .map(|s| BookmarkCursor(s.to_string()))
        .rev() // newest (dddddd) first, matching paginate's ordering contract
        .collect();

    let params = PageParams {
        max_id: Some("cccccc".to_string()),
        limit: Some(1),
        ..Default::default()
    }
    .parse::<BookmarkCursor>()
    .expect("well-formed bookmark cursor must decode");

    let page = paginate(&pool, |c| c.clone(), &params);

    assert_eq!(
        page.items,
        vec![BookmarkCursor("bbbbbb".to_string())],
        "max_id semantics (older side) must hold for a non-status-id Cursor category too"
    );
    assert_eq!(page.next_cursor.as_deref(), Some("bbbbbb"));

    let malformed = PageParams {
        max_id: Some("nope".to_string()),
        ..Default::default()
    };
    let err = malformed
        .parse::<BookmarkCursor>()
        .expect_err("malformed bookmark cursor must be rejected");
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
}

// ---------------------------------------------------------------------
// forwarded host/scheme resolution (6.7)
// ---------------------------------------------------------------------

#[test]
fn forwarded_origin_falls_back_to_connection_scheme_and_host_when_headers_absent() {
    let origin = ForwardedOrigin::resolve("http", "127.0.0.1:8080", None, None);
    assert_eq!(origin.scheme, "http");
    assert_eq!(origin.host, "127.0.0.1:8080");
}

#[test]
fn forwarded_origin_prefers_forwarded_headers_over_the_raw_connection() {
    let origin = ForwardedOrigin::resolve(
        "http",
        "127.0.0.1:8080",
        Some("https"),
        Some("kawasemi.example"),
    );
    assert_eq!(
        origin.scheme, "https",
        "must respect X-Forwarded-Proto, not the raw connection scheme"
    );
    assert_eq!(
        origin.host, "kawasemi.example",
        "must respect X-Forwarded-Host, not the raw connection host"
    );
}

#[test]
fn forwarded_origin_uses_the_client_nearest_entry_of_a_proxy_chain() {
    let origin = ForwardedOrigin::resolve(
        "http",
        "127.0.0.1:8080",
        Some("https, http"),
        Some(" kawasemi.example , internal-lb "),
    );
    assert_eq!(origin.scheme, "https");
    assert_eq!(origin.host, "kawasemi.example");
}

#[test]
fn forwarded_origin_ignores_blank_forwarded_headers() {
    let origin = ForwardedOrigin::resolve("http", "127.0.0.1:8080", Some(""), Some("   "));
    assert_eq!(
        origin.scheme, "http",
        "a blank X-Forwarded-Proto must not win over the fallback"
    );
    assert_eq!(
        origin.host, "127.0.0.1:8080",
        "a blank X-Forwarded-Host must not win over the fallback"
    );
}

// ---------------------------------------------------------------------
// Link header generation (6.1, 6.7)
// ---------------------------------------------------------------------

#[test]
fn link_header_carries_absolute_urls_through_the_forwarded_origin() {
    let origin = ForwardedOrigin::resolve(
        "http",
        "127.0.0.1:8080",
        Some("https"),
        Some("kawasemi.example"),
    );
    let ctx = RequestUriContext::new(origin, "/api/v1/timelines/home").with_query("limit", "20");
    let cursors = PageCursors {
        next: Some("40".to_string()),
        prev: Some("50".to_string()),
    };

    let header = build_link_header(&ctx, &cursors).expect("both cursors present");
    let rendered = header.to_str().unwrap();

    assert!(
        rendered.contains(
            "<https://kawasemi.example/api/v1/timelines/home?limit=20&max_id=40>; rel=\"next\""
        ),
        "got: {rendered}"
    );
    assert!(
        rendered.contains(
            "<https://kawasemi.example/api/v1/timelines/home?limit=20&min_id=50>; rel=\"prev\""
        ),
        "got: {rendered}"
    );
    assert!(
        !rendered.contains("127.0.0.1:8080") && !rendered.contains("http://"),
        "must not leak the raw connection scheme/host: {rendered}"
    );
}

#[test]
fn link_header_omits_a_direction_with_no_cursor() {
    let origin = ForwardedOrigin::resolve("https", "kawasemi.example", None, None);
    let ctx = RequestUriContext::new(origin, "/api/v1/bookmarks");
    let cursors = PageCursors {
        next: Some("bbbbbb".to_string()),
        prev: None,
    };

    let header = build_link_header(&ctx, &cursors).expect("next cursor present");
    let rendered = header.to_str().unwrap();

    assert!(rendered.contains("rel=\"next\""));
    assert!(!rendered.contains("rel=\"prev\""));
}

#[test]
fn link_header_is_absent_entirely_when_there_are_no_cursors() {
    let origin = ForwardedOrigin::resolve("https", "kawasemi.example", None, None);
    let ctx = RequestUriContext::new(origin, "/api/v1/bookmarks");
    let cursors = PageCursors::default();

    assert!(
        build_link_header(&ctx, &cursors).is_none(),
        "an empty result set must omit the Link header, not send an empty one"
    );
}

#[test]
fn link_header_percent_encodes_exotic_cursor_values() {
    let origin = ForwardedOrigin::resolve("https", "kawasemi.example", None, None);
    let ctx = RequestUriContext::new(origin, "/api/v1/bookmarks");
    let cursors = PageCursors {
        next: Some("a&b=c".to_string()),
        prev: None,
    };

    let header = build_link_header(&ctx, &cursors).unwrap();
    let rendered = header.to_str().unwrap();

    assert!(
        rendered.contains("max_id=a%26b%3Dc"),
        "raw '&'/'=' inside a cursor value must not corrupt the query string: {rendered}"
    );
}

// ---------------------------------------------------------------------
// end-to-end: Page::cursors() -> build_link_header, full pipeline
// ---------------------------------------------------------------------

#[test]
fn page_cursors_feed_build_link_header_end_to_end() {
    let pool = sample_pool();
    let params = PageParams {
        limit: Some(2),
        ..Default::default()
    }
    .parse::<StatusIdCursor>()
    .unwrap();
    let page = paginate(&pool, |c| *c, &params);

    let origin = ForwardedOrigin::resolve("https", "kawasemi.example", None, None);
    let ctx = RequestUriContext::new(origin, "/api/v1/timelines/home");

    let header = build_link_header(&ctx, &page.cursors()).expect("page has items");
    let rendered = header.to_str().unwrap();

    assert!(rendered.contains("max_id=40"));
    assert!(rendered.contains("min_id=50"));
}
