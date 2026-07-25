//! `InteractionService` (design.md "Service / サービス層" ->
//! `#### InteractionService / PollService`, design.md lines ~563-588;
//! Requirements 9.1-9.5, 10.1-10.4, 11.1-11.4, 12.1-12.4; task 5.2,
//! `Boundary: InteractionService`): the reblog/favourite/bookmark/pin
//! business orchestration — visibility check, duplicate prevention, record,
//! counter update, and (for reblog/favourite only) federated delivery — that
//! ties together `StatusRepository` (task 2.1, reblog's own row lifecycle),
//! `InteractionRepository` (task 2.2, favourite/bookmark/pin state),
//! `visibility::is_visible` (task 3.1), `addressing::derive_addressing`/
//! `derive_recipients` (task 3.2), and `StatusActivityBuilder` (task 4.1).
//!
//! ## Scope
//! Owns exactly the seven design.md `InteractionService` methods this task's
//! own instruction names — [`InteractionService::reblog`],
//! [`InteractionService::unreblog`], [`InteractionService::favourite`],
//! [`InteractionService::unfavourite`], [`InteractionService::bookmark`],
//! [`InteractionService::list_bookmarks`], [`InteractionService::pin`]. Does
//! **not** implement [`InteractionService::vote`]/`PollService` (design.md's
//! shared heading also lists `vote`, but that is task 5.3's own boundary,
//! `_Depends`-listed separately in `tasks.md` and explicitly out of scope for
//! this task's instruction). Does not implement the HTTP surface
//! (`StatusEndpoints`, task 7.x) or serialization (`favourited`/`reblogged`/
//! `bookmarked`/`pinned` viewer-state flags are a serializer concern applied
//! at the endpoint layer against the returned domain [`Status`] plus viewer
//! context — this service only returns [`Status`] values, mirroring
//! `status_service.rs`'s identical "service returns a domain struct; only the
//! endpoint layer serializes" boundary).
//!
//! ## Reblog is a `StatusRepository` row, not an `InteractionRepository` one
//! Per `InteractionRepository`'s own module doc comment ("Reblog scope") and
//! `tasks.md`'s 2.2 Implementation Note: a boost has no dedicated table —
//! it is persisted purely as a `statuses` row with `reblog_of_id` set. This
//! service therefore records/revokes a reblog via
//! [`crate::statuses::status_repository::insert_status`]/[`delete_status`],
//! using [`crate::statuses::interaction_repository::find_reblog`] only for
//! the duplicate-check/idempotent-lookup half (Requirement 9.3).
//!
//! ## Duplicate reblog: return the existing boost, not an error or a second
//! row (Requirement 9.3)
//! Mirrors `InteractionRepository`'s own established idempotent-`bool`
//! convention for favourite/bookmark/pin: a second reblog request by the
//! same actor of the same target is not an error and does not create a
//! second boost row — [`reblog`](InteractionService::reblog) returns the
//! *already-existing* boost `Status` (found via `find_reblog`) unchanged, no
//! further counter increment, no further `Announce` dispatch.
//!
//! ## Reblog's own visibility (CONCERN — documented judgment call)
//! Neither requirements.md nor design.md states what `Visibility` a newly
//! created boost `Status` row itself carries. This service uses the
//! **target's own visibility** as the reblog's visibility — the natural
//! default matching real Mastodon convention (a boost is exactly as visible
//! as what it boosts) and the simplest interpretation consistent with
//! Requirement 9.5's visibility gate (a reblog of something the actor cannot
//! see is rejected before a row is ever considered, so the boost row itself,
//! once created, always carries a visibility the reblogger could already
//! see).
//!
//! ## `reblog`'s own delivery addressing vs. `unreblog`/`favourite`/
//! `unfavourite`'s single-recipient shape (an asymmetry inherited from task
//! 4.1's already-reviewed `StatusActivityBuilder`, not introduced here)
//! [`StatusActivityBuilder::deliver_announce`] takes the **reblog's own**
//! `Addressing`/recipient set (its booster's followers/public collection,
//! exactly like [`StatusService::create_status`]'s own `Create` dispatch) —
//! so [`reblog`](InteractionService::reblog) derives addressing for the new
//! boost `Status` itself, via the same `derive_addressing`/
//! `derive_recipients`/`RelationshipQuery::followers_of` pattern
//! `status_service.rs::build_addressing` already establishes (minus mention
//! resolution: a boost has no `content` to extract mentions from).
//! [`StatusActivityBuilder::deliver_like`]/[`deliver_undo`], by contrast,
//! were already specified (task 4.1, reviewed, not modified here) to take a
//! single already-resolved `recipient: ActorRef` — **the target's author** —
//! rather than an `Addressing`. `unreblog`'s `Undo(Announce)` dispatch reuses
//! that same single-recipient `deliver_undo` signature, so it resolves
//! *target's* author, not the reblog's own followers — an asymmetry between
//! `reblog`'s own delivery shape and `unreblog`'s that is inherited from
//! task 4.1's already-shipped `StatusActivityBuilder` interface, not a choice
//! made in this task.
//!
//! ## Resolving a target's author is local-only (CONCERN — same structural
//! gap `status_service.rs` already documents for remote mentions)
//! [`favourite`](InteractionService::favourite)/
//! [`unfavourite`](InteractionService::unfavourite)/
//! [`unreblog`](InteractionService::unreblog) each need to resolve the
//! *target* status's author to an [`ActorRef`] (for `deliver_like`/
//! `deliver_undo`'s single-`recipient` parameter). This service's only
//! `Id -> Handle` port is [`ActorHandleLookup`] (reused from task 4.1), whose
//! production implementation, `ActorDirectory::resolve_actor_by_id`, only
//! ever resolves a **local** actor (`src/actor/directory.rs`'s own doc
//! comment: delegates to `repository::find_by_id` against `local_actors`
//! alone). A target authored by a remote actor therefore cannot have its
//! author resolved through this service's dependency set — the identical
//! "リモートアクターの完全なプロファイル永続化...は accounts-and-instance"
//! boundary `activity_builder.rs`/`status_service.rs` already name, not a new
//! gap introduced here. Favouriting/un-favouriting/un-reblogging a
//! remote-authored post therefore fails with this port's own `404`-shaped
//! error rather than silently degrading; `reblog` itself is unaffected (its
//! own `Announce` addresses the *reblogger's* followers, never the target's
//! author — see the section above), so only the four operations that need
//! the target author specifically are affected.
//!
//! ## Visibility-then-not-found ordering, mirroring `status_service.rs`'s
//! own `in_reply_to` convention
//! Requirement 9.5 says an invisible reblog target is "拒否"; this service
//! reports that the same way `status_service.rs::create_status` already
//! reports an invisible `in_reply_to` parent — a uniform `404 Not Found`
//! ("status not found"), not a distinct client error — so a caller cannot
//! distinguish "this id does not exist" from "this id exists but you may not
//! see it", matching this crate's established privacy-preserving
//! not-found-for-both convention (`status_repository::find_visible`'s own
//! doc comment: "a caller cannot learn a private post exists here"). The
//! same applies to `favourite`'s/`bookmark`'s own visibility gate
//! (Requirements 10.1's "可視な投稿", 11.1's "可視な投稿").
//!
//! ## Pin's two distinct rejections (Requirements 12.3, 12.4) get two
//! distinct error shapes
//! Ownership (12.3) mirrors `status_service.rs::delete_status`/`edit_status`'s
//! own "non-owner => 404, not a distinct 403" convention (hiding whether a
//! given id belongs to someone else, the same privacy rationale). Direct-
//! visibility rejection (12.4), by contrast, only ever triggers *after*
//! ownership has already passed (the actor already knows this is their own
//! post), so there is no existence-hiding rationale left — this is a genuine,
//! nameable validation failure, reported as a `422` via this module's own
//! `rejected()` helper, mirroring `status_service.rs`'s identical
//! poll/media-mutual-exclusivity `422` convention.
//!
//! ## Bookmark/pin: no counter, no `StatusActivityBuilder` call at all
//! (Requirements 11.4, and design.md's "pin はローカル状態")
//! [`bookmark`](InteractionService::bookmark)/[`pin`](InteractionService::pin)
//! call `InteractionRepository::add_bookmark`/`remove_bookmark`/`set_pin`
//! only — no `status_repository::adjust_counts` (neither `Status` nor
//! `migrations/0007_statuses.sql` has a bookmark/pin counter column at all)
//! and no `activity_builder` method of any kind, not even a best-effort one.
//!
//! ## `unreblog`/`unfavourite` return the (fresh) target/original status
//! (documented choice)
//! Neither method's caller-visible return type distinguishes "a boost/
//! favourite existed and was just revoked" from "nothing to revoke" — both
//! return the current, freshly-refetched *target*/original `Status` (its
//! `reblogs_count`/`favourites_count` reflecting whatever the call just did,
//! or unchanged if there was nothing to revoke), matching the idempotent-
//! no-op-success convention `InteractionRepository`'s own `remove_favourite`/
//! `find_reblog` doc comments already establish, and structurally impossible
//! to represent by returning the (deleted) boost row itself for `unreblog`
//! (rendering `reblogged=false`/updated counters is the caller/serializer's
//! job against this returned base `Status`, same as `favourite`'s own
//! `favourited=true` reflection, per this module's own "Scope" section).

#[cfg(test)]
mod tests;

use sqlx::postgres::PgPool;

use crate::api::pagination::{Page, PageParams};
use crate::domain::{Id, Visibility};
use crate::error::AppError;
use crate::federation::{ActorUrls, DeliverySink, LocalActorLookup, ObjectKind, Recipient};
use crate::runtime::RuntimeContext;
use crate::statuses::activity_builder::{ActorHandleLookup, StatusActivityBuilder, UndoKind};
use crate::statuses::addressing::{self, ActorRef, Addressing};
use crate::statuses::interaction_repository;
use crate::statuses::model::Status;
use crate::statuses::status_repository::{self, CountKind};
use crate::statuses::visibility::{self, RelationshipQuery};
use axum::http::StatusCode;

fn not_found() -> AppError {
    AppError::client(StatusCode::NOT_FOUND, "status not found")
}

fn rejected(message: impl Into<String>) -> AppError {
    AppError::client(StatusCode::UNPROCESSABLE_ENTITY, message.into())
}

/// The reblog/favourite/bookmark/pin business-service layer (design.md's
/// `InteractionService` half of the shared `InteractionService / PollService`
/// component, task 5.2). Generic over the same delegation ports
/// `StatusService` (task 5.1) already established for the identical reasons
/// (see that module's own doc comment) — `A`/`D`/`L`/`H` (via the embedded
/// [`StatusActivityBuilder`]) and `R` ([`RelationshipQuery`], task 3.1) — plus
/// its own copy of `A` ([`ActorHandleLookup`]) for resolving a reblog/
/// favourite *target*'s author, independent of whichever actor the embedded
/// `StatusActivityBuilder` resolves as the *sending* actor for a given call.
pub struct InteractionService<A, D, L, H, R>
where
    A: ActorHandleLookup,
    D: LocalActorLookup,
    L: DeliverySink,
    H: DeliverySink,
    R: RelationshipQuery,
{
    pool: PgPool,
    runtime: RuntimeContext,
    urls: ActorUrls,
    activity_builder: StatusActivityBuilder<A, D, L, H>,
    actor_lookup: A,
    relationship: R,
}

impl<A, D, L, H, R> InteractionService<A, D, L, H, R>
where
    A: ActorHandleLookup,
    D: LocalActorLookup,
    L: DeliverySink,
    H: DeliverySink,
    R: RelationshipQuery,
{
    /// Builds an `InteractionService` bound to `pool` (repository calls),
    /// `runtime` (id/clock injection for a freshly-minted reblog row, never
    /// ad hoc generation), `urls` (a freshly-created reblog's own object
    /// URI, plus resolving an author's `/followers` collection URI),
    /// `activity_builder` (Activity generation + delivery, task 4.1),
    /// `actor_lookup` ([`ActorHandleLookup`], resolving a reblog/favourite
    /// *target*'s author to an [`ActorRef`] — see this module's doc comment,
    /// "Resolving a target's author is local-only"), and `relationship`
    /// ([`RelationshipQuery`], task 3.1).
    pub fn new(
        pool: PgPool,
        runtime: RuntimeContext,
        urls: ActorUrls,
        activity_builder: StatusActivityBuilder<A, D, L, H>,
        actor_lookup: A,
        relationship: R,
    ) -> Self {
        Self {
            pool,
            runtime,
            urls,
            activity_builder,
            actor_lookup,
            relationship,
        }
    }

    /// Resolves whether `status` is visible to `viewer` through the real
    /// visibility policy ([`visibility::is_visible`], task 3.1) — the same
    /// policy `status_service.rs::visible_to` routes through, applied here to
    /// interaction targets (Requirements 9.5, 10.1, 11.1).
    async fn visible_to(&self, status: &Status, viewer: Option<Id>) -> Result<bool, AppError> {
        let rel = self
            .relationship
            .viewer_relation(status.actor_id, viewer)
            .await?;
        Ok(visibility::is_visible(status, viewer, &rel))
    }

    /// Resolves `actor_id` to an [`ActorRef`] (URI + delivery [`Recipient`])
    /// via this service's own [`ActorHandleLookup`] — see this module's doc
    /// comment ("Resolving a target's author is local-only") for the
    /// boundary this deliberately does not cross (no remote resolution).
    async fn actor_ref_for(&self, actor_id: Id) -> Result<ActorRef, AppError> {
        let handle = self.actor_lookup.resolve_handle(actor_id).await?;
        let uri = self.urls.actor_url(&handle);
        Ok(ActorRef {
            uri,
            recipient: Recipient::Local(handle),
        })
    }

    /// Derives a freshly-created `status` (here: always a reblog row, which
    /// carries no `content`/mentions of its own) own `Addressing`/recipient
    /// set — the mention-free counterpart of
    /// `status_service.rs::build_addressing` (see this module's doc comment,
    /// "`reblog`'s own delivery addressing...").
    async fn build_addressing(
        &self,
        status: &Status,
    ) -> Result<(Addressing, Vec<Recipient>), AppError> {
        let followers = self.relationship.followers_of(status.actor_id).await?;
        let author_ref = self.actor_ref_for(status.actor_id).await?;
        let followers_uri = format!("{}/followers", author_ref.uri);

        let addressing = addressing::derive_addressing(status, &[], &followers_uri);
        let recipients = addressing::derive_recipients(&addressing, &[], &followers);
        Ok((addressing, recipients))
    }

    /// Boosts `status_id` on `actor_id`'s behalf (Requirements 9.1-9.3, 9.5):
    /// rejects an invisible target (9.5, a uniform not-found — see this
    /// module's doc comment), returns the existing boost unchanged if
    /// `actor_id` already reblogged `status_id` (9.3, no duplicate row / no
    /// double counter / no second dispatch), otherwise records a new boost
    /// `Status` row (`reblog_of_id` set), increments the target's
    /// `reblogs_count`, and dispatches a canonical `Announce` (9.2).
    pub async fn reblog(&self, actor_id: Id, status_id: Id) -> Result<Status, AppError> {
        let target = status_repository::find_by_id(&self.pool, status_id)
            .await?
            .ok_or_else(not_found)?;
        if !self.visible_to(&target, Some(actor_id)).await? {
            return Err(not_found());
        }

        if let Some(existing) =
            interaction_repository::find_reblog(&self.pool, actor_id, status_id).await?
        {
            return Ok(existing);
        }

        let id = self.runtime.ids.next_id();
        let now = self.runtime.clock.now();
        let uri = self.urls.object_url(ObjectKind::new("statuses"), id);

        let reblog = Status {
            id,
            actor_id,
            uri: uri.clone(),
            url: Some(uri),
            content: String::new(),
            visibility: target.visibility,
            sensitive: false,
            spoiler_text: String::new(),
            in_reply_to_id: None,
            in_reply_to_account_id: None,
            reblog_of_id: Some(target.id),
            poll_id: None,
            language: None,
            reblogs_count: 0,
            favourites_count: 0,
            replies_count: 0,
            local: true,
            created_at: now,
            edited_at: None,
        };

        status_repository::insert_status(&self.pool, &reblog).await?;
        status_repository::adjust_counts(&self.pool, target.id, CountKind::Reblogs, 1).await?;

        let (addressing, recipients) = self.build_addressing(&reblog).await?;
        self.activity_builder
            .deliver_announce(&reblog, &target, &addressing, recipients)
            .await?;

        Ok(reblog)
    }

    /// Un-boosts `status_id` on `actor_id`'s behalf (Requirement 9.4): a
    /// no-op (no error, no dispatch) if `actor_id` had not reblogged
    /// `status_id`; otherwise deletes the boost row, decrements the target's
    /// `reblogs_count`, and dispatches a canonical `Undo(Announce)`. Returns
    /// the (possibly just-updated) target `Status` in either case — see this
    /// module's doc comment ("`unreblog`/`unfavourite` return...").
    pub async fn unreblog(&self, actor_id: Id, status_id: Id) -> Result<Status, AppError> {
        let target = status_repository::find_by_id(&self.pool, status_id)
            .await?
            .ok_or_else(not_found)?;

        let Some(reblog) =
            interaction_repository::find_reblog(&self.pool, actor_id, status_id).await?
        else {
            return Ok(target);
        };

        status_repository::delete_status(&self.pool, reblog.id).await?;
        status_repository::adjust_counts(&self.pool, target.id, CountKind::Reblogs, -1).await?;

        let recipient = self.actor_ref_for(target.actor_id).await?;
        self.activity_builder
            .deliver_undo(actor_id, UndoKind::Announce, &target, recipient)
            .await?;

        status_repository::find_by_id(&self.pool, target.id)
            .await?
            .ok_or_else(not_found)
    }

    /// Favourites `status_id` on `actor_id`'s behalf (Requirements 10.1,
    /// 10.2, 10.4): rejects an invisible target (a uniform not-found), is a
    /// silent no-op (no double counter / no second dispatch) if already
    /// favourited (10.4), otherwise records the favourite, increments
    /// `favourites_count`, and dispatches a canonical `Like` (10.2).
    pub async fn favourite(&self, actor_id: Id, status_id: Id) -> Result<Status, AppError> {
        let target = status_repository::find_by_id(&self.pool, status_id)
            .await?
            .ok_or_else(not_found)?;
        if !self.visible_to(&target, Some(actor_id)).await? {
            return Err(not_found());
        }

        let now = self.runtime.clock.now();
        let is_new =
            interaction_repository::add_favourite(&self.pool, actor_id, status_id, now).await?;
        if is_new {
            status_repository::adjust_counts(&self.pool, status_id, CountKind::Favourites, 1)
                .await?;
            let recipient = self.actor_ref_for(target.actor_id).await?;
            self.activity_builder
                .deliver_like(actor_id, &target, recipient)
                .await?;
        }

        status_repository::find_by_id(&self.pool, status_id)
            .await?
            .ok_or_else(not_found)
    }

    /// Un-favourites `status_id` on `actor_id`'s behalf (Requirement 10.3): a
    /// no-op (no error, no dispatch) if not currently favourited; otherwise
    /// removes the favourite, decrements `favourites_count`, and dispatches a
    /// canonical `Undo(Like)`.
    pub async fn unfavourite(&self, actor_id: Id, status_id: Id) -> Result<Status, AppError> {
        let target = status_repository::find_by_id(&self.pool, status_id)
            .await?
            .ok_or_else(not_found)?;

        let removed =
            interaction_repository::remove_favourite(&self.pool, actor_id, status_id).await?;
        if removed {
            status_repository::adjust_counts(&self.pool, status_id, CountKind::Favourites, -1)
                .await?;
            let recipient = self.actor_ref_for(target.actor_id).await?;
            self.activity_builder
                .deliver_undo(actor_id, UndoKind::Like, &target, recipient)
                .await?;
        }

        status_repository::find_by_id(&self.pool, status_id)
            .await?
            .ok_or_else(not_found)
    }

    /// Sets `actor_id`'s bookmark state of `status_id` to `on` (Requirements
    /// 11.1, 11.2, 11.4): rejects an invisible target when bookmarking
    /// (`on == true`; a uniform not-found), never checks visibility when
    /// unbookmarking (revoking a private state the actor already holds is
    /// always allowed, mirroring `unfavourite`/`unreblog`'s own no-gate
    /// convention). Purely local — never calls `StatusActivityBuilder`, no
    /// counter (Requirement 11.4).
    pub async fn bookmark(
        &self,
        actor_id: Id,
        status_id: Id,
        on: bool,
    ) -> Result<Status, AppError> {
        let target = status_repository::find_by_id(&self.pool, status_id)
            .await?
            .ok_or_else(not_found)?;

        if on {
            if !self.visible_to(&target, Some(actor_id)).await? {
                return Err(not_found());
            }
            let id = self.runtime.ids.next_id();
            let now = self.runtime.clock.now();
            interaction_repository::add_bookmark(&self.pool, id, actor_id, status_id, now).await?;
        } else {
            interaction_repository::remove_bookmark(&self.pool, actor_id, status_id).await?;
        }

        Ok(target)
    }

    /// Returns `actor_id`'s bookmarked posts, paginated (Requirement 11.3) —
    /// a thin pass-through to `InteractionRepository::list_bookmarks`, which
    /// owns the bookmark-specific cursor.
    pub async fn list_bookmarks(
        &self,
        actor_id: Id,
        page: PageParams,
    ) -> Result<Page<Status>, AppError> {
        interaction_repository::list_bookmarks(&self.pool, actor_id, page).await
    }

    /// Sets `actor_id`'s pin state of `status_id` to `on` (Requirements
    /// 12.1, 12.2): rejects a non-owned target (12.3, a uniform not-found —
    /// see this module's doc comment, "Pin's two distinct rejections") and a
    /// `direct`-visibility target when pinning (12.4, a genuine `422`).
    /// Purely local — never calls `StatusActivityBuilder`, no counter.
    pub async fn pin(&self, actor_id: Id, status_id: Id, on: bool) -> Result<Status, AppError> {
        let target = status_repository::find_by_id(&self.pool, status_id)
            .await?
            .ok_or_else(not_found)?;
        if target.actor_id != actor_id {
            return Err(not_found());
        }
        if on && target.visibility == Visibility::Direct {
            return Err(rejected(
                "a direct-visibility status cannot be pinned to a profile",
            ));
        }

        let now = self.runtime.clock.now();
        interaction_repository::set_pin(&self.pool, actor_id, status_id, on, now).await?;

        Ok(target)
    }
}
