//! `StatusHydrator` (design.md "Serialize / 具体化層" -> "StatusHydrator",
//! design.md lines ~349-368; Requirements 10.1, 10.2, 10.3, 10.4; task 4.1,
//! `Boundary: StatusHydrator`): hydrates a filtered timeline candidate
//! ([`crate::statuses::model::Status`]) into the exact same Status JSON
//! contract the posting API returns, by resolving every pre-resolved input
//! `crate::statuses::serializer::status_to_json` needs
//! ([`StatusRenderInput`]) and delegating the actual field-by-field mapping
//! to that function — never redefining the Status JSON shape itself
//! (Requirement 10.1: "本 spec で再定義しない").
//!
//! ## Scope
//! This module owns exactly [`StatusHydrator::hydrate`] and the private
//! `Status -> StatusRenderInput` assembly glue it needs
//! (`resolve_common`/`account_json`/`media_json`/`tags_json`/
//! `resolve_emojis`/`interaction_state`/`poll_json`/`leaf_render_input`/
//! `hydrate_one`) — mirroring
//! `crate::statuses::endpoints::StatusesEndpointsState`'s own identical
//! assembly-glue precedent (that module's own doc comment: "No existing
//! helper resolves `Status -> StatusRenderInput` end-to-end — you must
//! write this assembly glue yourself"), since `StatusesEndpointsState`'s
//! version is private to its own module and not reusable as a function
//! call from `timelines`. It reuses, never reimplements:
//! `crate::accounts::account_service::AccountService::show_account`
//! (Account JSON), `crate::statuses::status_repository::media_ids_for_status`,
//! `crate::media::media_repository::find_by_id`, and
//! `crate::media::serializer::to_media_attachment` (MediaAttachment JSON),
//! `crate::statuses::tag_repository::tags_for_status` (tags),
//! `crate::statuses::status_service::extract_content_tokens` +
//! `crate::accounts::emoji_repository::resolve_emojis` (emojis),
//! `crate::statuses::interaction_repository::{exists_favourite,
//! exists_bookmark, exists_pin, find_reblog}` (viewer operation state,
//! Requirement 10.2), `crate::statuses::poll_repository::{find_polls_by_ids,
//! tally_many}` + `crate::statuses::serializer::poll_to_json` (poll
//! rendering),
//! `crate::statuses::status_repository::find_by_id` (the boosted status'
//! own row, Requirement 10.3), and
//! `crate::statuses::serializer::{StatusRenderInput, status_to_json}` (the
//! actual Status JSON contract, Requirement 10.1). This module has no HTTP
//! surface and does not touch `TimelineService`/`TimelineEndpoints`/
//! `crate::state`/`crate::bootstrap`/`crate::server` (tasks 4.2, 5.1, 5.2) —
//! it is not wired into anything live yet, exactly like every earlier task
//! in `crate::timelines`'s own module doc comment already documents for
//! itself.
//!
//! ## Deliberate deviations from design.md's literal Service Interface
//! design.md's Service Interface sketch (design.md line ~366, predating
//! statuses-core's actual, already-implemented `status_to_json` signature —
//! see this task's own dispatch brief) is:
//! ```text
//! pub fn hydrate(&self, statuses: &[Status], viewer: Option<Id>, now: OffsetDateTime) -> Vec<serde_json::Value>;
//! ```
//! but the real `status_to_json(input: &StatusRenderInput) -> Value`
//! (`src/statuses/serializer.rs`) takes one pre-assembled input struct, not
//! `(status, ctx)` — this task's job is exactly the assembly glue that
//! bridges the two. Three further gaps, each resolved the same conservative
//! way this spec's earlier tasks already established
//! (`following_and_self` on `fetch_candidates`, `reblogged_author` on
//! `TimelineFilter::keep`, `tags`/`reblogged_author` on
//! `TimelineMatcher::matches` — see this crate's tasks.md, "Implementation
//! Notes"): extend the signature with exactly the parameter(s) the real
//! work needs, document why, never silently drop the requirement.
//!
//! 1. **`viewer: Option<Id>, now: OffsetDateTime` -> `ctx: &FilterContext`.**
//!    Requirement 10.2's `muted` operation-state field needs a mute signal
//!    statuses-core's own serializer explicitly does not (and, per its own
//!    boundary, cannot) resolve itself —
//!    `crate::statuses::serializer::StatusInteractionState::muted`'s own doc
//!    comment: "mute-state source... out of this spec's boundary... a
//!    future caller supplies this". `crate::timelines::model::FilterContext`
//!    (task 1.1, already defined, never redefined here) already carries
//!    exactly `viewer`/`now` *and* the `muted: HashSet<Id>` signal this task
//!    needs (sourced from social-graph's `FilterQuery`, per `TimelineFilter`'s
//!    own established precedent, `src/timelines/filter.rs`) — passing the
//!    whole, already-established context struct in place of two of its own
//!    fields is the conservative choice: no new type, no redefinition of
//!    `FilterContext` itself, and a caller (`TimelineService`, task 4.2)
//!    already holds one to pass.
//! 2. **`origin: &ForwardedOrigin` added.** Resolving `account`/
//!    `media_attachments`/`tags` all need
//!    `crate::api::pagination::ForwardedOrigin` (scheme+host for building
//!    URLs) to delegate to `AccountService::show_account`/
//!    `to_media_attachment`/tag URL construction — exactly
//!    `crate::statuses::endpoints::StatusesEndpointsState`'s own identical
//!    dependency, and design.md's sketch has no parameter for it at all
//!    (the same class of gap task 2.1/3.1/3.2 already hit and resolved the
//!    same way — see above). It is per-request state, not baked into this
//!    struct's own constructor (mirroring `StatusesEndpointsState`'s
//!    handlers threading `origin` through per call, never storing it on
//!    `self`).
//! 3. **`Vec<serde_json::Value>` -> `Result<Vec<serde_json::Value>, AppError>`,
//!    and `hydrate` is `async`.** Every field this task actually resolves
//!    (account/media/tags/emojis/interactions/poll/reblog target) is a real
//!    database round trip that can fail — design.md's infallible, sync
//!    sketch predates the concrete repository/service signatures this task
//!    discovered it must call (all `async fn ... -> Result<_, AppError>`,
//!    per this crate's own steering: "エラーは `AppError` に集約"). Silently
//!    swallowing a repository failure into an empty/default field would
//!    hide a real error from the caller; propagating it is the same
//!    discipline `CandidateRepository::fetch_candidates`
//!    (`src/timelines/candidate_repository.rs`) already established for
//!    this spec's own database-backed components.
//!
//! ## Graceful degradation on a missing referenced row (not an error)
//! A boosted status ([`Status::reblog_of_id`]) or a poll
//! ([`Status::poll_id`]) that has since become unresolvable (deleted /
//! referentially inconsistent) is *not* treated as a hydration failure —
//! mirrors `StatusesEndpointsState::render_status_json`'s own identical
//! precedent (its `reblog_target`/`media_json` both degrade to
//! `None`/skip-the-entry rather than erroring on a missing referenced row):
//! [`StatusHydrator`] renders `reblog: None`/`poll: None` respectively
//! rather than surfacing a `404`/`500` for what is, from a hydration
//! caller's perspective, just an absent optional field. This is a
//! deliberate judgment call, not something design.md specifies either way.
//!
//! ## Why `poll`/reblog-target resolution bypasses `PollService`/`StatusService`
//! `PollService::poll`/`StatusService::show` both re-check visibility before
//! returning — a check `TimelineFilter` (task 3.1) has already applied to
//! the *top-level* candidate this hydrator ever receives, and pulling either
//! service in would drag their full `<A, D, L, H, R>`/`<A, D, L, H, R, M>`
//! port-type parameter stacks into this component's constructor for a
//! redundant check on that row. Instead this module reads
//! `crate::statuses::poll_repository::{find_polls_by_ids, tally_many}` /
//! `crate::statuses::status_repository::find_by_id` directly — read-only
//! repository calls, mirroring `CandidateRepository`'s own "read-only, no
//! business-service dependency" precedent
//! (`src/timelines/candidate_repository.rs`) — and degrades gracefully (see
//! above) rather than erroring when the referenced row is absent.
//!
//! A boost's *nested reblog target*, however, is a **different** row from
//! the one `TimelineFilter` ever validated, so bypassing `StatusService`
//! there does *not* mean bypassing a visibility check altogether.
//! `TimelineFilter::keep` (`src/timelines/filter.rs`) only checks the boost
//! row's own `visibility` snapshot (copied at boost-creation time,
//! `InteractionService::reblog`'s `visibility: target.visibility`) with
//! `rel.is_follower` keyed to the *booster* — never against the
//! boosted-original's *own* author. Nesting a fetched reblog target's full
//! content under `reblog` on the strength of that check alone would leak,
//! e.g., a followers-only post from an author the viewer does not follow,
//! whenever some other, followed account boosts it. So before nesting a
//! fetched reblog target, [`StatusHydrator::hydrate_one`] independently
//! re-checks its visibility via `crate::statuses::visibility::is_visible` —
//! the exact same pure function `TimelineFilter::keep` itself calls — with a
//! [`crate::statuses::visibility::ViewerRelation`] built the identical way
//! (`is_follower = ctx.viewer.is_some() && ctx.following.contains(&target.
//! actor_id)`), just keyed to the target's own `actor_id` rather than the
//! booster's. This is the same visibility discipline `render_status_json`/
//! `StatusService::show` apply, applied directly via the pure visibility
//! function rather than through `StatusService`, because avoiding pulling
//! `StatusService`'s generic port-parameter stack into this component was
//! itself an intentional boundary decision (this task's own dispatch
//! brief). A target that fails this independent check is treated exactly
//! like a missing/dangling target (see above): `reblog: None`, never a
//! partial or unconditional render of a row the viewer has no right to
//! see.
//!
//! ## Non-recursive reblog nesting (Requirement 10.3)
//! Mirrors `StatusesEndpointsState::leaf_render_input`'s established
//! precedent exactly: a boosted status' own render input always sets its
//! `reblog` field to `None` unconditionally, even if the boosted status
//! were itself somehow a boost (statuses-core's own one-level-only reblog
//! nesting discipline; a boost-of-a-boost is not a shape any local
//! `create_status`/federation ingest path produces, but this module does
//! not assume that invariant holds forever — it enforces at most one level
//! of nesting structurally rather than recursing unboundedly).
//!
//! ## `mentions: Vec::new()` (unresolved)
//! Mirrors `StatusesEndpointsState::leaf_render_input`/`render_status_json`'s
//! own identical, already-reviewed gap: no production call site anywhere in
//! this crate yet resolves a `Status`'s `mentions` field, so this module
//! does not invent its own resolution either — reproducing the exact same
//! (pre-existing, out-of-this-task's-boundary) gap statuses-core's own
//! endpoint layer already has, rather than silently fixing it here as an
//! undocumented side effect of an unrelated task.
//!
//! No `TimelineService`/`TimelineEndpoints`/`TimelinesModule` (later tasks),
//! and no wiring into `crate::state`/`crate::bootstrap`/`crate::server`
//! (task 5.2) live here.

#[cfg(test)]
mod tests;

use std::sync::Arc;

use serde_json::Value;
use sqlx::PgPool;

use crate::accounts::account_service::AccountService;
use crate::api::pagination::ForwardedOrigin;
use crate::domain::Id;
use crate::error::AppError;
use crate::federation::signatures::ReqwestFederationHttpClient;
use crate::media::local_fs::LocalFsStore;
use crate::statuses::model::Status;
use crate::statuses::poll_repository;
use crate::statuses::render_assembler::{
    PollResolution, PollResolver, RenderContext, StatusRenderAssembler,
};
use crate::statuses::status_repository;
use crate::statuses::visibility::{ViewerRelation, is_visible};

use super::model::FilterContext;

/// Hydrates filtered timeline candidates into Status JSON (Requirements
/// 10.1-10.4). See this module's doc comment for the full reasoning behind
/// every constructor dependency and every deviation from design.md's
/// literal sketch.
///
/// Depends on the same concrete `AccountService<LocalFsStore,
/// ReqwestFederationHttpClient>`/`LocalFsStore` production types
/// `crate::statuses::endpoints::StatusesEndpointsState` already uses for its
/// own identical `accounts`/`media_store` fields (`crate::accounts::
/// AccountsModule::service`/`crate::media::MediaModule::store` are this
/// crate's one Composition Root source for both, per task 4.1's own
/// instruction: "consistent with how other structs... already take their
/// dependencies").
pub struct StatusHydrator {
    pool: PgPool,
    accounts: Arc<AccountService<LocalFsStore, ReqwestFederationHttpClient>>,
    media_store: LocalFsStore,
}

impl StatusHydrator {
    /// Builds a hydrator from already-constructed collaborators — mirrors
    /// this crate's established "bundle, don't build" business-layer
    /// constructor convention (e.g. `AccountService::new`,
    /// `StatusesEndpointsState`'s own field list).
    pub fn new(
        pool: PgPool,
        accounts: Arc<AccountService<LocalFsStore, ReqwestFederationHttpClient>>,
        media_store: LocalFsStore,
    ) -> Self {
        Self {
            pool,
            accounts,
            media_store,
        }
    }

    /// Hydrates every status in `statuses`, in the given order, into Status
    /// JSON (Requirement 10.1) reflecting `ctx.viewer`'s operation state
    /// (Requirement 10.2) with boosts nested under `reblog` (Requirement
    /// 10.3), delegating Account/MediaAttachment rendering upstream
    /// (Requirement 10.4). See this module's doc comment ("Deliberate
    /// deviations") for why this takes `ctx`/`origin` rather than
    /// design.md's literal `viewer`/`now` pair, and is `async`/fallible.
    /// Hydrates every status in `statuses`, in the given order, into Status
    /// JSON reflecting `ctx.viewer`'s operation state with boosts nested
    /// under `reblog`.
    pub async fn hydrate(
        &self,
        statuses: &[Status],
        ctx: &FilterContext,
        origin: &ForwardedOrigin,
    ) -> Result<Vec<Value>, AppError> {
        let mut reblog_targets = Vec::with_capacity(statuses.len());
        for status in statuses {
            reblog_targets.push(self.resolve_reblog_target(status, ctx).await?);
        }

        let polls = TolerantPolls {
            pool: self.pool.clone(),
        };
        let render_ctx = RenderContext {
            viewer: ctx.viewer,
            now: ctx.now,
            origin,
            // The one path with real mute state in hand. Every other caller
            // passes `None` and renders `muted: false`.
            muted: Some(&ctx.muted),
            polls: &polls,
        };
        self.assembler()
            .assemble_many(statuses, &reblog_targets, &render_ctx)
            .await
    }

    fn assembler(&self) -> StatusRenderAssembler {
        StatusRenderAssembler::new(
            self.pool.clone(),
            Arc::clone(&self.accounts),
            self.media_store.clone(),
        )
    }

    /// Fetches a boost's target and re-checks its visibility against the
    /// *target's own* author.
    ///
    /// A boost row's own visibility snapshot, already checked by
    /// `TimelineFilter::keep`, says nothing about the target's author — so a
    /// target that fails this check is treated exactly like a missing one:
    /// `reblog: None`, never a partial render of content the viewer has no
    /// right to see.
    async fn resolve_reblog_target(
        &self,
        status: &Status,
        ctx: &FilterContext,
    ) -> Result<Option<Status>, AppError> {
        let Some(target_id) = status.reblog_of_id else {
            return Ok(None);
        };
        let target = status_repository::find_by_id(&self.pool, target_id).await?;
        Ok(target.filter(|target| Self::reblog_target_visible(target, ctx)))
    }

    /// The same pure visibility function `TimelineFilter::keep` calls, with
    /// a [`ViewerRelation`] built the identical way, just keyed to
    /// `target`'s `actor_id` rather than the booster's.
    fn reblog_target_visible(target: &Status, ctx: &FilterContext) -> bool {
        let rel = ViewerRelation {
            is_follower: ctx.viewer.is_some() && ctx.following.contains(&target.actor_id),
        };
        is_visible(target, ctx.viewer, &rel)
    }
}

/// Reads polls straight from the repository and degrades to a poll-less
/// status when the row is gone, rather than failing the whole page.
struct TolerantPolls {
    pool: PgPool,
}

impl PollResolver for TolerantPolls {
    /// Two queries for the whole batch — one
    /// [`poll_repository::find_polls_by_ids`], one
    /// [`poll_repository::tally_many`] — regardless of how many ids it is
    /// given, and none at all for an empty one (Requirement 5.1: a
    /// timeline's poll lookups must not scale with its length).
    fn resolve_many<'a>(&'a self, poll_ids: &'a [Id], viewer: Option<Id>) -> PollResolution<'a> {
        Box::pin(async move {
            let polls = poll_repository::find_polls_by_ids(&self.pool, poll_ids).await?;

            // Tallied for the ids that actually resolved, not the whole
            // requested set: the per-id loop this replaces only reached
            // `tally` from inside `if let Some(poll)`, and a dangling id
            // would contribute nothing to `tally_many`'s result either (it
            // keys off the `polls` rows themselves), so including it would
            // only widen the query's id list.
            let found: Vec<Id> = poll_ids
                .iter()
                .copied()
                .filter(|poll_id| polls.contains_key(poll_id))
                .collect();
            let tallies = poll_repository::tally_many(&self.pool, &found, viewer).await?;

            // Built by filtering `poll_ids`, so `found` — and therefore the
            // result — keeps the order the caller asked in rather than
            // either map's iteration order.
            let mut out = Vec::with_capacity(found.len());
            for poll_id in found {
                // A poll deleted between the two queries above is dropped
                // here, which is this resolver's whole contract: a missing
                // poll costs its status its `poll` field, never the page.
                if let (Some(poll), Some(tally)) = (polls.get(&poll_id), tallies.get(&poll_id)) {
                    out.push((poll_id, poll.clone(), tally.clone()));
                }
            }
            Ok(out)
        })
    }
}
