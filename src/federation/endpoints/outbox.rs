//! `outbox` handler (design.md File Structure Plan `outbox.rs` "outbox
//! GET（順序付きコレクション・ページング。収録項目は `OutboxSource` レジス
//! トリから収集）"; design.md's `#### Endpoints` Responsibilities: "outbox:
//! `build_outbox_page` を返す（8.x）。収録項目は `OutboxSource` レジストリ
//! から収集し、下流未登録の間は空コレクションを返す。"; Requirements 8.1,
//! 8.2, 9.4; task 5.2, `Boundary: outbox`): the `application/activity+json`
//! GET path for a local actor's outbox, paged.
//!
//! ## Not wired into a router (task 5.4's job)
//! Mirrors `ap_get.rs`'s identical reasoning (itself mirroring task 5.1's
//! `webfinger.rs`/`nodeinfo.rs` precedent): [`outbox_get`] is a plain axum
//! handler shaped for `.route(...).with_state(...)`, exercised by this
//! module's own unit tests (`src/federation/endpoints/outbox/tests.rs`) and,
//! for full HTTP-observable behavior, by `tests/ap_get_outbox_it.rs`'s
//! test-local `axum::Router`. Task 5.4 is expected to mount it at
//! [`crate::federation::urls::ActorUrls::outbox_url`]'s shape
//! (`/users/{handle}/outbox`).
//!
//! ## No authorized-fetch gate here — deliberately, per design.md's own API
//! Contract table
//! `ap_get.rs`'s actor/object-URL rows in design.md's `## Endpoints` ->
//! API Contract table both list `401(secure)` among their documented error
//! responses; the `{outbox_url}` row lists only `404, 406` — `401(secure)`
//! is conspicuously absent from that one row. Requirement 6.4 (the only
//! authorized-fetch requirement this spec has) is itself scoped to
//! Requirement 6 ("アクター・オブジェクト・コレクションの ActivityPub
//! GET"); outbox publication is its own separate Requirement 8 with no such
//! clause. This handler therefore performs content negotiation (Requirement
//! 9.4) but never calls into `SignatureVerifier` at all — unlike
//! `ap_get.rs`'s [`crate::federation::endpoints::ap_get::ApGetState`], this
//! module's own state has no `secure_mode`/verifier field to omit in the
//! first place, not merely one left unused.
//!
//! ## No actor-existence check
//! [`ActivityPubDocumentBuilder::build_outbox_page`] takes any
//! [`Handle`], never checking whether a local actor is actually registered
//! under it — [`OutboxSourceRegistry::collect`] (task 3.5) is safe for any
//! handle, always returning `Ok(vec![])` while nothing is registered
//! (exactly this task's own completion condition: "outbox は空コレクション
//! になる"). Wiring an `ActorDirectory` existence check into this handler
//! purely to turn "outbox of an unregistered handle" into `404` would add a
//! dependency design.md's own Responsibilities bullet for this handler does
//! not mention (contrast `ap_get.rs`'s `actor_get`, which *does* need
//! `ActorDirectory` because it is the source of the actor document's own
//! fields, not merely an existence check) — a syntactically invalid handle
//! segment is still rejected (`404`, mirrors `webfinger.rs`'s "invalid
//! syntax == unknown" convention), but a syntactically valid, unregistered
//! one gets the same empty-collection response as a real actor with no
//! registered `OutboxSource` contributions yet.
//!
//! ## `page` query parameter: the inverse of `document.rs`'s private
//! `page_url`
//! [`ActivityPubDocumentBuilder::build_outbox_page`]'s own page `id`/`next`
//! fields are built by `document.rs`'s private `page_url` helper:
//! `{outbox_url}?page=true` for the head page (no cursor yet), or
//! `{outbox_url}?page=<token>` for a continuation. That helper is not
//! `pub`, so this module — the one place a `?page=` query string is ever
//! parsed back into a [`PageCursor`] — reimplements its inverse
//! independently here ([`cursor_from_query`]): a missing `page` parameter or
//! the literal value `"true"` both mean "start from the head"
//! ([`PageCursor::start`]); any other value is an opaque continuation token
//! ([`PageCursor::token`]), passed through to [`OutboxSourceRegistry`]
//! untouched (this crate's registries never interpret token contents — see
//! `document.rs`'s own doc comment, "`PageCursor`: not defined anywhere else
//! in this spec").

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;
use serde::Deserialize;
use serde_json::Value;

use crate::actor::Handle;
use crate::error::AppError;
use crate::federation::endpoints::document::{ActivityPubDocumentBuilder, PageCursor};
use crate::federation::jsonld::accepts_activitypub;

/// ActivityPub's primary media type, this endpoint's response
/// `Content-Type` on success (Requirement 8.1). Mirrors `ap_get.rs`'s own
/// private literal for the same string.
const ACTIVITY_JSON_MEDIA_TYPE: &str = "application/activity+json";

/// Everything [`outbox_get`] needs, bundled behind one
/// `axum::extract::State`-compatible handle.
#[derive(Clone)]
pub struct OutboxState {
    /// Builds the outbox page container (task 3.6), consuming whatever
    /// `OutboxSource`s (task 3.5) are registered on it. `Arc`-wrapped so
    /// this state stays cheaply `Clone` despite `ActivityPubDocumentBuilder`
    /// itself not being `Clone`.
    pub document_builder: Arc<ActivityPubDocumentBuilder>,
}

/// `GET {outbox_url}` query parameters: only `page` is used by this handler
/// (Requirement 8.2's paging). See this module's doc comment
/// ("`page` query parameter") for how it maps to a [`PageCursor`].
#[derive(Debug, Clone, Deserialize, Default)]
pub struct OutboxQuery {
    pub page: Option<String>,
}

/// Requirement 6.6-equivalent "未検出" for this handler's one rejection
/// case: a syntactically invalid handle path segment (mirrors
/// `webfinger.rs`'s/`ap_get.rs`'s identical treatment).
fn not_found() -> AppError {
    AppError::client(StatusCode::NOT_FOUND, "outbox not found")
}

/// Requirement 9.4's content-negotiation gate, identical to `ap_get.rs`'s
/// own `require_ap_accept` (duplicated here rather than shared across a new
/// cross-file helper module — two call sites, one line of real logic each,
/// not worth a new indirection for).
fn require_ap_accept(headers: &HeaderMap) -> Result<(), AppError> {
    let accept = headers
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if accepts_activitypub(accept) {
        Ok(())
    } else {
        Err(AppError::client(
            StatusCode::NOT_ACCEPTABLE,
            "Accept header must request an ActivityPub representation \
             (application/activity+json or application/ld+json)",
        ))
    }
}

/// Maps an incoming `?page=` query value to a [`PageCursor`] (see this
/// module's doc comment, "`page` query parameter").
fn cursor_from_query(page: Option<String>) -> PageCursor {
    match page {
        None => PageCursor::start(),
        Some(token) if token == "true" => PageCursor::start(),
        Some(token) => PageCursor::token(token),
    }
}

/// Renders `document` as an `application/activity+json` response
/// (Requirement 8.1). Mirrors `ap_get.rs`'s own `activity_json_response`.
fn activity_json_response(document: Value) -> Response {
    let body = serde_json::to_vec(&document).expect("an outbox page always serializes to JSON");
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, ACTIVITY_JSON_MEDIA_TYPE)
        .body(Body::from(body))
        .expect("a fixed status/header/JSON-body response is always well-formed")
}

/// `GET {outbox_url}` (Requirements 8.1, 8.2, 9.4): one page of a local
/// actor's outbox as an `OrderedCollectionPage`, content-negotiated. See
/// this module's doc comment for why this handler has no authorized-fetch
/// gate and no actor-existence check.
pub async fn outbox_get(
    State(state): State<OutboxState>,
    Path(handle_str): Path<String>,
    Query(query): Query<OutboxQuery>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    require_ap_accept(&headers)?;

    let handle = Handle::new(&handle_str).map_err(|_| not_found())?;
    let cursor = cursor_from_query(query.page);

    let document = state
        .document_builder
        .build_outbox_page(&handle, cursor)
        .await?;

    Ok(activity_json_response(document))
}
