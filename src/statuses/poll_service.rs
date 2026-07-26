//! `PollService` (design.md "Service / サービス層" -> `#### InteractionService
//! / PollService`, design.md lines ~563-588; Requirements 13.2, 13.3, 13.4,
//! 13.5, 13.6; task 5.3, `Boundary: PollService`): the poll get/vote
//! orchestration that ties together `PollRepository` (task 2.3, deadline/
//! range/single-vs-multiple/duplicate validation and tally), `StatusRepository`
//! (task 2.1, resolving a poll's owning `Status` for visibility), `visibility::
//! is_visible` (task 3.1), and `StatusActivityBuilder::deliver_vote` (task
//! 4.1).
//!
//! ## Scope
//! Owns exactly two methods — [`PollService::poll`] (a `get`, implied by the
//! API contract table's `GET /api/v1/polls/:id`, not itself named by any of
//! 13.2-13.6 but the minimal thing voting/inspection needs, mirroring how
//! `status_service.rs::show`/`interaction_service.rs` each supply an
//! analogous "fetch + visibility-gate" read alongside their mutating
//! methods) and [`PollService::vote`] (Requirements 13.2-13.6: visibility
//! gate, delegate validation to `PollRepository::record_vote`, interpret the
//! outcome, dispatch a vote Activity on success). Does **not** implement
//! `create_poll`/poll creation — see "Poll creation (Requirement 13.1) is out
//! of this task's scope" below, a documented boundary decision this task's
//! own instructions asked to be resolved and reported rather than guessed
//! past. Does not implement `InteractionService` (task 5.2, already
//! shipped, a separate boundary this module does not touch) or
//! `StatusService` (task 5.1, already shipped and likewise untouched — see
//! that section below). Does not implement the HTTP surface (`StatusEndpoints`,
//! a later task) or call `serializer::poll_to_json` itself — this service
//! only returns domain [`Poll`]/[`PollTally`] values, mirroring
//! `status_service.rs`'s/`interaction_service.rs`'s identical "service
//! returns a domain struct; only the endpoint layer serializes" boundary.
//!
//! ## Poll creation (Requirement 13.1) is out of this task's scope
//! (CONCERN — documented judgment call, per this task's own instructions)
//! `status_service.rs`'s own "Poll handling" doc comment (task 5.1) already
//! records the decision to 422-reject a poll-bearing `create_status` request
//! rather than persist it, citing design.md's Requirements Traceability
//! table (13.1's owning components: "PollService, PollRepository,
//! StatusActivityBuilder" — not `StatusService`) and naming *this* task
//! (5.3) as the one meant to actually own poll creation eventually.
//! Re-examining that hand-off against this task's own concrete instruction
//! text, three independent signals point the same way — **13.1/`create_poll`
//! wiring stays out of this task's scope, and `status_service.rs` is left
//! untouched**:
//!
//! 1. Task 5.3's own `_Requirements:_` line in `tasks.md` reads exactly
//!    "13.2, 13.3, 13.4, 13.5, 13.6" — 13.1 is conspicuously absent, even
//!    though it sits in this task's `_Depends` chain (2.3 covers 13.1's
//!    repository half). A task's own `_Requirements:_` line is this spec's
//!    authoritative scope declaration (every other task in this file follows
//!    the same convention — e.g. 5.1's line omits 9.x/10.x/13.x for the same
//!    structural reason), so its omission here is a scope boundary, not an
//!    oversight.
//! 2. Task 5.3's own instruction prose is exhaustively verb-limited: "poll
//!    取得・投票（締切/範囲/単複/重複検証）・集計反映・投票 Activity 配送を
//!    実装する" (get/vote/tally-reflect/dispatch) and its observable-
//!    completion line ("締切前の有効投票で集計が更新され投票 Activity が配送
//!    される、無効投票が拒否される") — neither mentions 作成 (creation)
//!    anywhere, unlike task 5.1's own instruction prose, which explicitly
//!    named "投票排他" (poll/media *exclusivity checking*, which 5.1 already
//!    implements) while still not naming poll *persistence* as its job
//!    either. No task in this spec's `tasks.md` currently claims poll
//!    *creation* as its named instruction verb.
//! 3. Actually wiring 13.1 through would require modifying
//!    `status_service.rs::create_status` (`status_service.rs`'s own boundary,
//!    already implemented and reviewed under task 5.1, `_Boundary:
//!    StatusService_` — disjoint from this task's own `_Boundary: PollService_`)
//!    to replace its current 422-rejection branch with a real
//!    `PollRepository::insert_poll` call, sequenced against the *same*
//!    transaction/id-minting that creates the owning `Status` row — a
//!    cross-cutting orchestration change to an already-reviewed, disjoint-
//!    boundary module that this task's critical constraints explicitly
//!    default against absent "strong, documented justification" for crossing
//!    it, and no such justification-by-necessity exists: this module can be
//!    fully implemented, tested, and pass its own observable-completion
//!    criterion (get/vote/tally/dispatch, all against *pre-existing* polls a
//!    test fixture inserts directly via `poll_repository::insert_poll`, the
//!    same "insert fixtures directly, bypass the creating service" pattern
//!    `interaction_service/tests.rs`'s own doc comment already establishes
//!    for reblog/favourite targets) without ever touching `create_status`.
//!
//! `PollRepository::insert_poll` (task 2.3, already shipped) remains fully
//! available, unmodified, and untouched by this task — the "create_poll-
//! capable repository call" a future task needs already exists; this task
//! adds no new orchestration wrapper around it. A `PollService::create_poll`
//! method is deliberately *not* added here: with `status_service.rs`
//! correctly left untouched (point 3 above), no caller in this run would
//! ever invoke such a method, and no test could exercise it end-to-end
//! (a poll-creation test would have nothing to assert beyond "a row I told
//! it to insert got inserted", already covered by `poll_repository/tests.rs`'s
//! own `insert_poll` tests) — adding it now would be speculative surface
//! this task's own "do not gold-plate" instruction argues against. The
//! actual 13.1 wiring — replacing `status_service.rs::create_status`'s 422
//! branch with a real create-poll call, most likely as part of whichever
//! future task threads `PollService` into the create-status flow or the
//! HTTP endpoint layer (task 7.x) — is left for that future task, exactly as
//! task 5.1's own doc comment already anticipated.
//!
//! ## `poll`/`vote`'s return shape: `(Poll, PollTally)`, not design.md's bare
//! `Poll` (documented deviation)
//! design.md's Service Interface sketch reads `vote(...) -> Result<Poll,
//! AppError>` and does not sketch a `get` method at all. [`Poll`]
//! (`model.rs`) itself carries no vote-count fields whatsoever (`id`/
//! `status_id`/`expires_at`/`multiple` only) — the "更新後の集計を反映した
//! Poll を返す" (13.2) and the analogous "get" this module's own instruction
//! asks for are structurally answerable only by also returning
//! [`crate::statuses::poll_repository::PollTally`] (`options`/`voters_count`/
//! `own_votes`), `PollRepository::tally`'s own already-shipped (task 2.3)
//! return type, one layer below the eventual `PollSerializer::poll_to_json`
//! (task 3.3's own doc comment: "事前解決済み集計値を受け取る"). Both
//! [`PollService::poll`] and [`PollService::vote`] therefore return
//! `(Poll, PollTally)` — the same "return everything the caller/serializer
//! needs, in domain form, since design.md's own model type cannot carry it"
//! reasoning `status_service.rs`/`serializer.rs` already applied to their own
//! comparable gaps.
//!
//! ## Uniform-404 for an invisible poll's owning status (mirrors 5.1/5.2's
//! established convention)
//! Both [`poll`](PollService::poll) and [`vote`](PollService::vote) resolve
//! the poll's owning [`Status`] and gate on [`visibility::is_visible`]
//! *before* touching `poll_repository` at all, reporting an invisible target
//! the same uniform `404 Not Found` `interaction_service.rs`'s own doc
//! comment ("Visibility-then-not-found ordering") already establishes for
//! reblog/favourite/bookmark targets — a caller cannot distinguish "no such
//! poll" from "this poll exists but you may not see it".
//!
//! ## Resolving `deliver_vote`'s recipient is local-only (same structural
//! gap 5.1/5.2 already document)
//! [`vote`](PollService::vote) resolves the poll-owning status's *author* to
//! an [`ActorRef`] (via [`ActorHandleLookup`], reused from task 4.1) to
//! supply [`StatusActivityBuilder::deliver_vote`]'s `recipient` parameter.
//! Exactly like `interaction_service.rs`'s own documented gap ("Resolving a
//! target's author is local-only"), this port only ever resolves a **local**
//! actor — voting on a remote-authored poll's owning status therefore fails
//! this port's own `404`-shaped error rather than silently degrading. Not a
//! new gap introduced here; the same accounts-and-instance boundary named
//! throughout this spec.
//!
//! ## Vote titles come from the post-vote `PollTally`, not the raw request
//! `choices` slice
//! [`vote`](PollService::vote) resolves `choice_titles` for
//! `deliver_vote` by re-reading `tally.own_votes` (the freshly-committed,
//! already-deduplicated set `PollRepository::record_vote` actually
//! persisted — see that function's own doc comment, "Vote validation order
//! and duplicate-request-choice handling") rather than re-deriving its own
//! dedup of the caller-supplied `choices: &[i32]` parameter a second time.
//! This guarantees the dispatched Activity's `name` fields always match
//! what is actually stored (single source of truth), rather than trusting
//! this module's own independent re-derivation of the repository's already-
//! applied dedup logic to stay in sync with it.

#[cfg(test)]
mod tests;

use axum::http::StatusCode;
use sqlx::postgres::PgPool;

use crate::domain::Id;
use crate::error::AppError;
use crate::federation::{ActorUrls, DeliverySink, LocalActorLookup, Recipient};
use crate::runtime::RuntimeContext;
use crate::statuses::activity_builder::{ActorHandleLookup, StatusActivityBuilder};
use crate::statuses::addressing::ActorRef;
use crate::statuses::model::{Poll, Status};
use crate::statuses::poll_repository::{self, PollTally};
use crate::statuses::status_repository;
use crate::statuses::visibility::{self, RelationshipQuery};

fn not_found() -> AppError {
    AppError::client(StatusCode::NOT_FOUND, "poll not found")
}

/// The poll get/vote business-service layer (design.md's `PollService` half
/// of the shared `InteractionService / PollService` component, task 5.3).
/// Generic over the same delegation ports `StatusService`/`InteractionService`
/// already established for the identical reasons (see those modules' own
/// doc comments) — `A`/`D`/`L`/`H` (via the embedded [`StatusActivityBuilder`])
/// and `R` ([`RelationshipQuery`], task 3.1).
pub struct PollService<A, D, L, H, R>
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

impl<A, D, L, H, R> PollService<A, D, L, H, R>
where
    A: ActorHandleLookup,
    D: LocalActorLookup,
    L: DeliverySink,
    H: DeliverySink,
    R: RelationshipQuery,
{
    /// Builds a `PollService` bound to `pool` (repository calls), `runtime`
    /// (clock injection for the vote-deadline check — `RuntimeContext::clock`,
    /// never a fresh ad hoc read), `urls` (resolving a poll's owning status's
    /// author's URL for `deliver_vote`'s `recipient`), `activity_builder`
    /// (Activity generation + delivery, task 4.1), `actor_lookup`
    /// ([`ActorHandleLookup`], resolving the poll-owning status's author to
    /// an [`ActorRef`] — see this module's doc comment, "Resolving
    /// `deliver_vote`'s recipient is local-only"), and `relationship`
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
    /// policy `status_service.rs`/`interaction_service.rs` route through,
    /// applied here to a poll's owning status (Requirement 13.2's "締切前の
    /// 可視な投票に対し").
    async fn visible_to(&self, status: &Status, viewer: Option<Id>) -> Result<bool, AppError> {
        let rel = self
            .relationship
            .viewer_relation(status.actor_id, viewer)
            .await?;
        Ok(visibility::is_visible(status, viewer, &rel))
    }

    /// Resolves `poll_id`'s owning [`Status`], gated on `viewer`
    /// visibility — the shared first half of both [`poll`](Self::poll) and
    /// [`vote`](Self::vote) (see this module's doc comment, "Uniform-404 for
    /// an invisible poll's owning status"). Returns both the [`Poll`] row and
    /// its owning [`Status`], since both callers need the latter (visibility
    /// already checked) for their own next step.
    async fn visible_poll_and_status(
        &self,
        poll_id: Id,
        viewer: Option<Id>,
    ) -> Result<(Poll, Status), AppError> {
        let poll = poll_repository::find_poll_by_id(&self.pool, poll_id)
            .await?
            .ok_or_else(not_found)?;
        let status = status_repository::find_by_id(&self.pool, poll.status_id)
            .await?
            .ok_or_else(not_found)?;
        if !self.visible_to(&status, viewer).await? {
            return Err(not_found());
        }
        Ok((poll, status))
    }

    /// Resolves `actor_id` to an [`ActorRef`] (URI + delivery [`Recipient`])
    /// via this service's own [`ActorHandleLookup`] — see this module's doc
    /// comment ("Resolving `deliver_vote`'s recipient is local-only") for the
    /// boundary this deliberately does not cross (no remote resolution).
    async fn actor_ref_for(&self, actor_id: Id) -> Result<ActorRef, AppError> {
        let handle = self.actor_lookup.resolve_handle(actor_id).await?;
        let uri = self.urls.actor_url(&handle);
        Ok(ActorRef {
            uri,
            recipient: Recipient::Local(handle),
        })
    }

    /// Fetches poll `poll_id` and its current tally, visibility-gated
    /// through its owning status (the "get" this module's own instruction
    /// asks for — see this module's doc comment, "Scope"). `viewer` both
    /// gates visibility (a `private` poll's owning status requires
    /// `viewer` to be a follower, or the author) and selects whose
    /// `own_votes` the returned [`PollTally`] reflects (Requirement 2.2).
    pub async fn poll(
        &self,
        viewer: Option<Id>,
        poll_id: Id,
    ) -> Result<(Poll, PollTally), AppError> {
        let (poll, _status) = self.visible_poll_and_status(poll_id, viewer).await?;
        let tally = poll_repository::tally(&self.pool, poll_id, viewer).await?;
        Ok((poll, tally))
    }

    /// Records `actor_id`'s vote for `choices` in poll `poll_id` (Requirements
    /// 13.2-13.6): resolves the poll's owning status and rejects an invisible
    /// one (a uniform not-found — see this module's doc comment), then
    /// delegates deadline/range/single-vs-multiple/duplicate validation to
    /// [`poll_repository::record_vote`] (this task's job is orchestration,
    /// not re-implementing validation `PollRepository` already owns per task
    /// 2.3's own doc comment). On success, re-fetches the updated tally,
    /// resolves the actually-persisted choices' titles from it, and
    /// dispatches a vote Activity via
    /// [`StatusActivityBuilder::deliver_vote`] (Requirement 13.6) before
    /// returning the updated `(Poll, PollTally)`. Any rejection
    /// (deadline passed, out-of-range choice, single-choice-with-multiple-
    /// selections, or duplicate vote) propagates as the `AppError`
    /// `poll_repository::record_vote` already reports — never silently
    /// swallowed, never re-mapped to a different shape.
    pub async fn vote(
        &self,
        actor_id: Id,
        poll_id: Id,
        choices: &[i32],
    ) -> Result<(Poll, PollTally), AppError> {
        let (poll, status) = self
            .visible_poll_and_status(poll_id, Some(actor_id))
            .await?;

        let now = self.runtime.clock.now();
        poll_repository::record_vote(&self.pool, poll_id, actor_id, choices, now).await?;

        let tally = poll_repository::tally(&self.pool, poll_id, Some(actor_id)).await?;

        // See this module's doc comment ("Vote titles come from the
        // post-vote `PollTally`, not the raw request `choices` slice"):
        // `tally.own_votes` is the authoritative, already-persisted,
        // already-deduplicated set of this actor's choices for this poll.
        let choice_titles: Vec<String> = tally
            .options
            .iter()
            .filter(|option| tally.own_votes.contains(&option.idx))
            .map(|option| option.title.clone())
            .collect();

        let recipient = self.actor_ref_for(status.actor_id).await?;
        self.activity_builder
            .deliver_vote(actor_id, &poll, &status, &choice_titles, recipient)
            .await?;

        Ok((poll, tally))
    }
}
