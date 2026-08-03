//! `TimelineEndpoints` (design.md "API / エンドポイント層" ->
//! "#### TimelineEndpoints", design.md lines ~393-416; Requirements 1.1,
//! 1.6, 2.1, 2.3, 3.1, 4.1, 7.2, 9.1, 9.2, 9.3, 9.4; task 5.1, `Boundary:
//! TimelineEndpoints`): the three HTTP handlers design.md's API Contract
//! table names — [`home_timeline`] (`GET /api/v1/timelines/home`),
//! [`public_timeline`] (`GET /api/v1/timelines/public`, which also serves
//! the local timeline via `?local=true`, Mastodon-compatible — design.md's
//! own explicit note: "ローカルタイムラインは `GET
//! /api/v1/timelines/public?local=true`"), and [`tag_timeline`] (`GET
//! /api/v1/timelines/tag/:hashtag`) — applying api-foundation's
//! Bearer/optional-auth + scope discipline (home requires `read:statuses`
//! and 401s when unauthenticated, Requirements 1.1, 1.6, 9.1; public/local/
//! tag accept optional auth and fall back to public-only results when
//! unauthenticated, Requirements 2.1, 2.3, 3.1, 4.1, 9.2), rendering every
//! failure through the already-wired `AppError`/`mastodon_error_body`
//! conversion (Requirement 9.4, never a bespoke error body), and attaching a
//! `Link` header (api-foundation's pagination toolkit, Requirement 7.2) to
//! every response.
//!
//! Scope: this module owns exactly the three axum handlers above, plus
//! [`TimelineEndpointsState`] (the router-local state bundle these handlers
//! close over) and the small wire-shape helpers they need (scope-literal
//! constructor, loosely-typed boolean/limit query parsing, the tag
//! timeline's `any[]`/`all[]`/`none[]` repeated-query-parameter extraction).
//! It reuses, never reimplements: [`crate::timelines::service::TimelineService::timeline`]
//! (task 4.2, already reviewed) for the entire candidate -> filter -> fill
//! -> hydrate -> `Page` pipeline, `crate::oauth::middleware`'s
//! `OptionalActor`/`RequiredActor`/`require_scope` (api-foundation) for
//! authentication/scope enforcement, `crate::api::pagination`'s
//! `PageParams`/`RequestUriContext`/`build_link_header` (api-foundation) for
//! pagination and the `Link` header, and `crate::media::ResolvedOrigin`
//! (media-pipeline's `ForwardedOrigin`-resolving axum extractor, already
//! reused by `accounts::endpoints`/`statuses::endpoints`/
//! `social_graph::endpoints` for the exact same reason: no per-request
//! `ForwardedOrigin` extractor exists anywhere closer to this module's own
//! boundary — `TimelineService::timeline` itself requires one, per task
//! 4.2's own documented deviation). This module does not touch
//! `src/timelines.rs` beyond the one-line `pub mod endpoints;` declaration,
//! `src/state.rs`, `src/bootstrap.rs`, or `src/server.rs` — mounting this
//! router onto the real application is task 5.2's boundary (`TimelinesModule`
//! wiring), not this one's.
//!
//! ## `RequestActorContext -> Id` extraction lives here
//! [`crate::timelines::service::TimelineService::timeline`] takes a plain
//! `viewer_id: Option<Id>`, not `Option<&RequestActorContext>` — `service.rs`'s
//! own doc comment ("Deliberate deviations from design.md's literal Service
//! Interface") explicitly names "`RequestActorContext -> Id` extraction...
//! is `TimelineEndpoints`'s job (task 5.1, out of this task's boundary)" as
//! the reason, mirroring `social_graph::endpoints`'s identical, already-
//! reviewed precedent for `FollowService`/etc. Every handler below performs
//! exactly that narrowing (`ctx.actor_id` for [`home_timeline`]'s mandatory
//! actor, `ctx.map(|c| c.actor_id)` for [`public_timeline`]/[`tag_timeline`]'s
//! optional one) after `RequiredActor`/`OptionalActor` extraction and (for
//! `home_timeline`) scope enforcement, nothing more.
//!
//! ## Kind selection for `public`/`local` (Requirement 3.1, design.md's
//! "ローカル TL は public?local=true 経路")
//! There is no separate `/api/v1/timelines/local` route. [`public_timeline`]
//! instead inspects the wire-level `local` query flag itself and selects
//! [`TimelineKind::Local`] (not [`TimelineKind::Public`] with
//! `TimelineParams.local = true`) when it is set, [`TimelineKind::Public`]
//! otherwise. This is a deliberate choice over the alternative reading
//! ("always `TimelineKind::Public`, let `TimelineParams.local` narrow it"):
//! `TimelineKind::Local` is a first-class, independently-defined kind
//! (`kind_rules.rs::TimelineKindRules::matches_local`,
//! `candidate_repository.rs`'s own `TimelineKind::Local` SQL branch) that
//! design.md's own model sketch and flowchart both name as one of exactly
//! four kinds (design.md lines 205-219, 272) — since this endpoint module is
//! the *only* production caller of `TimelineService::timeline` that will
//! ever exist for this spec, `TimelineKind::Local` is only ever exercised in
//! production if this module is the one that selects it. `TimelineParams`'s
//! own `local`/`remote` fields are still forwarded verbatim from whatever
//! the client sent (Requirements 2.3, 2.4) — this is deliberately redundant
//! with the kind selection when `local=true` (both the kind branch and the
//! request-level narrowing in `candidate_repository::fetch_candidates`
//! independently produce `AND s.local = TRUE`), but harmless: the two
//! conditions are logically idempotent when combined, and forwarding the raw
//! flag unconditionally (rather than zeroing it out once a kind decision has
//! been made from it) keeps this handler a straightforward pass-through with
//! no hidden state.
//!
//! ## Tag timeline: `any[]`/`all[]`/`none[]` repeated query parameters
//! Mirrors `accounts::endpoints::relationships`'s already-reviewed
//! `id`/`id[]` precedent exactly (this module's doc comment there: "a plain
//! `axum::extract::Query<T>` cannot itself aggregate repeated keys into a
//! `Vec<String>` struct field... this crate has no `axum_extra`/`serde_qs`
//! dependency"). [`tag_timeline`] therefore extracts the entire raw
//! query-pair sequence via `Query<Vec<(String, String)>>`
//! ([`parse_tag_query_pairs`]), accepting both Mastodon's real bracket
//! spelling (`any[]=`) and the plain repeated-key spelling (`any=`) for
//! `any`/`all`/`none`, alongside the scalar `local`/`only_media`/
//! `max_id`/`since_id`/`min_id`/`limit` parameters carried in the same pair
//! sequence. Hashtag normalization (case-folding) is entirely
//! `TimelineKindRules`'/`CandidateRepository`'s responsibility (see
//! `kind_rules.rs::fold_tag`, `candidate_repository.rs`'s own tag-matching
//! SQL) — this handler passes the path segment and every `any`/`all`/`none`
//! value through to [`TagFilter`] unmodified.
//!
//! ## `remote` has no effect on the tag timeline
//! Requirement 4.5 only names `local`/`only_media` as tag-timeline narrowing
//! flags (never `remote`); [`tag_timeline`] does not parse a `remote` query
//! parameter at all and always forwards `TimelineParams.remote = false`.
//!
//! ## Testing strategy: `#[cfg(test)] mod tests` only, no new `tests/*_it.rs`
//! (mirrors `social_graph::endpoints`'s task-5.1 precedent — the most recent
//! sibling "new, not-yet-mounted endpoint module" task in this codebase)
//! `tests.rs` builds its own small, test-only axum `Router` mounting exactly
//! these three handlers against [`TimelineEndpointsState`], dispatched via
//! `tower::ServiceExt::oneshot` against a real, `spawn_test_app`-backed
//! Postgres schema (mirroring `social_graph::endpoints::tests`'s own
//! `build_router`/`spawn_test_app` combination) — not the real production
//! router (nothing mounts this module onto it yet, task 5.2's job) and not a
//! new `tests/*_it.rs` integration test (this task's own instruction reuses
//! `social_graph::endpoints`'s established boundary: HTTP-layer concerns —
//! scope/auth/response-code/`Link`-header wiring — belong in this module's
//! own `#[cfg(test)] mod tests`, while `TimelineService`'s own aggregation
//! behavior is already exhaustively proven end to end by
//! `tests/timeline_service_it.rs`, task 4.2, and is not re-tested here).
//! Real Bearer tokens are issued through
//! `crate::oauth::token_repository::issue_token` (mirrors
//! `social_graph::endpoints::tests::issue_test_token`) — no handler here or
//! in its tests hand-constructs a `RequestActorContext`.
//!
//! ## Feature Flag Protocol: not applicable (brand-new, not-yet-mounted
//! module)
//! This module adds new HTTP handlers, but they are reachable by nothing —
//! `src/timelines.rs` gains only a `pub mod endpoints;` declaration (this
//! task's own explicit, narrow boundary), no router/`AppState`/bootstrap
//! wiring exists yet (task 5.2's job) — so nothing regresses by this
//! module's mere existence and a literal on/off flag would add
//! configuration surface no caller needs. Mirrors
//! `social_graph::endpoints`'s own identical "Feature Flag Protocol: not
//! applicable" precedent for the structurally identical situation. Standard
//! RED (tests written against a not-yet-existing module, failing to
//! compile) -> GREEN (handlers implemented, tests pass) is this crate's
//! already-established convention for this exact situation.
//!
//! ## No `pub fn router(...)` in this module (judgment call, follows
//! established precedent over the task brief's own "e.g." suggestion)
//! `src/server.rs` is where every existing sibling module's router-building
//! function actually lives (`social_graph_router()`, `accounts_router()`,
//! `media_router()`, etc. are all defined *in* `src/server.rs`, not in
//! `social_graph::endpoints`/`accounts::endpoints`/`media::endpoints`
//! themselves — confirmed by inspection). Task 5.1's own brief names this
//! choice explicitly as a judgment call ("e.g. `pub fn router(...)` ... or
//! similar") and directs following whatever the most recent sibling
//! endpoint task actually did; `social_graph::endpoints`'s task 5.1 (the
//! most recent such task) defines no `router()` function of its own either
//! — its `tests.rs` builds a test-only router directly by calling
//! `Router::new().route(...)` with its handlers. This module follows that
//! same precedent: no `router()` function here, so task 5.2 (`TimelinesModule`
//! wiring, `src/server.rs`) is unblocked to build `timelines_router()` there
//! exactly the way it builds every other module's router, with no
//! now-orphaned, never-called function left behind by this task.

#[cfg(test)]
mod tests;

use axum::extract::{FromRef, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Json, extract::Path};
use serde::Deserialize;

use crate::api::pagination::{PageParams, RequestUriContext, build_link_header};
use crate::error::AppError;
use crate::media::ResolvedOrigin;
use crate::oauth::middleware::{AuthState, OptionalActor, RequiredActor, require_scope};
use crate::oauth::scope::ScopeSet;
use crate::timelines::model::{TagFilter, TimelineKind, TimelineParams};
use crate::timelines::service::TimelineService;
use std::sync::Arc;

// ---- Route paths (axum 0.8 `{param}` syntax, mirroring
// `crate::statuses::endpoints`'s `STATUS_PATH`-style precedent) -----------

pub const HOME_TIMELINE_PATH: &str = "/api/v1/timelines/home";
pub const PUBLIC_TIMELINE_PATH: &str = "/api/v1/timelines/public";
pub const TAG_TIMELINE_PATH: &str = "/api/v1/timelines/tag/{hashtag}";

// ---- Scope (Requirement 9.1) --------------------------------------------

fn read_statuses_scope() -> ScopeSet {
    ScopeSet::parse("read:statuses").expect("\"read:statuses\" is a valid scope literal")
}

// ---- Loosely-typed query parsing (mirrors
// `accounts::endpoints::parse_optional_bool_query`/`parse_optional_limit`
// and `statuses::endpoints::parse_optional_limit`'s identical, already-
// reviewed precedent: every wire field stays `Option<String>` so a
// malformed value renders a `422` `AppError` rather than axum's own
// `QueryRejection`) -----------------------------------------------------

fn parse_loose_bool(field_name: &str, raw: &str) -> Result<bool, AppError> {
    match raw {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        other => Err(AppError::client(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("{field_name} must be \"true\"/\"1\" or \"false\"/\"0\", got {other:?}"),
        )),
    }
}

fn parse_optional_bool_query(field_name: &str, raw: Option<&str>) -> Result<bool, AppError> {
    match raw {
        None => Ok(false),
        Some(value) => parse_loose_bool(field_name, value),
    }
}

fn parse_optional_limit(raw: Option<&str>) -> Result<Option<u32>, AppError> {
    match raw {
        None => Ok(None),
        Some(value) => value.parse::<u32>().map(Some).map_err(|_| {
            AppError::client(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("limit must be a non-negative integer, got {value:?}"),
            )
        }),
    }
}

// ---- Router-local state --------------------------------------------------

/// The router-local state every handler in this module closes over —
/// mirrors `crate::media::endpoints::MediaEndpointsState`/
/// `crate::social_graph::endpoints::SocialGraphEndpointsState`'s own
/// established "small, `Clone`-cheap bundle, not the whole `AppState`"
/// convention. Unlike `SocialGraphEndpointsState`, this stays fully
/// concrete (not generic): `TimelineService` itself is not generic over any
/// `AL`/`AR`/`D`/`LS`/`HS`-style collaborator type parameter.
#[derive(Clone)]
pub struct TimelineEndpointsState {
    pub service: Arc<TimelineService>,
    pub auth: AuthState,
}

impl FromRef<TimelineEndpointsState> for AuthState {
    fn from_ref(state: &TimelineEndpointsState) -> Self {
        state.auth.clone()
    }
}

// ---- home ------------------------------------------------------------

/// `home`'s wire-level pagination query parameters — no `local`/`remote`/
/// `only_media` (design.md's API Contract table names only "Bearer
/// (`read:statuses`), pagination" for this route).
#[derive(Debug, Deserialize)]
pub struct HomeTimelineQueryParams {
    #[serde(default)]
    pub max_id: Option<String>,
    #[serde(default)]
    pub since_id: Option<String>,
    #[serde(default)]
    pub min_id: Option<String>,
    #[serde(default)]
    pub limit: Option<String>,
}

/// `GET /api/v1/timelines/home` (design.md's API Contract table): mandatory
/// `read:statuses` scope (Requirement 9.1), rejecting an unauthenticated
/// request with 401 before any scope check runs at all (`RequiredActor`'s
/// own extraction, Requirement 1.6) and an authenticated-but-insufficiently-
/// scoped request with 403 (Requirement 9.3), delegating to
/// `TimelineService::timeline(TimelineKind::Home, ...)` (task 4.2, already
/// reviewed) for the follows-plus-self aggregation (Requirement 1.1), and
/// attaching a `Link` header (Requirement 7.2) built from the resolved
/// page's cursors via `build_link_header`/`RequestUriContext`, respecting
/// `X-Forwarded-Proto`/`X-Forwarded-Host` through `ResolvedOrigin`.
pub async fn home_timeline(
    State(state): State<TimelineEndpointsState>,
    RequiredActor(ctx): RequiredActor,
    ResolvedOrigin(origin): ResolvedOrigin,
    Query(params): Query<HomeTimelineQueryParams>,
) -> Result<Response, AppError> {
    require_scope(&ctx, &read_statuses_scope())?;

    let limit = parse_optional_limit(params.limit.as_deref())?;
    let timeline_params = TimelineParams {
        local: false,
        remote: false,
        only_media: false,
        tag: None,
        page: PageParams {
            max_id: params.max_id.clone(),
            since_id: params.since_id.clone(),
            min_id: params.min_id.clone(),
            limit,
        },
    };

    let page = state
        .service
        .timeline(
            TimelineKind::Home,
            Some(ctx.actor_id),
            timeline_params,
            &origin,
        )
        .await?;

    let mut uri_ctx = RequestUriContext::new(origin, HOME_TIMELINE_PATH.to_string());
    if let Some(limit) = limit {
        uri_ctx = uri_ctx.with_query("limit", limit.to_string());
    }
    let link_header = build_link_header(&uri_ctx, &page.cursors());

    let mut response = (StatusCode::OK, Json(page.items)).into_response();
    if let Some(link) = link_header {
        response.headers_mut().insert(header::LINK, link);
    }
    Ok(response)
}

// ---- public / local ----------------------------------------------------

/// `public`'s wire-level query parameters (also covers the local timeline
/// via `local=true`, see this module's doc comment).
#[derive(Debug, Deserialize)]
pub struct PublicTimelineQueryParams {
    #[serde(default)]
    pub local: Option<String>,
    #[serde(default)]
    pub remote: Option<String>,
    #[serde(default)]
    pub only_media: Option<String>,
    #[serde(default)]
    pub max_id: Option<String>,
    #[serde(default)]
    pub since_id: Option<String>,
    #[serde(default)]
    pub min_id: Option<String>,
    #[serde(default)]
    pub limit: Option<String>,
}

/// `GET /api/v1/timelines/public` (design.md's API Contract table): optional
/// Bearer (Requirement 9.2), an unauthenticated request never rejected —
/// only public-visibility posts are ever returned regardless of auth state
/// (`TimelineKindRules::matches_public`/`TimelineFilter` already restrict to
/// `public`, and an unauthenticated `FilterContext.viewer` is `None`,
/// Requirement 5.2). `local`/`remote`/`only_media` narrow the result
/// (Requirements 2.3, 2.4, 2.5); `local=true` additionally selects
/// `TimelineKind::Local` (Requirement 3.1 — see this module's doc comment,
/// "Kind selection for public/local"). Attaches a `Link` header (Requirement
/// 7.2) preserving every filter/`limit` value actually in effect so
/// following a `next`/`prev` link does not silently drop the caller's own
/// filter choice.
pub async fn public_timeline(
    State(state): State<TimelineEndpointsState>,
    OptionalActor(ctx): OptionalActor,
    ResolvedOrigin(origin): ResolvedOrigin,
    Query(params): Query<PublicTimelineQueryParams>,
) -> Result<Response, AppError> {
    let local = parse_optional_bool_query("local", params.local.as_deref())?;
    let remote = parse_optional_bool_query("remote", params.remote.as_deref())?;
    let only_media = parse_optional_bool_query("only_media", params.only_media.as_deref())?;
    let limit = parse_optional_limit(params.limit.as_deref())?;

    let kind = if local {
        TimelineKind::Local
    } else {
        TimelineKind::Public
    };

    let timeline_params = TimelineParams {
        local,
        remote,
        only_media,
        tag: None,
        page: PageParams {
            max_id: params.max_id.clone(),
            since_id: params.since_id.clone(),
            min_id: params.min_id.clone(),
            limit,
        },
    };

    let viewer_id = ctx.as_ref().map(|actor| actor.actor_id);
    let page = state
        .service
        .timeline(kind, viewer_id, timeline_params, &origin)
        .await?;

    let mut uri_ctx = RequestUriContext::new(origin, PUBLIC_TIMELINE_PATH.to_string());
    if let Some(limit) = limit {
        uri_ctx = uri_ctx.with_query("limit", limit.to_string());
    }
    if local {
        uri_ctx = uri_ctx.with_query("local", "true");
    }
    if remote {
        uri_ctx = uri_ctx.with_query("remote", "true");
    }
    if only_media {
        uri_ctx = uri_ctx.with_query("only_media", "true");
    }
    let link_header = build_link_header(&uri_ctx, &page.cursors());

    let mut response = (StatusCode::OK, Json(page.items)).into_response();
    if let Some(link) = link_header {
        response.headers_mut().insert(header::LINK, link);
    }
    Ok(response)
}

// ---- tag -----------------------------------------------------------------

/// The scalar (non-repeated) subset of `tag`'s wire-level query parameters,
/// extracted from the raw pair sequence by [`parse_tag_query_pairs`] — see
/// this module's doc comment ("Tag timeline: any[]/all[]/none[] repeated
/// query parameters").
#[derive(Debug, Default)]
struct TagQueryParams {
    any: Vec<String>,
    all: Vec<String>,
    none: Vec<String>,
    local: Option<String>,
    only_media: Option<String>,
    max_id: Option<String>,
    since_id: Option<String>,
    min_id: Option<String>,
    limit: Option<String>,
}

/// Extracts `tag`'s query parameters from a raw pair sequence, accepting
/// both the bracket (`any[]=`) and plain (`any=`) spellings for the
/// repeated `any`/`all`/`none` conditions (Requirement 4.4) — see this
/// module's doc comment. Scalar keys keep the last value seen if a client
/// repeats one (an unspecified edge case; "last wins" matches
/// `serde_urlencoded`'s own default single-value-field behavior for a
/// repeated key).
fn parse_tag_query_pairs(pairs: Vec<(String, String)>) -> TagQueryParams {
    let mut out = TagQueryParams::default();
    for (key, value) in pairs {
        match key.as_str() {
            "any" | "any[]" => out.any.push(value),
            "all" | "all[]" => out.all.push(value),
            "none" | "none[]" => out.none.push(value),
            "local" => out.local = Some(value),
            "only_media" => out.only_media = Some(value),
            "max_id" => out.max_id = Some(value),
            "since_id" => out.since_id = Some(value),
            "min_id" => out.min_id = Some(value),
            "limit" => out.limit = Some(value),
            _ => {}
        }
    }
    out
}

/// `GET /api/v1/timelines/tag/:hashtag` (design.md's API Contract table):
/// optional Bearer (Requirement 9.2, same unauthenticated-allowed,
/// public-only discipline as [`public_timeline`]), `:hashtag` anchors the
/// primary tag condition (Requirement 4.1), `any[]`/`all[]`/`none[]`/
/// `local`/`only_media` narrow the result (Requirements 4.4, 4.5 — see this
/// module's doc comment for why no `remote` parameter exists here),
/// delegating to `TimelineService::timeline(TimelineKind::Tag, ...)` and
/// attaching a `Link` header preserving every filter/`limit` value actually
/// in effect, including the repeated tag conditions.
pub async fn tag_timeline(
    State(state): State<TimelineEndpointsState>,
    OptionalActor(ctx): OptionalActor,
    ResolvedOrigin(origin): ResolvedOrigin,
    Path(hashtag): Path<String>,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Result<Response, AppError> {
    let parsed = parse_tag_query_pairs(pairs);
    let local = parse_optional_bool_query("local", parsed.local.as_deref())?;
    let only_media = parse_optional_bool_query("only_media", parsed.only_media.as_deref())?;
    let limit = parse_optional_limit(parsed.limit.as_deref())?;

    let timeline_params = TimelineParams {
        local,
        remote: false,
        only_media,
        tag: Some(TagFilter {
            primary: hashtag.clone(),
            any: parsed.any.clone(),
            all: parsed.all.clone(),
            none: parsed.none.clone(),
        }),
        page: PageParams {
            max_id: parsed.max_id.clone(),
            since_id: parsed.since_id.clone(),
            min_id: parsed.min_id.clone(),
            limit,
        },
    };

    let viewer_id = ctx.as_ref().map(|actor| actor.actor_id);
    let page = state
        .service
        .timeline(TimelineKind::Tag, viewer_id, timeline_params, &origin)
        .await?;

    let mut uri_ctx = RequestUriContext::new(origin, format!("/api/v1/timelines/tag/{hashtag}"));
    if let Some(limit) = limit {
        uri_ctx = uri_ctx.with_query("limit", limit.to_string());
    }
    if local {
        uri_ctx = uri_ctx.with_query("local", "true");
    }
    if only_media {
        uri_ctx = uri_ctx.with_query("only_media", "true");
    }
    for value in &parsed.any {
        uri_ctx = uri_ctx.with_query("any[]", value.clone());
    }
    for value in &parsed.all {
        uri_ctx = uri_ctx.with_query("all[]", value.clone());
    }
    for value in &parsed.none {
        uri_ctx = uri_ctx.with_query("none[]", value.clone());
    }
    let link_header = build_link_header(&uri_ctx, &page.cursors());

    let mut response = (StatusCode::OK, Json(page.items)).into_response();
    if let Some(link) = link_header {
        response.headers_mut().insert(header::LINK, link);
    }
    Ok(response)
}
