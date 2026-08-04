//! `StatusEndpoints` (design.md "API / エンドポイント層" -> "StatusEndpoints",
//! design.md lines ~620-655; Requirements 3.1, 3.2, 6.1, 7.1, 7.2, 8.1, 8.5,
//! 9.1, 9.5, 10.1, 11.1, 11.3, 12.1, 12.3, 12.4, 13.2; task 7.1, `Boundary:
//! StatusEndpoints`): the 19 HTTP handlers design.md's API Contract table
//! names for statuses (create/get/delete/edit/history/source/context),
//! reblog/favourite/bookmark/pin (+ their `un*` counterparts), the bookmark
//! list, and polls (get/vote) — applying Bearer + scope discipline
//! (`crate::oauth::middleware`), `Idempotency-Key` acceptance on create,
//! Mastodon-compatible error rendering (`AppError`'s blanket `IntoResponse`),
//! and `Link`-header pagination on the bookmark list
//! (`crate::api::pagination`).
//!
//! ## Scope
//! This module owns exactly the 19 axum handlers below plus the small,
//! self-contained pieces they need that do not exist anywhere else yet —
//! [`StatusesEndpointsState`] (the state bundle these handlers close over,
//! mirroring `crate::accounts::endpoints::AccountsEndpointsState`'s/
//! `crate::media::endpoints::MediaEndpointsState`'s already-reviewed
//! precedent) and the `StatusRenderInput`-assembly glue
//! ([`StatusesEndpointsState::render_status_json`]) that resolves a bare
//! [`Status`] into the pre-resolved input `serializer::status_to_json`
//! needs — the task brief's own words: "No existing helper resolves
//! `Status -> StatusRenderInput` end-to-end — you must write this assembly
//! glue yourself." It reuses, never reimplements: `StatusService`/
//! `InteractionService`/`PollService` (tasks 5.1-5.3, this spec's own
//! already-reviewed business layer) for all business logic,
//! `serializer::status_to_json`/`poll_to_json` (task 3.3) for the JSON
//! shape itself, `crate::oauth::middleware`'s `OptionalActor`/
//! `RequiredActor`/`require_scope` (api-foundation) for authentication/scope
//! enforcement, `crate::api::pagination`'s `PageParams`/`build_link_header`/
//! `RequestUriContext` (api-foundation) for the bookmark list's `Link`
//! header, and `crate::media::ResolvedOrigin`/`to_media_attachment` (media-
//! pipeline) for the one piece of media rendering this module needs
//! directly. This module does **not** touch `src/bootstrap.rs`/
//! `src/state.rs`/`src/config.rs`/`src/server.rs`/federation-core's inbound
//! dispatcher registration — mounting this router onto the live
//! application, registering `register_status_handlers`, and wiring
//! `StatusActivityBuilder` to a concrete `DeliveryService` are task 7.2's
//! job (`_Boundary: StatusesModule, server, bootstrap, config_`), not this
//! task's.
//!
//! ## `StatusesEndpointsState<A, D, L, H, R, M>`: still generic — no
//! concrete production instantiation exists yet (CONCERN, documented
//! judgment call)
//! Unlike `AccountsEndpointsState`/`MediaEndpointsState<LocalFsStore>` (both
//! already monomorphized to this crate's one concrete production type per
//! non-`dyn`-safe port), no task in this spec's dependency chain has yet
//! picked concrete `A`/`D`/`L`/`H`/`R`/`M` types for `StatusService`/
//! `InteractionService`/`PollService` — `grep -rn
//! "StatusActivityBuilder<\|StatusService<\|InteractionService<\|PollService<"
//! src/` (outside this module and the services' own `#[cfg(test)]` modules)
//! finds no production call site at all; picking that concrete wiring is
//! task 7.2's own job (`StatusesModule` construction). [`StatusesEndpointsState`]
//! therefore stays generic over the same six type parameters
//! `StatusService`/`InteractionService`/`PollService` already declare —
//! mirroring `MediaEndpointsState<S: MediaStore + Clone>`'s identical
//! "handler-group state stays generic until a later task supplies the one
//! concrete production type" precedent, just with six parameters instead of
//! one. `Clone` is implemented by hand (not `#[derive(Clone)]`) precisely so
//! this genericity does not spuriously require `A`/`D`/`L`/`H`/`R`/`M: Clone`
//! — every one of those six is only ever held *inside* an `Arc<StatusService<...>>`/
//! `Arc<InteractionService<...>>`/`Arc<PollService<...>>` field, never by
//! value, so cloning the bundle only ever needs to clone those `Arc`s (plus
//! `PgPool`/`RuntimeContext`/`LocalFsStore`/`AuthState`, each already `Clone`
//! in its own right) — `#[derive(Clone)]` would otherwise add a spurious
//! `where A: Clone, D: Clone, ...` bound this module's own test doubles
//! (mirroring `status_service/tests.rs`'s `MockLocalActorLookup`, which is
//! not `Clone`) do not actually need to satisfy.
//!
//! `AccountService`/`LocalFsStore` (for account/media rendering) are, by
//! contrast, **not** additional generic parameters — this crate has exactly
//! one production `AccountService<S, C>` instantiation
//! (`AccountService<LocalFsStore, ReqwestFederationHttpClient>`, confirmed by
//! `src/accounts.rs`'s own `AccountsModule` field), so this module names it
//! directly, the same "one concrete type, no generic needed" judgment call
//! `AccountsEndpointsState` itself already made.
//!
//! ## `StatusRenderInput` assembly glue: what each field actually resolves to
//! [`StatusesEndpointsState::render_status_json`] (and its helper
//! [`StatusesEndpointsState::leaf_render_input`]) is the "no existing helper
//! resolves `Status -> StatusRenderInput`" glue this task's own brief calls
//! out. Concretely, against the `pool: PgPool` this bundle holds directly
//! (needed for exactly this glue — repository-level reads
//! `StatusService`/`InteractionService`/`PollService` do not themselves
//! expose, the same reason `MediaEndpointsState`'s own doc comment gives for
//! holding a bare `store: S` alongside `Arc<MediaService<S>>`):
//! - `account`: `AccountService::show_account(&status.actor_id.as_i64().to_string(), None, origin)`
//!   — reusing accounts-and-instance's already-reviewed Account JSON
//!   wholesale (never re-derived here).
//! - `media_attachments`: `status_repository::media_ids_for_status` + one
//!   `media_repository::find_by_id`/`media::to_media_attachment` call per
//!   attached medium.
//! - `tags`: `tag_repository::tags_for_status`, mapped to [`TagJson`] with a
//!   URL this module builds itself from the request's own resolved
//!   `ForwardedOrigin` (`{scheme}://{host}/tags/{name}`) — `serializer.rs`'s
//!   own doc comment explicitly named this as the endpoint layer's job
//!   ("this module has no per-instance base-domain configuration to build
//!   [a tag URL] from... so both arrive as caller-supplied... values"; this
//!   endpoint layer does have that per-request origin, via `ResolvedOrigin`,
//!   so it is the correct place to finally build one). Hashtag names are
//!   ASCII alphanumeric/underscore only (`extract_content_tokens`'s own
//!   scanner, task 5.1), so no percent-encoding is needed.
//! - `mentions`: always `Vec::new()` — `Status` has no mentions
//!   persistence at all (the same documented structural gap
//!   `status_service.rs`'s "Mention resolution: local only" section and
//!   `serializer.rs`'s own doc comment both already name; not a new gap
//!   introduced here).
//! - `emojis` (task 10.4, closing the gap the previous paragraph names for
//!   `Status`): `status_service::extract_content_tokens(&status.content)
//!   .emoji_shortcodes` (widened to `pub(crate)` by this task, the same
//!   treatment `hashtags`/`mentions` already got) resolved via
//!   `accounts::emoji_repository::resolve_emojis` — reused verbatim, never
//!   reimplemented (this module has no shortcode-matching pipeline of its
//!   own, and does not need one: an unregistered shortcode simply is not in
//!   `resolve_emojis`'s result, no error). `CustomEmojiView`'s -> JSON
//!   mapping stays `serializer.rs`'s own `emoji_to_json`, called from
//!   `to_status_json`/`to_poll_json`, not duplicated here.
//! - `poll`: `PollService::poll(viewer, poll_id)` + `poll_to_json`, with
//!   `emojis` resolved the same way as the `Status` case above, but scanned
//!   from every `PollOption::title` in the tally (a poll option's title is
//!   its own shortcode-bearing text, distinct from the owning `Status`'s
//!   `content` — Requirement 2.1 lists `emojis` as a Poll field in its own
//!   right, not merely inherited from the parent Status) when
//!   `status.poll_id.is_some()`.
//! - `interactions`: `interaction_repository::exists_favourite`/
//!   `exists_bookmark`/`exists_pin`/`find_reblog` against the *viewer*
//!   (never the post's own author unless they are also the viewer) —
//!   `Default` (all `false`) when unauthenticated, matching
//!   `StatusInteractionState`'s own doc comment. `muted` is always `false`
//!   (no mute feature exists in this spec's dependency set, same documented
//!   gap `tasks.md`'s own Implementation Notes name for `InteractionService`).
//! - `reblog`: **at most one level of nesting.** When `status.reblog_of_id`
//!   is `Some`, the target is fetched via `StatusService::show` (a
//!   visibility-gated fetch — a reblog of a post the *current* viewer can no
//!   longer see, e.g. because the target went private after the boost was
//!   made, renders with `reblog: None` rather than erroring the whole
//!   response) and rendered via [`leaf_render_input`](StatusesEndpointsState::leaf_render_input),
//!   whose own `reblog` field is unconditionally `None` — this module does
//!   not recurse into a boost-of-a-boost. Real Mastodon does not expose
//!   nested-more-than-one-level boosts either (boosting a boost normalizes
//!   to the original), and no requirement or design.md text asks for deeper
//!   nesting here.
//!
//! ## Wire-shape judgment calls not fixed by design.md's API Contract table
//! - **id path segments are `404`, not `422`, when unparseable** — mirrors
//!   `media/endpoints.rs::parse_media_id`'s identical, already-reviewed
//!   precedent and reasoning (an unparseable id and a nonexistent one are
//!   indistinguishable "not here" outcomes from the caller's perspective).
//! - **`source`'s required scope**: design.md's API Contract table row for
//!   `GET .../source` names only "Bearer（所有）", no scope literal — but
//!   this component's own Responsibilities prose lists `read:statuses`
//!   among the scopes "各エンドポイントで...要求（再利用）", and no other row
//!   in the table ever uses it. `source` is the one row whose error column
//!   lists `403` without a table-named scope to justify it (every other
//!   optional-Bearer row's error column omits `403` entirely). Reconciling
//!   these two facts: [`source`] requires `read:statuses`, the one
//!   Responsibilities-named scope with no other table row to attach to,
//!   which is exactly what makes its own `403` reachable.
//! - **`create_status`'s `status`/`poll`/`visibility` field names and
//!   defaults**: `status` (not `content`, matching design.md's own table
//!   cell verbatim and real Mastodon's wire convention) is the post body;
//!   an absent `visibility` defaults to `Visibility::Public` (Mastodon's own
//!   default when a client omits it); `poll.expires_in` is whole seconds
//!   from now, converted to `expires_at` via the endpoint's own injected
//!   `RuntimeContext::clock` (never ad hoc `OffsetDateTime::now_utc()`) —
//!   mirroring every other caller-minted-timestamp discipline this crate
//!   already established. A poll is validated for shape here but always
//!   422-rejected by `StatusService::create_status` itself (see that
//!   module's own "Poll handling" doc comment) — this endpoint does not
//!   special-case that outcome.
//! - **`in_reply_to_id`/`media_ids` malformed values are `422`, not
//!   `404`**: unlike a path segment, a malformed *body* field is a
//!   malformed request, not a lookup miss — `StatusService::create_status`
//!   itself already reports a valid-but-nonexistent `in_reply_to_id`/
//!   `media_ids` entry as `404`/`422` respectively; this endpoint only adds
//!   the one failure mode the service can't see (a body field that isn't
//!   even a decimal id).
//! - **`StatusEdit`/`StatusSource` JSON shapes**: design.md's table fixes
//!   only the response type name (`StatusEdit[]`/`StatusSource`), not a
//!   field list. [`StatusEditJson`] renders exactly the four fields
//!   `StatusService::history`'s own doc comment says the schema can
//!   retain (`content`/`spoiler_text`/`sensitive`/`created_at`);
//!   [`StatusSourceJson`] adds the path `id` alongside `StatusSource`'s own
//!   `text`/`spoiler_text`, matching real Mastodon's `StatusSource` entity
//!   (`{id, text, spoiler_text}`) — `StatusService::source`'s own domain
//!   type has no `id` field (it is already known from the request path), so
//!   this endpoint supplies it.
//! - **Delete's response reflects the post-cascade (largely empty)
//!   interaction/tag/media state** (CONCERN, minor): `delete_status` returns
//!   the pre-deletion domain `Status`, but this endpoint still renders it
//!   through the normal [`render_status_json`](StatusesEndpointsState::render_status_json)
//!   path *after* the row (and its `ON DELETE CASCADE`d media/tag/
//!   favourite/bookmark/pin associations) are already gone — so the
//!   returned representation's `media_attachments`/`tags`/`favourited`/
//!   `bookmarked`/`pinned` reflect the now-empty post-delete state, not a
//!   snapshot of what they were a moment before deletion. This matches real
//!   Mastodon's own observable behavior (a deleted status's own
//!   interactions are moot) and requirement 7.1 only asks for "削除された
//!   投稿の表現" (the deleted post's representation), not a frozen pre-delete
//!   interaction snapshot.

#[cfg(test)]
mod tests;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use axum::Json;
use axum::extract::{FromRef, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::postgres::PgPool;
use time::Duration as TimeDuration;

use crate::accounts::account_service::AccountService;
use crate::api::pagination::{PageParams, RequestUriContext, build_link_header};
use crate::api::query::parse_optional_limit;
use crate::api::time::format_time;
use crate::domain::{Id, Visibility};
use crate::error::AppError;
use crate::federation::signatures::ReqwestFederationHttpClient;
use crate::federation::{DeliverySink, LocalActorLookup};
use crate::media::ResolvedOrigin;
use crate::media::local_fs::LocalFsStore;
use crate::oauth::middleware::{AuthState, OptionalActor, RequiredActor, require_scope};
use crate::oauth::scope::ScopeSet;
use crate::runtime::RuntimeContext;
use crate::statuses::activity_builder::ActorHandleLookup;
use crate::statuses::interaction_service::InteractionService;
use crate::statuses::model::Poll;
use crate::statuses::model::{Status, StatusEdit};
use crate::statuses::poll_repository::PollTally;
use crate::statuses::poll_service::PollService;
use crate::statuses::render_assembler::{
    EmojiResolution, PollResolver, RenderContext, StatusRenderAssembler,
};
use crate::statuses::serializer::{SerializeContext, poll_to_json};

use crate::statuses::status_service::{
    CreateStatus, CreateStatusPoll, EditStatus, MentionLookup, StatusService,
};
use crate::statuses::visibility::RelationshipQuery;

// ---- Route paths (axum 0.8 `{id}` syntax, mirroring
// `crate::accounts::endpoints`'s `ACCOUNTS_SHOW_PATH` precedent) ----

pub const STATUSES_PATH: &str = "/api/v1/statuses";
pub const STATUS_PATH: &str = "/api/v1/statuses/{id}";
pub const STATUS_HISTORY_PATH: &str = "/api/v1/statuses/{id}/history";
pub const STATUS_SOURCE_PATH: &str = "/api/v1/statuses/{id}/source";
pub const STATUS_CONTEXT_PATH: &str = "/api/v1/statuses/{id}/context";
pub const STATUS_REBLOG_PATH: &str = "/api/v1/statuses/{id}/reblog";
pub const STATUS_UNREBLOG_PATH: &str = "/api/v1/statuses/{id}/unreblog";
pub const STATUS_FAVOURITE_PATH: &str = "/api/v1/statuses/{id}/favourite";
pub const STATUS_UNFAVOURITE_PATH: &str = "/api/v1/statuses/{id}/unfavourite";
pub const STATUS_BOOKMARK_PATH: &str = "/api/v1/statuses/{id}/bookmark";
pub const STATUS_UNBOOKMARK_PATH: &str = "/api/v1/statuses/{id}/unbookmark";
pub const STATUS_PIN_PATH: &str = "/api/v1/statuses/{id}/pin";
pub const STATUS_UNPIN_PATH: &str = "/api/v1/statuses/{id}/unpin";
pub const BOOKMARKS_PATH: &str = "/api/v1/bookmarks";
pub const POLL_PATH: &str = "/api/v1/polls/{id}";
pub const POLL_VOTES_PATH: &str = "/api/v1/polls/{id}/votes";

// ---- Scopes ----

fn write_statuses_scope() -> ScopeSet {
    ScopeSet::parse("write:statuses").expect("\"write:statuses\" is a valid scope literal")
}

fn write_favourites_scope() -> ScopeSet {
    ScopeSet::parse("write:favourites").expect("\"write:favourites\" is a valid scope literal")
}

fn write_bookmarks_scope() -> ScopeSet {
    ScopeSet::parse("write:bookmarks").expect("\"write:bookmarks\" is a valid scope literal")
}

fn read_bookmarks_scope() -> ScopeSet {
    ScopeSet::parse("read:bookmarks").expect("\"read:bookmarks\" is a valid scope literal")
}

/// See this module's doc comment ("`source`'s required scope").
fn read_statuses_scope() -> ScopeSet {
    ScopeSet::parse("read:statuses").expect("\"read:statuses\" is a valid scope literal")
}

fn not_found() -> AppError {
    AppError::client(StatusCode::NOT_FOUND, "status not found")
}

fn rejected(message: impl Into<String>) -> AppError {
    AppError::client(StatusCode::UNPROCESSABLE_ENTITY, message.into())
}

/// Parses a `/api/v*/{statuses,polls}/{id}` path segment into an [`Id`]. See
/// this module's doc comment ("id path segments are `404`, not `422`") for
/// why an unparseable segment is treated as `404`, mirroring
/// `media::endpoints::parse_media_id`'s identical precedent.
fn parse_id(raw: &str) -> Result<Id, AppError> {
    raw.parse::<i64>()
        .map(Id::from_i64)
        .map_err(|_| not_found())
}

/// Reads a single header's value as UTF-8, if present and valid — mirrors
/// `media::endpoints::header_str`'s identical established pattern.
fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok()
}

/// The router-local state every handler in this module closes over — see
/// this module's doc comment (`StatusesEndpointsState<A, D, L, H, R, M>`)
/// for why this stays generic and why `Clone` is implemented by hand.
pub struct StatusesEndpointsState<A, D, L, H, R, M>
where
    A: ActorHandleLookup,
    D: LocalActorLookup,
    L: DeliverySink,
    H: DeliverySink,
    R: RelationshipQuery,
    M: MentionLookup,
{
    pub status_service: Arc<StatusService<A, D, L, H, R, M>>,
    pub interaction_service: Arc<InteractionService<A, D, L, H, R>>,
    pub poll_service: Arc<PollService<A, D, L, H, R>>,
    pub accounts: Arc<AccountService<LocalFsStore, ReqwestFederationHttpClient>>,
    pub media_store: LocalFsStore,
    /// Direct pool access for the `StatusRenderInput`-assembly glue — see
    /// this module's doc comment.
    pub pool: PgPool,
    /// Clock injection for poll rendering's `now` (`SerializeContext::now`,
    /// Requirement 2.3's determinism discipline) and `create_status`'s
    /// `poll.expires_in` conversion — never ad hoc `OffsetDateTime::now_utc()`.
    pub runtime: RuntimeContext,
    pub auth: AuthState,
}

impl<A, D, L, H, R, M> Clone for StatusesEndpointsState<A, D, L, H, R, M>
where
    A: ActorHandleLookup,
    D: LocalActorLookup,
    L: DeliverySink,
    H: DeliverySink,
    R: RelationshipQuery,
    M: MentionLookup,
{
    fn clone(&self) -> Self {
        Self {
            status_service: Arc::clone(&self.status_service),
            interaction_service: Arc::clone(&self.interaction_service),
            poll_service: Arc::clone(&self.poll_service),
            accounts: Arc::clone(&self.accounts),
            media_store: self.media_store.clone(),
            pool: self.pool.clone(),
            runtime: self.runtime.clone(),
            auth: self.auth.clone(),
        }
    }
}

impl<A, D, L, H, R, M> FromRef<StatusesEndpointsState<A, D, L, H, R, M>> for AuthState
where
    A: ActorHandleLookup,
    D: LocalActorLookup,
    L: DeliverySink,
    H: DeliverySink,
    R: RelationshipQuery,
    M: MentionLookup,
{
    fn from_ref(state: &StatusesEndpointsState<A, D, L, H, R, M>) -> Self {
        state.auth.clone()
    }
}

impl<A, D, L, H, R, M> StatusesEndpointsState<A, D, L, H, R, M>
where
    A: ActorHandleLookup + 'static,
    D: LocalActorLookup + 'static,
    L: DeliverySink + 'static,
    H: DeliverySink + 'static,
    R: RelationshipQuery + 'static,
    M: MentionLookup + 'static,
{
    /// Builds the shared Status assembler this module renders through.
    ///
    /// Constructed per render rather than held on the state bundle: all
    /// three handles are cheap to clone, and threading a fourth field
    /// through every construction site of this generic state struct buys
    /// nothing at this call frequency.
    fn assembler(&self) -> StatusRenderAssembler {
        StatusRenderAssembler::new(
            self.pool.clone(),
            Arc::clone(&self.accounts),
            self.media_store.clone(),
        )
    }

    /// Resolves `status` (owned) into its full Mastodon-compatible JSON
    /// representation.
    ///
    /// Boost-target resolution stays here rather than moving into the
    /// assembler: this module resolves it through `StatusService::show`,
    /// which applies the visibility rules this endpoint surface owns.
    /// `status` is taken by value so the target fetched inside this
    /// function can be borrowed from a local that outlives the render.
    async fn render_status_json(
        &self,
        viewer: Option<Id>,
        status: Status,
        origin: &crate::api::pagination::ForwardedOrigin,
    ) -> Result<Value, AppError> {
        let reblog_target = match status.reblog_of_id {
            Some(target_id) => self.status_service.show(viewer, target_id).await?,
            None => None,
        };
        let polls = PollServiceResolver(Arc::clone(&self.poll_service));
        let ctx = RenderContext {
            viewer,
            now: self.runtime.clock.now(),
            origin,
            // This surface has no mute context to offer, so every status it
            // renders reports `muted: false` — the behavior it has always
            // had, now stated rather than hard-coded downstream.
            muted: None,
            polls: &polls,
            emojis: EmojiResolution {
                content: true,
                poll_options: true,
            },
        };
        self.assembler()
            .assemble_one(&status, reblog_target.as_ref(), &ctx)
            .await
    }
}

/// Resolves polls through [`PollService`], so this surface keeps applying
/// that service's own visibility check — and keeps raising when a poll is
/// missing or hidden, rather than silently rendering a poll-less status.
struct PollServiceResolver<A, D, L, H, R>(Arc<PollService<A, D, L, H, R>>)
where
    A: ActorHandleLookup,
    D: LocalActorLookup,
    L: DeliverySink,
    H: DeliverySink,
    R: RelationshipQuery;

impl<A, D, L, H, R> PollResolver for PollServiceResolver<A, D, L, H, R>
where
    A: ActorHandleLookup + 'static,
    D: LocalActorLookup + 'static,
    L: DeliverySink + 'static,
    H: DeliverySink + 'static,
    R: RelationshipQuery + 'static,
{
    fn resolve_many<'a>(
        &'a self,
        poll_ids: &'a [Id],
        viewer: Option<Id>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<(Id, Poll, PollTally)>, AppError>> + Send + 'a>>
    {
        Box::pin(async move {
            let mut out = Vec::with_capacity(poll_ids.len());
            for &poll_id in poll_ids {
                let (poll, tally) = self.0.poll(viewer, poll_id).await?;
                out.push((poll_id, poll, tally));
            }
            Ok(out)
        })
    }
}

// ---- Request bodies ----

/// `POST /api/v1/statuses` request body's `poll` sub-object — see this
/// module's doc comment ("`create_status`'s `status`/`poll`/`visibility`
/// field names and defaults").
#[derive(Debug, Deserialize)]
pub struct CreatePollRequest {
    pub options: Vec<String>,
    #[serde(default)]
    pub multiple: bool,
    #[serde(default)]
    pub expires_in: Option<i64>,
}

/// `POST /api/v1/statuses` request body (design.md's API Contract table:
/// "status, media_ids, poll, visibility, spoiler_text, in_reply_to_id,
/// language").
#[derive(Debug, Deserialize)]
pub struct CreateStatusRequest {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub media_ids: Vec<String>,
    #[serde(default)]
    pub poll: Option<CreatePollRequest>,
    #[serde(default)]
    pub visibility: Option<String>,
    #[serde(default)]
    pub spoiler_text: String,
    #[serde(default)]
    pub sensitive: bool,
    #[serde(default)]
    pub in_reply_to_id: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
}

/// `PUT /api/v1/statuses/:id` request body (design.md's API Contract table:
/// "編集内容"; `StatusService::edit_status`'s own `EditStatus` shape).
#[derive(Debug, Deserialize)]
pub struct EditStatusRequest {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub spoiler_text: String,
    #[serde(default)]
    pub sensitive: bool,
    #[serde(default)]
    pub media_ids: Vec<String>,
}

/// `POST /api/v1/polls/:id/votes` request body (design.md's API Contract
/// table: "choices\[\]").
#[derive(Debug, Deserialize)]
pub struct VoteRequest {
    #[serde(default)]
    pub choices: Vec<i32>,
}

fn parse_visibility(raw: &str) -> Result<Visibility, AppError> {
    match raw {
        "public" => Ok(Visibility::Public),
        "unlisted" => Ok(Visibility::Unlisted),
        "private" => Ok(Visibility::Private),
        "direct" => Ok(Visibility::Direct),
        other => Err(rejected(format!(
            "visibility must be one of \"public\"/\"unlisted\"/\"private\"/\"direct\", got {other:?}"
        ))),
    }
}

fn parse_media_ids(raw: &[String]) -> Result<Vec<Id>, AppError> {
    raw.iter()
        .map(|value| {
            value
                .parse::<i64>()
                .map(Id::from_i64)
                .map_err(|_| rejected(format!("invalid media id {value:?}")))
        })
        .collect()
}

// ---- Handlers ----

/// `POST /api/v1/statuses` (design.md's API Contract table): `write:statuses`
/// scope (Requirement 3.1), optional `Idempotency-Key` header (Requirement
/// 5.1), delegating to `StatusService::create_status` (task 5.1) for
/// content/media/poll/reply validation (422 on empty content, Requirement
/// 3.2) and creation.
pub async fn create_status<A, D, L, H, R, M>(
    State(state): State<StatusesEndpointsState<A, D, L, H, R, M>>,
    RequiredActor(ctx): RequiredActor,
    ResolvedOrigin(origin): ResolvedOrigin,
    headers: HeaderMap,
    Json(body): Json<CreateStatusRequest>,
) -> Result<Response, AppError>
where
    A: ActorHandleLookup + 'static,
    D: LocalActorLookup + 'static,
    L: DeliverySink + 'static,
    H: DeliverySink + 'static,
    R: RelationshipQuery + 'static,
    M: MentionLookup + 'static,
{
    require_scope(&ctx, &write_statuses_scope())?;

    let idem = header_str(&headers, "idempotency-key").map(str::to_string);
    let media_ids = parse_media_ids(&body.media_ids)?;
    let in_reply_to_id = match body.in_reply_to_id {
        Some(raw) => Some(
            raw.parse::<i64>()
                .map(Id::from_i64)
                .map_err(|_| rejected(format!("invalid in_reply_to_id {raw:?}")))?,
        ),
        None => None,
    };
    let visibility = match &body.visibility {
        Some(raw) => parse_visibility(raw)?,
        None => Visibility::Public,
    };
    let poll = match body.poll {
        Some(poll) => {
            let expires_at = poll
                .expires_in
                .map(|secs| state.runtime.clock.now() + TimeDuration::seconds(secs));
            Some(CreateStatusPoll {
                options: poll.options,
                multiple: poll.multiple,
                expires_at,
            })
        }
        None => None,
    };

    let input = CreateStatus {
        content: body.status,
        visibility,
        spoiler_text: body.spoiler_text,
        sensitive: body.sensitive,
        media_ids,
        in_reply_to_id,
        language: body.language,
        poll,
    };

    let status = state
        .status_service
        .create_status(ctx.actor_id, input, idem.as_deref())
        .await?;
    let body = state
        .render_status_json(Some(ctx.actor_id), status, &origin)
        .await?;
    Ok((StatusCode::OK, Json(body)).into_response())
}

/// `GET /api/v1/statuses/:id` (design.md's API Contract table): optional
/// Bearer, delegating to `StatusService::show` (Requirement 6.1), `404` for
/// an unknown or invisible post (Requirement 6.4).
pub async fn show_status<A, D, L, H, R, M>(
    State(state): State<StatusesEndpointsState<A, D, L, H, R, M>>,
    OptionalActor(ctx): OptionalActor,
    ResolvedOrigin(origin): ResolvedOrigin,
    Path(id_raw): Path<String>,
) -> Result<Response, AppError>
where
    A: ActorHandleLookup + 'static,
    D: LocalActorLookup + 'static,
    L: DeliverySink + 'static,
    H: DeliverySink + 'static,
    R: RelationshipQuery + 'static,
    M: MentionLookup + 'static,
{
    let id = parse_id(&id_raw)?;
    let viewer = ctx.map(|c| c.actor_id);
    let status = state
        .status_service
        .show(viewer, id)
        .await?
        .ok_or_else(not_found)?;
    let body = state.render_status_json(viewer, status, &origin).await?;
    Ok((StatusCode::OK, Json(body)).into_response())
}

/// `DELETE /api/v1/statuses/:id` (design.md's API Contract table):
/// owner-scoped `write:statuses` (Requirement 7.1), `404` for a non-owned or
/// unknown post, `422` for a reblog row (see `StatusService::delete_status`'s
/// own doc comment). See this module's doc comment ("Delete's response...")
/// for what the rendered response reflects.
pub async fn delete_status<A, D, L, H, R, M>(
    State(state): State<StatusesEndpointsState<A, D, L, H, R, M>>,
    RequiredActor(ctx): RequiredActor,
    ResolvedOrigin(origin): ResolvedOrigin,
    Path(id_raw): Path<String>,
) -> Result<Response, AppError>
where
    A: ActorHandleLookup + 'static,
    D: LocalActorLookup + 'static,
    L: DeliverySink + 'static,
    H: DeliverySink + 'static,
    R: RelationshipQuery + 'static,
    M: MentionLookup + 'static,
{
    require_scope(&ctx, &write_statuses_scope())?;
    let id = parse_id(&id_raw)?;
    let deleted = state.status_service.delete_status(ctx.actor_id, id).await?;
    let body = state
        .render_status_json(Some(ctx.actor_id), deleted, &origin)
        .await?;
    Ok((StatusCode::OK, Json(body)).into_response())
}

/// `PUT /api/v1/statuses/:id` (design.md's API Contract table): owner-scoped
/// `write:statuses` (Requirement 8.1), delegating validation/persistence to
/// `StatusService::edit_status`.
pub async fn edit_status<A, D, L, H, R, M>(
    State(state): State<StatusesEndpointsState<A, D, L, H, R, M>>,
    RequiredActor(ctx): RequiredActor,
    ResolvedOrigin(origin): ResolvedOrigin,
    Path(id_raw): Path<String>,
    Json(body): Json<EditStatusRequest>,
) -> Result<Response, AppError>
where
    A: ActorHandleLookup + 'static,
    D: LocalActorLookup + 'static,
    L: DeliverySink + 'static,
    H: DeliverySink + 'static,
    R: RelationshipQuery + 'static,
    M: MentionLookup + 'static,
{
    require_scope(&ctx, &write_statuses_scope())?;
    let id = parse_id(&id_raw)?;
    let media_ids = parse_media_ids(&body.media_ids)?;
    let input = EditStatus {
        content: body.status,
        spoiler_text: body.spoiler_text,
        sensitive: body.sensitive,
        media_ids,
    };
    let updated = state
        .status_service
        .edit_status(ctx.actor_id, id, input)
        .await?;
    let body = state
        .render_status_json(Some(ctx.actor_id), updated, &origin)
        .await?;
    Ok((StatusCode::OK, Json(body)).into_response())
}

/// One [`StatusEdit`] rendered for `GET /api/v1/statuses/:id/history` — see
/// this module's doc comment ("`StatusEdit`/`StatusSource` JSON shapes").
#[derive(Debug, serde::Serialize)]
pub struct StatusEditJson {
    pub content: String,
    pub spoiler_text: String,
    pub sensitive: bool,
    pub created_at: String,
}

fn edit_to_json(edit: &StatusEdit) -> StatusEditJson {
    StatusEditJson {
        content: edit.content.clone(),
        spoiler_text: edit.spoiler_text.clone(),
        sensitive: edit.sensitive,
        created_at: format_time(edit.created_at),
    }
}

/// `GET /api/v1/statuses/:id/history` (design.md's API Contract table):
/// optional Bearer (Requirement 8.2), delegating to `StatusService::history`.
pub async fn status_history<A, D, L, H, R, M>(
    State(state): State<StatusesEndpointsState<A, D, L, H, R, M>>,
    OptionalActor(ctx): OptionalActor,
    Path(id_raw): Path<String>,
) -> Result<Response, AppError>
where
    A: ActorHandleLookup + 'static,
    D: LocalActorLookup + 'static,
    L: DeliverySink + 'static,
    H: DeliverySink + 'static,
    R: RelationshipQuery + 'static,
    M: MentionLookup + 'static,
{
    let id = parse_id(&id_raw)?;
    let viewer = ctx.map(|c| c.actor_id);
    let edits = state.status_service.history(viewer, id).await?;
    let body: Vec<StatusEditJson> = edits.iter().map(edit_to_json).collect();
    Ok((StatusCode::OK, Json(body)).into_response())
}

/// `StatusSource` JSON shape — see this module's doc comment
/// ("`StatusEdit`/`StatusSource` JSON shapes").
#[derive(Debug, serde::Serialize)]
pub struct StatusSourceJson {
    pub id: Id,
    pub text: String,
    pub spoiler_text: String,
}

/// `GET /api/v1/statuses/:id/source` (design.md's API Contract table):
/// `read:statuses` scope (see this module's doc comment, "`source`'s
/// required scope") plus owner-scoping enforced inside
/// `StatusService::source` itself.
pub async fn status_source<A, D, L, H, R, M>(
    State(state): State<StatusesEndpointsState<A, D, L, H, R, M>>,
    RequiredActor(ctx): RequiredActor,
    Path(id_raw): Path<String>,
) -> Result<Response, AppError>
where
    A: ActorHandleLookup + 'static,
    D: LocalActorLookup + 'static,
    L: DeliverySink + 'static,
    H: DeliverySink + 'static,
    R: RelationshipQuery + 'static,
    M: MentionLookup + 'static,
{
    require_scope(&ctx, &read_statuses_scope())?;
    let id = parse_id(&id_raw)?;
    let source = state.status_service.source(ctx.actor_id, id).await?;
    let body = StatusSourceJson {
        id,
        text: source.text,
        spoiler_text: source.spoiler_text,
    };
    Ok((StatusCode::OK, Json(body)).into_response())
}

/// `GET /api/v1/statuses/:id/context` (design.md's API Contract table):
/// optional Bearer (Requirement 6.2), delegating to `StatusService::context`.
pub async fn status_context<A, D, L, H, R, M>(
    State(state): State<StatusesEndpointsState<A, D, L, H, R, M>>,
    OptionalActor(ctx): OptionalActor,
    ResolvedOrigin(origin): ResolvedOrigin,
    Path(id_raw): Path<String>,
) -> Result<Response, AppError>
where
    A: ActorHandleLookup + 'static,
    D: LocalActorLookup + 'static,
    L: DeliverySink + 'static,
    H: DeliverySink + 'static,
    R: RelationshipQuery + 'static,
    M: MentionLookup + 'static,
{
    let id = parse_id(&id_raw)?;
    let viewer = ctx.map(|c| c.actor_id);
    let context = state.status_service.context(viewer, id).await?;

    let mut ancestors = Vec::with_capacity(context.ancestors.len());
    for status in context.ancestors {
        ancestors.push(state.render_status_json(viewer, status, &origin).await?);
    }
    let mut descendants = Vec::with_capacity(context.descendants.len());
    for status in context.descendants {
        descendants.push(state.render_status_json(viewer, status, &origin).await?);
    }

    let body = json!({ "ancestors": ancestors, "descendants": descendants });
    Ok((StatusCode::OK, Json(body)).into_response())
}

/// `POST /api/v1/statuses/:id/reblog` (design.md's API Contract table):
/// `write:statuses` scope (Requirement 9.1), delegating to
/// `InteractionService::reblog`, rendering the newly-created boost `Status`.
pub async fn reblog_status<A, D, L, H, R, M>(
    State(state): State<StatusesEndpointsState<A, D, L, H, R, M>>,
    RequiredActor(ctx): RequiredActor,
    ResolvedOrigin(origin): ResolvedOrigin,
    Path(id_raw): Path<String>,
) -> Result<Response, AppError>
where
    A: ActorHandleLookup + 'static,
    D: LocalActorLookup + 'static,
    L: DeliverySink + 'static,
    H: DeliverySink + 'static,
    R: RelationshipQuery + 'static,
    M: MentionLookup + 'static,
{
    require_scope(&ctx, &write_statuses_scope())?;
    let id = parse_id(&id_raw)?;
    let reblog = state.interaction_service.reblog(ctx.actor_id, id).await?;
    let body = state
        .render_status_json(Some(ctx.actor_id), reblog, &origin)
        .await?;
    Ok((StatusCode::OK, Json(body)).into_response())
}

/// `POST /api/v1/statuses/:id/unreblog` (design.md's API Contract table):
/// `write:statuses` scope (Requirement 9.4), delegating to
/// `InteractionService::unreblog`, rendering the (possibly just-updated)
/// target `Status`.
pub async fn unreblog_status<A, D, L, H, R, M>(
    State(state): State<StatusesEndpointsState<A, D, L, H, R, M>>,
    RequiredActor(ctx): RequiredActor,
    ResolvedOrigin(origin): ResolvedOrigin,
    Path(id_raw): Path<String>,
) -> Result<Response, AppError>
where
    A: ActorHandleLookup + 'static,
    D: LocalActorLookup + 'static,
    L: DeliverySink + 'static,
    H: DeliverySink + 'static,
    R: RelationshipQuery + 'static,
    M: MentionLookup + 'static,
{
    require_scope(&ctx, &write_statuses_scope())?;
    let id = parse_id(&id_raw)?;
    let target = state.interaction_service.unreblog(ctx.actor_id, id).await?;
    let body = state
        .render_status_json(Some(ctx.actor_id), target, &origin)
        .await?;
    Ok((StatusCode::OK, Json(body)).into_response())
}

/// `POST /api/v1/statuses/:id/favourite` (design.md's API Contract table):
/// `write:favourites` scope (Requirement 10.1), delegating to
/// `InteractionService::favourite`.
pub async fn favourite_status<A, D, L, H, R, M>(
    State(state): State<StatusesEndpointsState<A, D, L, H, R, M>>,
    RequiredActor(ctx): RequiredActor,
    ResolvedOrigin(origin): ResolvedOrigin,
    Path(id_raw): Path<String>,
) -> Result<Response, AppError>
where
    A: ActorHandleLookup + 'static,
    D: LocalActorLookup + 'static,
    L: DeliverySink + 'static,
    H: DeliverySink + 'static,
    R: RelationshipQuery + 'static,
    M: MentionLookup + 'static,
{
    require_scope(&ctx, &write_favourites_scope())?;
    let id = parse_id(&id_raw)?;
    let target = state
        .interaction_service
        .favourite(ctx.actor_id, id)
        .await?;
    let body = state
        .render_status_json(Some(ctx.actor_id), target, &origin)
        .await?;
    Ok((StatusCode::OK, Json(body)).into_response())
}

/// `POST /api/v1/statuses/:id/unfavourite` (design.md's API Contract
/// table): `write:favourites` scope (Requirement 10.3), delegating to
/// `InteractionService::unfavourite`.
pub async fn unfavourite_status<A, D, L, H, R, M>(
    State(state): State<StatusesEndpointsState<A, D, L, H, R, M>>,
    RequiredActor(ctx): RequiredActor,
    ResolvedOrigin(origin): ResolvedOrigin,
    Path(id_raw): Path<String>,
) -> Result<Response, AppError>
where
    A: ActorHandleLookup + 'static,
    D: LocalActorLookup + 'static,
    L: DeliverySink + 'static,
    H: DeliverySink + 'static,
    R: RelationshipQuery + 'static,
    M: MentionLookup + 'static,
{
    require_scope(&ctx, &write_favourites_scope())?;
    let id = parse_id(&id_raw)?;
    let target = state
        .interaction_service
        .unfavourite(ctx.actor_id, id)
        .await?;
    let body = state
        .render_status_json(Some(ctx.actor_id), target, &origin)
        .await?;
    Ok((StatusCode::OK, Json(body)).into_response())
}

/// `POST /api/v1/statuses/:id/bookmark` (design.md's API Contract table):
/// `write:bookmarks` scope (Requirement 11.1), delegating to
/// `InteractionService::bookmark(..., on: true)`.
pub async fn bookmark_status<A, D, L, H, R, M>(
    State(state): State<StatusesEndpointsState<A, D, L, H, R, M>>,
    RequiredActor(ctx): RequiredActor,
    ResolvedOrigin(origin): ResolvedOrigin,
    Path(id_raw): Path<String>,
) -> Result<Response, AppError>
where
    A: ActorHandleLookup + 'static,
    D: LocalActorLookup + 'static,
    L: DeliverySink + 'static,
    H: DeliverySink + 'static,
    R: RelationshipQuery + 'static,
    M: MentionLookup + 'static,
{
    require_scope(&ctx, &write_bookmarks_scope())?;
    let id = parse_id(&id_raw)?;
    let target = state
        .interaction_service
        .bookmark(ctx.actor_id, id, true)
        .await?;
    let body = state
        .render_status_json(Some(ctx.actor_id), target, &origin)
        .await?;
    Ok((StatusCode::OK, Json(body)).into_response())
}

/// `POST /api/v1/statuses/:id/unbookmark` (design.md's API Contract table):
/// `write:bookmarks` scope (Requirement 11.2), delegating to
/// `InteractionService::bookmark(..., on: false)`.
pub async fn unbookmark_status<A, D, L, H, R, M>(
    State(state): State<StatusesEndpointsState<A, D, L, H, R, M>>,
    RequiredActor(ctx): RequiredActor,
    ResolvedOrigin(origin): ResolvedOrigin,
    Path(id_raw): Path<String>,
) -> Result<Response, AppError>
where
    A: ActorHandleLookup + 'static,
    D: LocalActorLookup + 'static,
    L: DeliverySink + 'static,
    H: DeliverySink + 'static,
    R: RelationshipQuery + 'static,
    M: MentionLookup + 'static,
{
    require_scope(&ctx, &write_bookmarks_scope())?;
    let id = parse_id(&id_raw)?;
    let target = state
        .interaction_service
        .bookmark(ctx.actor_id, id, false)
        .await?;
    let body = state
        .render_status_json(Some(ctx.actor_id), target, &origin)
        .await?;
    Ok((StatusCode::OK, Json(body)).into_response())
}

/// `POST /api/v1/statuses/:id/pin` (design.md's API Contract table):
/// `write:statuses` scope (Requirement 12.1), delegating to
/// `InteractionService::pin(..., on: true)` (422 for a `direct`-visibility
/// target, Requirement 12.4).
pub async fn pin_status<A, D, L, H, R, M>(
    State(state): State<StatusesEndpointsState<A, D, L, H, R, M>>,
    RequiredActor(ctx): RequiredActor,
    ResolvedOrigin(origin): ResolvedOrigin,
    Path(id_raw): Path<String>,
) -> Result<Response, AppError>
where
    A: ActorHandleLookup + 'static,
    D: LocalActorLookup + 'static,
    L: DeliverySink + 'static,
    H: DeliverySink + 'static,
    R: RelationshipQuery + 'static,
    M: MentionLookup + 'static,
{
    require_scope(&ctx, &write_statuses_scope())?;
    let id = parse_id(&id_raw)?;
    let target = state
        .interaction_service
        .pin(ctx.actor_id, id, true)
        .await?;
    let body = state
        .render_status_json(Some(ctx.actor_id), target, &origin)
        .await?;
    Ok((StatusCode::OK, Json(body)).into_response())
}

/// `POST /api/v1/statuses/:id/unpin` (design.md's API Contract table):
/// `write:statuses` scope (Requirement 12.2), delegating to
/// `InteractionService::pin(..., on: false)`.
pub async fn unpin_status<A, D, L, H, R, M>(
    State(state): State<StatusesEndpointsState<A, D, L, H, R, M>>,
    RequiredActor(ctx): RequiredActor,
    ResolvedOrigin(origin): ResolvedOrigin,
    Path(id_raw): Path<String>,
) -> Result<Response, AppError>
where
    A: ActorHandleLookup + 'static,
    D: LocalActorLookup + 'static,
    L: DeliverySink + 'static,
    H: DeliverySink + 'static,
    R: RelationshipQuery + 'static,
    M: MentionLookup + 'static,
{
    require_scope(&ctx, &write_statuses_scope())?;
    let id = parse_id(&id_raw)?;
    let target = state
        .interaction_service
        .pin(ctx.actor_id, id, false)
        .await?;
    let body = state
        .render_status_json(Some(ctx.actor_id), target, &origin)
        .await?;
    Ok((StatusCode::OK, Json(body)).into_response())
}

/// `GET /api/v1/bookmarks`'s wire-level pagination query parameters — see
/// `crate::accounts::endpoints::StatusesQueryParams`'s identical
/// established pattern (every field, including `limit`, stays
/// `Option<String>` so a malformed value renders a `422` `AppError` rather
/// than axum's own plain-text `QueryRejection`).
#[derive(Debug, Deserialize)]
pub struct BookmarksQueryParams {
    #[serde(default)]
    pub max_id: Option<String>,
    #[serde(default)]
    pub since_id: Option<String>,
    #[serde(default)]
    pub min_id: Option<String>,
    #[serde(default)]
    pub limit: Option<String>,
}

/// `GET /api/v1/bookmarks` (design.md's API Contract table): `read:bookmarks`
/// scope (Requirement 11.3), delegating to
/// `InteractionService::list_bookmarks`, attaching a `Link` header built
/// from the resolved page's cursors (mirroring
/// `accounts::endpoints::list_statuses`'s identical pattern).
pub async fn list_bookmarks<A, D, L, H, R, M>(
    State(state): State<StatusesEndpointsState<A, D, L, H, R, M>>,
    RequiredActor(ctx): RequiredActor,
    ResolvedOrigin(origin): ResolvedOrigin,
    Query(params): Query<BookmarksQueryParams>,
) -> Result<Response, AppError>
where
    A: ActorHandleLookup + 'static,
    D: LocalActorLookup + 'static,
    L: DeliverySink + 'static,
    H: DeliverySink + 'static,
    R: RelationshipQuery + 'static,
    M: MentionLookup + 'static,
{
    require_scope(&ctx, &read_bookmarks_scope())?;
    let limit = parse_optional_limit(params.limit.as_deref())?;

    let page_params = PageParams {
        max_id: params.max_id.clone(),
        since_id: params.since_id.clone(),
        min_id: params.min_id.clone(),
        limit,
    };
    let page = state
        .interaction_service
        .list_bookmarks(ctx.actor_id, page_params)
        .await?;

    let mut items = Vec::with_capacity(page.items.len());
    for status in page.items.clone() {
        items.push(
            state
                .render_status_json(Some(ctx.actor_id), status, &origin)
                .await?,
        );
    }

    let mut uri_ctx = RequestUriContext::new(origin, BOOKMARKS_PATH.to_string());
    if let Some(limit) = limit {
        uri_ctx = uri_ctx.with_query("limit", limit.to_string());
    }
    let link_header = build_link_header(&uri_ctx, &page.cursors());

    let mut response = (StatusCode::OK, Json(items)).into_response();
    if let Some(link) = link_header {
        response.headers_mut().insert(header::LINK, link);
    }
    Ok(response)
}

/// `GET /api/v1/polls/:id` (design.md's API Contract table): optional Bearer,
/// delegating to `PollService::poll`.
pub async fn show_poll<A, D, L, H, R, M>(
    State(state): State<StatusesEndpointsState<A, D, L, H, R, M>>,
    OptionalActor(ctx): OptionalActor,
    Path(id_raw): Path<String>,
) -> Result<Response, AppError>
where
    A: ActorHandleLookup + 'static,
    D: LocalActorLookup + 'static,
    L: DeliverySink + 'static,
    H: DeliverySink + 'static,
    R: RelationshipQuery + 'static,
    M: MentionLookup + 'static,
{
    let id = parse_id(&id_raw)?;
    let viewer = ctx.map(|c| c.actor_id);
    let (poll, tally) = state.poll_service.poll(viewer, id).await?;
    let body = state
        .assembler()
        .render_poll(&poll, &tally, viewer, state.runtime.clock.now())
        .await?;
    Ok((StatusCode::OK, Json(body)).into_response())
}

/// `POST /api/v1/polls/:id/votes` (design.md's API Contract table):
/// `write:statuses` scope (Requirement 13.2), delegating to
/// `PollService::vote` (deadline/range/single-vs-multiple/duplicate
/// validation, Requirements 13.3-13.5).
pub async fn vote_poll<A, D, L, H, R, M>(
    State(state): State<StatusesEndpointsState<A, D, L, H, R, M>>,
    RequiredActor(ctx): RequiredActor,
    Path(id_raw): Path<String>,
    Json(body): Json<VoteRequest>,
) -> Result<Response, AppError>
where
    A: ActorHandleLookup + 'static,
    D: LocalActorLookup + 'static,
    L: DeliverySink + 'static,
    H: DeliverySink + 'static,
    R: RelationshipQuery + 'static,
    M: MentionLookup + 'static,
{
    require_scope(&ctx, &write_statuses_scope())?;
    let id = parse_id(&id_raw)?;
    let (poll, tally) = state
        .poll_service
        .vote(ctx.actor_id, id, &body.choices)
        .await?;
    // CONCERN: this renders with no custom emoji resolved (`&[]`), while
    // `get_poll` above resolves them from the option titles. The two
    // therefore disagree on `poll.emojis` for the same poll depending on
    // whether the client just voted. Preserved verbatim here because this
    // refactor is behavior-preserving by construction; flagged for a
    // follow-up that is allowed to change what clients observe.
    let ctx_ser = SerializeContext {
        viewer: Some(ctx.actor_id),
        now: state.runtime.clock.now(),
    };
    let body = poll_to_json(&poll, &tally, &[], &ctx_ser);
    Ok((StatusCode::OK, Json(body)).into_response())
}
