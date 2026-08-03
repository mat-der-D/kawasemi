//! `MediaEndpoints` (design.md "Media API / エンドポイント層" ->
//! "MediaEndpoints / MediaAttachmentSerializer", Requirements 1.1, 2.1, 2.2,
//! 2.3, 3.1, 3.2, 6.5, 9.1, 9.2, 9.3, 9.4; task 5.1, `Boundary:
//! MediaEndpoints`): the HTTP surface for the three endpoints design.md's
//! API Contract table names — `POST /api/v2/media` (upload), `GET
//! /api/v1/media/:id` (poll), `PUT /api/v1/media/:id` (metadata update) —
//! each requiring `write:media` (design.md: "取得にも `write:media` を要求
//! する", matching Mastodon's real behavior of not exposing a read-only
//! media scope), returning the response-code discipline design.md's System
//! Flows fix (`202`/`206`/`200`/`404`/`422`), and rendering every failure
//! through `AppError`'s already-wired Mastodon-compatible error body
//! (`crate::api::error::mastodon_error_body`, api-foundation task 6.1/7.1 —
//! reused unchanged via plain `?`, never a bespoke error body here).
//!
//! Scope: this module owns exactly the three axum handlers
//! ([`upload_media`], [`show_media`], [`update_media`]) plus the small,
//! self-contained pieces they need that do not exist anywhere else yet —
//! [`MediaEndpointsState`] (the narrow state bundle these handlers close
//! over) and [`ResolvedOrigin`] (a [`ForwardedOrigin`]-resolving axum
//! extractor). It reuses, never reimplements: `MediaService` (task 4.1) for
//! all business logic, `serializer::to_media_attachment` (task 4.2) for the
//! MediaAttachment JSON shape, and `crate::oauth::middleware`'s
//! `RequiredActor`/`require_scope` (api-foundation task 6.4) for
//! authentication/scope enforcement — this module defines no auth/scope
//! logic of its own. It does **not** touch `AppState`/`src/bootstrap.rs`/
//! `src/server.rs`/`src/config.rs` (task 5.2's job, `_Depends: 5.1_`): no
//! route is mounted on the live application router by this task.
//!
//! ## Test-local router, not a `tests/*_it.rs` full-app integration test
//! (precedent: `tests/webfinger_nodeinfo_it.rs`, federation-core task 5.1)
//! Nothing wires these handlers into `crate::server::build_router`/
//! `AppState` yet (task 5.2's job), so — mirroring
//! `tests/webfinger_nodeinfo_it.rs`'s own documented precedent for the
//! *exact* same "endpoint implemented, router wiring not yet landed"
//! situation in federation-core's task 5.1 — this task's own integration
//! coverage (`tests/media_endpoints_it.rs`) builds a minimal, test-local
//! `axum::Router<MediaEndpointsState<LocalFsStore>>` mounting just these
//! three handlers, driven via `tower::ServiceExt::oneshot` against a real,
//! `spawn_test_app`-backed Postgres schema (token issuance/resolution and
//! media persistence both genuinely need the database — nothing here is
//! faked). `tests/media_upload_it.rs`/`media_poll_it.rs`/
//! `media_update_it.rs` (design.md's File Structure Plan) are task 6.1's
//! own, separate, `_Depends: 5.2_` files proving the same behavior through
//! the *real* mounted application router once task 5.2 lands — this task
//! does not preempt those filenames.
//!
//! ## `MediaEndpointsState<S>`: a router-local state bundle, not `AppState`
//! `crate::state::AppState` cannot hold a `MediaService<S>` yet (task 5.2's
//! job — see that task's own boundary note), so — mirroring
//! `apps_endpoint.rs`'s/`oauth/middleware.rs`'s established precedent of
//! defining a small, self-contained `*State`/`AuthState` bundle for exactly
//! what a not-yet-wired handler group needs — [`MediaEndpointsState`] holds
//! an `Arc<MediaService<S>>` (shared, since `MediaService` itself is not
//! `Clone` — it holds a `PgPool`/`RuntimeContext`/`MediaConfig`/`S`, and
//! nothing requires re-deriving one per request), a bare `S` (the same
//! store `MediaService` was built with, needed again here only because
//! `MediaAttachmentSerializer::to_media_attachment` takes a store reference
//! directly rather than routing through `MediaService`), and an
//! [`AuthState`] (`&PgPool` + `TokenHashKey`, api-foundation task 6.4) for
//! [`RequiredActor`] extraction. `impl FromRef<MediaEndpointsState<S>> for
//! AuthState` is what lets `RequiredActor`'s existing `where AuthState:
//! FromRef<S>` bound (`middleware.rs`) resolve against this router's state
//! type without any change to `middleware.rs` itself. Task 5.2 is
//! responsible for constructing a `MediaEndpointsState<LocalFsStore>` from
//! whatever `AppState` ends up holding and mounting these three handlers on
//! `Router<AppState>` via the same `FromRef` promotion technique
//! `middleware.rs`'s own doc comment describes.
//!
//! ## `ResolvedOrigin`: this crate's first live axum extractor for
//! `ForwardedOrigin` (judgment call — no prior implementation to reuse)
//! `crate::api::pagination`'s own doc comment says as much: "[`ForwardedOrigin::resolve`]
//! takes plain `Option<&str>` header values... A thin axum extractor... is
//! left to whichever endpoint spec first wires a live router (there is none
//! in this spec)." No endpoint anywhere in this crate has built one yet
//! (confirmed: `grep -rn "ForwardedOrigin" src/` finds only `pagination.rs`
//! itself and this module). [`ResolvedOrigin`] is exactly that thin
//! wrapper: it reads `Host`, `X-Forwarded-Proto`, `X-Forwarded-Host` from
//! the request's headers and calls `ForwardedOrigin::resolve`, using
//! `"http"` as the fallback scheme — this process's own listener always
//! speaks plain HTTP internally (design.md's whole point is that
//! `X-Forwarded-Proto` from a reverse proxy is what carries `https`
//! externally); there is no TLS-terminated connection-info extractor
//! anywhere in this crate to draw a "real" fallback scheme from instead.
//! Never rejects (`Rejection = Infallible`): a request with no `Host`
//! header at all (malformed, but not this endpoint's failure mode to
//! diagnose) degrades to a fixed `"localhost"` fallback host rather than
//! ever failing the request over an origin-resolution detail.
//!
//! ## CONCERN for task 5.2: axum's built-in 2MB body-limit default vs.
//! `MediaConfig::max_upload_size_bytes`
//! `axum::extract::Multipart::from_request` (axum 0.8.9) calls
//! `RequestExt::with_limited_body()` internally, which applies
//! `axum_core`'s hard-coded default 2MB request-body cap *unless* a
//! `DefaultBodyLimit` layer overrides it on the router the handler is
//! mounted on. This module's own `upload_media` handler validates against
//! `MediaConfig::max_upload_size_bytes` (default 10 MiB, `config.rs`)
//! *after* `Multipart` has already parsed the body — so, as currently
//! written, a legitimate upload between 2MB and the configured maximum
//! would be rejected by axum's own built-in limit (a plain-text 413, not a
//! Mastodon-compatible error body) before this module's validation logic
//! ever runs. This module's own test-local router
//! (`tests/media_endpoints_it.rs`) does not surface this gap because its
//! fixture uploads stay well under 2MB — the gap is real but latent.
//! Fixing it requires adding `.layer(DefaultBodyLimit::max(config.media.max_upload_size_bytes
//! as usize))` (or similar) to whatever router actually mounts
//! `upload_media` — squarely task 5.2's job (router construction is out of
//! this task's `_Boundary: MediaEndpoints_`), not something this task's own
//! handler code can fix by itself. Flagged here explicitly per this task's
//! own protocol ("any newly introduced runtime-sensitive... assumption...
//! called out explicitly").
//!
//! ## Wire shapes not specified by design.md's API Contract table
//! (judgment calls)
//! - **`focus` wire format**: design.md's API Contract table names `focus`
//!   as a request field on both `POST /api/v2/media` and `PUT
//!   /api/v1/media/:id` but does not fix its wire shape. Mirroring
//!   Mastodon's real API convention (a single `"x,y"` string, e.g.
//!   `"-0.5,0.3"`), [`parse_focus_param`] is the one shared parser both
//!   handlers use — malformed input (wrong field count, non-numeric) is a
//!   `422` [`AppError`], matching Requirement 7.4's rejection discipline
//!   (the *range* check itself still happens once, inside `MediaService`,
//!   via `Focus::new` — this parser only turns the wire string into a raw
//!   `(f32, f32)` pair, exactly what `UploadInput`/`MetadataPatch` already
//!   expect per `service.rs`'s own documented contract).
//! - **`PUT` request body shape**: design.md's table lists `description`,
//!   `focus` as `PUT /api/v1/media/:id`'s request fields without fixing
//!   `multipart/form-data` vs. JSON. [`update_media`] accepts a JSON body
//!   ([`UpdateMediaRequest`]) — unlike upload, an update never carries file
//!   bytes, so there is no multipart-specific reason to prefer form
//!   encoding, and a typed JSON body is this crate's prevailing idiom for
//!   every other non-file-upload write endpoint precedent
//!   (`apps_endpoint.rs`/`token_endpoint.rs`).
//! - **media-id path segment**: neither design.md nor any existing endpoint
//!   in this crate parses an `Id` out of an axum `Path` segment yet
//!   (`grep -rn "Path<" src/` finds only `Path<String>` precedents, e.g.
//!   `federation/endpoints/ap_get.rs`'s handle segment). [`parse_media_id`]
//!   follows that same `Path<String>` + manual-parse precedent rather than
//!   `Path<Id>` (whose `Deserialize` impl expects a decimal-string-shaped
//!   deserializer input and has no existing axum-`Path` precedent in this
//!   crate to confirm compatibility against). A path segment that fails to
//!   parse as a decimal `i64` is treated as `404` (Requirement 2.3's
//!   "存在しない...未検出", the same status a syntactically valid but
//!   nonexistent id already produces) rather than `422` — from the caller's
//!   perspective an unparseable id and a nonexistent one are
//!   indistinguishable "this resource is not here" outcomes, and Mastodon's
//!   real API does not carve out a separate status for a malformed id
//!   segment either.
//!
//! ## `GET`'s `Failed` branch never reaches the serializer (Requirement 6.5)
//! design.md's "処理状態のポーリング取得" flowchart routes `state == failed`
//! straight to `Err[422 mastodon error body]`, never through the
//! `MediaAttachment` representation at all — [`show_media`] mirrors that
//! exactly: the `Failed` arm returns a plain `AppError::client(422, ...)`,
//! never calling `to_media_attachment`. This is also why
//! `serializer.rs`'s own doc comment could already describe a `Failed`
//! media reaching it as "an edge case this module still handles safely
//! rather than panicking" instead of a real code path — this module is
//! confirmation that it is in fact never reached via `GET`.

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::extract::{FromRef, FromRequestParts, Multipart, Path, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Json, extract};
use serde::Deserialize;

use crate::api::pagination::ForwardedOrigin;
use crate::domain::Id;
use crate::error::AppError;
use crate::media::model::MediaState;
use crate::media::serializer::to_media_attachment;
use crate::media::service::{MediaService, MetadataPatch, UploadInput};
use crate::media::store::MediaStore;
use crate::oauth::middleware::{AuthState, RequiredActor, require_scope};
use crate::oauth::scope::ScopeSet;

/// The one scope every media endpoint requires (Requirement 9.1; design.md:
/// "アップロード・取得・更新のいずれも `write:media` を要求する").
fn write_media_scope() -> ScopeSet {
    ScopeSet::parse("write:media").expect("\"write:media\" is a valid scope literal")
}

/// Caller-facing message for a media resource that either does not exist,
/// or exists but is not owned by the requesting actor (Requirements 2.3,
/// 2.4, 3.3) — deliberately identical wording for both cases (never leaking
/// which one applies), matching `MediaService::show_media`'s/
/// `update_metadata`'s own already-established "indistinguishable
/// `Ok(None)`" contract (`service.rs`).
const NOT_FOUND_MESSAGE: &str = "media not found";

fn not_found_error() -> AppError {
    AppError::client(StatusCode::NOT_FOUND, NOT_FOUND_MESSAGE)
}

/// Parses a `/api/v*/media/:id` path segment into an [`Id`]. See this
/// module's doc comment ("media-id path segment") for why an unparseable
/// segment is treated as `404`, not `422`.
fn parse_media_id(raw: &str) -> Result<Id, AppError> {
    raw.parse::<i64>()
        .map(Id::from_i64)
        .map_err(|_| not_found_error())
}

/// Parses a Mastodon-style `"x,y"` focus coordinate string into a raw,
/// not-yet-range-validated `(f32, f32)` pair (see this module's doc
/// comment, "`focus` wire format"). Range validation itself happens once,
/// inside `MediaService`, via `Focus::new` — this function only rejects a
/// wire value that cannot even be parsed as two numbers.
fn parse_focus_param(raw: &str) -> Result<(f32, f32), AppError> {
    let mut parts = raw.split(',');
    let x = parts.next().and_then(|s| s.trim().parse::<f32>().ok());
    let y = parts.next().and_then(|s| s.trim().parse::<f32>().ok());
    match (x, y, parts.next()) {
        (Some(x), Some(y), None) => Ok((x, y)),
        _ => Err(AppError::client(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("invalid focus value {raw:?}, expected \"x,y\""),
        )),
    }
}

/// Reads a single header's value as UTF-8, if present and valid (used only
/// for [`ResolvedOrigin`]'s best-effort header reads — never a rejection
/// path).
fn header_str(headers: &HeaderMap, name: axum::http::HeaderName) -> Option<&str> {
    headers.get(name)?.to_str().ok()
}

/// This crate's first live axum extractor for [`ForwardedOrigin`] — see
/// this module's doc comment ("`ResolvedOrigin`") for why none existed
/// before this task and why it never rejects.
pub struct ResolvedOrigin(pub ForwardedOrigin);

impl<S: Send + Sync> FromRequestParts<S> for ResolvedOrigin {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let headers = &parts.headers;
        let fallback_host = header_str(headers, header::HOST).unwrap_or("localhost");
        let forwarded_proto = header_str(
            headers,
            header::HeaderName::from_static("x-forwarded-proto"),
        );
        let forwarded_host =
            header_str(headers, header::HeaderName::from_static("x-forwarded-host"));
        Ok(ResolvedOrigin(ForwardedOrigin::resolve(
            "http",
            fallback_host,
            forwarded_proto,
            forwarded_host,
        )))
    }
}

/// The router-local state [`upload_media`]/[`show_media`]/[`update_media`]
/// close over — see this module's doc comment
/// ("`MediaEndpointsState<S>`") for why this is not `AppState`.
#[derive(Clone)]
pub struct MediaEndpointsState<S: MediaStore + Clone> {
    pub media_service: Arc<MediaService<S>>,
    pub store: S,
    pub auth: AuthState,
}

impl<S: MediaStore + Clone> FromRef<MediaEndpointsState<S>> for AuthState {
    fn from_ref(state: &MediaEndpointsState<S>) -> Self {
        state.auth.clone()
    }
}

/// A parsed `POST /api/v2/media` multipart body: the required `file` field
/// plus the optional `description`/`focus` fields (design.md's API
/// Contract table).
struct ParsedUpload {
    bytes: Vec<u8>,
    content_type: String,
    description: Option<String>,
    focus: Option<(f32, f32)>,
}

/// Consumes `multipart` field by field, extracting the shape
/// [`ParsedUpload`] needs. A `multer`-level parse failure (malformed
/// multipart framing) is reported as `422` (input validation failure,
/// Requirement 1.3's spirit — the request is not a well-formed upload
/// attempt) rather than propagated as axum's own rejection type, so this
/// still renders through the Mastodon-compatible error body like every
/// other failure in this module.
async fn parse_upload_multipart(mut multipart: Multipart) -> Result<ParsedUpload, AppError> {
    let mut file_bytes: Option<Vec<u8>> = None;
    let mut file_content_type: Option<String> = None;
    let mut description: Option<String> = None;
    let mut focus: Option<(f32, f32)> = None;

    let multipart_error = |err: extract::multipart::MultipartError| {
        AppError::client(StatusCode::UNPROCESSABLE_ENTITY, err.to_string())
    };

    while let Some(field) = multipart.next_field().await.map_err(multipart_error)? {
        match field.name().unwrap_or("") {
            "file" => {
                let content_type = field
                    .content_type()
                    .unwrap_or("application/octet-stream")
                    .to_string();
                let data = field.bytes().await.map_err(multipart_error)?;
                file_bytes = Some(data.to_vec());
                file_content_type = Some(content_type);
            }
            "description" => {
                description = Some(field.text().await.map_err(multipart_error)?);
            }
            "focus" => {
                let raw = field.text().await.map_err(multipart_error)?;
                focus = Some(parse_focus_param(&raw)?);
            }
            // Unrecognized fields are ignored rather than rejected, matching
            // Mastodon's own tolerant multipart handling (e.g. clients that
            // also send a `thumbnail` field design.md's own API Contract
            // table lists as optional and unused by this MVP).
            _ => {}
        }
    }

    let (bytes, content_type) = match (file_bytes, file_content_type) {
        (Some(bytes), Some(content_type)) => (bytes, content_type),
        _ => {
            return Err(AppError::client(
                StatusCode::UNPROCESSABLE_ENTITY,
                "multipart upload is missing the required \"file\" field",
            ));
        }
    };

    Ok(ParsedUpload {
        bytes,
        content_type,
        description,
        focus,
    })
}

/// `POST /api/v2/media` (design.md's API Contract table): accepts a
/// multipart upload (`write:media` required), returning `202` with the
/// still-`processing` `MediaAttachment` representation (`url` null,
/// Requirement 1.1) once `MediaService::accept_upload` has stored the
/// original and enqueued its processing job. `Multipart` is deliberately
/// the last parameter (axum's own documented extractor-ordering
/// requirement: an extractor that consumes the request body must come
/// last).
pub async fn upload_media<S>(
    State(state): State<MediaEndpointsState<S>>,
    RequiredActor(ctx): RequiredActor,
    ResolvedOrigin(origin): ResolvedOrigin,
    multipart: Multipart,
) -> Result<Response, AppError>
where
    S: MediaStore + Clone + Send + Sync + 'static,
{
    require_scope(&ctx, &write_media_scope())?;

    let parsed = parse_upload_multipart(multipart).await?;
    let input = UploadInput {
        bytes: parsed.bytes,
        content_type: parsed.content_type,
        description: parsed.description,
        focus: parsed.focus,
    };

    let media = state
        .media_service
        .accept_upload(ctx.actor_id, input)
        .await?;
    let body = to_media_attachment(&media, &state.store, &origin);

    Ok((StatusCode::ACCEPTED, Json(body)).into_response())
}

/// `GET /api/v1/media/:id` (design.md's API Contract table / "処理状態のポ
/// ーリング取得" flow): owner-scoped status lookup (`write:media` required,
/// Requirement 9.1) returning `206` while still `processing`, `200` once
/// `ready`, `404` for a nonexistent or not-owned media (Requirements 2.3,
/// 2.4), and `422` with the Mastodon-compatible `{"error": ...}` body for a
/// `failed` media (Requirement 6.5) — see this module's doc comment ("`GET`'s
/// `Failed` branch") for why that last branch never reaches the serializer.
pub async fn show_media<S>(
    State(state): State<MediaEndpointsState<S>>,
    RequiredActor(ctx): RequiredActor,
    ResolvedOrigin(origin): ResolvedOrigin,
    Path(id_raw): Path<String>,
) -> Result<Response, AppError>
where
    S: MediaStore + Clone + Send + Sync + 'static,
{
    require_scope(&ctx, &write_media_scope())?;

    let media_id = parse_media_id(&id_raw)?;
    let media = state
        .media_service
        .show_media(ctx.actor_id, media_id)
        .await?
        .ok_or_else(not_found_error)?;

    match media.state {
        MediaState::Processing => {
            let body = to_media_attachment(&media, &state.store, &origin);
            Ok((StatusCode::PARTIAL_CONTENT, Json(body)).into_response())
        }
        MediaState::Ready => {
            let body = to_media_attachment(&media, &state.store, &origin);
            Ok((StatusCode::OK, Json(body)).into_response())
        }
        MediaState::Failed => Err(AppError::client(
            StatusCode::UNPROCESSABLE_ENTITY,
            "media processing failed",
        )),
    }
}

/// `PUT /api/v1/media/:id` request body (see this module's doc comment,
/// "`PUT` request body shape"). Both fields are optional and independently
/// applied — `None` means "leave unchanged", mirroring
/// `MetadataPatch`'s/`media_repository::update_metadata`'s own established
/// patch semantics (`service.rs`'s doc comment).
#[derive(Debug, Deserialize)]
pub struct UpdateMediaRequest {
    #[serde(default)]
    pub description: Option<String>,
    /// `"x,y"` string (see this module's doc comment, "`focus` wire
    /// format"), e.g. `"-0.5,0.3"`.
    #[serde(default)]
    pub focus: Option<String>,
}

/// `PUT /api/v1/media/:id` (design.md's API Contract table): owner-scoped
/// description/focus update (`write:media` required), accepted even while
/// the media is still `processing` (Requirement 3.4, unconditionally
/// delegated to `MediaService::update_metadata`, which never filters on
/// state). `200` with the updated representation on success; `422` for an
/// out-of-range or malformed `focus` (Requirement 3.2); `404` for a
/// nonexistent or not-owned media (Requirement 3.3). `Json<UpdateMediaRequest>`
/// is deliberately the last parameter (body-consuming extractor).
pub async fn update_media<S>(
    State(state): State<MediaEndpointsState<S>>,
    RequiredActor(ctx): RequiredActor,
    ResolvedOrigin(origin): ResolvedOrigin,
    Path(id_raw): Path<String>,
    Json(body): Json<UpdateMediaRequest>,
) -> Result<Response, AppError>
where
    S: MediaStore + Clone + Send + Sync + 'static,
{
    require_scope(&ctx, &write_media_scope())?;

    let media_id = parse_media_id(&id_raw)?;
    let focus = body.focus.as_deref().map(parse_focus_param).transpose()?;
    let patch = MetadataPatch {
        description: body.description,
        focus,
    };

    let media = state
        .media_service
        .update_metadata(ctx.actor_id, media_id, patch)
        .await?
        .ok_or_else(not_found_error)?;

    let response_body = to_media_attachment(&media, &state.store, &origin);
    Ok((StatusCode::OK, Json(response_body)).into_response())
}
