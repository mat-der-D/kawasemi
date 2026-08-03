//! `TimelineService` (design.md "Service / サービス層" -> `#### TimelineService`,
//! Requirements 1.1, 2.1, 3.1, 4.1, 5.1, 6.1, 7.1, 7.2, 7.3, 7.4, 10.1; task
//! 4.2, `Boundary: TimelineService`).
//!
//! Scope: this module owns exactly [`TimelineService::timeline`] — the
//! aggregation this task's own instruction names end to end: load the
//! viewer's relationship sets once into a [`FilterContext`] (`FilterQuery`,
//! Requirement 6.1) -> [`TimelineMatcher::candidate_spec`] -> repeated
//! [`crate::timelines::candidate_repository::fetch_candidates`] batches ->
//! [`TimelineFilter::keep`] -> post-filter fill-to-`limit` (bounded by
//! [`MAX_FILL_ITERATIONS`], Requirement 7.4) -> [`StatusHydrator::hydrate`]
//! -> a cursor-stable [`Page`] (api-foundation's `max_id`/`since_id`/
//! `min_id`/`limit` convention, Requirements 7.1-7.3). This module never
//! reimplements any of `TimelineMatcher`/`CandidateRepository`/
//! `TimelineFilter`/`StatusHydrator`/`paginate`'s own logic — it only
//! sequences calls into them (this task's own instruction: "候補取得→...
//! →フィルタ後充填...→具体化→...Page 組み立てを実装する").
//!
//! No `TimelineEndpoints`/`TimelinesModule` (later tasks), and no wiring
//! into `crate::state`/`crate::bootstrap`/`crate::server` (task 5.2) live
//! here — this component is not yet mounted anywhere, exactly like every
//! earlier task in `crate::timelines`'s own module doc comment already
//! documents for itself.
//!
//! ## Deliberate deviations from design.md's literal Service Interface
//! design.md's sketch (design.md line ~389) is:
//! ```text
//! pub async fn timeline(&self, kind: TimelineKind, viewer: Option<&RequestActorContext>, params: TimelineParams) -> Result<Page<serde_json::Value>, AppError>;
//! ```
//! Two deviations, each resolved the same conservative way this spec's
//! earlier tasks already established (`following_and_self` on
//! `fetch_candidates`, `reblogged_author` on `TimelineFilter::keep`,
//! `tags`/`reblogged_author` on `TimelineMatcher::matches`, `ctx`/`origin` on
//! `StatusHydrator::hydrate` — see this crate's tasks.md, "Implementation
//! Notes"): extend the signature with exactly the parameter(s)/type change
//! the real work needs, document why, never silently drop the requirement.
//!
//! 1. **`viewer: Option<&RequestActorContext>` -> `viewer_id: Option<Id>`.**
//!    Mirrors `crate::social_graph::follow_service::FollowService::follow`/
//!    `unfollow`'s own already-reviewed, identically-reasoned precedent
//!    (`src/social_graph/follow_service.rs`'s doc comment, "Deliberate
//!    deviations"): every use this method makes of "the caller" is a plain
//!    local actor [`Id`] (`FilterQuery::blocked_set`/`following_set`/
//!    `reblogs_hidden_set` all key on `AccountRef::Local(id)`, and
//!    `FilterContext::viewer`/`StatusHydrator`'s own viewer-scoped lookups
//!    are all plain `Id`-keyed) — never a scope set or token metadata. Since
//!    `RequestActorContext` (`crate::oauth::model`) belongs to a different
//!    spec (api-foundation/oauth) and this method never reads anything from
//!    it beyond `actor_id`, taking `Option<Id>` directly avoids adding an
//!    `oauth`-module dependency to `timelines` purely to immediately discard
//!    everything else `RequestActorContext` carries. `read:statuses` scope
//!    verification (Requirement 9.1) and the `RequestActorContext -> Id`
//!    extraction itself are `TimelineEndpoints`'s job (task 5.1, out of this
//!    task's boundary), exactly like `FollowService`'s own precedent leaves
//!    that extraction to its own endpoints layer.
//! 2. **`origin: &ForwardedOrigin` added.** [`StatusHydrator::hydrate`] (task
//!    4.1, already committed) requires a [`ForwardedOrigin`] to resolve
//!    Account/media/tag URLs — design.md's sketch predates that already-
//!    established deviation and has no parameter for it at all. It is
//!    per-request state, threaded straight through to `hydrate`, never
//!    stored on `self` (mirroring `StatusHydrator`'s own identical
//!    threading convention).
//!
//! ## The fill loop (Requirement 7.4)
//! [`MAX_FILL_ITERATIONS`] bounds the number of candidate batches a single
//! `timeline()` call will fetch, per design.md's own "小さな固定値、目安 5
//! 反復" (design.md ~line 382). Each iteration:
//! 1. Fetches one batch via `fetch_candidates`, bounded above by a
//!    shrinking `max_id` cursor (the previous batch's smallest fetched id,
//!    exclusive) and below by the *request's own* unchanged `since_id`/
//!    `min_id` — so consecutive batches are contiguous, non-overlapping id
//!    ranges: no candidate is ever re-fetched, and none is skipped between
//!    batches (Requirement 7.4's "欠落・重複・無限ループ無し").
//! 2. Filters the batch with [`TimelineFilter::keep`], resolving each
//!    boost's original-author id via `status_repository::find_by_id` first
//!    (reusing the exact lookup `StatusHydrator::hydrate_one`
//!    (`src/timelines/hydrator.rs`) already uses for its own nested-`reblog`
//!    resolution, per this task's own instruction not to re-derive that
//!    lookup differently) and appending survivors to an accumulator that
//!    stays globally id-descending across the whole loop (each batch is
//!    itself id-descending and strictly below the previous batch's floor).
//! 3. Stops when: the batch returned fewer rows than requested (the
//!    underlying candidate range is exhausted — no more rows to fetch, so
//!    continuing would be pointless, not "near-full-table" caution); *or*,
//!    for every anchor direction except the `min_id`-anchored "walk forward
//!    from the oldest edge" case (see below), the accumulator already holds
//!    `limit` survivors; *or* [`MAX_FILL_ITERATIONS`] batches have already
//!    been fetched.
//!
//! **Why the `min_id`-anchored case does not stop early at `limit`
//! survivors**: [`crate::api::pagination::paginate`]'s own documented
//! `min_id`-without-`since_id` semantics (`anchor_oldest`) select the
//! `limit` survivors *closest to* `min_id` — i.e. the *tail* of the
//! candidate window, not its head. Stopping as soon as `limit` survivors
//! have accumulated from the *top* of the window (nearest the unbounded/
//! `max_id` end) would hand `paginate` the wrong `limit`-sized slice for
//! that direction (the newest `limit` items, not the ones nearest
//! `min_id`). So for that one direction this loop instead continues
//! fetching regardless of accumulated count until the range is genuinely
//! exhausted or the iteration cap is hit — the documented cost of this
//! choice (design.md's own "上限に達した場合はエラーやブロッキングにせず...
//! 部分ページ...を返す") is that a `min_id` request spanning more than
//! `MAX_FILL_ITERATIONS × batch_limit` candidates gets a best-effort partial
//! page anchored at wherever the loop was forced to stop, not the
//! mathematically exact tail — an explicit, accepted tradeoff, not an
//! oversight.
//!
//! ## Cap-hit cursor override — only when `paginate` alone would leave none
//! When the cap is hit, this module hands the accumulated (possibly
//! `limit`-short) survivor list straight to [`paginate`] using the
//! *original* request's parsed cursors, and in the common case (at least one
//! survivor accumulated) does **not** independently override the resulting
//! `next_cursor` with the loop's own last-scanned-id bookkeeping: both
//! choices are equally correct there (no gap, no duplicate — the range
//! between the smallest surviving candidate's id and the true last-scanned
//! id was already fully examined and legitimately filtered out, so resuming
//! from either cursor value re-examines at most that already-excluded
//! range, never skips a real candidate), so reusing `paginate`'s own cursor
//! computation unmodified is the conservative choice there.
//!
//! But when the cap is hit with **zero** accumulated survivors (e.g. every
//! candidate in every scanned batch was filtered out — a heavily-blocking
//! viewer, or a very sparse tag), `paginate` on an empty pool returns
//! `next_cursor: None`, which is indistinguishable from its own "no more
//! results, this timeline is genuinely exhausted" case — but design.md's own
//! text is explicit that a cap-hit must still return "続きから再開可能な有
//! 効なカーソル" (a valid, resumable continuation cursor), precisely because
//! there usually *are* more unscanned candidates below the last-fetched
//! batch. So this is the one case this module does override `paginate`'s own
//! `next_cursor`: with the loop's own last-scanned id (`next_upper_bound`
//! after the final iteration), so a follow-up request with that `max_id`
//! resumes past the already-scanned-and-discarded range instead of
//! re-scanning the exact same dead range forever (or a client reading
//! `next_cursor: None` as "stop paginating" when the timeline is not, in
//! fact, exhausted).
//!
//! ## Batch sizing
//! Each fetch batch requests `parsed.limit * `[`BATCH_LIMIT_MULTIPLIER`]`
//! rows (clamped to at least 1) from `CandidateRepository` — enough headroom
//! that a single batch usually satisfies `limit` even after some candidates
//! are filtered out, without requesting an unbounded amount. `4` is this
//! module's own chosen constant (design.md specifies no exact multiplier);
//! combined with api-foundation's `MAX_LIMIT` (40), the largest a single
//! batch ever requests is `160` rows.
//!
//! ## Unauthenticated viewer (Requirement 9.2)
//! `viewer_id: None` builds an all-empty [`FilterContext`] without calling
//! `FilterQuery` at all (there is no viewer to load relationship sets for) —
//! `public`/`local`/`tag` must keep working unauthenticated (only `public`
//! visibility survives `TimelineFilter::keep`'s own `is_visible` delegation
//! either way); an unauthenticated `home` request simply yields an empty
//! `following_and_self` array (no rows match), rather than this module
//! rejecting it itself — enforcing the actual 401 (Requirement 1.6) is
//! `TimelineEndpoints`'s job (task 5.1, out of this task's boundary).

use serde_json::Value;
use sqlx::PgPool;
use std::collections::HashSet;

use crate::api::pagination::{Cursor, ForwardedOrigin, Page, StatusIdCursor, paginate};
use crate::domain::{AccountRef, Id};
use crate::error::AppError;
use crate::runtime::RuntimeContext;
use crate::social_graph::providers::FilterQuery;
use crate::statuses::model::Status;
use crate::statuses::status_repository;

use super::candidate_repository::fetch_candidates;
use super::filter::TimelineFilter;
use super::hydrator::StatusHydrator;
use super::matcher::TimelineMatcher;
use super::model::{FilterContext, TimelineKind, TimelineParams};

/// Fixed upper bound on the fill loop's batch-fetch iterations (Requirement
/// 7.4) — see this module's doc comment, "The fill loop", for the exact
/// stop conditions and design.md's own "目安 5 反復" rationale.
const MAX_FILL_ITERATIONS: u32 = 5;

/// Multiplier applied to the resolved page `limit` to size each fill-loop
/// batch fetch — see this module's doc comment, "Batch sizing".
const BATCH_LIMIT_MULTIPLIER: u32 = 4;

/// Converts a social-graph `Vec<AccountRef>` relationship set into the bare
/// `HashSet<Id>` shape [`FilterContext`]'s five relationship fields carry —
/// the established repo-wide extraction pattern (`AccountRef::Local(id) |
/// AccountRef::Remote(id) => id`, mirrored from
/// `crate::social_graph::relationship_mapper::RelationshipMapper::to_view`/
/// `crate::social_graph::repository`).
fn to_id_set(refs: Vec<AccountRef>) -> HashSet<Id> {
    refs.into_iter()
        .map(|account_ref| match account_ref {
            AccountRef::Local(id) | AccountRef::Remote(id) => id,
        })
        .collect()
}

/// `TimelineService` aggregates timeline retrieval end to end — see this
/// module's doc comment for the exact call sequence and every deviation
/// from design.md's literal sketch.
pub struct TimelineService {
    pool: PgPool,
    runtime: RuntimeContext,
    hydrator: StatusHydrator,
}

impl TimelineService {
    /// Builds a `TimelineService` from already-constructed collaborators —
    /// mirrors this crate's established "bundle, don't build" business-layer
    /// constructor convention (e.g. `StatusHydrator::new`).
    pub fn new(pool: PgPool, runtime: RuntimeContext, hydrator: StatusHydrator) -> Self {
        Self {
            pool,
            runtime,
            hydrator,
        }
    }

    /// Retrieves one page of `kind`'s timeline for `viewer_id` (`None` for
    /// an unauthenticated request) under `params`, following the sequence
    /// this module's doc comment names in full. See this module's doc
    /// comment ("Deliberate deviations") for why this signature takes
    /// `viewer_id: Option<Id>` + `origin: &ForwardedOrigin` rather than
    /// design.md's literal `viewer: Option<&RequestActorContext>` (no
    /// `origin`).
    ///
    /// Postcondition (design.md): each returned element is a Status JSON
    /// value that satisfies `kind`'s membership for `viewer_id`, id-
    /// descending, `limit` items or fewer, with a stable continuation
    /// cursor — including when the fill loop's iteration cap is reached
    /// (Requirement 7.4's partial-page-plus-valid-cursor guarantee, never an
    /// error or an infinite loop).
    pub async fn timeline(
        &self,
        kind: TimelineKind,
        viewer_id: Option<Id>,
        params: TimelineParams,
        origin: &ForwardedOrigin,
    ) -> Result<Page<Value>, AppError> {
        // Authoritative cursor/limit parsing first (Requirements 7.1, 7.4) —
        // a malformed cursor string rejects here, before any relationship
        // load or candidate fetch runs at all.
        let parsed = params.page.parse::<StatusIdCursor>()?;

        // Relationship sets, loaded once per viewer (Requirement 6.1) — see
        // this module's doc comment, "Unauthenticated viewer".
        let ctx = self.build_filter_context(viewer_id).await?;

        // Requirement 1.1's "following ∪ self" — see
        // `candidate_repository::fetch_candidates`'s own doc comment for why
        // this array (not `TimelineQuerySpec`) is `Home`'s following-set
        // delivery channel. Ignored for every other kind.
        let following_and_self: Vec<Id> = if matches!(kind, TimelineKind::Home) {
            let mut ids: Vec<Id> = ctx.following.iter().copied().collect();
            if let Some(viewer_id) = viewer_id {
                ids.push(viewer_id);
            }
            ids
        } else {
            Vec::new()
        };

        // Mandated call: `TimelineMatcher.candidate_spec` (this task's own
        // instruction; see `matcher.rs`'s own "Signature deviation #1" for
        // why `ctx` is accepted but not read by `candidate_spec` itself).
        let matcher = TimelineMatcher;
        let spec = matcher.candidate_spec(kind, &params, &ctx);

        let filter = TimelineFilter;
        let batch_limit = parsed.limit.saturating_mul(BATCH_LIMIT_MULTIPLIER).max(1);
        // See this module's doc comment, "Why the `min_id`-anchored case
        // does not stop early" — mirrors `paginate`'s own `anchor_oldest`
        // computation exactly (`crate::api::pagination::paginate`'s doc
        // comment).
        let anchor_oldest = parsed.since_id.is_none() && parsed.min_id.is_some();

        let mut accumulated: Vec<Status> = Vec::new();
        let mut next_upper_bound = spec.max_id;
        let mut iterations_run: u32 = 0;
        let mut hit_cap = false;

        loop {
            iterations_run += 1;

            let mut iter_spec = spec.clone();
            iter_spec.max_id = next_upper_bound;

            let batch =
                fetch_candidates(&self.pool, &iter_spec, &following_and_self, batch_limit).await?;
            if batch.is_empty() {
                break;
            }
            let batch_len = batch.len();
            // Batches are id-descending (`fetch_candidates`'s own
            // postcondition); the last row is this batch's smallest id, the
            // next iteration's exclusive upper bound — contiguous, no gap,
            // no re-fetch (see this module's doc comment, "The fill loop").
            next_upper_bound = batch.last().map(|status| status.id);

            for status in &batch {
                let reblogged_author = self.resolve_reblogged_author(status.reblog_of_id).await?;
                if filter.keep(status, reblogged_author, &ctx) {
                    accumulated.push(status.clone());
                }
            }

            let exhausted = (batch_len as u32) < batch_limit;
            let sufficient = !anchor_oldest && accumulated.len() >= parsed.limit as usize;
            if exhausted || sufficient {
                break;
            }
            if iterations_run >= MAX_FILL_ITERATIONS {
                hit_cap = true;
                break;
            }
        }

        if hit_cap {
            // Structured diagnostic event (design.md Monitoring: "充填ルー
            // プが `MAX_FILL_ITERATIONS` 上限に到達したケース...も専用ログ
            // イベントとして記録") — mirrors this crate's established
            // structured-tracing-event field convention
            // (`src/media/worker.rs`/`src/server.rs`).
            tracing::warn!(
                kind = ?kind,
                viewer_id = viewer_id.map(|id| id.as_i64()),
                iterations = iterations_run,
                accumulated = accumulated.len(),
                requested_limit = parsed.limit,
                "timeline fill loop reached its iteration cap before satisfying the requested \
                 limit; returning a partial page with a valid continuation cursor rather than \
                 erroring or scanning further (Requirement 7.4)"
            );
        }

        // Final windowing + cursor computation (Requirements 7.1-7.3) —
        // reused wholesale, never reimplemented (this task's own
        // instruction).
        let page = paginate(
            &accumulated,
            |status| StatusIdCursor(status.id.as_i64() as u64),
            &parsed,
        );

        // Cap-hit override for the zero-survivor case only — see this
        // module's doc comment, "Cap-hit cursor override".
        let next_cursor = match (hit_cap, &page.next_cursor) {
            (true, None) => next_upper_bound.map(|id| StatusIdCursor(id.as_i64() as u64).encode()),
            _ => page.next_cursor.clone(),
        };

        // Hydration (Requirement 10.x) — reused wholesale.
        let items = self.hydrator.hydrate(&page.items, &ctx, origin).await?;

        Ok(Page {
            items,
            prev_cursor: page.prev_cursor,
            next_cursor,
        })
    }

    /// Loads `viewer_id`'s relationship sets once via `FilterQuery`
    /// (Requirement 6.1) and assembles a [`FilterContext`] — `None` builds
    /// an all-empty context without any `FilterQuery` call at all (see this
    /// module's doc comment, "Unauthenticated viewer").
    async fn build_filter_context(&self, viewer_id: Option<Id>) -> Result<FilterContext, AppError> {
        let now = self.runtime.clock.now();
        let Some(viewer_id) = viewer_id else {
            return Ok(FilterContext {
                viewer: None,
                blocked: HashSet::new(),
                blocked_by: HashSet::new(),
                muted: HashSet::new(),
                following: HashSet::new(),
                reblogs_hidden: HashSet::new(),
                now,
            });
        };

        // Mirrors `crate::social_graph::follow_service::FollowService`'s
        // established `AccountRef::Local(viewer_id)` precedent — a
        // Bearer-token-authenticated viewer is always a local actor.
        let viewer_ref = AccountRef::Local(viewer_id);
        let query = FilterQuery::new(self.pool.clone(), self.runtime.clone());
        let relationship_sets = query.blocked_set(&viewer_ref).await?;
        let following = query.following_set(&viewer_ref).await?;
        let reblogs_hidden = query.reblogs_hidden_set(&viewer_ref).await?;

        Ok(FilterContext {
            viewer: Some(viewer_id),
            blocked: to_id_set(relationship_sets.blocked),
            blocked_by: to_id_set(relationship_sets.blocked_by),
            muted: to_id_set(relationship_sets.muted),
            following: to_id_set(following),
            reblogs_hidden: to_id_set(reblogs_hidden),
            now,
        })
    }

    /// Resolves a boost candidate's boosted-original post's author id, for
    /// `TimelineFilter::keep`'s `reblogged_author` parameter (Requirement
    /// 6.3) — reuses the exact same repository lookup
    /// `StatusHydrator::hydrate_one` (`src/timelines/hydrator.rs`) already
    /// uses for its own nested-`reblog` resolution, per this task's own
    /// instruction not to re-derive that lookup differently. `None` (no
    /// boost, or the referenced row is missing/dangling) is a graceful,
    /// non-error result — mirrors `StatusHydrator`'s identical "missing
    /// referenced row is not a hydration failure" precedent.
    async fn resolve_reblogged_author(
        &self,
        reblog_of_id: Option<Id>,
    ) -> Result<Option<Id>, AppError> {
        let Some(target_id) = reblog_of_id else {
            return Ok(None);
        };
        let target = status_repository::find_by_id(&self.pool, target_id).await?;
        Ok(target.map(|status| status.actor_id))
    }
}
