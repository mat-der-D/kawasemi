//! `SocialGraphEndpoints` (design.md "API / エンドポイント層" ->
//! "#### SocialGraphEndpoints", design.md lines ~578-604; Requirements 1.8,
//! 2.7, 4.6, 5.6, 10.1, 10.2, 10.4, 10.5; task 5.1, `Boundary:
//! SocialGraphEndpoints`): the nine HTTP handlers design.md's API Contract
//! table names — `follow`/`unfollow` (`POST /api/v1/accounts/:id/follow` /
//! `.../unfollow`), `list_follow_requests` (`GET /api/v1/follow_requests`),
//! `authorize_follow_request`/`reject_follow_request` (`POST
//! /api/v1/follow_requests/:id/authorize` / `.../reject`), `mute`/`unmute`
//! (`POST /api/v1/accounts/:id/mute` / `.../unmute`), and `block`/`unblock`
//! (`POST /api/v1/accounts/:id/block` / `.../unblock`) — applying
//! api-foundation's Bearer + scope discipline (Requirement 10.1), rendering
//! every failure through the already-wired `AppError`/`mastodon_error_body`
//! conversion (Requirement 10.2, never a bespoke error body), returning the
//! updated Relationship (via the already-implemented task 3.x business
//! services, which themselves consume accounts-and-instance's
//! `RelationshipMapper`/`RelationshipSerializer` — Requirement 8.3, this
//! module never rebuilds that JSON itself), and attaching a `Link` header
//! (api-foundation's pagination toolkit, Requirement 10.4) to the one
//! genuinely paginated route (`list_follow_requests`).
//!
//! Scope: this module owns exactly the nine axum handlers above, plus
//! [`SocialGraphEndpointsState`] (the router-local state bundle these
//! handlers close over) and the small wire-shape helpers they need
//! (scope-literal constructors, optional-JSON-body parsing, query-parameter
//! parsing). It reuses, never reimplements: `FollowService`/
//! `FollowRequestService`/`MuteService`/`BlockService` (tasks 3.1-3.4,
//! already reviewed) for all business logic — including the target-not-found
//! 404 (Requirement 10.5) and self-follow/self-block 422, which these
//! already fall out of `resolve_target`'s own 404-shaped `AppError` and the
//! services' own `cannot_follow_self`/`cannot_block_self` helpers, so this
//! module adds no 404/422 logic of its own — `crate::oauth::middleware`'s
//! `RequiredActor`/`require_scope` (api-foundation) for authentication/scope
//! enforcement, `crate::api::pagination`'s `PageParams`/`RequestUriContext`/
//! `build_link_header` (api-foundation) for `list_follow_requests`'s
//! pagination, and `crate::media::ResolvedOrigin` (media-pipeline's
//! `ForwardedOrigin`-resolving axum extractor, already reused by
//! `accounts::endpoints` for the exact same reason: no per-request
//! `ForwardedOrigin` extractor exists anywhere closer to this module's own
//! boundary). This module does not touch `src/social_graph.rs` beyond the
//! one-line `mod endpoints;` declaration, `src/state.rs`, `src/bootstrap.rs`,
//! or `src/server.rs` — mounting this router onto the real application is
//! task 5.2's boundary (`SocialGraphModule` wiring), not this one's.
//!
//! ## `RequestActorContext -> Id` extraction lives here, as task 3.1's own
//! doc comment names as this task's responsibility
//! Every task-3.x service (`FollowService::follow`/`unfollow`,
//! `FollowRequestService::list_requests`/`authorize_request`/
//! `reject_request`, `MuteService::mute`/`unmute`, `BlockService::block`/
//! `unblock`) takes a plain `viewer_id`/`owner_id`: `Id`, not
//! `&RequestActorContext` — `follow_service.rs`'s own doc comment ("Deliberate
//! deviations from design.md's literal Service Interface") explicitly names
//! "`RequestActorContext -> Id` extraction is task 5.1's (endpoints)
//! responsibility, not this service's" as the reason. Every handler below
//! does exactly that narrowing (`ctx.actor_id`) after `RequiredActor`
//! extraction and scope enforcement, nothing more — no handler here inspects
//! `ctx.scopes` beyond `require_scope`'s own check.
//!
//! ## Scope-per-endpoint: requirements.md 10.1's literal text (and design.md's
//! own adjacent prose) win over design.md's API Contract table (CONCERN —
//! documented judgment call; the table itself appears to be an unreconciled
//! design.md documentation defect, out of this task's boundary to fix)
//! requirements.md 10.1 states plainly: "follow / unfollow / mute / unmute /
//! block / unblock / follow_requests 操作に対し...スコープ（`follow` または
//! `write:follows` / `read:follows` 相当）の内包判定を適用する" — i.e. every
//! one of the six write endpoints must accept `write:follows` as an
//! alternative to `follow`. design.md's own Responsibilities prose (line
//! ~586) echoes this exact wording: "follow/unfollow/mute/unmute/block/
//! unblock = `follow`（または `write:follows`）". design.md's own, separate
//! **API Contract table** (lines ~595-604) instead differentiates per
//! endpoint — `follow`/`unfollow`/`authorize`/`reject` = `follow` alone (no
//! `write:follows` alternative listed), `mute`/`unmute` = `follow`/
//! `write:mutes`, `block`/`unblock` = `follow`/`write:blocks` — which matches
//! neither requirements.md 10.1 nor design.md's own prose in the same
//! section. This module treats requirements.md 10.1 and design.md's prose as
//! authoritative over the table: every one of the six write endpoints
//! additionally accepts `write:follows`, on top of whichever
//! action-specific granular scope the table also names. Concretely:
//! [`follow`]/[`unfollow`] require `follow` **or** `write:follows`;
//! [`authorize_follow_request`]/[`reject_follow_request`] require exactly
//! `follow` (requirements.md 10.1's own list of six write operations does not
//! include authorize/reject, and both design.md's prose and table agree they
//! stay `follow`-only, so no change applies there); [`list_follow_requests`]
//! requires `follow` **or** `read:follows` (round-2 review finding: design.md's
//! prose (line ~586, `follow`-only for the six write endpoints) and its API
//! Contract table (lines ~595-604, `read:follows`-only for this one endpoint)
//! again disagree with requirements.md 10.1's own literal text, which lists
//! `follow_requests` alongside the six write operations under the same
//! general "`follow` または ... `read:follows` 相当" pattern; Requirement 2.7
//! only narrows *authorize*/*reject* to `follow`-only, never naming this list
//! endpoint, so nothing in requirements.md actually restricts
//! `list_follow_requests` to `read:follows` alone. As with the six write
//! endpoints above, this module treats requirements.md 10.1 and the general
//! pattern as authoritative over design.md's API Contract table — consistent
//! with real Mastodon's own behavior for `GET /api/v1/follow_requests`, which
//! also accepts a `follow`-scoped token); [`mute`]/[`unmute`] require `follow`
//! **or** `write:follows` **or** `write:mutes`; [`block`]/[`unblock`] require
//! `follow` **or** `write:follows` **or** `write:blocks`. [`require_any_scope`]
//! implements the "or" by trying each candidate `ScopeSet` through the
//! already-shared `crate::oauth::middleware::require_scope` (never
//! reimplementing its `model::ScopeSet -> scope::ScopeSet` bridging or its
//! inclusion judgment) and succeeding on the first satisfied candidate.
//! Reconciling design.md's own table against its own prose/against
//! requirements.md 10.1 is a spec-artifact fix left for a human/future task —
//! this module does not edit design.md.
//!
//! ## Optional JSON request bodies: raw `Bytes` + manual parse, never
//! `axum::Json<T>` directly (Requirement 10.2)
//! `follow`/`mute` accept an *optional* JSON body (`{reblogs, notify,
//! languages}` / `{notifications, duration}`) — a real Mastodon client may
//! POST with no body at all. `axum::extract::Json<T>` rejects with a raw,
//! non-`AppError`-routed `JsonRejection` on a missing/empty/malformed body,
//! which `accounts::endpoints`'s own doc comment already documents as a
//! Requirement-10.3-analogous violation for a different extractor
//! (`Query<T>`'s `QueryRejection`) — the same reasoning applies here
//! unchanged. Both [`follow`]/[`mute`] instead take a raw `axum::body::Bytes`
//! (mirrors `federation::endpoints::inbox`'s own established "`Bytes`
//! extractor, empty body treated as `None`" precedent) and parse it by hand
//! ([`parse_follow_options`]/[`parse_mute_options`]): an empty body maps to
//! Mastodon's own real per-field defaults (`reblogs: true, notify: false,
//! languages: []` / `notifications: true, duration: None`), a non-empty body
//! is parsed as JSON with `serde`'s own per-field `#[serde(default)]`
//! filling in any omitted key, and a syntactically invalid non-empty body is
//! a `422` [`AppError`] — never a raw axum rejection.
//!
//! ## `list_follow_requests` returns Account JSON + `Link`, not Relationship
//! (Requirement 2.2, matching real Mastodon's own `GET
//! /api/v1/follow_requests` shape)
//! [`crate::social_graph::follow_request_service::FollowRequestService::list_requests`]
//! (task 3.2, already reviewed) already returns `api::pagination::Page<
//! serde_json::Value>` of real Account JSON (via
//! `AccountService::show_account`, accounts-and-instance's own contract) —
//! this handler only has to attach the `Link` header
//! (`build_link_header`/`RequestUriContext`, mirroring
//! `accounts::endpoints::list_statuses`'s identical pattern) around that
//! already-assembled page; it builds no JSON shape of its own.
//!
//! ## Generic state, deferred concrete pinning (mirrors every task-3.x
//! service's own generic shape; judgment call)
//! [`SocialGraphEndpointsState`] and every handler below stay generic over
//! the same `AL: LocalActorLookup, AR: RemoteActorLookup, D:
//! DeliveryLocalActorLookup, LS: DeliverySink, HS: DeliverySink` parameters
//! `FollowService`/`FollowRequestService`/`BlockService` (and `AL` alone for
//! `MuteService`) are already generic over — rather than committing to one
//! concrete production instantiation the way `accounts::endpoints`'s
//! `AccountsEndpointsState` does (that module's own doc comment: "there is
//! exactly one production instantiation to mount"). Unlike
//! `AccountsModule`, no `SocialGraphModule` exists yet to name a single
//! concrete pair — assembling one (analogous to `src/statuses.rs`'s own
//! `Concrete*` type-alias family, which pins `StatusActivityBuilder`/
//! `InteractionService` to federation-core's real `LocalDeliverySink`/
//! `HttpDeliverySink` monomorphization) is explicitly task 5.2's
//! (`SocialGraphModule` wiring) job, not this one's — this module's own
//! task brief forbids touching bootstrap/state/server wiring. Task 5.2 is
//! expected to instantiate `SocialGraphEndpointsState<ActorDirectory,
//! PgRemoteActorLookup, ActorDirectory, <federation-core's concrete local
//! sink>, <federation-core's concrete http sink>>` (the same concrete pair
//! `follow_service/tests.rs` et al. already use for their own
//! `spawn_test_app`-backed tests, minus the `RecordingSink` doubles) and
//! mount `follow::<...>`/`unfollow::<...>`/etc. as its router's handlers.
//!
//! ## Feature Flag Protocol: not applicable (brand-new, not-yet-mounted
//! module)
//! This module adds new HTTP handlers, but they are reachable by nothing —
//! `src/social_graph.rs` gains only a `mod endpoints;` declaration (this
//! task's own explicit, narrow boundary), no router/`AppState`/bootstrap
//! wiring exists yet (task 5.2's job) — so nothing regresses by this
//! module's mere existence and a literal on/off flag would add
//! configuration surface no caller needs, mirroring
//! `accounts::endpoints`'s own "Feature Flag Protocol: not applicable"
//! precedent for the structurally similar (if not identical) situation of
//! standard RED (tests written against a not-yet-existing module, failing
//! to compile) -> GREEN (handlers implemented, tests pass) being this
//! crate's already-established convention here.
//!
//! ## Testing strategy: split by whether a running instance is needed
//! `tests/social_graph_endpoints_it.rs` builds its own small, test-only axum
//! `Router` (mirroring `crate::oauth::middleware`'s own "real, test-only
//! `Router` dispatched via `tower::ServiceExt::oneshot` against a real,
//! `spawn_test_app`-backed Postgres schema" precedent, and the social-graph
//! service tests' established `build_service`/`RecordingSink`/real-actor-row
//! fixtures) rather than driving the real production router (nothing mounts
//! this module onto it yet, task 5.2's job). Real Bearer tokens are issued
//! through `crate::oauth::token_repository::issue_token` — no handler here
//! or in its tests hand-constructs a `RequestActorContext`. This module's
//! own `#[cfg(test)] mod tests` keeps only the pure, instance-free unit
//! tests of the wire-shape parsing helpers (`parse_follow_options`,
//! `parse_mute_options`, `parse_optional_limit`).

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{FromRef, Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::api::pagination::{PageParams, RequestUriContext, build_link_header};
use crate::api::query::parse_optional_limit;
use crate::error::AppError;
use crate::federation::DeliverySink;
use crate::federation::LocalActorLookup as DeliveryLocalActorLookup;
use crate::media::ResolvedOrigin;
use crate::oauth::RequestActorContext;
use crate::oauth::middleware::{AuthState, RequiredActor, require_scope};
use crate::oauth::scope::ScopeSet;
use crate::social_graph::activity_builder::{LocalActorLookup, RemoteActorLookup};
use crate::social_graph::block_service::BlockService;
use crate::social_graph::follow_request_service::FollowRequestService;
use crate::social_graph::follow_service::FollowService;
use crate::social_graph::model::{FollowOptions, MuteOptions};
use crate::social_graph::mute_service::MuteService;

// ---- Scope literals (Requirement 10.1; see this module's doc comment,
// "Scope-per-endpoint") -------------------------------------------------

fn follow_scope() -> ScopeSet {
    ScopeSet::parse("follow").expect("\"follow\" is a valid scope literal")
}

fn read_follows_scope() -> ScopeSet {
    ScopeSet::parse("read:follows").expect("\"read:follows\" is a valid scope literal")
}

fn write_follows_scope() -> ScopeSet {
    ScopeSet::parse("write:follows").expect("\"write:follows\" is a valid scope literal")
}

fn write_mutes_scope() -> ScopeSet {
    ScopeSet::parse("write:mutes").expect("\"write:mutes\" is a valid scope literal")
}

fn write_blocks_scope() -> ScopeSet {
    ScopeSet::parse("write:blocks").expect("\"write:blocks\" is a valid scope literal")
}

/// Succeeds if `ctx` satisfies *any* one of `candidates` (mirrors
/// `require_scope`'s own 403 [`AppError`] shape on the last-tried failure
/// when none do) — see this module's doc comment ("Scope-per-endpoint") for
/// why `mute`/`unmute`/`block`/`unblock` need this "or" composition rather
/// than a single required [`ScopeSet`]. Reuses
/// [`crate::oauth::middleware::require_scope`] for every candidate check —
/// no scope-bridging or inclusion logic is reimplemented here.
fn require_any_scope(ctx: &RequestActorContext, candidates: &[ScopeSet]) -> Result<(), AppError> {
    let mut last_err = None;
    for candidate in candidates {
        match require_scope(ctx, candidate) {
            Ok(()) => return Ok(()),
            Err(err) => last_err = Some(err),
        }
    }
    Err(last_err.expect("require_any_scope is always called with a non-empty candidate list"))
}

// ---- Optional JSON body parsing (Requirement 10.2; see this module's doc
// comment, "Optional JSON request bodies") -------------------------------

/// Wire shape of `follow`'s optional JSON body. Every field defaults to
/// Mastodon's own real per-field default when omitted, matching
/// [`FollowOptions`]'s own semantics for "no follow-behavior options were
/// sent at all" (Requirement 1.5).
#[derive(Debug, Deserialize)]
struct FollowOptionsBody {
    #[serde(default = "default_true")]
    reblogs: bool,
    #[serde(default)]
    notify: bool,
    #[serde(default)]
    languages: Vec<String>,
}

fn default_true() -> bool {
    true
}

/// Parses `follow`'s optional request body into [`FollowOptions`] — an empty
/// body (no body sent at all, the common case) maps to Mastodon's own real
/// defaults directly; a non-empty body is parsed as JSON, with any omitted
/// field falling back to the same default via `FollowOptionsBody`'s own
/// `#[serde(default...)]` attributes. A syntactically invalid non-empty body
/// is a `422` [`AppError`], never a raw axum rejection (see this module's
/// doc comment).
fn parse_follow_options(body: &[u8]) -> Result<FollowOptions, AppError> {
    if body.is_empty() {
        return Ok(FollowOptions {
            reblogs: true,
            notify: false,
            languages: Vec::new(),
        });
    }
    let parsed: FollowOptionsBody = serde_json::from_slice(body).map_err(|err| {
        AppError::client(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("invalid follow options body: {err}"),
        )
    })?;
    Ok(FollowOptions {
        reblogs: parsed.reblogs,
        notify: parsed.notify,
        languages: parsed.languages,
    })
}

/// Wire shape of `mute`'s optional JSON body. `notifications` defaults to
/// `true` (Mastodon's own real default: muting an account also mutes its
/// notifications unless explicitly told otherwise); `duration` defaults to
/// `None` (unbounded), matching [`MuteOptions::duration`]'s own "`None` =
/// unbounded" semantics (Requirement 4.3).
#[derive(Debug, Deserialize)]
struct MuteOptionsBody {
    #[serde(default = "default_true")]
    notifications: bool,
    #[serde(default)]
    duration: Option<i64>,
}

/// Parses `mute`'s optional request body into [`MuteOptions`] — mirrors
/// [`parse_follow_options`]'s identical empty-body/malformed-body discipline.
fn parse_mute_options(body: &[u8]) -> Result<MuteOptions, AppError> {
    if body.is_empty() {
        return Ok(MuteOptions {
            notifications: true,
            duration: None,
        });
    }
    let parsed: MuteOptionsBody = serde_json::from_slice(body).map_err(|err| {
        AppError::client(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("invalid mute options body: {err}"),
        )
    })?;
    Ok(MuteOptions {
        notifications: parsed.notifications,
        duration: parsed.duration,
    })
}

// ---- list_follow_requests query parameters -----------------------------

/// `list_follow_requests`'s wire-level query parameters — every field is a
/// raw `Option<String>` (mirrors
/// `accounts::endpoints::StatusesQueryParams`'s identical, already-documented
/// rationale: `axum::extract::Query<T>`'s own `QueryRejection` for a
/// malformed numeric `limit` would bypass `AppError`/`mastodon_error_body`,
/// Requirement 10.2), parsed by hand into [`PageParams`] inside
/// [`list_follow_requests`].
#[derive(Debug, Deserialize)]
pub struct FollowRequestsQueryParams {
    #[serde(default)]
    pub max_id: Option<String>,
    #[serde(default)]
    pub since_id: Option<String>,
    #[serde(default)]
    pub min_id: Option<String>,
    #[serde(default)]
    pub limit: Option<String>,
}

// ---- Router-local state -------------------------------------------------

/// The router-local state every handler in this module closes over — see
/// this module's doc comment ("Generic state, deferred concrete pinning")
/// for why this stays generic rather than naming one concrete production
/// pair the way `accounts::endpoints::AccountsEndpointsState` does.
pub struct SocialGraphEndpointsState<AL, AR, D, LS, HS>
where
    AL: LocalActorLookup,
    AR: RemoteActorLookup,
    D: DeliveryLocalActorLookup,
    LS: DeliverySink,
    HS: DeliverySink,
{
    pub follow: Arc<FollowService<AL, AR, D, LS, HS>>,
    pub follow_requests: Arc<FollowRequestService<AL, AR, D, LS, HS>>,
    pub mute: Arc<MuteService<AL>>,
    pub block: Arc<BlockService<AL, AR, D, LS, HS>>,
    pub auth: AuthState,
}

// A hand-written `Clone` impl (rather than `#[derive(Clone)]`) deliberately
// avoids adding `AL: Clone`/`AR: Clone`/... bounds a derive would otherwise
// require even though every field is already `Arc`-wrapped (or, for `auth`,
// `AuthState`'s own already-`Clone` bundle) and needs no such bound to be
// cloned itself.
impl<AL, AR, D, LS, HS> Clone for SocialGraphEndpointsState<AL, AR, D, LS, HS>
where
    AL: LocalActorLookup,
    AR: RemoteActorLookup,
    D: DeliveryLocalActorLookup,
    LS: DeliverySink,
    HS: DeliverySink,
{
    fn clone(&self) -> Self {
        Self {
            follow: Arc::clone(&self.follow),
            follow_requests: Arc::clone(&self.follow_requests),
            mute: Arc::clone(&self.mute),
            block: Arc::clone(&self.block),
            auth: self.auth.clone(),
        }
    }
}

impl<AL, AR, D, LS, HS> FromRef<SocialGraphEndpointsState<AL, AR, D, LS, HS>> for AuthState
where
    AL: LocalActorLookup,
    AR: RemoteActorLookup,
    D: DeliveryLocalActorLookup,
    LS: DeliverySink,
    HS: DeliverySink,
{
    fn from_ref(state: &SocialGraphEndpointsState<AL, AR, D, LS, HS>) -> Self {
        state.auth.clone()
    }
}

// ---- Handlers -------------------------------------------------------------

/// `POST /api/v1/accounts/:id/follow` (design.md's API Contract table):
/// mandatory `follow` **or** `write:follows` scope (Requirements 1.8, 10.1 —
/// see this module's doc comment, "Scope-per-endpoint"), an optional JSON body
/// (`{reblogs, notify, languages}`, Requirement 1.5 — see this module's doc
/// comment, "Optional JSON request bodies"), delegating to
/// `FollowService::follow` (task 3.1, already reviewed) for target
/// resolution (404 if unresolvable, Requirement 10.5), self-follow rejection
/// (422), idempotency, approval-necessity judgment, and Follow Activity
/// delivery, returning the updated Relationship (Requirement 1.1) built via
/// accounts-and-instance's own serializer (Requirement 8.3).
pub async fn follow<AL, AR, D, LS, HS>(
    State(state): State<SocialGraphEndpointsState<AL, AR, D, LS, HS>>,
    RequiredActor(ctx): RequiredActor,
    Path(target): Path<String>,
    body: Bytes,
) -> Result<Response, AppError>
where
    AL: LocalActorLookup,
    AR: RemoteActorLookup,
    D: DeliveryLocalActorLookup,
    LS: DeliverySink,
    HS: DeliverySink,
{
    require_any_scope(&ctx, &[follow_scope(), write_follows_scope()])?;
    let opts = parse_follow_options(&body)?;
    let relationship = state.follow.follow(ctx.actor_id, &target, opts).await?;
    Ok((StatusCode::OK, Json(relationship)).into_response())
}

/// `POST /api/v1/accounts/:id/unfollow` (design.md's API Contract table):
/// mandatory `follow` **or** `write:follows` scope (Requirements 1.8, 10.1),
/// delegating to
/// `FollowService::unfollow` (task 3.1, already reviewed) for target
/// resolution (404, Requirement 10.5), idempotent relationship/pending-request
/// removal, and Undo(Follow) delivery, returning the updated Relationship
/// (Requirement 1.4).
pub async fn unfollow<AL, AR, D, LS, HS>(
    State(state): State<SocialGraphEndpointsState<AL, AR, D, LS, HS>>,
    RequiredActor(ctx): RequiredActor,
    Path(target): Path<String>,
) -> Result<Response, AppError>
where
    AL: LocalActorLookup,
    AR: RemoteActorLookup,
    D: DeliveryLocalActorLookup,
    LS: DeliverySink,
    HS: DeliverySink,
{
    require_any_scope(&ctx, &[follow_scope(), write_follows_scope()])?;
    let relationship = state.follow.unfollow(ctx.actor_id, &target).await?;
    Ok((StatusCode::OK, Json(relationship)).into_response())
}

/// `GET /api/v1/follow_requests` (design.md's API Contract table): mandatory
/// `follow` **or** `read:follows` scope (Requirements 2.7, 10.1 -- see this
/// module's doc comment, "Scope-per-endpoint"; requirements.md 10.1's general
/// "follow / ... / follow_requests 操作に対し...スコープ（`follow` または
/// ... `read:follows` 相当）" pattern applies to this list endpoint too --
/// Requirement 2.7 only narrows authorize/reject to `follow`-only, not this
/// route), returning the pending inbound requesters'
/// Account JSON (Requirement 2.2, not Relationship -- see this module's doc
/// comment, "`list_follow_requests` returns Account JSON") with a `Link`
/// header (Requirement 10.4) built from `FollowRequestService::list_requests`'s
/// (task 3.2, already reviewed) own page cursors via
/// `build_link_header`/`RequestUriContext` (api-foundation's pagination
/// toolkit), respecting `X-Forwarded-Proto`/`X-Forwarded-Host` through
/// [`ResolvedOrigin`].
pub async fn list_follow_requests<AL, AR, D, LS, HS>(
    State(state): State<SocialGraphEndpointsState<AL, AR, D, LS, HS>>,
    RequiredActor(ctx): RequiredActor,
    ResolvedOrigin(origin): ResolvedOrigin,
    Query(params): Query<FollowRequestsQueryParams>,
) -> Result<Response, AppError>
where
    AL: LocalActorLookup,
    AR: RemoteActorLookup,
    D: DeliveryLocalActorLookup,
    LS: DeliverySink,
    HS: DeliverySink,
{
    require_any_scope(&ctx, &[follow_scope(), read_follows_scope()])?;

    let limit = parse_optional_limit(params.limit.as_deref())?;
    let page_params = PageParams {
        max_id: params.max_id.clone(),
        since_id: params.since_id.clone(),
        min_id: params.min_id.clone(),
        limit,
    };

    let page = state
        .follow_requests
        .list_requests(ctx.actor_id, page_params)
        .await?;

    let mut uri_ctx = RequestUriContext::new(origin, "/api/v1/follow_requests".to_string());
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

/// `POST /api/v1/follow_requests/:id/authorize` (design.md's API Contract
/// table): mandatory `follow` scope (Requirements 2.7, 10.1), delegating to
/// `FollowRequestService::authorize_request` (task 3.2, already reviewed) —
/// `:id` is the requester's account id (Mastodon's own real
/// `follow_requests` API shape, matching
/// `FollowRequestService::authorize_request`'s own `requester_target: &str`
/// parameter), 404 when no such pending request exists (Requirement 10.5),
/// establishing the follow and delivering `Accept(Follow)` (Requirement
/// 2.3), returning the updated Relationship.
pub async fn authorize_follow_request<AL, AR, D, LS, HS>(
    State(state): State<SocialGraphEndpointsState<AL, AR, D, LS, HS>>,
    RequiredActor(ctx): RequiredActor,
    Path(requester): Path<String>,
) -> Result<Response, AppError>
where
    AL: LocalActorLookup,
    AR: RemoteActorLookup,
    D: DeliveryLocalActorLookup,
    LS: DeliverySink,
    HS: DeliverySink,
{
    require_scope(&ctx, &follow_scope())?;
    let relationship = state
        .follow_requests
        .authorize_request(ctx.actor_id, &requester)
        .await?;
    Ok((StatusCode::OK, Json(relationship)).into_response())
}

/// `POST /api/v1/follow_requests/:id/reject` (design.md's API Contract
/// table): mandatory `follow` scope (Requirements 2.7, 10.1), delegating to
/// `FollowRequestService::reject_request` (task 3.2, already reviewed) --
/// mirrors [`authorize_follow_request`]'s identical `:id`-is-the-requester's-
/// account-id shape, dropping the pending request and delivering
/// `Reject(Follow)` (Requirement 2.4) instead of establishing a follow, 404
/// when no such pending request exists (Requirement 10.5), returning the
/// updated Relationship.
pub async fn reject_follow_request<AL, AR, D, LS, HS>(
    State(state): State<SocialGraphEndpointsState<AL, AR, D, LS, HS>>,
    RequiredActor(ctx): RequiredActor,
    Path(requester): Path<String>,
) -> Result<Response, AppError>
where
    AL: LocalActorLookup,
    AR: RemoteActorLookup,
    D: DeliveryLocalActorLookup,
    LS: DeliverySink,
    HS: DeliverySink,
{
    require_scope(&ctx, &follow_scope())?;
    let relationship = state
        .follow_requests
        .reject_request(ctx.actor_id, &requester)
        .await?;
    Ok((StatusCode::OK, Json(relationship)).into_response())
}

/// `POST /api/v1/accounts/:id/mute` (design.md's API Contract table):
/// mandatory `follow` **or** `write:follows` **or** `write:mutes` scope
/// (Requirements 4.6, 10.1 -- see this module's doc comment,
/// "Scope-per-endpoint"), an optional JSON
/// body (`{notifications, duration}`, Requirements 4.2, 4.3 -- see this
/// module's doc comment, "Optional JSON request bodies"), delegating to
/// `MuteService::mute` (task 3.3, already reviewed) for target resolution
/// (404, Requirement 10.5) and the DB-only mute upsert (no federation
/// Activity, Requirement 4.5), returning the updated Relationship
/// (`muting`/`muting_notifications`, Requirement 4.1).
pub async fn mute<AL, AR, D, LS, HS>(
    State(state): State<SocialGraphEndpointsState<AL, AR, D, LS, HS>>,
    RequiredActor(ctx): RequiredActor,
    Path(target): Path<String>,
    body: Bytes,
) -> Result<Response, AppError>
where
    AL: LocalActorLookup,
    AR: RemoteActorLookup,
    D: DeliveryLocalActorLookup,
    LS: DeliverySink,
    HS: DeliverySink,
{
    require_any_scope(
        &ctx,
        &[follow_scope(), write_follows_scope(), write_mutes_scope()],
    )?;
    let opts = parse_mute_options(&body)?;
    let relationship = state.mute.mute(ctx.actor_id, &target, opts).await?;
    Ok((StatusCode::OK, Json(relationship)).into_response())
}

/// `POST /api/v1/accounts/:id/unmute` (design.md's API Contract table):
/// mandatory `follow` **or** `write:follows` **or** `write:mutes` scope
/// (Requirements 4.6, 10.1), delegating to `MuteService::unmute` (task 3.3, already reviewed) for
/// target resolution (404, Requirement 10.5) and the idempotent mute-row
/// removal, returning the updated Relationship (Requirement 4.4).
pub async fn unmute<AL, AR, D, LS, HS>(
    State(state): State<SocialGraphEndpointsState<AL, AR, D, LS, HS>>,
    RequiredActor(ctx): RequiredActor,
    Path(target): Path<String>,
) -> Result<Response, AppError>
where
    AL: LocalActorLookup,
    AR: RemoteActorLookup,
    D: DeliveryLocalActorLookup,
    LS: DeliverySink,
    HS: DeliverySink,
{
    require_any_scope(
        &ctx,
        &[follow_scope(), write_follows_scope(), write_mutes_scope()],
    )?;
    let relationship = state.mute.unmute(ctx.actor_id, &target).await?;
    Ok((StatusCode::OK, Json(relationship)).into_response())
}

/// `POST /api/v1/accounts/:id/block` (design.md's API Contract table):
/// mandatory `follow` **or** `write:follows` **or** `write:blocks` scope
/// (Requirements 5.6, 10.1), delegating to `BlockService::block` (task 3.4, already reviewed) for
/// target resolution (404, Requirement 10.5), self-block rejection (422),
/// idempotency, the relationship-clearing state transition, and Block
/// Activity delivery, returning the updated Relationship (`blocking`,
/// Requirement 5.1).
pub async fn block<AL, AR, D, LS, HS>(
    State(state): State<SocialGraphEndpointsState<AL, AR, D, LS, HS>>,
    RequiredActor(ctx): RequiredActor,
    Path(target): Path<String>,
) -> Result<Response, AppError>
where
    AL: LocalActorLookup,
    AR: RemoteActorLookup,
    D: DeliveryLocalActorLookup,
    LS: DeliverySink,
    HS: DeliverySink,
{
    require_any_scope(
        &ctx,
        &[follow_scope(), write_follows_scope(), write_blocks_scope()],
    )?;
    let relationship = state.block.block(ctx.actor_id, &target).await?;
    Ok((StatusCode::OK, Json(relationship)).into_response())
}

/// `POST /api/v1/accounts/:id/unblock` (design.md's API Contract table):
/// mandatory `follow` **or** `write:follows` **or** `write:blocks` scope
/// (Requirements 5.6, 10.1), delegating to `BlockService::unblock` (task 3.4, already reviewed) for
/// target resolution (404, Requirement 10.5), the idempotent block-row
/// removal, and Undo(Block) delivery, returning the updated Relationship
/// (Requirement 5.4).
pub async fn unblock<AL, AR, D, LS, HS>(
    State(state): State<SocialGraphEndpointsState<AL, AR, D, LS, HS>>,
    RequiredActor(ctx): RequiredActor,
    Path(target): Path<String>,
) -> Result<Response, AppError>
where
    AL: LocalActorLookup,
    AR: RemoteActorLookup,
    D: DeliveryLocalActorLookup,
    LS: DeliverySink,
    HS: DeliverySink,
{
    require_any_scope(
        &ctx,
        &[follow_scope(), write_follows_scope(), write_blocks_scope()],
    )?;
    let relationship = state.block.unblock(ctx.actor_id, &target).await?;
    Ok((StatusCode::OK, Json(relationship)).into_response())
}
