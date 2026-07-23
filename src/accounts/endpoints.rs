//! `AccountsEndpoints` (design.md "API / エンドポイント層" ->
//! "AccountsEndpoints", Requirements 2.1, 2.3, 3.3, 3.4, 4.1, 5.1, 5.5, 6.4,
//! 8.1, 9.1, 10.1, 10.2, 10.3, 10.4, 10.5; task 6, `Boundary:
//! AccountsEndpoints, AccountsModule`): the seven HTTP handlers design.md's
//! API Contract table names — `verify_credentials`, `relationships`,
//! `update_credentials`, `accounts/:id` (`show_account`), `accounts/:id/
//! statuses` (`list_statuses`), `GET /api/v2/instance` (`instance_v2`), `GET
//! /api/v1/custom_emojis` (`custom_emojis`) — applying api-foundation's
//! Bearer+Scope discipline where design.md's Responsibilities call for it
//! ("認証要求: verify_credentials=`read:accounts`、relationships=
//! `read:follows`、update_credentials=`write:accounts`"), leaving the
//! remaining four public/任意 ("accounts/:id・accounts/:id/statuses・
//! instance・custom_emojis はトークン未提示でも応答"), and rendering every
//! failure through the already-wired `AppError`/`mastodon_error_body`
//! conversion (Requirement 10.3) — never a bespoke error body.
//!
//! Scope: this module owns exactly the seven axum handlers below plus the
//! small, self-contained pieces they need that do not exist anywhere else
//! yet — [`AccountsEndpointsState`] (the state bundle these handlers close
//! over, mirroring `crate::media::endpoints::MediaEndpointsState`'s already-
//! reviewed precedent) and the wire-shape judgment calls documented below.
//! It reuses, never reimplements: `AccountService`/`InstanceService`/
//! `CustomEmojiService` (tasks 5.1-5.6, this spec's already-reviewed
//! business layer) for all business logic, `crate::oauth::middleware`'s
//! `OptionalActor`/`RequiredActor`/`require_scope` (api-foundation task 6.4)
//! for authentication/scope enforcement, `crate::api::pagination`'s
//! `build_link_header`/`RequestUriContext` (api-foundation task 6.2) for the
//! one genuinely paginated route (`list_statuses`), and
//! `crate::media::ResolvedOrigin` (media-pipeline task 5.1's own
//! `ForwardedOrigin`-resolving axum extractor, already `pub`-exported from
//! `crate::media` — confirmed via `crate::media`'s own `pub use
//! endpoints::{..., ResolvedOrigin, ...}` re-export, so this module reuses
//! it directly rather than defining a second copy the way
//! `media::endpoints`'s own doc comment anticipated a *future* caller might
//! have to). This module does not touch `src/bootstrap.rs`/`src/config.rs`
//! (no new composition-root wiring beyond `src/server.rs`'s own `FromRef`
//! bridge and route mounting, both this task's own boundary).
//!
//! ## `AccountsEndpointsState`: a router-local state bundle, not `AppState`
//! Mirrors `MediaEndpointsState<S>`'s own already-reviewed precedent
//! exactly: `crate::state::AppState` already holds an `AccountsModule`
//! (tasks 1.4/5.1/5.5/5.6), but a handler mounted on `Router<AppState>`
//! needs its *own* small, `Clone`-cheap bundle of exactly the `Arc<...>`
//! service handles it closes over plus an [`AuthState`] for
//! `OptionalActor`/`RequiredActor` extraction — `src/server.rs`'s own `impl
//! FromRef<AppState> for AccountsEndpointsState` (this task's own change)
//! derives one from the other, the same promotion technique
//! `MediaEndpointsState<LocalFsStore>`'s bridge already established.
//!
//! ## Wire shapes not fixed by design.md's API Contract table (judgment
//! calls, matching `media/endpoints.rs`'s own documented-judgment-call bar)
//!
//! - **`relationships`' repeated `id` query parameter**: design.md's table
//!   names the request as "Bearer（`read:follows`）, id\[\]" but does not fix
//!   how repeated identifiers are actually carried on the wire. Mastodon's
//!   real API uses bracket syntax (`id[]=1&id[]=2`); a plain
//!   `axum::extract::Query<T>` cannot itself aggregate repeated keys into a
//!   `Vec<String>` struct field at all — axum's own doc comment for
//!   [`axum::extract::Query`] says so explicitly ("For handling multiple
//!   values for the same query parameter... use `axum_extra::extract::Query`
//!   instead"), and this crate has no `axum_extra`/`serde_qs` dependency
//!   (`Cargo.toml` checked). [`relationships`] therefore extracts the
//!   **entire raw pair sequence** via `Query<Vec<(String, String)>>` (a
//!   top-level output shape `serde_urlencoded` — the crate axum's `Query`
//!   itself is built on — explicitly documents supporting, "sequences of
//!   pairs, with or without a given length"), then filters for either `id`
//!   *or* `id[]` as the key ([`extract_relationship_ids`]): both Mastodon's
//!   real bracket convention and the plainer repeated-key convention this
//!   crate's own `axum::extract::Query` documentation names as its
//!   supported style are accepted, at no extra parsing cost, so a client
//!   using either spelling is served correctly.
//! - **`update_credentials`'s multipart field names**: design.md's table
//!   names the request only as "multipart/form" without fixing individual
//!   field names for `fields_attributes`/`source`. This module mirrors
//!   Mastodon's own real, long-standing wire convention verbatim —
//!   `fields_attributes[N][name]` / `fields_attributes[N][value]` (parsed by
//!   [`parse_field_attr_key`]) and `source[privacy]` / `source[sensitive]` /
//!   `source[language]` — over inventing a simpler ad hoc convention,
//!   because these are exactly the field names real Mastodon API clients
//!   (Ivory/Elk/Phanpy, this spec's own named target clients) already send
//!   unconditionally; picking anything else would silently break every one
//!   of them despite this module technically satisfying Requirement 6.1's
//!   text. No `serde_qs`/form-nested-object crate is added for this — the
//!   handful of literal field-name shapes above are matched and parsed by
//!   hand ([`parse_update_credentials_multipart`]), the same
//!   hand-rolled-parsing convention `media/endpoints.rs`'s own
//!   `parse_upload_multipart`/`parse_focus_param` already established for
//!   this crate.
//! - **Boolean field wire encoding** (`locked`/`bot`/`discoverable`/
//!   `source[sensitive]` on `update_credentials`; `pinned`/`only_media`/
//!   `exclude_replies`/`exclude_reblogs` on `list_statuses`): neither
//!   requirements.md nor design.md fixes a boolean literal convention.
//!   [`parse_loose_bool`] accepts `"true"`/`"1"` as `true` and
//!   `"false"`/`"0"` as `false` (rejecting anything else as a `422`) —
//!   covering both a JSON-flavored client (`"true"`/`"false"`, matching
//!   Rust's own `bool::from_str`) and an HTML-form-flavored one
//!   (`"1"`/`"0"`, matching checkbox-style submission), the same two
//!   conventions real Mastodon clients are known to send interchangeably.
//! - **`source[language]`'s three-way `Option<Option<String>>` wire mapping**:
//!   [`UpdateCredentialsInput::source_language`] already has to distinguish
//!   "leave unchanged" from "explicitly clear" (`ProfilePatch::source_language`'s
//!   own shape, task 5.4). This module maps an **absent** `source[language]`
//!   field to `None` (leave unchanged), a **present but empty** value to
//!   `Some(None)` (explicit clear — matching Mastodon's own real behavior of
//!   clearing the default posting language when the field is submitted
//!   blank), and any **present, non-empty** value to `Some(Some(value))`.
//! - **`avatar`/`header` never carry a `focus` coordinate**: `MediaUploadInput`
//!   has a `focus: Option<(f32, f32)>` field (shared shape with
//!   `media::service::UploadInput`), but Requirement 6.1's own enumerated
//!   `update_credentials` field list names only the images themselves, never
//!   a focus point for either — matching real Mastodon behavior (profile
//!   avatar/header never carry a focus coordinate; only status media does)
//!   and `account_service.rs`'s own doc comment, which already documents the
//!   sibling judgment call that `update_credentials` supports no "explicit
//!   clear" for avatar/header either. Every avatar/header upload this module
//!   builds therefore always sets `focus: None`.
//! - **`list_statuses`'s query fields are all `Option<String>`, manually
//!   parsed, never a typed `Query<T>` numeric field (CONCERN, reviewer
//!   should confirm)**: this crate already has three other `Query<T>`
//!   extractor call sites (`oauth::authorize_endpoint`,
//!   `federation::endpoints::webfinger`, `federation::endpoints::outbox`),
//!   none of which ever fail to deserialize in practice because none of
//!   their fields are numeric (`OutboxQuery::page: Option<String>`, for
//!   example) — a malformed value for any of *their* fields is simply a
//!   different, still-valid `String`. `limit` is this module's one field
//!   that is conceptually numeric; deserializing it directly as
//!   `Option<u32>` through axum's `Query<T>` would let a non-numeric value
//!   reject the request via axum's own `QueryRejection` (a plain-text `400`,
//!   never routed through `AppError`/`mastodon_error_body`) — violating
//!   Requirement 10.3's "すべてのエラー応答...Mastodon 互換エラー本文"
//!   for exactly this one input shape. [`StatusesQueryParams`] therefore
//!   declares every field (including `limit`) as `Option<String>`, so
//!   `Query<T>`'s own deserialization step can never fail, and
//!   [`parse_optional_limit`]/[`parse_optional_bool_query`] perform the
//!   actual numeric/boolean parsing inside the handler body, each returning
//!   a `422` [`AppError`] on failure — the same hand-parsed-body-parameter
//!   discipline `media/endpoints.rs`'s `parse_focus_param` already
//!   established for its own numeric-ish field, applied here to a query
//!   parameter instead of a multipart one.
//! - **`relationships` carries no `Link` header, unlike `list_statuses`**:
//!   Requirement 10.4's text names both "`accounts/:id/statuses` /
//!   `relationships`" as "リスト系応答" needing `Link`/pagination discipline,
//!   but design.md's own, more specific API Contract table only appends
//!   "`+ Link`" to the `accounts/:id/statuses` row — the `relationships` row
//!   is plain `Relationship[]`. This is not a gap this task introduces:
//!   `AccountService::relationships` (task 5.3, already reviewed) returns a
//!   flat `Result<Value, AppError>` array with no `Page<T>`/cursor concept at
//!   all (a batch id lookup has no natural "next page" to link to, unlike a
//!   genuinely paginated status timeline) — there is no cursor data for this
//!   module to build a `Link` header out of even if it tried. This module
//!   therefore attaches `Link` only to [`list_statuses`], resolving
//!   Requirement 10.4's broader wording in favor of design.md's own more
//!   specific table and the already-reviewed service signature it names.
//!
//! ## Feature Flag Protocol: not applicable (route-mounting change over an
//! already-well-defined `501` baseline)
//! This task replaces `src/server.rs`'s explicit `501 Not Implemented`
//! placeholder handlers (task 1.4) with the real handlers below — a
//! behavioral HTTP-surface change, but one whose "before" state is itself a
//! well-defined, already-passing-test baseline
//! (`tests/accounts_module_wiring_it.rs`), not an unspecified blank slate. A
//! literal on/off feature flag would add configuration surface no caller
//! needs (there is no "roll back to the placeholder" operational scenario
//! for a single-owner deployment); standard RED (existing placeholder tests
//! updated to fail against the new expectations) -> GREEN (real handlers
//! mounted, tests pass) is this crate's already-established convention for
//! this exact situation (`media::endpoints`'s own "Feature Flag Protocol"
//! section reasons identically for its task).

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{FromRef, Multipart, Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Json, extract};
use serde::Deserialize;

use crate::accounts::account_service::{
    AccountService, MediaUploadInput, ProfileFieldInput, StatusesQueryInput, UpdateCredentialsInput,
};
use crate::accounts::emoji_service::CustomEmojiService;
use crate::accounts::instance_service::InstanceService;
use crate::api::pagination::{PageParams, RequestUriContext, build_link_header};
use crate::domain::Visibility;
use crate::error::AppError;
use crate::federation::signatures::ReqwestFederationHttpClient;
use crate::media::ResolvedOrigin;
use crate::media::local_fs::LocalFsStore;
use crate::oauth::middleware::{AuthState, OptionalActor, RequiredActor, require_scope};
use crate::oauth::scope::ScopeSet;

/// `verify_credentials`'s required scope (design.md's Responsibilities:
/// "verify_credentials=`read:accounts`", Requirement 10.1).
fn read_accounts_scope() -> ScopeSet {
    ScopeSet::parse("read:accounts").expect("\"read:accounts\" is a valid scope literal")
}

/// `relationships`'s required scope (design.md's Responsibilities:
/// "relationships=`read:follows`", Requirements 5.5, 10.1).
fn read_follows_scope() -> ScopeSet {
    ScopeSet::parse("read:follows").expect("\"read:follows\" is a valid scope literal")
}

/// `update_credentials`'s required scope (design.md's Responsibilities:
/// "update_credentials=`write:accounts`", Requirements 6.4, 10.1).
fn write_accounts_scope() -> ScopeSet {
    ScopeSet::parse("write:accounts").expect("\"write:accounts\" is a valid scope literal")
}

/// The router-local state every handler in this module closes over — see
/// this module's doc comment ("`AccountsEndpointsState`") for why this is
/// not `AppState` directly. `LocalFsStore`/`ReqwestFederationHttpClient` are
/// the one concrete production pair `AccountsModule`/`AccountService` are
/// monomorphized over (`crate::accounts::AccountsModule`'s own doc
/// comment), so this state names them explicitly rather than staying
/// generic — there is exactly one production instantiation to mount.
#[derive(Clone)]
pub struct AccountsEndpointsState {
    pub service: Arc<AccountService<LocalFsStore, ReqwestFederationHttpClient>>,
    pub instance: Arc<InstanceService>,
    pub emojis: Arc<CustomEmojiService>,
    pub auth: AuthState,
}

impl FromRef<AccountsEndpointsState> for AuthState {
    fn from_ref(state: &AccountsEndpointsState) -> Self {
        state.auth.clone()
    }
}

/// `GET /api/v1/accounts/verify_credentials` (design.md's API Contract
/// table): mandatory `read:accounts` scope (Requirements 2.3, 10.1),
/// returning the Bearer-token-bound actor's CredentialAccount
/// (Requirement 2.1) via `AccountService::verify_credentials` (task 5.1,
/// already reviewed) unchanged.
pub async fn verify_credentials(
    State(state): State<AccountsEndpointsState>,
    RequiredActor(ctx): RequiredActor,
    ResolvedOrigin(origin): ResolvedOrigin,
) -> Result<Response, AppError> {
    require_scope(&ctx, &read_accounts_scope())?;
    let body = state.service.verify_credentials(&ctx, &origin).await?;
    Ok((StatusCode::OK, Json(body)).into_response())
}

/// Extracts every `id`/`id[]` value from a raw query-pair sequence — see
/// this module's doc comment ("`relationships`' repeated `id` query
/// parameter") for why the whole pair sequence is captured this way rather
/// than a typed `Vec<String>` struct field.
fn extract_relationship_ids(pairs: Vec<(String, String)>) -> Vec<String> {
    pairs
        .into_iter()
        .filter_map(|(key, value)| (key == "id" || key == "id[]").then_some(value))
        .collect()
}

/// `GET /api/v1/accounts/relationships` (design.md's API Contract table):
/// mandatory `read:follows` scope (Requirements 5.5, 10.1), returning a
/// Relationship JSON array for every resolvable requested `id`
/// (Requirement 5.1) via `AccountService::relationships` (task 5.3, already
/// reviewed) unchanged. No `Link` header — see this module's doc comment
/// ("`relationships` carries no `Link` header").
pub async fn relationships(
    State(state): State<AccountsEndpointsState>,
    RequiredActor(ctx): RequiredActor,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Result<Response, AppError> {
    require_scope(&ctx, &read_follows_scope())?;
    let ids = extract_relationship_ids(pairs);
    let body = state.service.relationships(&ctx, &ids).await?;
    Ok((StatusCode::OK, Json(body)).into_response())
}

/// Accepted spellings for a loosely-typed boolean multipart/query field
/// value — see this module's doc comment ("Boolean field wire encoding").
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

/// Parses `source[privacy]`'s literal wire value into a [`Visibility`]
/// (Requirement 6.3's "公開範囲の許容値" — the type system already rejects an
/// invalid discriminant once past this parse, per `UpdateCredentialsInput`'s
/// own doc comment).
fn parse_visibility(raw: &str) -> Result<Visibility, AppError> {
    match raw {
        "public" => Ok(Visibility::Public),
        "unlisted" => Ok(Visibility::Unlisted),
        "private" => Ok(Visibility::Private),
        "direct" => Ok(Visibility::Direct),
        other => Err(AppError::client(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "source[privacy] must be one of \"public\"/\"unlisted\"/\"private\"/\"direct\", got {other:?}"
            ),
        )),
    }
}

/// One `fields_attributes[N][...]` multipart field name's decoded shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FieldAttrKind {
    Name,
    Value,
}

/// Parses a `fields_attributes[N][name]`/`fields_attributes[N][value]`
/// multipart field name into its index and which half it carries — see this
/// module's doc comment ("`update_credentials`'s multipart field names").
/// Any other shape (including a syntactically similar but unrecognized
/// suffix) is `None`, which callers treat as "not a `fields_attributes`
/// field at all" (silently ignored, matching `media/endpoints.rs`'s own
/// "unrecognized fields are ignored" tolerant-multipart precedent).
fn parse_field_attr_key(raw: &str) -> Option<(usize, FieldAttrKind)> {
    let rest = raw.strip_prefix("fields_attributes[")?;
    let (idx_str, rest) = rest.split_once("][")?;
    let idx: usize = idx_str.parse().ok()?;
    let key = rest.strip_suffix(']')?;
    match key {
        "name" => Some((idx, FieldAttrKind::Name)),
        "value" => Some((idx, FieldAttrKind::Value)),
        _ => None,
    }
}

/// Converts the accumulated `fields_attributes[N][...]` entries (keyed by
/// index, `BTreeMap` so iteration order matches ascending index order) into
/// `UpdateCredentialsInput::fields_attributes`'s `Option<Vec<ProfileFieldInput>>`
/// shape: `None` when no `fields_attributes[...]` field was present at all
/// (leave unchanged, matching every other `update_credentials` field's own
/// "absent = unchanged" convention), `Some(...)` otherwise. An index whose
/// `name` or `value` half was never sent defaults to an empty string for
/// that half, rather than dropping the entry — a partially-sent index is
/// still a real, present entry (e.g. a client that only ever sends `[name]`
/// for a slot it wants blank-valued), not an absent one.
fn build_fields_attributes(
    fields: BTreeMap<usize, (Option<String>, Option<String>)>,
) -> Option<Vec<ProfileFieldInput>> {
    if fields.is_empty() {
        return None;
    }
    Some(
        fields
            .into_values()
            .map(|(name, value)| ProfileFieldInput {
                name: name.unwrap_or_default(),
                value: value.unwrap_or_default(),
            })
            .collect(),
    )
}

/// The accumulated, not-yet-`UpdateCredentialsInput`-shaped result of
/// consuming an `update_credentials` multipart body field by field.
#[derive(Default)]
struct ParsedUpdateCredentials {
    display_name: Option<String>,
    note: Option<String>,
    locked: Option<bool>,
    bot: Option<bool>,
    discoverable: Option<bool>,
    fields: BTreeMap<usize, (Option<String>, Option<String>)>,
    source_privacy: Option<Visibility>,
    source_sensitive: Option<bool>,
    source_language: Option<Option<String>>,
    avatar: Option<(Vec<u8>, String)>,
    header: Option<(Vec<u8>, String)>,
}

impl ParsedUpdateCredentials {
    /// Converts the accumulated fields into `AccountService::update_credentials`'s
    /// actual input type, applying every judgment call this module's doc
    /// comment documents (fields_attributes ordering/defaulting, `focus:
    /// None` for avatar/header).
    fn into_input(self) -> UpdateCredentialsInput {
        UpdateCredentialsInput {
            display_name: self.display_name,
            note: self.note,
            locked: self.locked,
            bot: self.bot,
            discoverable: self.discoverable,
            fields_attributes: build_fields_attributes(self.fields),
            source_privacy: self.source_privacy,
            source_sensitive: self.source_sensitive,
            source_language: self.source_language,
            avatar: self.avatar.map(|(bytes, content_type)| MediaUploadInput {
                bytes,
                content_type,
                focus: None,
            }),
            header: self.header.map(|(bytes, content_type)| MediaUploadInput {
                bytes,
                content_type,
                focus: None,
            }),
        }
    }
}

/// Consumes `multipart` field by field for `update_credentials`, mirroring
/// `media::endpoints::parse_upload_multipart`'s established shape (a
/// `multer`-level parse failure is a `422`, not a raw axum rejection; an
/// unrecognized field name is silently ignored, never rejected). See this
/// module's doc comment for the wire-shape judgment calls this function
/// applies (`fields_attributes[N][...]`, `source[...]`, loose booleans,
/// `source[language]`'s three-way mapping).
async fn parse_update_credentials_multipart(
    mut multipart: Multipart,
) -> Result<ParsedUpdateCredentials, AppError> {
    let mut parsed = ParsedUpdateCredentials::default();

    let multipart_error = |err: extract::multipart::MultipartError| {
        AppError::client(StatusCode::UNPROCESSABLE_ENTITY, err.to_string())
    };

    while let Some(field) = multipart.next_field().await.map_err(multipart_error)? {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "display_name" => {
                parsed.display_name = Some(field.text().await.map_err(multipart_error)?);
            }
            "note" => {
                parsed.note = Some(field.text().await.map_err(multipart_error)?);
            }
            "locked" => {
                let raw = field.text().await.map_err(multipart_error)?;
                parsed.locked = Some(parse_loose_bool("locked", &raw)?);
            }
            "bot" => {
                let raw = field.text().await.map_err(multipart_error)?;
                parsed.bot = Some(parse_loose_bool("bot", &raw)?);
            }
            "discoverable" => {
                let raw = field.text().await.map_err(multipart_error)?;
                parsed.discoverable = Some(parse_loose_bool("discoverable", &raw)?);
            }
            "source[privacy]" => {
                let raw = field.text().await.map_err(multipart_error)?;
                parsed.source_privacy = Some(parse_visibility(&raw)?);
            }
            "source[sensitive]" => {
                let raw = field.text().await.map_err(multipart_error)?;
                parsed.source_sensitive = Some(parse_loose_bool("source[sensitive]", &raw)?);
            }
            "source[language]" => {
                let raw = field.text().await.map_err(multipart_error)?;
                // Absent field -> `None` (handled by never reaching this
                // arm at all). Present-but-empty -> explicit clear
                // (`Some(None)`). Present, non-empty -> `Some(Some(raw))`.
                // See this module's doc comment.
                parsed.source_language = Some(if raw.is_empty() { None } else { Some(raw) });
            }
            "avatar" => {
                let content_type = field
                    .content_type()
                    .unwrap_or("application/octet-stream")
                    .to_string();
                let bytes = field.bytes().await.map_err(multipart_error)?.to_vec();
                parsed.avatar = Some((bytes, content_type));
            }
            "header" => {
                let content_type = field
                    .content_type()
                    .unwrap_or("application/octet-stream")
                    .to_string();
                let bytes = field.bytes().await.map_err(multipart_error)?.to_vec();
                parsed.header = Some((bytes, content_type));
            }
            other => {
                if let Some((idx, kind)) = parse_field_attr_key(other) {
                    let value = field.text().await.map_err(multipart_error)?;
                    let entry = parsed.fields.entry(idx).or_insert((None, None));
                    match kind {
                        FieldAttrKind::Name => entry.0 = Some(value),
                        FieldAttrKind::Value => entry.1 = Some(value),
                    }
                }
                // Any other unrecognized field name is ignored, matching
                // `media/endpoints.rs`'s own tolerant-multipart precedent.
            }
        }
    }

    Ok(parsed)
}

/// `PATCH /api/v1/accounts/update_credentials` (design.md's API Contract
/// table): mandatory `write:accounts` scope (Requirements 6.4, 10.1),
/// parsing a `multipart/form-data` body (see this module's doc comment for
/// every wire-shape judgment call) into `UpdateCredentialsInput` and
/// delegating to `AccountService::update_credentials` (task 5.4, already
/// reviewed) for validation (422 on violation, Requirement 6.3), media
/// ingestion (Requirement 6.2), and the partial profile update itself
/// (Requirement 6.1). `Multipart` is deliberately the last parameter (axum's
/// own documented extractor-ordering requirement: a body-consuming
/// extractor must come last), mirroring `media::endpoints::upload_media`.
pub async fn update_credentials(
    State(state): State<AccountsEndpointsState>,
    RequiredActor(ctx): RequiredActor,
    ResolvedOrigin(origin): ResolvedOrigin,
    multipart: Multipart,
) -> Result<Response, AppError> {
    require_scope(&ctx, &write_accounts_scope())?;
    let parsed = parse_update_credentials_multipart(multipart).await?;
    let input = parsed.into_input();
    let body = state
        .service
        .update_credentials(&ctx, input, &origin)
        .await?;
    Ok((StatusCode::OK, Json(body)).into_response())
}

/// `GET /api/v1/accounts/:id` (design.md's API Contract table): optional
/// Bearer (Requirements 3.4, 10.2), delegating id resolution (local/known-
/// remote/needs-fetching, 404 for anything else — Requirement 3.3) to
/// `AccountService::show_account` (task 5.1, already reviewed) unchanged.
pub async fn show_account(
    State(state): State<AccountsEndpointsState>,
    OptionalActor(ctx): OptionalActor,
    ResolvedOrigin(origin): ResolvedOrigin,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    let body = state
        .service
        .show_account(&id, ctx.as_ref(), &origin)
        .await?;
    Ok((StatusCode::OK, Json(body)).into_response())
}

/// `accounts/:id/statuses`'s wire-level query parameters, before this
/// handler's own manual parsing into [`StatusesQueryInput`] — see this
/// module's doc comment ("`list_statuses`'s query fields are all
/// `Option<String>`") for why every field, including `limit`, is a raw
/// string rather than a typed `Option<u32>`/`bool`.
#[derive(Debug, Deserialize)]
pub struct StatusesQueryParams {
    #[serde(default)]
    pub max_id: Option<String>,
    #[serde(default)]
    pub since_id: Option<String>,
    #[serde(default)]
    pub min_id: Option<String>,
    #[serde(default)]
    pub limit: Option<String>,
    #[serde(default)]
    pub pinned: Option<String>,
    #[serde(default)]
    pub only_media: Option<String>,
    #[serde(default)]
    pub exclude_replies: Option<String>,
    #[serde(default)]
    pub exclude_reblogs: Option<String>,
}

/// Parses `limit`'s raw wire value (if present) into a `u32`, as a `422`
/// [`AppError`] on failure rather than axum's own `QueryRejection` — see
/// this module's doc comment.
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

/// Parses an optional loosely-typed boolean query field, defaulting to
/// `false` when absent (Mastodon's own convention for `pinned`/`only_media`/
/// `exclude_replies`/`exclude_reblogs`: omitting the filter means "do not
/// filter", not an error).
fn parse_optional_bool_query(field_name: &str, raw: Option<&str>) -> Result<bool, AppError> {
    match raw {
        None => Ok(false),
        Some(value) => parse_loose_bool(field_name, value),
    }
}

/// `GET /api/v1/accounts/:id/statuses` (design.md's API Contract table):
/// optional Bearer (Requirements 3.4, 10.2), parsing pagination
/// (`max_id`/`since_id`/`min_id`/`limit`, Requirement 4.1) and filter
/// (`pinned`/`only_media`/`exclude_replies`/`exclude_reblogs`, Requirement
/// 4.4) query parameters into [`StatusesQueryInput`], delegating to
/// `AccountService::list_statuses` (task 5.2, already reviewed) for id
/// resolution (404 for an unresolvable id) and the actual page (empty with
/// no `Link` while no `AccountStatusesProvider` is registered, Requirement
/// 4.3), and attaching a `Link` header (Requirement 10.4) built from the
/// resolved page's cursors via `build_link_header`/`RequestUriContext`
/// (api-foundation's already-reviewed pagination toolkit) — respecting
/// `X-Forwarded-Proto`/`X-Forwarded-Host` through [`ResolvedOrigin`]
/// (Requirement 10.4's "プロキシ尊重の絶対 URL"). Every filter/`limit` value
/// actually in effect is preserved on the generated `Link` URLs (via
/// `RequestUriContext::with_query`) so following a `next`/`prev` link does
/// not silently drop the caller's own filter/limit choice.
pub async fn list_statuses(
    State(state): State<AccountsEndpointsState>,
    OptionalActor(ctx): OptionalActor,
    ResolvedOrigin(origin): ResolvedOrigin,
    Path(id): Path<String>,
    Query(params): Query<StatusesQueryParams>,
) -> Result<Response, AppError> {
    let limit = parse_optional_limit(params.limit.as_deref())?;
    let pinned = parse_optional_bool_query("pinned", params.pinned.as_deref())?;
    let only_media = parse_optional_bool_query("only_media", params.only_media.as_deref())?;
    let exclude_replies =
        parse_optional_bool_query("exclude_replies", params.exclude_replies.as_deref())?;
    let exclude_reblogs =
        parse_optional_bool_query("exclude_reblogs", params.exclude_reblogs.as_deref())?;

    let query = StatusesQueryInput {
        page: PageParams {
            max_id: params.max_id.clone(),
            since_id: params.since_id.clone(),
            min_id: params.min_id.clone(),
            limit,
        },
        pinned,
        only_media,
        exclude_replies,
        exclude_reblogs,
    };

    let page = state
        .service
        .list_statuses(&id, query, ctx.as_ref())
        .await?;

    let mut uri_ctx = RequestUriContext::new(origin, format!("/api/v1/accounts/{id}/statuses"));
    if let Some(limit) = limit {
        uri_ctx = uri_ctx.with_query("limit", limit.to_string());
    }
    if pinned {
        uri_ctx = uri_ctx.with_query("pinned", "true");
    }
    if only_media {
        uri_ctx = uri_ctx.with_query("only_media", "true");
    }
    if exclude_replies {
        uri_ctx = uri_ctx.with_query("exclude_replies", "true");
    }
    if exclude_reblogs {
        uri_ctx = uri_ctx.with_query("exclude_reblogs", "true");
    }

    let link_header = build_link_header(&uri_ctx, &page.cursors());
    let mut response = (StatusCode::OK, Json(page.items)).into_response();
    if let Some(link) = link_header {
        response.headers_mut().insert(header::LINK, link);
    }
    Ok(response)
}

/// `GET /api/v2/instance` (design.md's API Contract table): no
/// authentication at all (Requirements 8.1, 10.2), delegating to
/// `InstanceService::instance_v2` (task 5.5, already reviewed) unchanged.
pub async fn instance_v2(
    State(state): State<AccountsEndpointsState>,
) -> Result<Response, AppError> {
    let body = state.instance.instance_v2().await?;
    Ok((StatusCode::OK, Json(body)).into_response())
}

/// `GET /api/v1/custom_emojis` (design.md's API Contract table): no
/// authentication at all (Requirements 9.1, 10.2), delegating to
/// `CustomEmojiService::list_custom_emojis` (task 5.6, already reviewed)
/// unchanged.
pub async fn custom_emojis(
    State(state): State<AccountsEndpointsState>,
) -> Result<Response, AppError> {
    let body = state.emojis.list_custom_emojis().await?;
    Ok((StatusCode::OK, Json(body)).into_response())
}
