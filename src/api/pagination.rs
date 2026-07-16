//! Pagination toolkit (api-foundation `Pagination` boundary, task 6.2).
//!
//! Scope: this module is a standalone library — cursor-parameter
//! interpretation (`max_id`/`since_id`/`min_id`/`limit`), a category-swappable
//! `Cursor` abstraction, a page representation, and `Link` (next/prev) header
//! generation that respects reverse-proxy forwarded host/scheme. Unlike task
//! 6.1's `MastodonError`, there is no cross-cutting tower layer or endpoint
//! in this spec that consumes this toolkit (see `tasks.md` task 7.1, which
//! deliberately does not depend on 6.2). Downstream feature specs
//! (timelines, notifications, bookmarks, ...) call this module directly from
//! their own list endpoints. This module does not touch `src/server.rs`,
//! `src/bootstrap.rs`, `src/state.rs`, or any router.
//!
//! ## Design note: reconciling design.md's sketch with what this module
//! actually needs
//!
//! design.md's illustrative Service Interface for `Pagination` sketches:
//! ```ignore
//! pub struct PageParams { pub max_id: Option<String>, pub since_id: Option<String>, pub min_id: Option<String>, pub limit: Option<u32> }
//! pub trait Cursor: Sized { fn encode(&self) -> String; fn decode(raw: &str) -> Result<Self, AppError>; }
//! pub struct Page<T> { pub items: Vec<T>, pub prev_cursor: Option<String>, pub next_cursor: Option<String> }
//! pub fn build_link_header(req_uri: &RequestUriContext, page_cursors: &PageCursors) -> Option<HeaderValue>;
//! ```
//! This module keeps that shape but fills in what the sketch left
//! unspecified:
//! - [`Cursor`] additionally requires `Clone + Ord` (not just `Sized`): the
//!   direction semantics of Requirements 6.2–6.4 (older/newer, anchored at
//!   the newest vs. walking from the oldest) are comparisons over cursor
//!   values, so a category-swappable cursor must be orderable, not just
//!   encodable/decodable as an opaque string.
//! - [`PageParams`] gains a [`PageParams::parse`] method the sketch doesn't
//!   show: something has to bridge the raw `Option<String>` wire params into
//!   typed `C: Cursor` values via `Cursor::decode`, and that bridging point
//!   is also where malformed cursors become a 422-ish [`AppError`]
//!   (Requirement 6.7's "reuse `AppError`, don't invent a new error type"
//!   discipline, carried over from task 6.1 even though 6.1's own
//!   Requirements are 7.x).
//! - `RequestUriContext` has no precedent anywhere in this repo (confirmed:
//!   `grep -rn "X-Forwarded\|forwarded" src/` finds only a forward-looking
//!   comment in `src/oauth/authorize_endpoint.rs` pointing at this task).
//!   It is built here as [`ForwardedOrigin`] (scheme/host resolution from
//!   plain `Option<&str>` header values — axum-agnostic and unit-testable
//!   without a live request, per this task's brief) plus
//!   [`RequestUriContext`] (adds the request path and any extra query
//!   params to preserve, e.g. `limit`). A thin axum extractor that pulls
//!   `Host`/`X-Forwarded-Proto`/`X-Forwarded-Host` and calls
//!   [`ForwardedOrigin::resolve`] is left to whichever endpoint spec first
//!   wires a live router (there is none in this spec, per the boundary note
//!   above).
//! - [`PageCursors`] is `Page<T>`'s next/prev cursor strings without `T`, so
//!   [`build_link_header`] doesn't need to be generic over the item type —
//!   matching the sketch's separation of `Page<T>` (data) from
//!   `PageCursors` (what the header generator actually needs).
//!
//! [`StatusIdCursor`] is provided as the common-case [`Cursor`] impl (most
//! lists page by an entity's own snowflake-like id). Requirement 6.6's
//! "category-swappable, non-status-id cursor" claim is proven in
//! `tests.rs` with a second, structurally different `Cursor` impl — adding
//! it here in the main module would just be an unused example, not evidence
//! of pluggability.

#[cfg(test)]
mod tests;

use crate::error::AppError;
use axum::http::{HeaderValue, StatusCode};

/// Maximum number of items a single page may contain, regardless of the
/// requested `limit` (Requirement 6.5's "上限"). Mastodon's real API varies
/// this per endpoint, but does not define a single spec-wide number here
/// either; `40` is chosen as this toolkit's convention default because it
/// matches the cap Mastodon uses for the majority of its list endpoints
/// (e.g. `GET /api/v1/notifications`, `GET /api/v1/accounts/:id/statuses`).
/// A downstream endpoint spec that needs a different per-endpoint cap can
/// still clamp further itself before calling [`PageParams::parse`]; this
/// constant only fixes the toolkit-wide ceiling requirement 6.5 requires to
/// exist at all.
pub const MAX_LIMIT: u32 = 40;

/// Number of items returned when the caller does not specify `limit` at
/// all. Requirement 6.5 only mandates clamping an over-max request; it does
/// not require a default to exist. This toolkit still needs one for
/// [`PageParams::parse`] to always produce a usable `limit`, so `20` is
/// chosen to match Mastodon's common timeline default.
pub const DEFAULT_LIMIT: u32 = 20;

fn resolve_limit(requested: Option<u32>) -> u32 {
    match requested {
        None => DEFAULT_LIMIT,
        Some(n) => n.min(MAX_LIMIT),
    }
}

/// Raw cursor/limit query parameters as received on the wire, before
/// decoding into a concrete [`Cursor`] type (Requirements 6.2–6.5).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageParams {
    pub max_id: Option<String>,
    pub since_id: Option<String>,
    pub min_id: Option<String>,
    pub limit: Option<u32>,
}

impl PageParams {
    /// Decodes the raw string cursor params into typed `C` values and
    /// resolves `limit` (default when absent, clamped to [`MAX_LIMIT`] when
    /// present). A malformed cursor string is rejected via `C::decode`'s
    /// [`AppError`] (Requirement 6.6's "category-swappable" abstraction
    /// applied at the parsing boundary, and the "reuse `AppError`, don't
    /// invent a new error type" discipline from this spec's task 6.1).
    pub fn parse<C: Cursor>(&self) -> Result<ParsedPageParams<C>, AppError> {
        Ok(ParsedPageParams {
            max_id: self.max_id.as_deref().map(C::decode).transpose()?,
            since_id: self.since_id.as_deref().map(C::decode).transpose()?,
            min_id: self.min_id.as_deref().map(C::decode).transpose()?,
            limit: resolve_limit(self.limit),
        })
    }
}

/// [`PageParams`] after cursor decoding, ready to drive [`paginate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedPageParams<C: Cursor> {
    pub max_id: Option<C>,
    pub since_id: Option<C>,
    pub min_id: Option<C>,
    pub limit: u32,
}

/// A category-swappable cursor type (Requirement 6.6). Most lists page by
/// an entity's own id ([`StatusIdCursor`]), but a category whose cursor
/// isn't the target entity's id (e.g. a bookmark/favourite join row, or a
/// notification group key) can supply its own `Cursor` impl and use the
/// same [`PageParams::parse`] / [`paginate`] / [`build_link_header`]
/// pipeline unchanged.
///
/// `Ord` is required (beyond design.md's sketch, which only required
/// `Sized`): [`paginate`] selects items by comparing cursor values against
/// the requested bounds, so a pluggable cursor must be orderable in
/// whatever sense "older"/"newer" means for its category, not merely
/// opaque.
pub trait Cursor: Sized + Clone + Ord {
    /// Renders this cursor as the opaque string carried in `Link` header
    /// query parameters and (conceptually) in `max_id`/`since_id`/`min_id`.
    fn encode(&self) -> String;

    /// Parses a raw cursor string back into `Self`. Malformed input is a
    /// caller error (422-ish), not a panic or a silently-ignored cursor.
    fn decode(raw: &str) -> Result<Self, AppError>;
}

/// The common-case [`Cursor`]: an unsigned 64-bit id, as used by
/// statuses/timelines and most other Mastodon-compatible list endpoints.
/// Larger values are newer (matches this project's snowflake-like id
/// convention: ids are monotonically increasing over time).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StatusIdCursor(pub u64);

impl Cursor for StatusIdCursor {
    fn encode(&self) -> String {
        self.0.to_string()
    }

    fn decode(raw: &str) -> Result<Self, AppError> {
        raw.parse::<u64>().map(StatusIdCursor).map_err(|_| {
            AppError::client(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("invalid cursor value: '{raw}'"),
            )
        })
    }
}

/// A page of items plus the opaque cursor strings a caller can hand back to
/// [`build_link_header`] to link to the adjacent page in each direction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    pub items: Vec<T>,
    /// Cursor of the newest (first) item in this page — used to build a
    /// `prev` link that catches up on anything newer without a gap
    /// (rendered as `min_id`, see [`build_link_header`]).
    pub prev_cursor: Option<String>,
    /// Cursor of the oldest (last) item in this page — used to build a
    /// `next` link that continues older (rendered as `max_id`).
    pub next_cursor: Option<String>,
}

impl<T> Page<T> {
    /// Extracts the next/prev cursor strings, decoupled from the item type
    /// `T`, for handing to [`build_link_header`].
    pub fn cursors(&self) -> PageCursors {
        PageCursors {
            next: self.next_cursor.clone(),
            prev: self.prev_cursor.clone(),
        }
    }
}

/// Selects the page of `pool` matching `params`'s cursor direction and
/// limit (Requirements 6.2–6.4).
///
/// `pool` must already be sorted **descending** by cursor (newest first) —
/// the same order a typical `ORDER BY id DESC` repository query returns.
/// `cursor_of` extracts each item's cursor value.
///
/// Direction semantics:
/// - Only `max_id` (or no cursor at all): items older than `max_id` (or all
///   items, if none), newest-of-that-set first — Requirement 6.2.
/// - `since_id` set (optionally combined with `max_id` as an extra upper
///   bound): items newer than `since_id`, anchored at the newest end — if
///   more than `limit` such items exist, the ones closest to "now" win and
///   older ones within the window are dropped (Requirement 6.4's "先頭（最新）
///   を固定した向き").
/// - `min_id` set instead of `since_id` (optionally combined with `max_id`):
///   items newer than `min_id`, anchored at the *oldest* end of that
///   window — if more than `limit` such items exist, the ones closest to
///   `min_id` win, so repeated calls can walk forward without skipping any
///   (Requirement 6.3's "古い方から進む向き"). This is the behavioral
///   difference from `since_id` that Requirement 6.4 requires tests to
///   prove, not just that both return "newer" items.
/// - If both `since_id` and `min_id` are present, `since_id` takes
///   precedence (deterministic, documented choice — the two are mutually
///   exclusive directions and Mastodon's own API does not define combined
///   behavior for supplying both).
pub fn paginate<T, C, F>(pool: &[T], cursor_of: F, params: &ParsedPageParams<C>) -> Page<T>
where
    T: Clone,
    C: Cursor,
    F: Fn(&T) -> C,
{
    let anchor_oldest = params.since_id.is_none() && params.min_id.is_some();
    let lower = params.since_id.as_ref().or(params.min_id.as_ref());
    let upper = params.max_id.as_ref();

    let filtered: Vec<&T> = pool
        .iter()
        .filter(|item| {
            let cursor = cursor_of(item);
            let above_lower = lower.is_none_or(|lo| cursor > *lo);
            let below_upper = upper.is_none_or(|hi| cursor < *hi);
            above_lower && below_upper
        })
        .collect();

    let limit = params.limit as usize;
    let selected: Vec<&T> = if anchor_oldest {
        let start = filtered.len().saturating_sub(limit);
        filtered[start..].to_vec()
    } else {
        filtered.into_iter().take(limit).collect()
    };

    let items: Vec<T> = selected.iter().map(|item| (*item).clone()).collect();
    let next_cursor = selected.last().map(|item| cursor_of(item).encode());
    let prev_cursor = selected.first().map(|item| cursor_of(item).encode());

    Page {
        items,
        prev_cursor,
        next_cursor,
    }
}

/// A [`Page`]'s next/prev cursor strings, independent of the item type
/// (Requirement 6.1's `Link` header only needs these, never the items
/// themselves).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageCursors {
    pub next: Option<String>,
    pub prev: Option<String>,
}

/// The external (client-visible) scheme and host a reverse proxy presents
/// to the client — as opposed to this process's own connection
/// scheme/host, which is what a naive implementation would otherwise use
/// (Requirement 6.7).
///
/// Deliberately axum-agnostic: [`ForwardedOrigin::resolve`] takes plain
/// `Option<&str>` header values (not an axum `HeaderMap`/`Request`) so it
/// stays unit-testable without spinning up a real request. A thin axum
/// extractor, if one is added by a later spec's live router, is expected to
/// be a trivial wrapper: pull `Host`, `X-Forwarded-Proto`,
/// `X-Forwarded-Host` header values plus the connection's own scheme, and
/// call this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardedOrigin {
    pub scheme: String,
    pub host: String,
}

impl ForwardedOrigin {
    /// Resolves the external scheme/host: prefers `X-Forwarded-Proto` /
    /// `X-Forwarded-Host` when present and non-empty, otherwise falls back
    /// to the connection's own `fallback_scheme` / `fallback_host` (e.g.
    /// this process's listener scheme and the `Host` header).
    ///
    /// A forwarded header may carry a comma-separated chain when multiple
    /// proxies are involved (`X-Forwarded-Proto: https, http`); per
    /// convention the first (client-nearest) entry is authoritative, so
    /// only it is used.
    pub fn resolve(
        fallback_scheme: &str,
        fallback_host: &str,
        forwarded_proto: Option<&str>,
        forwarded_host: Option<&str>,
    ) -> Self {
        ForwardedOrigin {
            scheme: first_forwarded_value(forwarded_proto)
                .unwrap_or(fallback_scheme)
                .to_string(),
            host: first_forwarded_value(forwarded_host)
                .unwrap_or(fallback_host)
                .to_string(),
        }
    }
}

/// Extracts the first, client-nearest value from a (possibly
/// comma-separated, possibly absent, possibly blank) forwarded header
/// value.
fn first_forwarded_value(raw: Option<&str>) -> Option<&str> {
    raw.and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// Everything [`build_link_header`] needs to render absolute `Link` URLs:
/// the resolved external origin, the request path, and any non-cursor
/// query parameters to preserve across pages (e.g. `limit`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestUriContext {
    origin: ForwardedOrigin,
    path: String,
    extra_query: Vec<(String, String)>,
}

impl RequestUriContext {
    pub fn new(origin: ForwardedOrigin, path: impl Into<String>) -> Self {
        RequestUriContext {
            origin,
            path: path.into(),
            extra_query: Vec::new(),
        }
    }

    /// Adds a query parameter (e.g. `limit`) to preserve on every
    /// generated link.
    pub fn with_query(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra_query.push((key.into(), value.into()));
        self
    }

    fn url_with(&self, cursor_param: &str, cursor_value: &str) -> String {
        let mut url = format!("{}://{}{}", self.origin.scheme, self.origin.host, self.path);
        let mut pairs: Vec<(&str, &str)> = self
            .extra_query
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        pairs.push((cursor_param, cursor_value));
        for (index, (key, value)) in pairs.iter().enumerate() {
            url.push(if index == 0 { '?' } else { '&' });
            url.push_str(&percent_encode_query(key));
            url.push('=');
            url.push_str(&percent_encode_query(value));
        }
        url
    }
}

/// Minimal RFC 3986 percent-encoding for a query key/value component. No
/// `url`-crate dependency exists in this repo (`Cargo.toml` checked), and
/// cursor values are simple enough (numeric ids, opaque tokens) that a
/// small self-contained encoder is preferable to adding a new dependency
/// for this alone.
fn percent_encode_query(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            _ => {
                out.push('%');
                out.push_str(&format!("{byte:02X}"));
            }
        }
    }
    out
}

/// Builds the `Link` header value (Requirement 6.1) with absolute URLs
/// respecting the reverse-proxy-forwarded host/scheme in `ctx`
/// (Requirement 6.7). `next` is rendered as a `max_id` link (continue
/// older); `prev` is rendered as a `min_id` link (catch up on newer items
/// without a gap — see [`paginate`]'s doc comment on why `min_id`, not
/// `since_id`, is the gap-safe choice for a `prev` link that may be
/// followed repeatedly).
///
/// Returns `None` when there is nothing to link (no cursors at all, e.g. an
/// empty result set) rather than an empty header value.
pub fn build_link_header(ctx: &RequestUriContext, cursors: &PageCursors) -> Option<HeaderValue> {
    let mut parts = Vec::with_capacity(2);
    if let Some(next) = &cursors.next {
        parts.push(format!("<{}>; rel=\"next\"", ctx.url_with("max_id", next)));
    }
    if let Some(prev) = &cursors.prev {
        parts.push(format!("<{}>; rel=\"prev\"", ctx.url_with("min_id", prev)));
    }
    if parts.is_empty() {
        return None;
    }
    HeaderValue::from_str(&parts.join(", ")).ok()
}
