//! `NotificationService` (design.md "Service / サービス層" ->
//! `NotificationService`; Requirements 2.1, 2.2, 2.3, 2.4, 3.1, 3.2, 4.1,
//! 4.2, 4.3, 4.4; task 3.2, `Boundary: NotificationService`): aggregates the
//! notification retrieval/dismissal business — list (pagination + type/
//! account filter + dismissed exclusion + serialization), single fetch
//! (other-recipient/nonexistent -> 404), dismiss, and clear.
//!
//! ## Scope
//! This module owns exactly [`NotificationService`] and its four public
//! methods ([`NotificationService::list`]/[`NotificationService::show`]/
//! [`NotificationService::dismiss`]/[`NotificationService::clear`]),
//! design.md's own Service Interface reproduced verbatim (`&self, ctx:
//! &RequestActorContext, ...`). It composes — never reimplements —
//! [`crate::notifications::repository`] (task 1.3) and
//! [`crate::notifications::serializer`] (task 2.1), plus the upstream
//! `account`/`status` embed serializers this module's own dispatch brief
//! names (`crate::accounts::serializer::account_to_json`'s higher-level
//! entry point, `AccountService::show_account`, and `crate::statuses::
//! serializer::status_to_json`). No HTTP surface (`NotificationEndpoints`,
//! task 4.1 — no `account_id` string-to-`AccountRef` resolution lives here,
//! see below), and no `AppState`/bootstrap/router wiring (task 4.x) live
//! here either.
//!
//! ## `ListFilter.account_id` arrives pre-resolved (mirrors `repository.rs`'s
//! own discipline one layer up)
//! `repository.rs`'s own `ListFilter` doc comment states plainly: "本層では
//! ID 解決を行わない" (`account_id` is an already-resolved [`AccountRef`],
//! not a string). This service's own [`NotificationService::list`] takes
//! design.md's literal `filter: ListFilter` (`crate::notifications::
//! repository::ListFilter`) unchanged — the same discipline applies one
//! layer up: string-id-to-`AccountRef` resolution (and the "unknown id ->
//! 200 + empty array, not 404" rule design.md's `NotificationEndpoints`
//! section documents) is `NotificationEndpoints`'s job, task 4.1, strictly
//! out of this task's boundary.
//!
//! ## Deliberate deviations from design.md's literal Service Interface
//! design.md's sketch (lines ~433-438) is:
//! ```text
//! pub async fn list(&self, ctx: &RequestActorContext, page: PageParams, filter: ListFilter) -> Result<Page<serde_json::Value>, AppError>;
//! pub async fn show(&self, ctx: &RequestActorContext, id: Id) -> Result<serde_json::Value, AppError>;
//! pub async fn dismiss(&self, ctx: &RequestActorContext, id: Id) -> Result<(), AppError>;
//! pub async fn clear(&self, ctx: &RequestActorContext) -> Result<(), AppError>;
//! ```
//! Unlike `TimelineService::timeline`'s/`FollowRequestService`'s own
//! documented deviation away from `RequestActorContext` (both take a bare
//! `Id`/`Option<Id>` instead, to avoid an `oauth`-module dependency purely
//! to immediately discard everything else the context carries), this
//! module keeps `ctx: &RequestActorContext` exactly as design.md specifies:
//! this task's own dispatch brief explicitly names `&RequestActorContext`
//! as the design constraint to follow, and `crate::accounts::
//! account_service::AccountService::show_account` (this module's own
//! account-embed collaborator) already threads `Option<&RequestActorContext>`
//! through its own signature — an `oauth`-module dependency already exists
//! on this module's dependency path regardless, so there is no "avoid a new
//! dependency" reason to deviate here the way the two sibling services had.
//! Every method extracts `ctx.actor_id` as the recipient/viewer, and reads
//! no other field of `ctx` (scope verification is `NotificationEndpoints`'s
//! job, task 4.1, per design.md's own `NotificationEndpoints` Responsibilities
//! entry: "スコープ: ... Bearer + Scope は api-foundation 再利用").
//!
//! ## Rendering `account`/`status`: a self-contained `Status` render glue,
//! duplicated from `account_provider.rs`'s own precedent (CONCERN)
//! [`crate::notifications::serializer::NotificationRenderInput`] needs the
//! origin `account` and, for post-related kinds, the related `status`
//! **already rendered** as JSON (that module's own doc comment,
//! "Deliberate deviations": "the pre-rendered `Value`s a caller...is
//! expected to have produced...before calling this module"). The `account`
//! half is simple: `AccountService::show_account` already resolves a bare
//! numeric id — local or remote — into full Account JSON (this module's own
//! dispatch brief flags this as the intended embedding path; `crate::
//! accounts::account_service::AccountService::show_account`'s own doc
//! comment: "Resolves `id` — local, known-remote..."), so
//! [`NotificationService::account_json`] is a thin wrapper over it.
//!
//! The `status` half has no equivalent one-call entry point:
//! `crate::statuses::status_service::StatusService::show` returns a bare
//! [`crate::statuses::model::Status`], not rendered JSON — turning that into
//! full Status JSON (`account`/`media_attachments`/`tags`/`poll`/
//! `interactions`, one nested level for a `reblog`) is exactly the assembly
//! glue `crate::statuses::account_provider::AccountStatusesProviderImpl::
//! render`/`leaf_render_input` (statuses-core task 9.1, this crate's closest
//! structural precedent — resolving a `Status` into JSON several layers away
//! from a live HTTP request, no per-request `ForwardedOrigin` available
//! either) already had to solve, and that module's own doc comment
//! documents *why* it could not reuse `crate::statuses::endpoints`'s own
//! private `render_status_json`/`resolve_common`/`leaf_render_input`
//! methods (private to a router-local, still-generic state bundle this
//! module has no more reason to parameterize over than `account_provider.rs`
//! did) and instead wrote its own small, self-contained equivalent, reusing
//! the exact same underlying repository/serializer functions. This module
//! follows that identical, already-reviewed judgment call rather than
//! inventing a third shape: [`NotificationService::render_status`]/
//! [`NotificationService::leaf_render_input`] duplicate `account_provider.rs`'s
//! own assembly glue almost verbatim (same repository calls, same
//! `StatusRenderInput` construction) — the same "small helper duplication
//! across sibling modules is this crate's own documented convention" this
//! spec's own Implementation Notes already invoke elsewhere (`UndoKind`,
//! `format_time`, `account_provider.rs` itself for
//! `account_kind`/`account_id`/`account_ref_from`). Flagged here as a CONCERN
//! for reviewer confirmation, not a silent gap: a fourth call site
//! duplicating this exact assembly a future task adds might be the signal to
//! finally extract a shared, `pub(crate)` helper.
//!
//! One narrowing versus `account_provider.rs`'s own `render`: this module's
//! [`NotificationService::render_status`] does **not** re-check
//! `visibility::is_visible` on a nested reblog target (`account_provider.rs`'s
//! own `self.visible_to(&target, viewer)` guard). A notification's
//! `status_id` was only ever attached at generation time to a recipient
//! legitimately entitled to see it (Requirement 1.2's "受信者視点" is
//! about *whose* interaction state is embedded, not a fresh visibility
//! re-check — no requirement in this spec's Requirement 1/2/3/4 asks this
//! service to re-derive statuses-core's own visibility policy, and doing so
//! would pull in `RelationshipQueryRegistry` purely for one nested field this
//! task's own Requirements do not exercise). Flagged as a CONCERN alongside
//! the duplication above, not silently dropped.
//!
//! ## Dangling references are not errors
//! A notification's `origin`/`status_id` are logical references
//! (`repository.rs`'s own doc comment on the physical model: "受信者・通知
//! 元・対象投稿は論理参照"). If the referenced status has since been deleted,
//! [`NotificationService::status_json`] returns `Ok(None)` rather than
//! propagating `crate::statuses::status_repository::find_by_id`'s `None` as
//! an error — mirrors `account_provider.rs::render`'s own "missing
//! referenced row is not a hydration failure" precedent for exactly the
//! same situation (a dangling reblog target). The origin *account* is not
//! given the same treatment: every notification's `origin` is written by
//! `NotificationGenerator` from an `event.origin` that was itself already a
//! real `AccountRef` at generation time, and unlike a post, an account row
//! is never hard-deleted in this crate (no delete path exists for either
//! `actor`/`accounts` or `remote_accounts`) — so `account_json`'s own
//! `AccountService::show_account` call is allowed to propagate its very-
//! unlikely-in-practice 404 as this method's own error, exactly like every
//! other embed call in this module.
//!
//! ## 404 for other-recipient/nonexistent (Requirements 3.2, 4.3)
//! [`NotificationService::show`] maps `repository::find_for_recipient`'s
//! `None` (covers "does not exist", "belongs to another recipient", and
//! "already dismissed" all at once — that function's own doc comment) to a
//! `404`-shaped [`AppError`], mirroring this crate's established
//! `AppError::client(StatusCode::NOT_FOUND, ...)` convention
//! (`follow_request_service.rs::account_not_found`/`no_pending_request`,
//! `account_provider.rs::not_found`). [`NotificationService::dismiss`] maps
//! `repository::dismiss`'s `Ok(false)` (that function's own doc comment:
//! "the service layer, task 3.2, is expected to map this to Requirement
//! 4.3's 404") the same way — `repository::dismiss` never distinguishes
//! "nonexistent" from "exists but belongs to someone else" either, so this
//! module cannot and does not either; both collapse to the same 404, exactly
//! as Requirement 3.2/4.3's own wording ("要求アクター宛でない、または存在
//! しないとき") specifies without asking for a distinguishing error.
//!
//! ## `clear`: always succeeds, no notion of "nothing to clear" (Requirement
//! 4.1)
//! `repository::clear` is unconditionally idempotent (that function's own
//! doc comment: "always succeeds (including when `recipient` has no
//! notifications at all)") — this module adds no additional existence check
//! before calling it, mirroring `repository.rs`'s own "no redundant
//! pre-check" discipline `generator.rs`'s doc comment already establishes
//! for `insert_dedup`.

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::Value;
use sqlx::PgPool;

use crate::accounts::account_service::AccountService;
use crate::api::pagination::{ForwardedOrigin, Page, PageParams};
use crate::domain::{AccountRef, Id};
use crate::error::AppError;
use crate::federation::signatures::ReqwestFederationHttpClient;
use crate::media::LocalFsStore;
use crate::media::media_repository;
use crate::media::serializer::to_media_attachment;
use crate::notifications::model::Notification;
use crate::notifications::repository::{self, ListFilter};
use crate::notifications::serializer::{NotificationRenderInput, notification_to_json};
use crate::oauth::model::RequestActorContext;
use crate::runtime::RuntimeContext;
use crate::statuses::interaction_repository;
use crate::statuses::model::Status;
use crate::statuses::poll_repository;
use crate::statuses::serializer::{
    SerializeContext as StatusSerializeContext, StatusInteractionState, StatusRenderInput, TagJson,
    poll_to_json, status_to_json,
};
use crate::statuses::status_repository;
use crate::statuses::tag_repository;

/// Requirements 3.2, 4.3's 404 — see this module's doc comment.
fn notification_not_found(id: Id) -> AppError {
    AppError::client(
        StatusCode::NOT_FOUND,
        format!("notification '{}' was not found", id.as_i64()),
    )
}

fn poll_not_found() -> AppError {
    AppError::client(StatusCode::NOT_FOUND, "poll not found")
}

/// The notification retrieval/dismissal business-service layer (design.md's
/// exact `NotificationService`). See this module's doc comment for the full
/// rationale and every deviation from design.md's literal sketch.
pub struct NotificationService {
    pool: PgPool,
    runtime: RuntimeContext,
    domain: String,
    accounts: Arc<AccountService<LocalFsStore, ReqwestFederationHttpClient>>,
    media_store: LocalFsStore,
}

impl NotificationService {
    /// Builds a `NotificationService` bound to `pool`/`runtime` (repository
    /// reads, poll `now`), `domain` (this instance's own configured server
    /// domain — see this module's doc comment, "Rendering `account`/
    /// `status`", for the synthesized-origin rationale it shares with
    /// `account_provider.rs`/`follow_request_service.rs`), `accounts`
    /// (Account-embed rendering, `crate::accounts::build_accounts_module`'s
    /// own `AccountService` handle — the same one `AppState::accounts()
    /// .service()` already exposes), and `media_store` (media-attachment URL
    /// rendering inside a related post's `status` embed,
    /// `crate::media::build_media_module`'s own `LocalFsStore` handle).
    pub fn new(
        pool: PgPool,
        runtime: RuntimeContext,
        domain: impl Into<String>,
        accounts: Arc<AccountService<LocalFsStore, ReqwestFederationHttpClient>>,
        media_store: LocalFsStore,
    ) -> Self {
        Self {
            pool,
            runtime,
            domain: domain.into(),
            accounts,
            media_store,
        }
    }

    /// See this module's doc comment ("Rendering `account`/`status`").
    fn origin(&self) -> ForwardedOrigin {
        ForwardedOrigin::resolve("https", &self.domain, None, None)
    }

    /// Resolves `account` (the notification's origin, or a related status's
    /// author) into full Account JSON via `AccountService::show_account` —
    /// see this module's doc comment ("Rendering `account`/`status`").
    async fn account_json(&self, id: Id, origin: &ForwardedOrigin) -> Result<Value, AppError> {
        self.accounts
            .show_account(&id.as_i64().to_string(), None, origin)
            .await
    }

    async fn media_json(
        &self,
        status_id: Id,
        origin: &ForwardedOrigin,
    ) -> Result<Vec<Value>, AppError> {
        let media_ids = status_repository::media_ids_for_status(&self.pool, status_id).await?;
        let mut out = Vec::with_capacity(media_ids.len());
        for media_id in media_ids {
            if let Some(media) = media_repository::find_by_id(&self.pool, media_id).await? {
                out.push(
                    serde_json::to_value(to_media_attachment(&media, &self.media_store, origin))
                        .expect("MediaAttachmentJson always serializes to JSON"),
                );
            }
        }
        Ok(out)
    }

    async fn tags_json(
        &self,
        status_id: Id,
        origin: &ForwardedOrigin,
    ) -> Result<Vec<TagJson>, AppError> {
        let tags = tag_repository::tags_for_status(&self.pool, status_id).await?;
        Ok(tags
            .into_iter()
            .map(|tag| TagJson {
                url: format!("{}://{}/tags/{}", origin.scheme, origin.host, tag.name),
                name: tag.name,
            })
            .collect())
    }

    /// The related post's interaction state, from `viewer` (the
    /// notification's recipient)'s own viewpoint (Requirement 1.2's "受信者
    /// 視点").
    async fn interaction_state(
        &self,
        viewer: Id,
        status_id: Id,
    ) -> Result<StatusInteractionState, AppError> {
        let favourited =
            interaction_repository::exists_favourite(&self.pool, viewer, status_id).await?;
        let bookmarked =
            interaction_repository::exists_bookmark(&self.pool, viewer, status_id).await?;
        let pinned = interaction_repository::exists_pin(&self.pool, viewer, status_id).await?;
        let reblogged = interaction_repository::find_reblog(&self.pool, viewer, status_id)
            .await?
            .is_some();
        Ok(StatusInteractionState {
            favourited,
            reblogged,
            bookmarked,
            pinned,
            muted: false,
        })
    }

    async fn poll_json(&self, viewer: Id, poll_id: Id) -> Result<Value, AppError> {
        let poll = poll_repository::find_poll_by_id(&self.pool, poll_id)
            .await?
            .ok_or_else(poll_not_found)?;
        let tally = poll_repository::tally(&self.pool, poll_id, Some(viewer)).await?;
        let ctx = StatusSerializeContext {
            viewer: Some(viewer),
            now: self.runtime.clock.now(),
        };
        Ok(poll_to_json(&poll, &tally, &[], &ctx))
    }

    /// Builds a non-recursive [`StatusRenderInput`] (its own `reblog` field
    /// always `None`) — used only for a reblog *target*, mirroring
    /// `account_provider.rs::leaf_render_input`'s identical "at most one
    /// level of nesting" precedent.
    async fn leaf_render_input<'a>(
        &self,
        viewer: Id,
        status: &'a Status,
        origin: &ForwardedOrigin,
    ) -> Result<StatusRenderInput<'a>, AppError> {
        let account = self.account_json(status.actor_id, origin).await?;
        let media_attachments = self.media_json(status.id, origin).await?;
        let tags = self.tags_json(status.id, origin).await?;
        let interactions = self.interaction_state(viewer, status.id).await?;
        let poll = match status.poll_id {
            Some(poll_id) => Some(self.poll_json(viewer, poll_id).await?),
            None => None,
        };
        Ok(StatusRenderInput {
            status,
            account,
            media_attachments,
            mentions: Vec::new(),
            tags,
            emojis: Vec::new(),
            poll,
            interactions,
            reblog: None,
        })
    }

    /// Resolves an already-fetched `status` into its full Mastodon-
    /// compatible JSON representation, from `viewer`'s own viewpoint — see
    /// this module's doc comment ("Rendering `account`/`status`") for why
    /// this duplicates `account_provider.rs::render` rather than reusing it,
    /// and for the one narrowing (no nested-reblog visibility re-check) this
    /// copy makes.
    async fn render_status(
        &self,
        viewer: Id,
        status: Status,
        origin: &ForwardedOrigin,
    ) -> Result<Value, AppError> {
        let reblog_target = match status.reblog_of_id {
            Some(target_id) => status_repository::find_by_id(&self.pool, target_id).await?,
            None => None,
        };
        let reblog_box = match &reblog_target {
            Some(target) => Some(Box::new(
                self.leaf_render_input(viewer, target, origin).await?,
            )),
            None => None,
        };
        let account = self.account_json(status.actor_id, origin).await?;
        let media_attachments = self.media_json(status.id, origin).await?;
        let tags = self.tags_json(status.id, origin).await?;
        let interactions = self.interaction_state(viewer, status.id).await?;
        let poll = match status.poll_id {
            Some(poll_id) => Some(self.poll_json(viewer, poll_id).await?),
            None => None,
        };
        let input = StatusRenderInput {
            status: &status,
            account,
            media_attachments,
            mentions: Vec::new(),
            tags,
            emojis: Vec::new(),
            poll,
            interactions,
            reblog: reblog_box,
        };
        Ok(status_to_json(&input))
    }

    /// Resolves `status_id` (a notification's optional related post) into
    /// rendered JSON, or `Ok(None)` both when there is no related post and
    /// when the referenced post has since been deleted — see this module's
    /// doc comment ("Dangling references are not errors").
    async fn status_json(
        &self,
        viewer: Id,
        status_id: Option<Id>,
        origin: &ForwardedOrigin,
    ) -> Result<Option<Value>, AppError> {
        let Some(status_id) = status_id else {
            return Ok(None);
        };
        let Some(status) = status_repository::find_by_id(&self.pool, status_id).await? else {
            return Ok(None);
        };
        Ok(Some(self.render_status(viewer, status, origin).await?))
    }

    /// Renders one already-fetched [`Notification`] into its full
    /// Notification JSON (account + status embeds resolved, then delegated
    /// to [`notification_to_json`] for the outer shell + null discipline —
    /// task 2.1's own boundary, not reimplemented here).
    async fn render_notification(
        &self,
        viewer: Id,
        notification: &Notification,
        origin: &ForwardedOrigin,
    ) -> Result<Value, AppError> {
        let origin_id = match notification.origin {
            AccountRef::Local(id) | AccountRef::Remote(id) => id,
        };
        let account = self.account_json(origin_id, origin).await?;
        let status = self
            .status_json(viewer, notification.status_id, origin)
            .await?;
        let input = NotificationRenderInput {
            notification,
            account,
            status,
        };
        Ok(notification_to_json(&input))
    }

    /// Returns `ctx.actor_id`'s notifications, newest-first, dismissed
    /// excluded, narrowed by `filter`, serialized (Requirements 2.1-2.4).
    /// `filter.account_id` arrives pre-resolved — see this module's doc
    /// comment ("`ListFilter.account_id` arrives pre-resolved").
    pub async fn list(
        &self,
        ctx: &RequestActorContext,
        page: PageParams,
        filter: ListFilter,
    ) -> Result<Page<Value>, AppError> {
        let recipient = ctx.actor_id;
        let page_result = repository::list(&self.pool, recipient, &page, &filter).await?;

        let origin = self.origin();
        let mut items = Vec::with_capacity(page_result.items.len());
        for notification in &page_result.items {
            items.push(
                self.render_notification(recipient, notification, &origin)
                    .await?,
            );
        }

        Ok(Page {
            items,
            prev_cursor: page_result.prev_cursor,
            next_cursor: page_result.next_cursor,
        })
    }

    /// Fetches a single notification `id` scoped to `ctx.actor_id`
    /// (Requirement 3.1); 404s when it belongs to another recipient or does
    /// not exist (Requirement 3.2) — see this module's doc comment ("404 for
    /// other-recipient/nonexistent").
    pub async fn show(&self, ctx: &RequestActorContext, id: Id) -> Result<Value, AppError> {
        let recipient = ctx.actor_id;
        let notification = repository::find_for_recipient(&self.pool, id, recipient)
            .await?
            .ok_or_else(|| notification_not_found(id))?;

        let origin = self.origin();
        self.render_notification(recipient, &notification, &origin)
            .await
    }

    /// Dismisses notification `id` scoped to `ctx.actor_id` (Requirement
    /// 4.2); 404s when it belongs to another recipient or does not exist
    /// (Requirement 4.3) — see this module's doc comment ("404 for
    /// other-recipient/nonexistent"). Requirement 4.4 (post-dismiss
    /// exclusion) holds by construction: [`repository::dismiss`]/
    /// [`repository::find_for_recipient`]/[`repository::list`] already
    /// exclude dismissed rows, nothing further is needed here.
    pub async fn dismiss(&self, ctx: &RequestActorContext, id: Id) -> Result<(), AppError> {
        let recipient = ctx.actor_id;
        let found = repository::dismiss(&self.pool, id, recipient).await?;
        if !found {
            return Err(notification_not_found(id));
        }
        Ok(())
    }

    /// Dismisses every one of `ctx.actor_id`'s notifications (Requirement
    /// 4.1) — see this module's doc comment ("`clear`: always succeeds").
    pub async fn clear(&self, ctx: &RequestActorContext) -> Result<(), AppError> {
        repository::clear(&self.pool, ctx.actor_id).await
    }
}
