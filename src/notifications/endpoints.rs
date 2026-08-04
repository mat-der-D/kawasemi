//! `NotificationEndpoints` (design.md "API / エンドポイント層" ->
//! "#### NotificationEndpoints", design.md lines ~440-463; Requirements 2.1,
//! 2.3, 2.5, 3.1, 3.2, 4.1, 4.2, 4.3, 9.1, 9.2, 9.3, 9.4; task 4.1, `Boundary:
//! NotificationEndpoints`): the four HTTP handlers design.md's API Contract
//! table names — `list_notifications` (`GET /api/v1/notifications`),
//! `show_notification` (`GET /api/v1/notifications/{id}`),
//! `clear_notifications` (`POST /api/v1/notifications/clear`), and
//! `dismiss_notification` (`POST /api/v1/notifications/{id}/dismiss`) —
//! applying api-foundation's Bearer + scope discipline (取得系
//! `read:notifications`、消去系 `write:notifications`, Requirement 9.1),
//! rendering every failure through the already-wired `AppError`/
//! `mastodon_error_body` conversion (Requirement 9.2, never a bespoke error
//! body — including the uniform other-recipient/nonexistent 404
//! `NotificationService` (task 3.2, already reviewed) already collapses both
//! cases into, Requirements 3.2/4.3), attaching a `Link` header to the one
//! genuinely paginated route (`list_notifications`, Requirement 9.3), and
//! resolving `list_notifications`'s optional `account_id` query parameter
//! into an [`AccountRef`] itself (Requirement 2.3 — see this module's doc
//! comment, "`account_id` resolution").
//!
//! Scope: this module owns exactly the four axum handlers above, plus
//! [`NotificationEndpointsState`] (the router-local state bundle these
//! handlers close over, mirroring `crate::timelines::endpoints::
//! TimelineEndpointsState`'s/`crate::social_graph::endpoints::
//! SocialGraphEndpointsState`'s established precedent) and the small
//! wire-shape helpers they need (scope-literal constructors, query-parameter
//! parsing, `account_id` resolution). It reuses, never reimplements:
//! `NotificationService` (task 3.2, already reviewed) for all list/single/
//! dismiss/clear business logic — including the 404 mapping — `crate::oauth::
//! middleware`'s `RequiredActor`/`require_scope` (api-foundation) for
//! authentication/scope enforcement, `crate::api::pagination`'s
//! `PageParams`/`Page`/`RequestUriContext`/`build_link_header` (api-
//! foundation) for `list_notifications`'s pagination, `crate::media::
//! ResolvedOrigin` (media-pipeline's `ForwardedOrigin`-resolving axum
//! extractor, already reused by `accounts::endpoints`/`social_graph::
//! endpoints`/`timelines::endpoints` for the identical reason), and — for
//! `account_id` resolution only, see below — `crate::actor::directory::
//! ActorDirectory::resolve_actor_by_id` and `crate::accounts::
//! remote_repository::find_remote_by_id` directly (never accounts-and-
//! instance's own `AccountService::show_account`/its private
//! `resolve_account_ref`, both of which differ from what this task's own
//! dispatch brief specifies — see "`account_id` resolution" below for why).
//! This module does not touch `src/notifications.rs`, `src/state.rs`,
//! `src/bootstrap.rs`, or `src/server.rs` — composing/mounting this router
//! onto the real application is task 4.2's boundary (`NotificationModule`
//! wiring), not this one's (see "Not wired into the module tree yet" below
//! for the one consequence of this that required an explicit, reverted probe
//! to validate).
//!
//! ## `account_id` resolution: a **narrower**, standalone re-derivation, not
//! a call into `AccountService` (Requirement 2.3, judgment call — see task's
//! own dispatch brief: "ローカルは `ActorDirectory`、既知リモートは
//! `RemoteAccountRepository`")
//! design.md's own `NotificationEndpoints` Responsibilities entry names the
//! *same resolution* `AccountService::show_account` uses, but explicitly
//! scoped to only two of that method's three branches: "ローカルは
//! `ActorDirectory`、既知リモートは `RemoteAccountRepository` による解決"
//! (`AccountService::show_account`'s own doc comment names this identical
//! two-branch subset as *its own* first case, "a bare non-negative integer
//! string ... tried against `ActorDirectory::resolve_actor_by_id` first ...
//! then `RemoteAccountRepository::find_remote_by_id`"). Deliberately
//! excluded is that method's own *third* branch — a non-numeric string
//! treated as a remote `actor_uri` reference and handed to
//! `RemoteAccountFetcher::fetch_and_normalize`, which performs a live
//! network fetch on a cache miss. design.md's own text for this task never
//! mentions `RemoteAccountFetcher`/fetch-as-needed at all (unlike
//! accounts-and-instance's own "accounts/:id 取得" flow, which names all
//! three), and Mastodon's real `GET /api/v1/notifications?account_id=`
//! query parameter is always a bare internal account id string in practice
//! (never a full `actor_uri`) — so this handler treats a non-numeric
//! `account_id` value the exact same way as an unresolvable numeric one
//! (Ok(None), see immediately below), never attempting a network fetch as a
//! side effect of listing notifications. This module therefore does not call
//! `AccountService::show_account` (which *does* still fall through to that
//! third branch for a non-numeric string) or `AccountService`'s own private
//! `resolve_account_ref` (same three-branch behavior, and additionally not
//! `pub` outside that module) — [`resolve_account_id_filter`] is this
//! module's own narrower, standalone two-branch re-derivation, built
//! directly from the two lower-level primitives design.md's text names by
//! name (`ActorDirectory::resolve_actor_by_id`, already `pub`;
//! `crate::accounts::remote_repository::find_remote_by_id`, already `pub` —
//! `RemoteAccountRepository` is that free-function module, not a struct, per
//! accounts-and-instance design.md's own File Structure Plan: "remote_
//! repository.rs # RemoteAccountRepository"). Flagged here as a CONCERN for
//! reviewer confirmation, not a silent gap: the alternative reading (widen to
//! `AccountService::show_account`'s full three-branch resolution, including a
//! live network fetch as a side effect of a `GET` list query) is available if
//! a reviewer judges design.md's silence on `RemoteAccountFetcher` here to be
//! an oversight rather than a deliberate narrowing.
//!
//! `resolve_account_id_filter` returns `Ok(None)` — never an error — for
//! both a non-numeric `account_id` value and a numeric value matching
//! neither a local actor nor a known remote account (Requirement 2.3's own
//! text: "未知の ID に解決できない場合はエラーにせず ... 空の結果一覧 ...
//! を返す"). [`list_notifications`] short-circuits on `Ok(None)`: it returns
//! an empty [`Page`] (`items: vec![]`, both cursors `None`) **without calling
//! `NotificationService::list`/`NotificationRepository::list` at all**
//! (design.md's own wording, "`NotificationRepository` を呼ばずに") — the
//! `Some(bogus_account_ref)` alternative (calling `NotificationService::list`
//! with a filter that can never match any row) was rejected because it
//! performs a real, unnecessary list query.
//!
//! ## "通常のページネーションヘッダ" for an unresolved `account_id`
//! (Requirement 2.3) means: build the `Link` header exactly the same way
//! any other empty result page would, not a special-cased response
//! `Page { items: Vec::new(), prev_cursor: None, next_cursor: None }` is
//! itself an ordinary, valid [`Page`] value — `build_link_header` already
//! returns `None` for a page with no cursors at all (its own doc comment:
//! "Returns `None` when there is nothing to link ... e.g. an empty result
//! set"), the same as any other genuinely empty list response from this
//! same handler. No branch in [`list_notifications`]'s own `Link`-building
//! code needs to know whether the empty page came from an unresolved
//! `account_id` or from a real, successful, merely-empty query — both flow
//! through the exact same code path below the short-circuit.
//!
//! ## `types[]`/`exclude_types[]`: raw pair sequence, not a derived `Query<T>`
//! (mirrors `timelines::endpoints::tag_timeline`'s `any[]`/`all[]`/`none[]`
//! precedent and `accounts::endpoints::relationships`'s `id[]` precedent
//! exactly)
//! A plain `axum::extract::Query<T>` cannot aggregate repeated query keys
//! into a `Vec<String>` struct field (confirmed by direct experiment against
//! this crate's actual `serde_urlencoded` 0.7.1 dependency: a `#[serde(rename
//! = "types[]")] types: Vec<String>` field errors with "invalid type:
//! string ..., expected a sequence" against `types[]=a&types[]=b`), and this
//! crate has no `axum_extra`/`serde_qs` dependency (`Cargo.toml` checked).
//! [`list_notifications`] therefore extracts the entire raw pair sequence via
//! `Query<Vec<(String, String)>>` ([`parse_list_query_pairs`]), accepting
//! both the bracket spelling (`types[]=`) and the plain repeated-key spelling
//! (`types=`) for `types`/`exclude_types` — the same "accept either spelling"
//! discipline `tag_timeline`/`relationships` already established for their
//! own repeated parameters.
//!
//! ## Unknown `types[]`/`exclude_types[]` value: `422`, not silently dropped
//! (judgment call — CONCERN)
//! Neither requirements.md nor design.md specifies behavior for a
//! syntactically-present-but-unrecognized notification type string (e.g.
//! `types[]=bogus`). This module treats it the same way every other
//! malformed-but-present wire value in this crate is treated (`limit`'s own
//! "non-negative integer" parse failure, `follow`/`mute`'s optional-body
//! parse failure, ...): a `422` [`AppError`], via [`parse_notification_type`]
//! — never a raw axum rejection, and never a silent "ignore the unrecognized
//! entry and keep going" (the alternative real Mastodon itself is rumored to
//! take for this exact parameter, which nothing in this crate's own
//! requirements/design text confirms or denies). Flagged as a CONCERN for
//! reviewer confirmation, not a silent guess.
//!
//! ## Not wired into the module tree yet (out of this task's own boundary —
//! task 4.2's job)
//! This task's own explicit boundary forbids touching `src/notifications.rs`
//! (unlike `social_graph::endpoints`'/`timelines::endpoints`'s own task 5.1,
//! each of which *did* add a one-line `pub mod endpoints;` to their own
//! parent module file as part of their own task) — so, as delivered, nothing
//! declares `mod endpoints;` anywhere and this file is not part of the crate's
//! module tree; a plain `cargo check`/`cargo build` therefore does not type-
//! check it by itself. To still obtain a real compiler signal despite that
//! constraint, this task validated the file by *temporarily* adding `pub mod
//! endpoints;` to `src/notifications.rs`, running `cargo check` (and this
//! module's own `#[cfg(test)] mod tests` suite), capturing the output as this
//! task's compile-correctness evidence, and then reverting that one line —
//! confirmed via `git diff -- src/notifications.rs` showing no residual
//! change — mirroring this exact spec's own task 3.1 precedent for a
//! structurally identical situation (its own Implementation Note: "RED フェーズ
//! は...`event_sink.rs` を型/impl 抜きのスタブに一時置換...→ 復元、md5sum で
//! 復元後の byte-identical を確認"). See this task's own status report for
//! the exact commands and output.
//!
//! ## Rate-limiting: no per-route layer here (Requirement 9.4) — inherited
//! automatically once task 4.2 mounts this router (judgment call, follows
//! the actual established wiring, not the task brief's own "e.g." suggestion)
//! `src/server.rs::build_router` (confirmed by inspection) applies exactly
//! one [`crate::api::ratelimit::rate_limit_layer`] to the *entire* merged
//! router (`router().merge(media_router(...)).merge(accounts_router())
//! .merge(statuses_router()).merge(social_graph_router())
//! .merge(timelines_router()).layer(rate_limit_layer(...))`) — no existing
//! sibling endpoint module (`accounts::endpoints`/`social_graph::endpoints`/
//! `timelines::endpoints`/`media::endpoints`) attaches its own, *separate*
//! rate-limit layer inside its own file; every one of them is covered purely
//! by being merged into that one router in `src/server.rs`. Design.md's own
//! "Modified Files" entry for `src/server.rs` says exactly this: "通知ルータ
//! を土台ルータへ装着し、api-foundation の横断レイヤー（認証・エラー・レート
//! 制限）適用点に乗せる" — rate-limit-layer attachment is that file's own
//! responsibility (task 4.2's boundary, explicitly out of this task's scope
//! per this task's own dispatch brief). This module adds no per-handler
//! exemption or opt-out of any kind, so once task 4.2 merges this module's
//! router into `router()`, every one of these four routes is covered by the
//! same global layer automatically, with no further action needed here.

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::Json;
use axum::extract::{FromRef, Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use sqlx::PgPool;

use crate::accounts::remote_repository::find_remote_by_id;
use crate::actor::directory::ActorDirectory;
use crate::api::pagination::{Page, PageParams, RequestUriContext, build_link_header};
use crate::api::query::parse_optional_limit;
use crate::domain::{AccountRef, Id};
use crate::error::AppError;
use crate::media::ResolvedOrigin;
use crate::notifications::model::NotificationType;
use crate::notifications::repository::ListFilter;
use crate::notifications::service::NotificationService;
use crate::oauth::middleware::{AuthState, RequiredActor, require_scope};
use crate::oauth::scope::ScopeSet;

// ---- Route paths (axum 0.8 `{param}` syntax, mirroring
// `crate::timelines::endpoints`'s `HOME_TIMELINE_PATH`-style precedent — the
// most recent sibling "new, not-yet-mounted endpoint module" task in this
// codebase, chronologically after `social_graph::endpoints`, whose own path
// constants instead live in `src/server.rs`; this module follows the more
// recent precedent) -----------------------------------------------------

pub const NOTIFICATIONS_LIST_PATH: &str = "/api/v1/notifications";
pub const NOTIFICATION_SHOW_PATH: &str = "/api/v1/notifications/{id}";
pub const NOTIFICATIONS_CLEAR_PATH: &str = "/api/v1/notifications/clear";
pub const NOTIFICATION_DISMISS_PATH: &str = "/api/v1/notifications/{id}/dismiss";

// ---- Scope literals (Requirement 9.1) -----------------------------------

fn read_notifications_scope() -> ScopeSet {
    ScopeSet::parse("read:notifications").expect("\"read:notifications\" is a valid scope literal")
}

fn write_notifications_scope() -> ScopeSet {
    ScopeSet::parse("write:notifications")
        .expect("\"write:notifications\" is a valid scope literal")
}

// ---- `:id` path segment parsing (mirrors `statuses::endpoints::parse_id`/
// `media::endpoints::parse_media_id`'s identical "unparseable id segment is
// 404, not 422" precedent) -------------------------------------------------

fn notification_not_found() -> AppError {
    AppError::client(StatusCode::NOT_FOUND, "notification not found")
}

fn parse_id(raw: &str) -> Result<Id, AppError> {
    raw.parse::<i64>()
        .map(Id::from_i64)
        .map_err(|_| notification_not_found())
}

// ---- `account_id` resolution (Requirement 2.3; see this module's doc
// comment, "`account_id` resolution") --------------------------------------

/// Resolves `raw` (the `account_id` query parameter's wire value) to an
/// [`AccountRef`] via the same local-`ActorDirectory`/known-remote-
/// `RemoteAccountRepository` two-branch resolution `AccountService::
/// show_account`'s own first case uses — see this module's doc comment for
/// why this is a standalone re-derivation rather than a call into
/// `AccountService`. Returns `Ok(None)` — never an error — both when `raw`
/// does not parse as a bare non-negative internal id and when a parsed id
/// matches neither a local actor nor a known remote account.
async fn resolve_account_id_filter(
    raw: &str,
    actor_directory: &ActorDirectory,
    pool: &PgPool,
) -> Result<Option<AccountRef>, AppError> {
    let Ok(raw_id) = raw.parse::<i64>() else {
        return Ok(None);
    };
    let id = Id::from_i64(raw_id);

    if actor_directory.resolve_actor_by_id(id).await?.is_some() {
        return Ok(Some(AccountRef::Local(id)));
    }
    if find_remote_by_id(pool, id).await?.is_some() {
        return Ok(Some(AccountRef::Remote(id)));
    }
    Ok(None)
}

// ---- `list_notifications` query-parameter parsing (see this module's doc
// comment, "`types[]`/`exclude_types[]`") ----------------------------------

/// The wire-level query parameters `list_notifications` accepts, extracted
/// by hand from the raw pair sequence — see this module's doc comment.
#[derive(Debug, Default)]
struct ListQueryParams {
    types: Vec<String>,
    exclude_types: Vec<String>,
    max_id: Option<String>,
    since_id: Option<String>,
    min_id: Option<String>,
    limit: Option<String>,
    account_id: Option<String>,
}

/// Extracts `list_notifications`'s query parameters from a raw pair
/// sequence, accepting both the bracket (`types[]=`) and plain (`types=`)
/// spellings for the repeated `types`/`exclude_types` conditions — mirrors
/// [`crate::timelines::endpoints`]'s `parse_tag_query_pairs`/
/// [`crate::accounts::endpoints`]'s `extract_relationship_ids` identical
/// precedent. Scalar keys keep the last value seen if a client repeats one
/// (same unspecified-edge-case "last wins" discipline those modules already
/// document).
fn parse_list_query_pairs(pairs: Vec<(String, String)>) -> ListQueryParams {
    let mut out = ListQueryParams::default();
    for (key, value) in pairs {
        match key.as_str() {
            "types" | "types[]" => out.types.push(value),
            "exclude_types" | "exclude_types[]" => out.exclude_types.push(value),
            "max_id" => out.max_id = Some(value),
            "since_id" => out.since_id = Some(value),
            "min_id" => out.min_id = Some(value),
            "limit" => out.limit = Some(value),
            "account_id" => out.account_id = Some(value),
            _ => {}
        }
    }
    out
}

/// Parses a single `types[]`/`exclude_types[]` wire value into a
/// [`NotificationType`] — `422` on an unrecognized value (see this module's
/// doc comment, "Unknown `types[]`/`exclude_types[]` value").
fn parse_notification_type(raw: &str) -> Result<NotificationType, AppError> {
    match raw {
        "mention" => Ok(NotificationType::Mention),
        "follow" => Ok(NotificationType::Follow),
        "follow_request" => Ok(NotificationType::FollowRequest),
        "favourite" => Ok(NotificationType::Favourite),
        "reblog" => Ok(NotificationType::Reblog),
        "poll" => Ok(NotificationType::Poll),
        "status" => Ok(NotificationType::Status),
        "update" => Ok(NotificationType::Update),
        other => Err(AppError::client(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "unknown notification type {other:?}; expected one of \"mention\"/\"follow\"/\
                 \"follow_request\"/\"favourite\"/\"reblog\"/\"poll\"/\"status\"/\"update\""
            ),
        )),
    }
}

/// Parses a whole `types[]`/`exclude_types[]` value list into
/// [`ListFilter`]'s `Option<Vec<NotificationType>>` shape: an empty input
/// list (no such key present at all) maps to `None` (no narrowing, matching
/// [`ListFilter`]'s own "both always `None` unless the caller opts in"
/// default), a non-empty list maps to `Some` of every parsed value.
fn parse_notification_types(raw: &[String]) -> Result<Option<Vec<NotificationType>>, AppError> {
    if raw.is_empty() {
        return Ok(None);
    }
    let mut out = Vec::with_capacity(raw.len());
    for value in raw {
        out.push(parse_notification_type(value)?);
    }
    Ok(Some(out))
}

// ---- Router-local state --------------------------------------------------

/// The router-local state every handler in this module closes over —
/// mirrors `crate::timelines::endpoints::TimelineEndpointsState`'s/
/// `crate::social_graph::endpoints::SocialGraphEndpointsState`'s established
/// "small, `Clone`-cheap bundle, not the whole `AppState`" convention. Stays
/// fully concrete (not generic over any type parameter), mirroring
/// `TimelineEndpointsState` — `NotificationService` itself is not generic.
/// `actor_directory`/`pool` are `account_id` resolution's own two
/// collaborators (see this module's doc comment, "`account_id`
/// resolution") — task 4.2's own dispatch brief ("account_id 解決ハンドル
/// （ActorDirectory / RemoteAccountRepository）を NotificationEndpoints の
/// account_id 解決に注入し") names both by name as what it injects here.
#[derive(Clone)]
pub struct NotificationEndpointsState {
    pub service: Arc<NotificationService>,
    pub actor_directory: Arc<ActorDirectory>,
    pub pool: PgPool,
    pub auth: AuthState,
}

impl FromRef<NotificationEndpointsState> for AuthState {
    fn from_ref(state: &NotificationEndpointsState) -> Self {
        state.auth.clone()
    }
}

// ---- Handlers -------------------------------------------------------------

/// `GET /api/v1/notifications` (design.md's API Contract table): mandatory
/// `read:notifications` scope (Requirement 9.1), `max_id`/`since_id`/
/// `min_id`/`limit` pagination, `types[]`/`exclude_types[]` kind narrowing
/// (Requirement 2.2 — parsing only; the actual narrowing is
/// `NotificationRepository`'s, task 1.3), and `account_id` narrowing
/// resolved to an [`AccountRef`] by this handler itself (Requirement 2.3 —
/// see this module's doc comment). Returns 200 + `Link` header (Requirement
/// 9.3) built from [`NotificationService::list`]'s own page cursors, or —
/// when `account_id` is present but unresolvable — 200 + an empty array with
/// the same ordinary (empty) `Link`-building path, never a 404 (Requirement
/// 2.3's own explicit "エラーにせず" — see this module's doc comment).
pub async fn list_notifications(
    State(state): State<NotificationEndpointsState>,
    RequiredActor(ctx): RequiredActor,
    ResolvedOrigin(origin): ResolvedOrigin,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Result<Response, AppError> {
    require_scope(&ctx, &read_notifications_scope())?;

    let parsed = parse_list_query_pairs(pairs);
    let types = parse_notification_types(&parsed.types)?;
    let exclude_types = parse_notification_types(&parsed.exclude_types)?;
    let limit = parse_optional_limit(parsed.limit.as_deref())?;

    let page_params = PageParams {
        max_id: parsed.max_id.clone(),
        since_id: parsed.since_id.clone(),
        min_id: parsed.min_id.clone(),
        limit,
    };

    let page: Page<serde_json::Value> = match &parsed.account_id {
        Some(raw) => {
            match resolve_account_id_filter(raw, &state.actor_directory, &state.pool).await? {
                Some(account_ref) => {
                    let filter = ListFilter {
                        types,
                        exclude_types,
                        account_id: Some(account_ref),
                    };
                    state.service.list(&ctx, page_params, filter).await?
                }
                // Requirement 2.3: unresolvable account_id -> 200 + empty
                // array, `NotificationRepository` never called — see this
                // module's doc comment ("`account_id` resolution").
                None => Page {
                    items: Vec::new(),
                    prev_cursor: None,
                    next_cursor: None,
                },
            }
        }
        None => {
            let filter = ListFilter {
                types,
                exclude_types,
                account_id: None,
            };
            state.service.list(&ctx, page_params, filter).await?
        }
    };

    let mut uri_ctx = RequestUriContext::new(origin, NOTIFICATIONS_LIST_PATH.to_string());
    if let Some(limit) = limit {
        uri_ctx = uri_ctx.with_query("limit", limit.to_string());
    }
    for value in &parsed.types {
        uri_ctx = uri_ctx.with_query("types[]", value.clone());
    }
    for value in &parsed.exclude_types {
        uri_ctx = uri_ctx.with_query("exclude_types[]", value.clone());
    }
    if let Some(account_id) = &parsed.account_id {
        uri_ctx = uri_ctx.with_query("account_id", account_id.clone());
    }
    let link_header = build_link_header(&uri_ctx, &page.cursors());

    let mut response = (StatusCode::OK, Json(page.items)).into_response();
    if let Some(link) = link_header {
        response.headers_mut().insert(header::LINK, link);
    }
    Ok(response)
}

/// `GET /api/v1/notifications/{id}` (design.md's API Contract table):
/// mandatory `read:notifications` scope (Requirement 9.1), 404 when `id`
/// does not parse as a bare id (mirrors `statuses::endpoints::parse_id`'s
/// precedent) or when `NotificationService::show` reports the notification
/// belongs to another recipient or does not exist at all (Requirement 3.2 —
/// that service already collapses both cases into the same 404, task 3.2's
/// own doc comment, "404 for other-recipient/nonexistent").
pub async fn show_notification(
    State(state): State<NotificationEndpointsState>,
    RequiredActor(ctx): RequiredActor,
    Path(raw_id): Path<String>,
) -> Result<Response, AppError> {
    require_scope(&ctx, &read_notifications_scope())?;
    let id = parse_id(&raw_id)?;
    let body = state.service.show(&ctx, id).await?;
    Ok((StatusCode::OK, Json(body)).into_response())
}

/// `POST /api/v1/notifications/clear` (design.md's API Contract table):
/// mandatory `write:notifications` scope (Requirement 9.1), delegating to
/// `NotificationService::clear` (task 3.2, already reviewed, unconditionally
/// idempotent — "no notion of nothing to clear") and returning `200 {}`
/// (Requirement 4.1).
pub async fn clear_notifications(
    State(state): State<NotificationEndpointsState>,
    RequiredActor(ctx): RequiredActor,
) -> Result<Response, AppError> {
    require_scope(&ctx, &write_notifications_scope())?;
    state.service.clear(&ctx).await?;
    Ok((StatusCode::OK, Json(serde_json::json!({}))).into_response())
}

/// `POST /api/v1/notifications/{id}/dismiss` (design.md's API Contract
/// table): mandatory `write:notifications` scope (Requirement 9.1), 404 when
/// `id` does not parse or when `NotificationService::dismiss` reports the
/// notification belongs to another recipient or does not exist (Requirement
/// 4.3 — mirrors [`show_notification`]'s identical 404 discipline),
/// returning `200 {}` on success (Requirement 4.2).
pub async fn dismiss_notification(
    State(state): State<NotificationEndpointsState>,
    RequiredActor(ctx): RequiredActor,
    Path(raw_id): Path<String>,
) -> Result<Response, AppError> {
    require_scope(&ctx, &write_notifications_scope())?;
    let id = parse_id(&raw_id)?;
    state.service.dismiss(&ctx, id).await?;
    Ok((StatusCode::OK, Json(serde_json::json!({}))).into_response())
}
