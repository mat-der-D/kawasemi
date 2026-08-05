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
//! ## Rendering `account`/`status`: the envelope's own, and the shared
//! assembler's
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
//! [`NotificationService::account_json`] is a thin wrapper over it. That one
//! stays per notification: it is the *envelope's* account (the notification's
//! `origin`), not a status author, and nothing batches it.
//!
//! The `status` half goes through
//! [`crate::statuses::render_assembler::StatusRenderAssembler`], which every
//! module that renders statuses now goes through. This module used to carry
//! its own copy of that assembly glue — one of five in the crate, written
//! because `crate::statuses::endpoints`'s equivalent was private to a
//! router-local, six-parameter generic state bundle this service had no
//! reason to parameterize over. The extraction has happened; what remains
//! here is only the part that is genuinely this module's own: which post a
//! notification refers to, and whether it is still there.
//!
//! That handoff is a single [`crate::statuses::render_assembler::StatusRenderAssembler::assemble_many`]
//! call per page rather than one per notification, so the media/tag/emoji/
//! interaction/poll lookups a page needs are issued a number of times that
//! does not depend on how many notifications it holds, and an author whose
//! posts appear twice on one page is resolved once (Requirements 5.1, 5.2,
//! 5.3, 5.6). Boost targets ride in the same batch. Notifications with no
//! live post contribute nothing to it and simply take no slot in the result
//! — see "Dangling references are not errors" below.
//!
//! What stays outside that batch, and stays per notification, is the walk
//! that decides *which* posts reach it: the envelope's own
//! [`NotificationService::account_json`], and the
//! [`status_repository::find_by_id`] pair that resolves a notification's
//! related post and that post's boost target. The design leaves boost
//! resolution with the caller, and neither the envelope account nor the
//! status row itself is one of the per-Status materials Requirement 5.1
//! enumerates. The envelope account is nonetheless a real remaining
//! per-notification `show_account`: a page of N notifications from N
//! distinct origins issues N of them, and two notifications from the *same*
//! origin resolve that origin twice, which the assembler's own author
//! memoization would have collapsed had the origin been a status author.
//! Batching it means resolving accounts for a set rather than one at a
//! time, which `AccountService` has no entry point for — noted here rather
//! than attempted, since the task that batched this rendering deliberately
//! left the envelope alone.
//!
//! One narrowing versus `account_provider.rs`'s own equivalent: this module's
//! [`NotificationService::resolve_reblog_target`] does **not** re-check
//! `visibility::is_visible` on a nested reblog target (`account_provider.rs`'s
//! own `self.visible_to(&target, viewer)` guard). A notification's
//! `status_id` was only ever attached at generation time to a recipient
//! legitimately entitled to see it (the notifications spec's Requirement
//! 1.2's "受信者視点" is about *whose* interaction state is embedded, not a
//! fresh visibility re-check — no requirement in that spec's Requirement
//! 1/2/3/4 asks this service to re-derive statuses-core's own visibility
//! policy, and doing so would pull in `RelationshipQueryRegistry` purely for
//! one nested field those Requirements do not exercise). That narrowing
//! predates the batching refactor and is preserved by it, pinned by
//! `tests::list_renders_a_mixed_page_of_every_status_shape_in_order`'s
//! `private` boost target — flagged as a CONCERN, not silently dropped.
//!
//! ## Dangling references are not errors
//! A notification's `origin`/`status_id` are logical references
//! (`repository.rs`'s own doc comment on the physical model: "受信者・通知
//! 元・対象投稿は論理参照"). If the referenced status has since been deleted,
//! [`NotificationService::render_page`] renders `status: null` rather than
//! propagating `crate::statuses::status_repository::find_by_id`'s `None` as
//! an error, and does so through the *same* path that renders a notification
//! which never had a related post at all — the two are deliberately
//! indistinguishable in the output. Mirrors `account_provider.rs`'s own
//! "missing referenced row is not a hydration failure" precedent for exactly
//! the same situation (a dangling reblog target). The origin *account* is not
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
use crate::api::origin::self_origin;
use crate::api::pagination::{ForwardedOrigin, Page, PageParams};
use crate::domain::{AccountRef, Id};
use crate::error::AppError;
use crate::federation::signatures::ReqwestFederationHttpClient;
use crate::media::LocalFsStore;
use crate::notifications::model::Notification;
use crate::notifications::repository::{self, ListFilter};
use crate::notifications::serializer::{NotificationRenderInput, notification_to_json};
use crate::oauth::model::RequestActorContext;
use crate::runtime::RuntimeContext;
use crate::statuses::model::Status;
use crate::statuses::poll_repository;
use crate::statuses::render_assembler::{
    PollResolution, PollResolver, RenderContext, StatusRenderAssembler,
};
use crate::statuses::status_repository;

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
        self_origin(&self.domain)
    }

    /// Resolves `account` (the notification's origin, or a related status's
    /// author) into full Account JSON via `AccountService::show_account` —
    /// see this module's doc comment ("Rendering `account`/`status`").
    async fn account_json(&self, id: Id, origin: &ForwardedOrigin) -> Result<Value, AppError> {
        self.accounts
            .show_account(&id.as_i64().to_string(), None, origin)
            .await
    }

    /// Builds the shared Status assembler this service renders through.
    fn assembler(&self) -> StatusRenderAssembler {
        StatusRenderAssembler::new(
            self.pool.clone(),
            Arc::clone(&self.accounts),
            self.media_store.clone(),
        )
    }

    /// Fetches a boost's target, or `None` when `status` is not a boost or
    /// its target has since been deleted.
    ///
    /// Boost-target resolution stays here rather than moving into the
    /// assembler — design.md's `Status 一覧の組み立て` flow, "ブースト先の
    /// 解決と可視性判定は**呼び出し元に残る**" — and deliberately performs
    /// **no** visibility re-check on the target, unlike every sibling caller.
    /// See this module's doc comment ("Rendering `account`/`status`") for why
    /// that narrowing exists and why batching does not change it.
    async fn resolve_reblog_target(&self, status: &Status) -> Result<Option<Status>, AppError> {
        let Some(target_id) = status.reblog_of_id else {
            return Ok(None);
        };
        status_repository::find_by_id(&self.pool, target_id).await
    }

    /// Renders one already-fetched page of [`Notification`]s into their full
    /// Notification JSON, in the order given (account + status embeds
    /// resolved, then delegated to [`notification_to_json`] for the outer
    /// shell + null discipline — task 2.1's own boundary, not reimplemented
    /// here).
    ///
    /// The page's related posts and their boost targets are gathered first
    /// and handed to [`StatusRenderAssembler::assemble_many`] as **one**
    /// batch, so the materials they need are fetched a number of times that
    /// does not depend on the page's length (Requirement 5.1 of
    /// structural-refactor) and targets ride in the same batch (5.6).
    ///
    /// The gathering pass issues its lookups in exactly the order the
    /// per-notification loop this replaces did — for each notification in
    /// turn, its origin account, then its related post, then that post's
    /// boost target — so a failure in any of them still aborts the whole
    /// page on the notification it aborted on before (Requirement 1.1).
    /// Notably, a post is resolved even for a `Follow`/`FollowRequest`
    /// notification, whose rendered `status` [`notification_to_json`] then
    /// discards: skipping it would turn an erroring page into a rendering
    /// one, which is a change even though the discarded value is not.
    async fn render_page(
        &self,
        viewer: Id,
        notifications: &[Notification],
        origin: &ForwardedOrigin,
    ) -> Result<Vec<Value>, AppError> {
        let mut accounts = Vec::with_capacity(notifications.len());
        // `slots[i]` is where notification `i`'s post landed in the batch, or
        // `None` when it has none to render. Both degradations this module
        // documents — no related post at all, and a related post that has
        // since been deleted — arrive here as the same `None` and stay
        // indistinguishable from each other downstream.
        let mut slots: Vec<Option<usize>> = Vec::with_capacity(notifications.len());
        let mut statuses = Vec::new();
        let mut reblog_targets = Vec::new();

        for notification in notifications {
            let origin_id = match notification.origin {
                AccountRef::Local(id) | AccountRef::Remote(id) => id,
            };
            accounts.push(self.account_json(origin_id, origin).await?);

            let status = match notification.status_id {
                Some(status_id) => status_repository::find_by_id(&self.pool, status_id).await?,
                None => None,
            };
            match status {
                None => slots.push(None),
                Some(status) => {
                    reblog_targets.push(self.resolve_reblog_target(&status).await?);
                    slots.push(Some(statuses.len()));
                    statuses.push(status);
                }
            }
        }

        let polls = RequiredPolls {
            pool: self.pool.clone(),
        };
        let ctx = RenderContext {
            // One `now` for the page rather than one per notification. The
            // values it feeds — a poll's `expired` flag — are now answered
            // consistently across a single response, which rendering each
            // embedded post against its own clock reading did not guarantee.
            viewer: Some(viewer),
            now: self.runtime.clock.now(),
            origin,
            muted: None,
            polls: &polls,
        };
        let rendered = self
            .assembler()
            .assemble_many(&statuses, &reblog_targets, &ctx)
            .await?;

        Ok(notifications
            .iter()
            .zip(accounts)
            .zip(slots)
            .map(|((notification, account), slot)| {
                let input = NotificationRenderInput {
                    notification,
                    account,
                    status: slot.map(|index| rendered[index].clone()),
                };
                notification_to_json(&input)
            })
            .collect())
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
        let items = self
            .render_page(recipient, &page_result.items, &origin)
            .await?;

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
    ///
    /// Renders through [`Self::render_page`] with a one-element page, the
    /// same way [`StatusRenderAssembler::assemble_one`] delegates to
    /// `assemble_many`: one notification and a page of them cannot then
    /// drift apart.
    pub async fn show(&self, ctx: &RequestActorContext, id: Id) -> Result<Value, AppError> {
        let recipient = ctx.actor_id;
        let notification = repository::find_for_recipient(&self.pool, id, recipient)
            .await?
            .ok_or_else(|| notification_not_found(id))?;

        let origin = self.origin();
        let rendered = self
            .render_page(recipient, std::slice::from_ref(&notification), &origin)
            .await?;
        Ok(rendered
            .into_iter()
            .next()
            .expect("a one-element page renders exactly one notification"))
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

/// Reads polls straight from the repository and treats a dangling
/// `poll_id` as this module's own not-found error, matching what this path
/// has always done.
struct RequiredPolls {
    pool: PgPool,
}

impl PollResolver for RequiredPolls {
    /// Two queries for the whole batch — one
    /// [`poll_repository::find_polls_by_ids`], one
    /// [`poll_repository::tally_many`] — regardless of how many ids it is
    /// given, and none at all for an empty one (Requirement 5.1: a
    /// notification page must not have its poll lookups scale with its
    /// length).
    fn resolve_many<'a>(&'a self, poll_ids: &'a [Id], viewer: Option<Id>) -> PollResolution<'a> {
        Box::pin(async move {
            let polls = poll_repository::find_polls_by_ids(&self.pool, poll_ids).await?;

            // Walked in `poll_ids` order, so a dangling id raises where the
            // per-id loop this replaces raised: on the *first* one, not on
            // whichever the map happened to iterate to. Both this ordering
            // and the result's own are fixed by this one pass.
            let mut resolved = Vec::with_capacity(poll_ids.len());
            for &poll_id in poll_ids {
                let poll = polls.get(&poll_id).ok_or_else(poll_not_found)?;
                resolved.push((poll_id, poll.clone()));
            }

            // Only reached once every id resolved, so `tally_many` is never
            // asked about a poll that does not exist — the same condition
            // under which the per-id loop reached `tally`.
            let tallies = poll_repository::tally_many(&self.pool, poll_ids, viewer).await?;

            let mut out = Vec::with_capacity(resolved.len());
            for (poll_id, poll) in resolved {
                // Absent only for a poll deleted between the two queries
                // above, which this resolver reports exactly as it reports
                // one that was never there.
                let tally = tallies.get(&poll_id).ok_or_else(poll_not_found)?;
                out.push((poll_id, poll, tally.clone()));
            }
            Ok(out)
        })
    }
}
