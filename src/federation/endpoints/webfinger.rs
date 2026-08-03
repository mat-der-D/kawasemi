//! `webfinger` handler (design.md "Endpoints（handlers）"; Requirements
//! 4.1, 4.2, 4.3, 4.4, 4.5; task 5.1, `Boundary: webfinger`): resolves an
//! `acct:user@domain` WebFinger query to a local actor's `self` link, or
//! reports it as unresolved/not-found.
//!
//! Per design.md's Responsibilities for this handler: "`acct:` を解析し自
//! ドメイン照合（4.3）、`ActorDirectory::resolve_actor_by_handle`（owner 非
//! 露出, 4.5）で複数アクター解決（4.2）、self link を JRD で返す（4.1）、
//! 不在は未検出（4.4）".
//!
//! ## Not wired into a router (task 5.4's job)
//! Mirrors api-foundation's `src/oauth/apps_endpoint.rs` precedent (same
//! reasoning restated here): [`webfinger`] is a plain axum handler
//! (`State<WebfingerState>`, `Query<WebfingerQuery>`) shaped so task 5.4 can
//! mount it with `.route(...).with_state(...)` verbatim, but nothing in this
//! crate currently does so. This module's own tests
//! (`src/federation/endpoints/webfinger/tests.rs`) call it directly as an
//! ordinary async function; `tests/webfinger_nodeinfo_it.rs` additionally
//! mounts it on a minimal, test-local `axum::Router` (per this task's own
//! instructions, since no prior endpoint-shaped task exists yet in this
//! spec to establish precedent either way) to prove the full HTTP-observable
//! behavior (status codes, `Content-Type`, JRD body).
//!
//! ## `WebfingerState`: domain carried separately from `ActorUrls`
//! `ActorUrls` (task 1.3) does not expose its own `domain` field (private,
//! no accessor) -- adding one would mean editing `src/federation/urls.rs`,
//! outside this task's boundary. [`WebfingerState`] therefore carries
//! `domain` as its own field alongside `urls: ActorUrls`, duplicating the
//! one string `ActorUrls` already holds internally -- the same "hold a
//! narrow duplicate rather than widen an already-reviewed component's
//! public surface" judgment call `AppsEndpointState` documents for its own
//! `pool`/`token_hash_key` fields.
//!
//! ## Domain match is case-insensitive
//! Requirement 4.3 does not specify case sensitivity; domain names are
//! conventionally case-insensitive (DNS), so the acct resource's domain
//! part is compared against the configured instance domain via
//! `eq_ignore_ascii_case`.
//!
//! ## Error mapping: 400 vs. 404
//! design.md's API Contract table names both `400` and `404` as possible
//! errors for this endpoint without pinning which condition maps to which.
//! This handler draws the line as: a `resource` value that is not even a
//! well-formed `acct:user@domain` string is a malformed *request* (`400`);
//! a well-formed `acct:` resource whose domain does not match this
//! instance (Requirement 4.3), or whose user segment does not resolve to a
//! known local actor (Requirement 4.4) -- including a user segment that is
//! not even a valid [`Handle`], which by construction can never name a real
//! actor -- is treated identically as "not resolved" (`404`), so a prober
//! cannot distinguish "wrong domain" from "unknown handle" from "invalid
//! handle syntax" by status code alone.

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::Response;
use serde::{Deserialize, Serialize};

use crate::actor::{ActorDirectory, Handle};
use crate::error::AppError;
use crate::federation::urls::ActorUrls;

/// ActivityPub's primary media type, used as this JRD's `self` link `type`
/// (Requirement 4.1). Kept as a private literal here (mirrors
/// `jsonld.rs`'s own private `ACTIVITY_JSON_MEDIA_TYPE` constant, not
/// exported from that module) rather than introducing a new cross-module
/// dependency for one string literal.
const ACTIVITY_JSON_MEDIA_TYPE: &str = "application/activity+json";

/// The WebFinger JRD (JSON Resource Descriptor) media type (RFC 7033
/// section 10.2), returned as this endpoint's `Content-Type`.
const JRD_MEDIA_TYPE: &str = "application/jrd+json";

/// Everything [`webfinger`] needs, bundled behind one
/// `axum::extract::State`-compatible handle (mirrors `AppsEndpointState`'s
/// established shape for a not-yet-wired endpoint).
#[derive(Clone)]
pub struct WebfingerState {
    /// Resolves a local actor by handle, owner-non-exposing (Requirement
    /// 4.5). `Arc`-wrapped so this state stays cheaply `Clone`.
    pub directory: Arc<ActorDirectory>,
    /// Builds the resolved actor's ActivityPub actor URL for the `self`
    /// link (Requirement 4.1).
    pub urls: ActorUrls,
    /// This instance's own configured domain (`ServerConfig::domain`),
    /// matched against the acct resource's domain part (Requirement 4.3).
    /// See this module's doc comment ("`WebfingerState`: domain carried
    /// separately...") for why this duplicates a value `urls` also holds.
    pub domain: String,
}

/// `GET /.well-known/webfinger` query parameters (RFC 7033): only
/// `resource` is used by this handler.
#[derive(Debug, Clone, Deserialize)]
pub struct WebfingerQuery {
    pub resource: String,
}

/// One JRD `links` entry (RFC 7033 section 4.4.4.1).
#[derive(Debug, Clone, Serialize)]
struct JrdLink {
    rel: String,
    #[serde(rename = "type")]
    media_type: String,
    href: String,
}

/// A JSON Resource Descriptor document (RFC 7033 section 4.4).
#[derive(Debug, Clone, Serialize)]
struct JrdDocument {
    subject: String,
    links: Vec<JrdLink>,
}

/// Resolves a WebFinger `acct:` query to a local actor's JRD `self` link
/// (`GET /.well-known/webfinger?resource=acct:user@domain`, design.md's API
/// Contract, Requirements 4.1-4.5). See this module's doc comment ("Error
/// mapping") for the 400-vs-404 split.
pub async fn webfinger(
    State(state): State<WebfingerState>,
    Query(query): Query<WebfingerQuery>,
) -> Result<Response, AppError> {
    let (user, domain) = parse_acct_resource(&query.resource).ok_or_else(|| {
        AppError::client(
            StatusCode::BAD_REQUEST,
            "resource must be an acct: URI of the form acct:user@domain",
        )
    })?;

    if !domain.eq_ignore_ascii_case(&state.domain) {
        // Requirement 4.3: a non-matching domain is never resolved as this
        // instance's actor.
        return Err(not_found());
    }

    // A user segment that fails `Handle` validation cannot name a real
    // local actor (actor creation only ever accepts valid handles) -- treat
    // it identically to "unknown handle" (Requirement 4.4) rather than
    // leaking handle-syntax validation as a distinct error.
    let handle = Handle::new(user).map_err(|_| not_found())?;

    let resolved = state
        .directory
        .resolve_actor_by_handle(&handle)
        .await?
        .ok_or_else(not_found)?;

    let actor_url = state.urls.actor_url(&resolved.handle);
    let document = JrdDocument {
        subject: query.resource.clone(),
        links: vec![JrdLink {
            rel: "self".to_string(),
            media_type: ACTIVITY_JSON_MEDIA_TYPE.to_string(),
            href: actor_url,
        }],
    };

    Ok(jrd_response(StatusCode::OK, &document))
}

/// Requirement 4.4's "未検出" (not found) response: a plain `404`, with no
/// distinction in the public message between "wrong domain" and "unknown
/// handle" (see this module's doc comment, "Error mapping").
fn not_found() -> AppError {
    AppError::client(StatusCode::NOT_FOUND, "webfinger resource not found")
}

/// Parses `resource` as an `acct:user@domain` URI, returning
/// `(user, domain)` borrowed from `resource` itself. Returns `None` for
/// anything else: missing `acct:` prefix, no `@` separator, or an empty
/// user/domain segment.
fn parse_acct_resource(resource: &str) -> Option<(&str, &str)> {
    let rest = resource.strip_prefix("acct:")?;
    let (user, domain) = rest.split_once('@')?;
    if user.is_empty() || domain.is_empty() {
        return None;
    }
    Some((user, domain))
}

/// Renders `document` as a JRD response (RFC 7033: `Content-Type:
/// application/jrd+json`).
fn jrd_response(status: StatusCode, document: &JrdDocument) -> Response {
    let body = serde_json::to_vec(document).expect("JrdDocument always serializes to JSON");
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, JRD_MEDIA_TYPE)
        .body(Body::from(body))
        .expect("a fixed status/header/JSON-body response is always well-formed")
}
