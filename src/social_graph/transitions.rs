//! `Transitions` (design.md "Social Graph Domain / ドメイン層" ->
//! `#### Transitions`; Requirements 1.1, 1.4, 2.5, 2.6, 3.1, 3.2, 5.1, 5.2,
//! 7.2, 7.3, 7.7; task 2.2, `Boundary: Transitions, RelationshipRepository`):
//! the common relationship state-transition functions the API path (task
//! 3.x's `FollowService`/`FollowRequestService`/`BlockService`) and the
//! inbound Activity path (task 4.1's `InboundHandler`) both converge on, so
//! neither path can drift on what a "follow established"/"blocked" state
//! transition actually does (design.md's "意味論対称"). This is also the
//! *sole* point that emits notifications' `follow`/`follow_request`
//! `NotificationEvent`s after a successful commit.
//!
//! ## Scope
//! This module owns exactly design.md's `Transitions` Service Interface:
//! [`Transitions::establish_follow`], [`Transitions::remove_follow`],
//! [`Transitions::record_pending`], [`Transitions::promote_pending`],
//! [`Transitions::drop_pending`], [`Transitions::apply_block`],
//! [`Transitions::clear_block`], [`Transitions::mark_blocked_by`],
//! [`Transitions::clear_blocked_by`]. No `FollowApprovalPolicy` (task 2.1,
//! already implemented — this module never re-derives same-server/approval
//! decisions, it only executes the transition a caller has already decided
//! on), no `ActivityBuilder`, no business service, no inbound Activity
//! handler, and no HTTP surface live here — those *call* this module but are
//! out of scope for task 2.2 (`Boundary: Transitions, RelationshipRepository`).
//! This task's boundary does include `RelationshipRepository` itself: see
//! `repository.rs`'s own doc comment ("Task 2.2 additions") for the two
//! small, additive extensions this module needed from it.
//!
//! ## `&self` struct, not free functions (unlike `repository.rs`)
//! `repository.rs`'s own doc comment documents why *that* module uses free
//! `pub async fn name(pool: &PgPool, ...)` functions instead of design.md's
//! illustrative `&self` sketch: every actually-existing `*_repository.rs`
//! file in this crate does that, without exception — a Data-layer
//! convention. `Transitions` is a Service-layer component (design.md's own
//! "Contracts: Service [x]" tag, same as `FollowApprovalPolicy`,
//! `FollowService`, `StatusService`, `InteractionService`), and every
//! already-implemented Service-layer component in this crate
//! (`FollowApprovalPolicy` — task 2.1; `StatusService`/`InteractionService`
//! — statuses-core) is a `&self` struct holding its own `pool`/`runtime`/
//! collaborator handles. This module follows that Service-layer convention
//! (and design.md's own literal `&self` Service Interface sketch) rather
//! than the Data-layer one.
//!
//! ## Collaborators held (mirrors `StatusService`/`InteractionService`)
//! - `pool: PgPool` — drives `repository.rs` calls directly, and opens its
//!   own `sqlx::Transaction` for [`Transitions::apply_block`]/
//!   [`Transitions::mark_blocked_by`]'s single-transaction requirement
//!   (design.md: "`apply_block` は単一トランザクションで...解消してから確
//!   定", Requirement 5.2). No other function needs a transaction: each of
//!   `establish_follow`/`remove_follow`/`record_pending`/`promote_pending`/
//!   `drop_pending`/`clear_block`/`clear_blocked_by` performs exactly one
//!   relationship-table write (already atomic as a single statement).
//! - `runtime: RuntimeContext` — `runtime.ids.next_id()` mints every new
//!   `follows.id`/`follow_requests.id`/`blocks.id` this module writes, and
//!   `runtime.clock.now()` stamps every `created_at`/`occurred_at` this
//!   module writes/emits — never `OffsetDateTime::now_utc()` directly
//!   (steering's determinism rule; mirrors `ActorService::create_actor`'s
//!   identical `self.runtime.ids`/`self.runtime.clock` precedent).
//! - `notifications: NotificationSinkRegistry` — the "実装ハンドルを保持す
//!   る（`AppState` 上のレジストリ経由...）" handle design.md's own
//!   `Transitions` doc calls for. `NotificationEventSink`/`NotificationEvent`/
//!   `NotificationType`/`NotificationSinkRegistry`/`NoopSink` are defined in
//!   `crate::statuses::notification_sink`, not a `crate::notifications`
//!   module — see that module's own doc comment ("Why this module exists
//!   inside statuses-core, not notifications") for why: notifications is
//!   downstream of statuses-core in the roadmap and has no implemented
//!   module yet, so statuses-core (which needed this seam first) defined
//!   the port `notifications/design.md` specifies verbatim, for any spec
//!   that needs it before notifications itself exists — social-graph is
//!   exactly that situation. Held as `NotificationSinkRegistry` (not a bare
//!   `Arc<dyn NotificationEventSink>`) to mirror `StatusService`/
//!   `InteractionService`'s identical field/constructor shape exactly: a
//!   fresh `NotificationSinkRegistry::new()` already defaults to `NoopSink`,
//!   so a `Transitions` built without any bootstrap wiring (this task's own
//!   situation — `SocialGraphModule` wiring is task 5.2's boundary) is safe
//!   and complete on its own (design.md: "notifications 未構築時は既定
//!   `NoopSink` のため安全に no-op").
//!
//! ## Notification emit: only on a genuinely *new* transition (Requirements
//! 1.1, 1.4, 2.5, 2.6, 7.7 — "冪等に emit")
//! [`Transitions::establish_follow`] and [`Transitions::record_pending`] are
//! the *only* two functions that ever call `self.notifications.emit(...)`
//! (every other function in this module never touches `notifications` at
//! all) — design.md's exact "emit は `establish_follow` / `record_pending`
//! という単一の合流点でのみ行い...二重に生成しない" constraint. Each
//! function emits *conditionally*, only when the underlying
//! `repository::upsert_follow`/`upsert_request` call reports it just
//! inserted a brand-new row (`is_new == true`), never on a repeat/idempotent
//! re-application of an already-established follow or already-pending
//! request — see `repository.rs`'s own doc comment ("Task 2.2 additions")
//! for why that repository-level `bool` (not a plain `rows_affected()`
//! count) is what makes this distinction possible under this module's own
//! `DO UPDATE`-based idempotent upsert. This exactly mirrors
//! `interaction_service.rs::favourite`'s identical established precedent:
//! "emit exactly once per *new* favourite...a repeat call never re-emits".
//!
//! `establish_follow` additionally only emits when `followee` is
//! [`AccountRef::Local`] (notifications' recipients are local-only by
//! construction — emitting for a remote `followee` would violate that
//! invariant, design.md's own "`followee` がリモートなら emit しない"
//! wording). `record_pending` additionally only emits when
//! `req.direction == FollowRequestDirection::Inbound` (an outbound pending
//! request's target is always remote — the same recipient-must-be-local
//! reasoning, design.md's "`direction == Outbound` は宛先がリモートのため
//! emit しない").
//!
//! ## Emit propagates a sink failure via `?`, does not swallow it
//! `establish_follow`/`record_pending` call `self.notifications.emit(...)
//! .await?` — propagating `Result<(), AppError>` with `?`, exactly like
//! `status_service.rs::create_status`'s and
//! `interaction_service.rs::favourite`'s identical established precedent for
//! the exact same `NotificationSinkRegistry::emit` call. This is not a
//! contradiction of "notification failures must not fail the transition":
//! the DB write (the actual relationship-state transition) has already been
//! committed by the time `emit` runs (this module's own "コミット後" doc
//! wording, design.md's requirement) — a failing `emit` call surfaces as
//! this function's own `Err`, but the caller-visible symptom is "the
//! notification-emit step failed", not "the follow was not established";
//! nothing this module does rolls the already-committed follow/pending-
//! request row back on an emit failure. Following the crate's one existing
//! precedent for this exact call, rather than inventing a new
//! log-and-ignore policy, is this task's own explicit instruction.
//!
//! ## `apply_block` / `mark_blocked_by` share one private transactional core
//! Both perform the identical sequence — delete both-direction follows,
//! delete all four requester/direction combinations of pending requests
//! between the pair, then upsert the `Block` row — in one transaction
//! (design.md, Requirement 5.2's "双方向フォロー、...両方向の保留中フォロー
//! リクエストを解消する"). The only difference is *why* each is called
//! (`apply_block`: this instance's own owner actively blocking someone,
//! Requirement 5.1/5.2, carries a real outbound Block Activity id;
//! `mark_blocked_by`: reacting to a received Block Activity naming this
//! instance's own local actor as the target, Requirement 7.4 — design.md's
//! signature for this case takes no `activity_id` at all, since this
//! instance never sends its own Undo(Block) for a block *against* it). Both
//! delegate to the private [`Transitions::block_and_clear`] helper, which
//! takes an already-resolved `activity_id: String` — `apply_block` passes
//! its caller-supplied Activity id; `mark_blocked_by` passes an empty
//! string (documented at its own call site) since there is no locally
//! meaningful outbound Activity id for a `blocked_by`-only relationship.
//! Deleting all four `(requester, direction)` combinations for the pair
//! (rather than guessing which one direction applies) is deliberately
//! over-inclusive but always correct: at most one of the four rows can ever
//! exist for a given (blocker, blocked) ordered pair under this table's own
//! uniqueness constraints (`migrations/0012_social_graph.sql`), so the other
//! three deletes are simply no-ops.
//!
//! ## Idempotency (Requirements 1.6, 7.7) is inherited structurally, not
//! re-implemented
//! Every function in this module either (a) calls a `repository.rs` upsert
//! that is itself idempotent via `ON CONFLICT ... DO UPDATE` (repeat calls
//! never duplicate a row, only refresh its mutable fields), or (b) calls a
//! `repository.rs` delete that is itself idempotent (absence is a `false`/
//! `None` success, never an error). This module adds no separate
//! idempotency bookkeeping of its own — a second `apply_block` call, for
//! instance, simply finds nothing left to delete and re-upserts the same
//! `Block` row's `activity_id`, never erroring and never corrupting state.
//!
//! ## `promote_pending`'s follow options (design.md's own signature gap)
//! design.md's `promote_pending(&self, requester, target)` signature takes
//! no [`FollowOptions`] — unlike `establish_follow`, which does — and
//! [`FollowRequest`] (task 1.2, `model.rs`) carries no options field to
//! recover one from either. The newly-established [`Follow`] this function
//! writes therefore uses Mastodon's own documented defaults
//! (`reblogs: true`, `notify: false`, `languages: []`) rather than
//! inventing an options-recovery mechanism outside this task's boundary —
//! a caller that needs different behavior-option handling for the
//! post-Accept follow is a `FollowService`/task-3.x-level concern, not this
//! function's. What *is* recovered and preserved is the original pending
//! request's `activity_id` (the outbound Follow Activity id this instance
//! already sent) via [`crate::social_graph::repository::take_request`], so
//! the newly-established follow's eventual Undo(Follow) still references
//! the correct Activity.

#[cfg(test)]
mod tests;

use axum::http::StatusCode;
use sqlx::postgres::PgPool;

use crate::domain::AccountRef;
use crate::error::AppError;
use crate::runtime::RuntimeContext;
use crate::social_graph::model::{
    Block, Follow, FollowOptions, FollowRequest, FollowRequestDirection,
};
use crate::social_graph::repository;
use crate::statuses::notification_sink::{
    NotificationEvent, NotificationSinkRegistry, NotificationType,
};

fn map_tx_error(source: sqlx::Error) -> AppError {
    AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source)
}

/// Derives the [`FollowRequestDirection`] a pending request between
/// `requester` and some target must have been recorded under, from
/// `requester`'s own [`AccountRef`] variant — mirrors
/// `FollowApprovalPolicy::requires_approval`'s established idiom (task 2.1)
/// of deriving same-server-ness by matching `AccountRef` variants directly,
/// rather than accepting a precomputed value from the caller. Matches
/// `record_pending`'s/`model.rs`'s own documented convention: a request
/// `requester` sent (`AccountRef::Local`, this instance's own outbound
/// Follow) is always recorded `Outbound`; a request `requester` sent to us
/// (`AccountRef::Remote`) is always recorded `Inbound`. [`Self::promote_pending`]
/// and [`Self::drop_pending`] both use this to look up the correct pending
/// row regardless of which side originated the request.
fn pending_direction_for(requester: &AccountRef) -> FollowRequestDirection {
    match requester {
        AccountRef::Local(_) => FollowRequestDirection::Outbound,
        AccountRef::Remote(_) => FollowRequestDirection::Inbound,
    }
}

/// The common relationship state-transition functions (design.md's exact
/// `Transitions`). See this module's doc comment for the full rationale
/// behind its shape, collaborators, and the notification-emit convergence
/// point.
#[derive(Clone)]
pub struct Transitions {
    pool: PgPool,
    runtime: RuntimeContext,
    notifications: NotificationSinkRegistry,
}

impl Transitions {
    /// Builds a `Transitions` bound to `pool` (repository calls / its own
    /// transactions), `runtime` (id/clock injection, this module's doc
    /// comment), and `notifications` (the `NotificationEventSink` handle —
    /// a fresh [`NotificationSinkRegistry::new()`] already defaults to
    /// `NoopSink`, so passing one here is safe even before any bootstrap
    /// wiring exists, per this module's doc comment).
    pub fn new(
        pool: PgPool,
        runtime: RuntimeContext,
        notifications: NotificationSinkRegistry,
    ) -> Self {
        Self {
            pool,
            runtime,
            notifications,
        }
    }

    /// Establishes (or idempotently re-affirms) that `follower` follows
    /// `followee`, applying `opts`'s behavior flags and stamping the
    /// relationship's `activity_id` (Requirements 1.1, 1.5). After the
    /// underlying write commits, emits a `NotificationType::Follow`
    /// [`NotificationEvent`] — but only when this call's write was a
    /// genuinely *new* follow (not a repeat/option-refresh call) and only
    /// when `followee` is [`AccountRef::Local`] — see this module's doc
    /// comment ("Notification emit").
    pub async fn establish_follow(
        &self,
        follower: &AccountRef,
        followee: &AccountRef,
        opts: &FollowOptions,
        activity_id: &str,
    ) -> Result<(), AppError> {
        let now = self.runtime.clock.now();
        let id = self.runtime.ids.next_id();
        let follow = Follow {
            follower: *follower,
            followee: *followee,
            reblogs: opts.reblogs,
            notify: opts.notify,
            languages: opts.languages.clone(),
            activity_id: activity_id.to_string(),
            created_at: now,
        };

        let is_new = repository::upsert_follow(&self.pool, id, &follow).await?;

        if is_new && matches!(followee, AccountRef::Local(_)) {
            self.notifications
                .emit(NotificationEvent {
                    recipient: *followee,
                    origin: *follower,
                    kind: NotificationType::Follow,
                    target_status_id: None,
                    occurred_at: now,
                })
                .await?;
        }

        Ok(())
    }

    /// Removes the `follower` -> `followee` follow, if any (Requirement
    /// 1.4). Idempotent: a repeat call (or a call when no such follow ever
    /// existed) is a no-op success, never an error. No notification is
    /// emitted (design.md lists only `establish_follow`/`record_pending` as
    /// emit points).
    pub async fn remove_follow(
        &self,
        follower: &AccountRef,
        followee: &AccountRef,
    ) -> Result<(), AppError> {
        repository::delete_follow(&self.pool, follower, followee).await?;
        Ok(())
    }

    /// Records (or idempotently re-affirms) `req` as a pending follow
    /// request (Requirement 2.1). After the underlying write commits, emits
    /// a `NotificationType::FollowRequest` [`NotificationEvent`] — but only
    /// when this call's write was a genuinely *new* pending request and only
    /// when `req.direction == FollowRequestDirection::Inbound` — see this
    /// module's doc comment ("Notification emit").
    pub async fn record_pending(&self, req: &FollowRequest) -> Result<(), AppError> {
        let id = self.runtime.ids.next_id();
        let is_new = repository::upsert_request(&self.pool, id, req).await?;

        if is_new && req.direction == FollowRequestDirection::Inbound {
            self.notifications
                .emit(NotificationEvent {
                    recipient: req.target,
                    origin: req.requester,
                    kind: NotificationType::FollowRequest,
                    target_status_id: None,
                    occurred_at: req.created_at,
                })
                .await?;
        }

        Ok(())
    }

    /// Promotes `requester`'s pending follow request to `target` into an
    /// established [`Follow`], on receipt of an Accept(Follow) (Requirement
    /// 2.5, and — via `FollowRequestService.authorize_request`, task 3.2 —
    /// Requirement 2.3). The pending row this consumes may be either
    /// direction depending on who called this function: `requester` being
    /// [`AccountRef::Local`] means *we* sent the original outbound Follow
    /// (Requirement 2.5's `InboundHandler` case, row recorded `Outbound`);
    /// `requester` being [`AccountRef::Remote`] means they sent us an inbound
    /// Follow our local owner is now authorizing (Requirement 2.3's
    /// `FollowRequestService.authorize_request` case, row recorded
    /// `Inbound`) — see [`pending_direction_for`], which derives this from
    /// `requester`'s own `AccountRef` variant rather than accepting a
    /// precomputed direction. If no such pending request exists (already
    /// promoted by an earlier call, or never requested), this is an
    /// idempotent no-op success. See this module's doc comment
    /// ("`promote_pending`'s follow options") for the behavior-option
    /// default this uses and why. No notification is emitted.
    pub async fn promote_pending(
        &self,
        requester: &AccountRef,
        target: &AccountRef,
    ) -> Result<(), AppError> {
        let Some(existing) = repository::take_request(
            &self.pool,
            requester,
            target,
            pending_direction_for(requester),
        )
        .await?
        else {
            return Ok(());
        };

        let now = self.runtime.clock.now();
        let id = self.runtime.ids.next_id();
        let follow = Follow {
            follower: *requester,
            followee: *target,
            reblogs: true,
            notify: false,
            languages: Vec::new(),
            activity_id: existing.activity_id,
            created_at: now,
        };
        repository::upsert_follow(&self.pool, id, &follow).await?;

        Ok(())
    }

    /// Drops `requester`'s pending follow request to `target`, on receipt of
    /// a Reject(Follow) (Requirement 2.6, and — via
    /// `FollowRequestService.reject_request`, task 3.2 — Requirement 2.4).
    /// As with [`Self::promote_pending`], the pending row this deletes may be
    /// either direction depending on `requester`'s own [`AccountRef`] variant
    /// — see [`pending_direction_for`]. Idempotent: a repeat call (or no such
    /// pending request) is a no-op success. No notification is emitted.
    pub async fn drop_pending(
        &self,
        requester: &AccountRef,
        target: &AccountRef,
    ) -> Result<(), AppError> {
        repository::delete_request(
            &self.pool,
            requester,
            target,
            pending_direction_for(requester),
        )
        .await?;
        Ok(())
    }

    /// Applies `blocker`'s block of `blocked` (Requirements 5.1, 5.2):
    /// within a single transaction, removes both-direction follows and all
    /// pending follow requests between the pair, then commits the `Block`
    /// row carrying `activity_id` (this instance's own outbound Block
    /// Activity id, for a future Undo(Block)). Idempotent: a repeat call
    /// finds nothing left to remove and simply re-affirms the `Block` row's
    /// `activity_id`. No notification is emitted.
    pub async fn apply_block(
        &self,
        blocker: &AccountRef,
        blocked: &AccountRef,
        activity_id: &str,
    ) -> Result<(), AppError> {
        self.block_and_clear(blocker, blocked, activity_id.to_string())
            .await
    }

    /// Removes `blocker`'s block of `blocked`, if any (Requirement 5.4,
    /// unblock). Idempotent no-op when no such block exists. Does not
    /// restore any follow/pending-request state the original block may have
    /// cleared — unblocking never implicitly re-establishes a relationship.
    /// No notification is emitted.
    pub async fn clear_block(
        &self,
        blocker: &AccountRef,
        blocked: &AccountRef,
    ) -> Result<(), AppError> {
        repository::delete_block(&self.pool, blocker, blocked).await?;
        Ok(())
    }

    /// Records that `target` (this instance's own local actor) is blocked by
    /// `source`, on receipt of a Block Activity naming `target` (Requirement
    /// 7.4): within a single transaction, removes both-direction follows and
    /// all pending follow requests between the pair, then commits the
    /// `Block` row. See this module's doc comment ("`apply_block` /
    /// `mark_blocked_by` share one private transactional core") for why no
    /// `activity_id` is recorded. Idempotent. No notification is emitted.
    pub async fn mark_blocked_by(
        &self,
        source: &AccountRef,
        target: &AccountRef,
    ) -> Result<(), AppError> {
        self.block_and_clear(source, target, String::new()).await
    }

    /// Clears the `source` being blocked by `target`... i.e. removes the
    /// `source` -> `target` `Block` row recorded by [`Self::mark_blocked_by`],
    /// on receipt of an Undo(Block) (Requirement 7.6). Idempotent no-op when
    /// no such block exists. No notification is emitted.
    pub async fn clear_blocked_by(
        &self,
        source: &AccountRef,
        target: &AccountRef,
    ) -> Result<(), AppError> {
        repository::delete_block(&self.pool, source, target).await?;
        Ok(())
    }

    /// Shared transactional core for [`Self::apply_block`]/
    /// [`Self::mark_blocked_by`] — see this module's doc comment for the
    /// full rationale. Opens one `sqlx::Transaction`, deletes both-direction
    /// follows and all four `(requester, direction)` pending-request
    /// combinations between `blocker`/`blocked`, upserts the `Block` row
    /// carrying `activity_id`, and commits. An early `?`-propagated error
    /// drops `tx` without committing — mirroring
    /// `ActorService::create_actor`'s identical established precedent
    /// (dropping an uncommitted transaction rolls every write inside it
    /// back, so a failure partway through never leaves a half-cleared
    /// relationship state).
    async fn block_and_clear(
        &self,
        blocker: &AccountRef,
        blocked: &AccountRef,
        activity_id: String,
    ) -> Result<(), AppError> {
        let now = self.runtime.clock.now();
        let id = self.runtime.ids.next_id();
        let block = Block {
            blocker: *blocker,
            blocked: *blocked,
            activity_id,
            created_at: now,
        };

        let mut tx = self.pool.begin().await.map_err(map_tx_error)?;

        repository::delete_follow(&mut *tx, blocker, blocked).await?;
        repository::delete_follow(&mut *tx, blocked, blocker).await?;
        repository::delete_request(&mut *tx, blocker, blocked, FollowRequestDirection::Outbound)
            .await?;
        repository::delete_request(&mut *tx, blocker, blocked, FollowRequestDirection::Inbound)
            .await?;
        repository::delete_request(&mut *tx, blocked, blocker, FollowRequestDirection::Outbound)
            .await?;
        repository::delete_request(&mut *tx, blocked, blocker, FollowRequestDirection::Inbound)
            .await?;
        repository::upsert_block(&mut *tx, id, &block).await?;

        tx.commit().await.map_err(map_tx_error)?;

        Ok(())
    }
}
