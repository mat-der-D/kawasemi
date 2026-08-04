//! `SearchEndpoint` (design.md "API / エンドポイント層" -> "#### SearchEndpoint",
//! design.md lines ~478-495; Requirements 2.1, 2.3, 2.4, 9.1, 9.2, 9.3, 9.4,
//! 9.5; task 5.2, `Boundary: SearchEndpoint`): the `GET /api/v2/search` HTTP
//! handler — Bearer + `read:search` (9.1), `q`/`type`/`resolve`/`following`/
//! `account_id`/`limit`/`offset`/`exclude_unreviewed` extraction into a
//! [`crate::search::model::SearchParams`] (`limit`/`offset` rounded per the
//! api-foundation convention, 2.5/9.3), and Mastodon-compatible error bodies
//! on every failure (9.2) — mirroring the axum handler shape design.md's own
//! Architecture mermaid diagram draws for this component
//! (`Endpoint --> Bearer/ScopeChk/MastoErr/Pager/RateLimit`, all api-
//! foundation cross-cutting layers this module mounts onto rather than
//! reimplements).
//!
//! Scope: this module owns exactly [`search`] (the one axum handler design.md's
//! API Contract table names) plus [`SearchEndpointsState`] (the router-local
//! state bundle it closes over, mirroring `crate::media::endpoints::
//! MediaEndpointsState<S>`'s established "generic over the service's own
//! type parameters" precedent — [`crate::search::service::SearchService`]
//! is generic over `B: SearchBackend, H: FederationHttpClient, R:
//! RemoteActorResolver, M: LocalMentionResolver`, task 5.1's own doc
//! comment, "Engine-agnostic by construction") and the small wire-shape
//! parsing helpers `search` needs. It reuses, never reimplements:
//! [`crate::search::service::SearchService::search`] (task 5.1, already
//! reviewed) for the entire parse -> resolve -> match -> hydrate -> assemble
//! pipeline, `crate::oauth::middleware`'s `RequiredActor`/`require_scope`
//! (api-foundation) for authentication/scope enforcement, and
//! `AppError`'s already-wired `IntoResponse` impl
//! (`crate::error::AppError::into_response_with(crate::api::error::
//! mastodon_error_body)`, confirmed by reading `src/error.rs`) for every
//! failure body — no bespoke error rendering lives here. This module does
//! not touch `src/search.rs` beyond the one `pub mod endpoint;`
//! declaration/re-export this task's own dispatch brief grants, `src/
//! state.rs`, `src/bootstrap.rs`, or `src/server.rs` — composing/mounting
//! this handler onto the real application (default `PgSearchBackend`
//! wiring, `AppState` storage, router merge) is task 5.3's boundary
//! (`SearchModule` wiring), not this one's.
//!
//! ## No `pub fn router(...)` in this module (judgment call, follows the
//! established precedent this same spec's own service layer and every
//! sibling "not yet wired" endpoint module already set)
//! Mirrors `crate::timelines::endpoints`'s/`crate::social_graph::
//! endpoints`'s identical documented choice: every existing sibling
//! module's actual router-building function lives in `src/server.rs`, not
//! inside its own `endpoints.rs`/`endpoint.rs`. This module therefore
//! defines no `router()` function; [`tests`] builds a small test-only
//! `Router::new().route(SEARCH_PATH, get(search::<..>))` directly, and task
//! 5.3 (`SearchModule` wiring, `src/server.rs`) is unblocked to build
//! `search_router()` there exactly the way it builds every other module's
//! router.
//!
//! ## `SearchEndpointsState<B, H, R, M>`: hand-written `Clone`, not
//! `#[derive(Clone)]` (judgment call)
//! `#[derive(Clone)]` on a generic struct adds a `where B: Clone, H: Clone,
//! ..` bound to the generated impl even when every generic parameter is
//! only ever held behind an `Arc` (confirmed: this is `derive(Clone)`'s
//! well-known limitation, unlike a hand-written impl). This struct holds
//! `search_service: Arc<SearchService<B, H, R, M>>` only — `B`/`H`/`R`/`M`
//! themselves are never stored bare — so a hand-written [`Clone`] impl that
//! only requires each of them to satisfy their own respective port trait
//! (not `Clone`) is the correct, least-constraining choice; task 5.3's
//! eventual concrete instantiation (`PgSearchBackend`/`RemoteResolver`/
//! `ActorDirectory`/..) is not required to implement `Clone` as a result.
//! `crate::media::endpoints::MediaEndpointsState<S>` derives `Clone`
//! instead only because it also holds a bare `store: S` field directly
//! (its own `S: MediaStore + Clone` bound reflects that difference, not a
//! contradiction of this reasoning).
//!
//! ## Query-parameter extraction: a typed `Query<SearchQueryParams>`, not a
//! raw pair sequence (unlike `notifications::endpoints`'s `types[]`/
//! `exclude_types[]`)
//! Every one of this task's eight wire parameters (`q`/`type`/`resolve`/
//! `following`/`account_id`/`limit`/`offset`/`exclude_unreviewed`) is a
//! single scalar value, never a repeated key — the situation
//! `axum::extract::Query<T>` (backed by `serde_urlencoded`) already handles
//! natively, unlike `notifications::endpoints::list_notifications`'s
//! `types[]=a&types[]=b` aggregation problem (that module's own doc
//! comment). [`SearchQueryParams`] therefore mirrors `crate::timelines::
//! endpoints::HomeTimelineQueryParams`'s identical "every field
//! `#[serde(default)] Option<String>`, hand-parsed in the handler body"
//! shape rather than that raw-pairs workaround.
//!
//! ## `limit`/`offset` rounding (Requirements 2.5, 9.3) — reusing
//! api-foundation's `MAX_LIMIT`/`DEFAULT_LIMIT` constants directly, not
//! `crate::api::pagination::PageParams`/`resolve_limit` (CONCERN, judgment
//! call)
//! `crate::api::pagination` (api-foundation's own `Pagination` boundary,
//! confirmed by reading `src/api/pagination.rs` in full) is built entirely
//! around **cursor** pagination (`max_id`/`since_id`/`min_id`/`limit`) —
//! it has no `offset` concept anywhere, and its own `resolve_limit`
//! function (the actual `limit` clamp-to-`MAX_LIMIT`/default-to-
//! `DEFAULT_LIMIT` logic Requirement 2.5 names) is a private, non-`pub`
//! module-internal function reachable only through `PageParams::parse`,
//! which itself requires a `Cursor` type parameter this endpoint has no use
//! for (`crate::search::model::SearchParams` — already implemented,
//! task 1.2, reviewed — has plain `limit: u32, offset: u32` fields, not a
//! cursor). [`resolve_search_limit`] therefore reimplements exactly
//! `resolve_limit`'s own formula (`None` -> [`DEFAULT_LIMIT`], `Some(n)` ->
//! `n.min(`[`MAX_LIMIT`]`)`) against the same two `pub` constants
//! `crate::api::pagination` already exports, rather than duplicating the
//! numeric values themselves. `offset` has **no** equivalent api-foundation
//! convention to reuse at all (confirmed: `offset` does not appear
//! anywhere in `src/api/pagination.rs`) — [`resolve_search_offset`] applies
//! the same "malformed wire value is `422`, absent value defaults" parsing
//! discipline every other optional numeric query parameter in this crate
//! already follows (`notifications::endpoints::parse_optional_limit`,
//! `accounts::endpoints`'s identical precedent), defaulting absent `offset`
//! to `0` and leaving it otherwise unclamped (there is no known upper
//! bound to clamp it to). Flagged here as a CONCERN for reviewer
//! confirmation: this task's own dispatch brief bundles `limit`/`offset`
//! together under one "api-foundation 規約で丸め" instruction, but only
//! `limit` actually has an api-foundation-defined rounding rule to reuse;
//! `offset`'s rounding is this module's own, narrower "parse or 422,
//! default 0" invention.
//!
//! ## `account_id`: `422` on a malformed value, not `404`/silent-`None`
//! (judgment call, distinguishing this from `notifications::endpoints`'s
//! own `account_id` handling)
//! `notifications::endpoints::resolve_account_id_filter` deliberately
//! treats a non-numeric `account_id` as `Ok(None)` (no error at all) per
//! that spec's own Requirement 2.3 text ("未知の ID に解決できない場合は
//! エラーにせず"). This spec's own requirements/design text has no
//! equivalent instruction for `account_id`, and `crate::search::ports::
//! StatusQuery::account_id`/`crate::search::model::SearchParams::
//! account_id` are both a bare `Option<Id>` — an internal numeric id, never
//! independently resolved against `ActorDirectory`/`RemoteAccountRepository`
//! the way `notifications`'s filter is (that resolution has no counterpart
//! anywhere in this spec's own boundary; `SearchService`/`PgSearchBackend`
//! consume `account_id` as a bare numeric scope directly, never resolving
//! it to confirm existence). A malformed (non-numeric) `account_id` is
//! therefore treated the same as every other malformed optional numeric
//! query value in this crate — a `422` [`AppError`] via
//! [`parse_optional_account_id`] — rather than silently ignored.
//!
//! ## Boolean parsing
//! [`crate::api::query::parse_optional_bool_query`] accepts `"true"`/`"1"`
//! as `true`, `"false"`/`"0"` as `false`, defaults to `false` when the
//! parameter is absent, and rejects anything else as `422` — the same
//! interpretation every endpoint in this API applies, because it is the
//! same function.
//!
//! ## `type`: `422` on an unrecognized value (mirrors `notifications::
//! endpoints::parse_notification_type`'s identical precedent)
//! design.md's/requirements.md's text never specifies what happens for a
//! syntactically-present-but-unrecognized `type` value (e.g. `type=bogus`).
//! [`parse_search_type`] treats it the same way `notifications::endpoints::
//! parse_notification_type` treats an unrecognized `types[]` entry: a `422`
//! [`AppError`], never a silently-ignored/defaulted-to-"search everything"
//! fallback.
//!
//! ## `max_id`/`min_id`: not extracted (CONCERN — design.md/task-brief
//! mismatch, resolved in favor of the already-implemented upstream type)
//! design.md's own API Contract table for this component additionally
//! lists optional `max_id`/`min_id` request parameters, but this task's own
//! exact dispatch-brief text names only eight parameters to extract
//! (`q`/`type`/`resolve`/`following`/`account_id`/`limit`/`offset`/
//! `exclude_unreviewed`) — and `crate::search::model::SearchParams` (task
//! 1.2, already implemented and reviewed, not modifiable by this task) has
//! no `max_id`/`min_id` field at all, only plain `limit`/`offset`. There is
//! therefore no field on the already-fixed upstream type this handler could
//! even thread a parsed `max_id`/`min_id` value into. This handler does not
//! extract them, following the task brief's explicit (narrower) parameter
//! list and the already-implemented `SearchParams` shape over design.md's
//! API Contract table's own broader listing — flagged here as a CONCERN for
//! reviewer confirmation, not a silent gap.
//!
//! ## Feature Flag Protocol: not applicable (judgment call, mirrors
//! `crate::timelines::endpoints`'s/`crate::social_graph::endpoints`'s
//! identical precedent for the structurally identical situation)
//! This handler is not mounted onto the live router by this task (task
//! 5.3's job) — nothing observable changes for real traffic until then, so
//! a runtime feature-flag toggle here would be configuration surface no
//! caller can reach yet. Standard RED (tests against a not-yet-existing
//! `search::endpoint` module) -> GREEN (handler implemented, tests pass) is
//! this crate's already-established convention for this exact situation.
//!
//! ## Testing approach: DB-backed HTTP integration tests for the genuinely
//! new HTTP-layer behavior, pure unit tests for the parsing/rounding helpers
//! (judgment call)
//! [`tests`] drives a real, test-only axum `Router` via
//! `tower::ServiceExt::oneshot` against a real `crate::test_harness::
//! spawn_test_app`-backed Postgres schema for auth/scope/response-code/
//! wiring coverage — mirrors `crate::notifications::endpoints::tests`'s/
//! `crate::search::service::tests`'s identical "real router, real DB, no
//! mocked auth" precedent (`SearchService::new` itself requires a fully
//! built `RemoteResolver`/`SearchHydrator`, both of which need a real
//! `PgPool`, exactly as `search::service::tests`'s own doc comment already
//! establishes). [`resolve_search_limit`]/[`resolve_search_offset`]/
//! [`parse_search_type`]/[`parse_optional_bool_query`]/
//! [`parse_optional_account_id`] are additionally covered by plain,
//! DB-independent unit tests that exhaustively prove the api-foundation
//! rounding convention (default/clamp/malformed-422) directly — mirroring
//! `accounts::endpoints::tests`'s own identical "unit-test the parsing
//! helper directly, don't re-prove `PageParams`'s already-proven numeric
//! clamp end-to-end via a 41-row DB fixture" precedent (confirmed: no
//! sibling endpoint test file in this crate seeds enough rows to observe
//! `MAX_LIMIT` clamping through a live HTTP round trip either). A separate
//! DB-backed integration test proves `limit`/`offset` are correctly parsed
//! out of the query string and threaded all the way through to
//! `SearchParams`/the `SearchBackend` call (the endpoint's own genuinely
//! new wiring responsibility), reusing `crate::search::ports::
//! StubSearchBackend`'s already-proven `limit`/`offset` pagination
//! arithmetic (task 1.4, reviewed) rather than re-deriving it. Flagged as a
//! CONCERN for reviewer confirmation against this task's own "統合テスト"
//! (integration test) wording for the rounding behavior specifically.

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::Json;
use axum::extract::{FromRef, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::api::pagination::{DEFAULT_LIMIT, MAX_LIMIT};
use crate::api::query::parse_optional_bool_query;
use crate::domain::Id;
use crate::error::AppError;
use crate::federation::signatures::FederationHttpClient;
use crate::oauth::middleware::{AuthState, RequiredActor, require_scope};
use crate::oauth::scope::ScopeSet;
use crate::search::model::{SearchParams, SearchType};
use crate::search::ports::SearchBackend;
use crate::search::service::SearchService;
use crate::statuses::inbound_handlers::{LocalMentionResolver, RemoteActorResolver};

/// `GET /api/v2/search`'s route path (design.md's API Contract table).
pub const SEARCH_PATH: &str = "/api/v2/search";

/// The `read:search` scope literal this endpoint requires (Requirement
/// 9.1) — mirrors `notifications::endpoints::read_notifications_scope`'s
/// identical `ScopeSet::parse(..).expect(..)` construction of a fixed,
/// known-valid literal.
fn read_search_scope() -> ScopeSet {
    ScopeSet::parse("read:search").expect("\"read:search\" is a valid scope literal")
}

// ---- Query-parameter extraction (see this module's doc comment, "Query-
// parameter extraction") -----------------------------------------------

/// The wire-level query parameters [`search`] accepts (design.md's API
/// Contract table), extracted via a typed `Query<T>` since every one of
/// them is a single scalar value — see this module's doc comment.
#[derive(Debug, Deserialize)]
pub struct SearchQueryParams {
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub resolve: Option<String>,
    #[serde(default)]
    pub following: Option<String>,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub limit: Option<String>,
    #[serde(default)]
    pub offset: Option<String>,
    #[serde(default)]
    pub exclude_unreviewed: Option<String>,
}

/// Parses `type`'s raw wire value into a [`SearchType`] — `422` on an
/// unrecognized value (see this module's doc comment, "`type`").
fn parse_search_type(raw: &str) -> Result<SearchType, AppError> {
    match raw {
        "accounts" => Ok(SearchType::Accounts),
        "statuses" => Ok(SearchType::Statuses),
        "hashtags" => Ok(SearchType::Hashtags),
        other => Err(AppError::client(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "unknown search type {other:?}; expected one of \"accounts\"/\"statuses\"/\
                 \"hashtags\""
            ),
        )),
    }
}

/// Parses `account_id`'s raw wire value (if present) into an [`Id`] — `422`
/// on a non-numeric value (see this module's doc comment, "`account_id`").
fn parse_optional_account_id(raw: Option<&str>) -> Result<Option<Id>, AppError> {
    match raw {
        None => Ok(None),
        Some(value) => value
            .parse::<i64>()
            .map(Id::from_i64)
            .map(Some)
            .map_err(|_| {
                AppError::client(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!("account_id must be a non-negative integer, got {value:?}"),
                )
            }),
    }
}

/// Resolves `limit`'s raw wire value into the api-foundation "default when
/// absent, clamp when over-max" convention (Requirements 2.5, 9.3) — see
/// this module's doc comment, "`limit`/`offset` rounding", for why this
/// reimplements `crate::api::pagination`'s own private `resolve_limit`
/// formula against its `pub` [`DEFAULT_LIMIT`]/[`MAX_LIMIT`] constants
/// rather than importing a `pub` function that does not exist. A malformed
/// (non-numeric) value is `422`, mirroring every other optional numeric
/// query parameter in this crate.
fn resolve_search_limit(raw: Option<&str>) -> Result<u32, AppError> {
    match raw {
        None => Ok(DEFAULT_LIMIT),
        Some(value) => {
            let parsed = value.parse::<u32>().map_err(|_| {
                AppError::client(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!("limit must be a non-negative integer, got {value:?}"),
                )
            })?;
            Ok(parsed.min(MAX_LIMIT))
        }
    }
}

/// Resolves `offset`'s raw wire value, defaulting to `0` when absent — see
/// this module's doc comment, "`limit`/`offset` rounding", for why there is
/// no api-foundation-defined upper bound to clamp this to. A malformed
/// (non-numeric) value is `422`, mirroring [`resolve_search_limit`].
fn resolve_search_offset(raw: Option<&str>) -> Result<u32, AppError> {
    match raw {
        None => Ok(0),
        Some(value) => value.parse::<u32>().map_err(|_| {
            AppError::client(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("offset must be a non-negative integer, got {value:?}"),
            )
        }),
    }
}

// ---- Router-local state (see this module's doc comment,
// "`SearchEndpointsState<B, H, R, M>`") -------------------------------------

/// The router-local state [`search`] closes over — see this module's doc
/// comment for why this is generic over `SearchService`'s own type
/// parameters and why [`Clone`] is hand-written rather than derived.
pub struct SearchEndpointsState<B, H, R, M>
where
    B: SearchBackend,
    H: FederationHttpClient,
    R: RemoteActorResolver,
    M: LocalMentionResolver,
{
    pub search_service: Arc<SearchService<B, H, R, M>>,
    pub auth: AuthState,
}

impl<B, H, R, M> Clone for SearchEndpointsState<B, H, R, M>
where
    B: SearchBackend,
    H: FederationHttpClient,
    R: RemoteActorResolver,
    M: LocalMentionResolver,
{
    fn clone(&self) -> Self {
        Self {
            search_service: Arc::clone(&self.search_service),
            auth: self.auth.clone(),
        }
    }
}

impl<B, H, R, M> FromRef<SearchEndpointsState<B, H, R, M>> for AuthState
where
    B: SearchBackend,
    H: FederationHttpClient,
    R: RemoteActorResolver,
    M: LocalMentionResolver,
{
    fn from_ref(state: &SearchEndpointsState<B, H, R, M>) -> Self {
        state.auth.clone()
    }
}

// ---- Handler ---------------------------------------------------------

/// `GET /api/v2/search` (design.md's API Contract table): mandatory
/// `read:search` scope (Requirement 9.1, 401 for no/invalid bearer token
/// via [`RequiredActor`]'s own extraction, 403 for insufficient scope via
/// [`require_scope`], Requirement 2.4), building a [`SearchParams`] from the
/// extracted/parsed query parameters (`viewer` populated from the
/// authenticated actor — `crate::search::service`'s own doc comment,
/// "`read:search` は認証必須のため viewer は常に存在") and delegating the
/// entire parse/resolve/match/hydrate/assemble pipeline to
/// [`SearchService::search`] (task 5.1, already reviewed), which itself
/// returns `422` for an empty/whitespace-only `q` (Requirement 2.3). Every
/// failure — this handler's own parsing errors and any [`AppError`]
/// [`SearchService::search`] returns alike — renders through `AppError`'s
/// already-wired Mastodon-compatible `IntoResponse` impl (Requirement 9.2).
pub async fn search<B, H, R, M>(
    State(state): State<SearchEndpointsState<B, H, R, M>>,
    RequiredActor(ctx): RequiredActor,
    Query(params): Query<SearchQueryParams>,
) -> Result<Response, AppError>
where
    B: SearchBackend + Send + Sync + 'static,
    H: FederationHttpClient + Send + Sync + 'static,
    R: RemoteActorResolver + Send + Sync + 'static,
    M: LocalMentionResolver + Send + Sync + 'static,
{
    require_scope(&ctx, &read_search_scope())?;

    let kind = params.kind.as_deref().map(parse_search_type).transpose()?;
    let resolve = parse_optional_bool_query("resolve", params.resolve.as_deref())?;
    let following = parse_optional_bool_query("following", params.following.as_deref())?;
    let account_id = parse_optional_account_id(params.account_id.as_deref())?;
    let limit = resolve_search_limit(params.limit.as_deref())?;
    let offset = resolve_search_offset(params.offset.as_deref())?;
    let exclude_unreviewed =
        parse_optional_bool_query("exclude_unreviewed", params.exclude_unreviewed.as_deref())?;

    let search_params = SearchParams {
        q: params.q.unwrap_or_default(),
        kind,
        resolve,
        following,
        account_id,
        limit,
        offset,
        exclude_unreviewed,
        viewer: ctx.actor_id,
    };

    let body = state.search_service.search(search_params).await?;
    Ok((StatusCode::OK, Json(body)).into_response())
}
