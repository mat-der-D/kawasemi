//! `AccountStatusesProviderImpl` / `AccountCountsContribution` (statuses-core
//! task 9.1, `_Boundary: AccountStatusesProviderImpl, AccountCountsContribution_`,
//! Requirements 6.1, 7.1; design.md's Boundary Commitments: "accounts-and-
//! instance が定義する委譲ポートの実装供給: `AccountStatusesProvider`...を
//! 実装し、`AccountCountsProvider` へ `statuses_count`（および
//! `last_status_at`）を供給する。ポートの契約定義自体は accounts-and-instance
//! が所有し、本 spec は実装登録のみ").
//!
//! ## Scope
//! This module supplies exactly two real implementations of ports
//! *defined and owned* by `crate::accounts::ports` (task 1.3,
//! accounts-and-instance) — it does not redeclare either trait, its
//! `StatusesQuery`/`AccountCounts` carrier types, or the registry
//! (`AccountPortsRegistry`) those ports are swapped into:
//! - [`AccountStatusesProviderImpl`] implements
//!   [`crate::accounts::ports::AccountStatusesProvider`]: `GET
//!   /accounts/:id/statuses`'s real Status page, visibility-filtered
//!   through [`visibility::is_visible`] (task 3.1, the same policy every
//!   other retrieval path in this spec already funnels through) and
//!   rendered through the same `serializer::status_to_json` contract
//!   (task 3.3) every other statuses-core endpoint uses.
//! - [`AccountCountsContribution`] implements
//!   [`crate::accounts::ports::AccountCountsProvider`]: supplies the real
//!   `statuses`/`last_status_at` sub-counts. `followers`/`following` are
//!   always `0` here — those are social-graph's own sub-counts (design.md:
//!   "followers/following: social-graph; statuses/last_status_at:
//!   statuses-core"), and `AccountPortsRegistry` holds exactly one
//!   replaceable slot per port (never a composed/merged value from two
//!   registrants at once, see that registry's own doc comment, "Registry
//!   shape") — so, until social-graph lands and registers its own full
//!   replacement, `0`/`0` is this contribution's own honest, safe-by-
//!   construction answer for the two sub-counts it does not own, exactly
//!   mirroring [`crate::accounts::ports::ZeroCountsProvider`]'s own
//!   all-zero default for the fields it cannot supply either (CONCERN,
//!   flagged in this task's own status report — not a new gap, the same
//!   one `crate::accounts::account_service::AccountService::verify_credentials`'s
//!   own doc comment already documents: "`ZeroCountsProvider` until a
//!   downstream spec (social-graph/statuses-core) registers a real one").
//!
//! ## Rendering without a live request's own forwarded origin (CONCERN)
//! [`crate::accounts::ports::StatusesQuery`] (accounts-and-instance's own
//! contract, not redefined here) carries no `ForwardedOrigin`/request-URI
//! context at all — unlike `crate::statuses::endpoints`'s own handlers,
//! which resolve one per request via `ResolvedOrigin` (`X-Forwarded-Proto`/
//! `X-Forwarded-Host`), this port is called from *inside*
//! `AccountService::list_statuses`, several layers away from the original
//! HTTP request, with no per-request origin threaded through the port
//! contract this task is not permitted to redefine. [`AccountStatusesProviderImpl::origin`]
//! therefore synthesizes a fixed `https://{domain}` origin (`domain`
//! supplied at construction, this instance's own configured server domain —
//! the same fallback `ForwardedOrigin::resolve` itself falls back to when no
//! forwarded header is present) for every absolute URL this provider's own
//! rendering builds (embedded Account's media URLs, tag URLs). Behind a
//! reverse proxy presenting a different external scheme/host than this
//! instance's own configured `domain`, those specific URLs (unlike every
//! other statuses-core endpoint's own request-derived ones) will not reflect
//! the proxy's externally-visible origin. Flagged in this task's own status
//! report `CONCERNS` for reviewer confirmation — not a silent gap.
//!
//! ## Status JSON assembly is shared, and batched per page
//! [`AccountStatusesProviderImpl::render_page`] resolves each boost target
//! and its visibility itself, then hands the whole page to
//! [`crate::statuses::render_assembler::StatusRenderAssembler`], which every
//! module that renders statuses now goes through. This module used to carry
//! its own copy of that assembly glue — one of four in the crate — written
//! because `crate::statuses::endpoints`'s equivalent was private to a
//! router-local, six-parameter generic state bundle this provider (which
//! only ever *reads*) had no reason to parameterize over. The extraction has
//! happened; what remains here is only the part that is genuinely this
//! module's own: which target is visible, and to whom.
//!
//! That handoff is a single
//! [`crate::statuses::render_assembler::StatusRenderAssembler::assemble_many`]
//! call per page rather than one per status, so the media/tag/emoji/
//! interaction/poll lookups a page needs are issued a number of times that
//! does not depend on how many statuses it holds, and an author appearing
//! twice on one page is resolved once. Boost targets ride in the same batch.
//! What stays outside it — and stays per status — is exactly the two
//! judgments above this rendering:
//! [`AccountStatusesProviderImpl::visible_to`] and
//! [`AccountStatusesProviderImpl::passes_filters`]. Those decide *which*
//! statuses reach the page, which stays with this module rather than the
//! assembler.
//!
//! One of them still costs a query per candidate, and that is a real
//! remaining gap rather than a property of the design:
//! [`AccountStatusesProviderImpl::passes_filters`] issues
//! [`status_repository::media_ids_for_status`] when `only_media` is set and
//! [`interaction_repository::exists_pin`] when `pinned` is set — media and
//! interaction state, both of which the assembler otherwise batches. Batching
//! them would not change which statuses reach the page:
//! [`status_repository::media_ids_for_statuses`] and
//! [`interaction_repository::pinned_status_ids`] already exist and answer the
//! same question for a whole candidate set. Doing so means restructuring the
//! filter chain, which batching this rendering deliberately left untouched,
//! so it is still outstanding.
//!
//! ## Filtering: fetch-then-filter-then-paginate, not sixteen SQL variants
//! [`status_repository::list_by_actor`] fetches every status `query.target`
//! authored, entirely unfiltered. [`AccountStatusesProviderImpl::list_statuses`]
//! then applies, in this order, in Rust: `query.exclude_replies`/
//! `query.exclude_reblogs` (cheap field checks), `query.only_media`/
//! `query.pinned` (one extra read per candidate each), then visibility
//! (`visibility::is_visible`, which needs a resolved `ViewerRelation` no
//! `WHERE` clause could express anyway) — then paginates the *filtered*
//! list via `crate::api::pagination::paginate`, and only then renders the
//! one resulting page's items (never the full candidate set) into JSON.
//! Mirrors `interaction_repository.rs::list_bookmarks`'s own established
//! "fetch the full matching set, filter/paginate in Rust" convention
//! (itself required because `sqlx`'s `SqlSafeStr` bound rejects a
//! dynamically-built query string — see `status_repository.rs::adjust_counts`'s
//! own doc comment) rather than selecting among the 16 statically-combined
//! `WHERE`-clause variants four independent boolean filters would otherwise
//! require.
//!
//! ## `RelationshipQuery`: [`RelationshipQueryRegistry`], not a generic parameter
//! (updated by the feature-level `kiro-validate-impl` remediation round 1
//! follow-up, 2026-07-31, closing a gap that round's own reviewer flagged:
//! this module was the one consumer of `RelationshipQuery` left directly
//! instantiating [`visibility::NoRelationshipQuery`] after
//! [`visibility::RelationshipQueryRegistry`] was introduced for every other
//! consumer.) Every other statuses-core service generic over
//! `R: RelationshipQuery` (`StatusService`/`InteractionService`/
//! `PollService`) is, in this crate's one production build, always
//! monomorphized to [`visibility::RelationshipQueryRegistry`]
//! (`crate::statuses`'s own `Concrete*` type aliases) — a runtime-replaceable
//! slot `crate::social_graph::build_social_graph_module` registers its own
//! real, follow-graph-backed implementation into, defaulting to
//! [`visibility::NoRelationshipQuery`]'s own safe behavior until it does.
//! This module skips the generic parameter entirely and instead holds one
//! [`RelationshipQueryRegistry`] field, supplied at construction
//! (`crate::statuses::register_account_ports`'s own `relationship_query`
//! parameter — the exact same registry handle
//! `crate::statuses::build_statuses_module`'s `StatusService`/
//! `InteractionService`/`PollService` already share, see
//! `crate::statuses::StatusesModule::relationship_query_registry`'s own doc
//! comment), the same "concrete, not generic" judgment call
//! `crate::statuses::ProdRemoteActorResolver` already makes for the same
//! reason (adding a type parameter buys no real flexibility this task's
//! tests need — a fake `RelationshipQuery` is only useful to a caller that
//! can also *supply* an alternate one at construction, and this module's own
//! `new` already accepts exactly that via the registry's
//! `set_relationship_query`).

#[cfg(test)]
mod tests;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::Value;
use sqlx::PgPool;

use crate::accounts::account_service::AccountService;
use crate::accounts::model::AccountCounts;
use crate::accounts::ports::{AccountCountsProvider, AccountStatusesProvider, StatusesQuery};
use crate::api::origin::self_origin;
use crate::api::pagination::{ForwardedOrigin, Page, StatusIdCursor, paginate};
use crate::domain::{AccountRef, Id};
use crate::error::AppError;
use crate::federation::signatures::ReqwestFederationHttpClient;
use crate::media::local_fs::LocalFsStore;
use crate::runtime::RuntimeContext;
use crate::statuses::interaction_repository;
use crate::statuses::model::Status;
use crate::statuses::poll_repository;
use crate::statuses::render_assembler::{
    PollResolution, PollResolver, RenderContext, StatusRenderAssembler,
};
use crate::statuses::status_repository;
use crate::statuses::visibility::{self, RelationshipQuery, RelationshipQueryRegistry};

/// Recovers the [`Id`] both ports need from an [`AccountRef`], regardless of
/// local/remote-ness — mirrors `crate::accounts::ports::account_ref_id`'s
/// identical (but private-to-that-module) helper.
fn account_ref_id(target: &AccountRef) -> Id {
    match *target {
        AccountRef::Local(id) => id,
        AccountRef::Remote(id) => id,
    }
}

fn not_found() -> AppError {
    AppError::client(StatusCode::NOT_FOUND, "status not found")
}

/// This spec's own implementation of `crate::accounts::ports::AccountStatusesProvider`
/// (task 9.1) — see this module's own doc comment for the full contract and
/// its documented judgment calls.
pub struct AccountStatusesProviderImpl {
    pool: PgPool,
    runtime: RuntimeContext,
    domain: String,
    accounts: Arc<AccountService<LocalFsStore, ReqwestFederationHttpClient>>,
    media_store: LocalFsStore,
    relationship_query: RelationshipQueryRegistry,
}

impl AccountStatusesProviderImpl {
    /// Builds a provider bound to `pool`/`runtime` (repository reads/poll
    /// rendering's `now`), `domain` (this instance's own configured server
    /// domain — see this module's doc comment, "Rendering without a live
    /// request's own forwarded origin"), `accounts` (Account-embed
    /// rendering, `crate::accounts::build_accounts_module`'s own
    /// `AccountService` handle), `media_store` (media URL rendering,
    /// `crate::media::build_media_module`'s own `LocalFsStore` handle), and
    /// `relationship_query` (the live [`RelationshipQueryRegistry`] handle
    /// — see this module's own doc comment, "`RelationshipQuery`:
    /// `RelationshipQueryRegistry`, not a generic parameter").
    pub fn new(
        pool: PgPool,
        runtime: RuntimeContext,
        domain: impl Into<String>,
        accounts: Arc<AccountService<LocalFsStore, ReqwestFederationHttpClient>>,
        media_store: LocalFsStore,
        relationship_query: RelationshipQueryRegistry,
    ) -> Self {
        Self {
            pool,
            runtime,
            domain: domain.into(),
            accounts,
            media_store,
            relationship_query,
        }
    }

    /// See this module's doc comment ("Rendering without a live request's
    /// own forwarded origin").
    fn origin(&self) -> ForwardedOrigin {
        self_origin(&self.domain)
    }

    /// The real visibility judgment (task 3.1's [`visibility::is_visible`]),
    /// resolved through this provider's own [`RelationshipQueryRegistry`]
    /// handle — see this module's own doc comment ("`RelationshipQuery`:
    /// `RelationshipQueryRegistry`, not a generic parameter").
    async fn visible_to(&self, status: &Status, viewer: Option<Id>) -> Result<bool, AppError> {
        let rel = self
            .relationship_query
            .viewer_relation(status.actor_id, viewer)
            .await?;
        Ok(visibility::is_visible(status, viewer, &rel))
    }

    /// Applies `query`'s `exclude_replies`/`exclude_reblogs`/`only_media`/
    /// `pinned` filters (Requirement 4.4) to one candidate `status` already
    /// known to belong to `target` — see this module's own doc comment
    /// ("Filtering: fetch-then-filter-then-paginate").
    async fn passes_filters(
        &self,
        status: &Status,
        target: Id,
        query: &StatusesQuery,
    ) -> Result<bool, AppError> {
        if query.exclude_replies && status.in_reply_to_id.is_some() {
            return Ok(false);
        }
        if query.exclude_reblogs && status.reblog_of_id.is_some() {
            return Ok(false);
        }
        if query.only_media {
            let media_ids = status_repository::media_ids_for_status(&self.pool, status.id).await?;
            if media_ids.is_empty() {
                return Ok(false);
            }
        }
        if query.pinned {
            let pinned = interaction_repository::exists_pin(&self.pool, target, status.id).await?;
            if !pinned {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Builds the shared Status assembler this provider renders through.
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
    /// Boost-target resolution stays here rather than moving into the
    /// assembler — boost-target resolution and visibility judgment stay with
    /// the assembler's *caller*: this provider re-checks the
    /// target against its own [`RelationshipQueryRegistry`]-backed
    /// visibility judgment, keyed to the *target's* author rather than the
    /// booster's, and a target that fails is dropped entirely rather than
    /// partially rendered.
    async fn resolve_reblog_target(
        &self,
        status: &Status,
        viewer: Option<Id>,
    ) -> Result<Option<Status>, AppError> {
        let Some(target_id) = status.reblog_of_id else {
            return Ok(None);
        };
        let Some(target) = status_repository::find_by_id(&self.pool, target_id).await? else {
            return Ok(None);
        };
        Ok(self.visible_to(&target, viewer).await?.then_some(target))
    }

    /// Renders one already-filtered, already-paginated page into Status
    /// JSON, in the order given.
    ///
    /// Every boost target is resolved first, so that the whole page —
    /// targets included — reaches
    /// [`StatusRenderAssembler::assemble_many`] as one batch and its
    /// per-status materials are fetched a number of times that does not
    /// depend on the page's length.
    async fn render_page(
        &self,
        viewer: Option<Id>,
        statuses: &[Status],
        origin: &ForwardedOrigin,
    ) -> Result<Vec<Value>, AppError> {
        let mut reblog_targets = Vec::with_capacity(statuses.len());
        for status in statuses {
            reblog_targets.push(self.resolve_reblog_target(status, viewer).await?);
        }

        let polls = RequiredPolls {
            pool: self.pool.clone(),
        };
        let ctx = RenderContext {
            // One `now` for the page rather than one per status. The values
            // it feeds — a poll's `expired` flag — are now answered
            // consistently across a single response, which rendering each
            // status against its own clock reading did not guarantee.
            viewer,
            now: self.runtime.clock.now(),
            origin,
            muted: None,
            polls: &polls,
        };
        self.assembler()
            .assemble_many(statuses, &reblog_targets, &ctx)
            .await
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
    /// given, and none at all for an empty one, so an account's status page
    /// does not have its poll lookups scale with its length.
    fn resolve_many<'a>(&'a self, poll_ids: &'a [Id], viewer: Option<Id>) -> PollResolution<'a> {
        Box::pin(async move {
            let polls = poll_repository::find_polls_by_ids(&self.pool, poll_ids).await?;

            // Walked in `poll_ids` order, so a dangling id raises where the
            // per-id loop this replaces raised: on the *first* one, not on
            // whichever the map happened to iterate to. Both this ordering
            // and the result's own are fixed by this one pass.
            let mut resolved = Vec::with_capacity(poll_ids.len());
            for &poll_id in poll_ids {
                let poll = polls.get(&poll_id).ok_or_else(not_found)?;
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
                let tally = tallies.get(&poll_id).ok_or_else(not_found)?;
                out.push((poll_id, poll, tally.clone()));
            }
            Ok(out)
        })
    }
}

impl AccountStatusesProvider for AccountStatusesProviderImpl {
    fn list_statuses<'a>(
        &'a self,
        query: &'a StatusesQuery,
    ) -> Pin<Box<dyn Future<Output = Result<Page<Value>, AppError>> + Send + 'a>> {
        Box::pin(async move {
            let target = account_ref_id(&query.target);
            let candidates = status_repository::list_by_actor(&self.pool, target).await?;

            let mut visible = Vec::with_capacity(candidates.len());
            for status in candidates {
                if !self.visible_to(&status, query.viewer).await? {
                    continue;
                }
                if !self.passes_filters(&status, target, query).await? {
                    continue;
                }
                visible.push(status);
            }

            let parsed = query.page.parse::<StatusIdCursor>()?;
            let paged = paginate(
                &visible,
                |status| StatusIdCursor(status.id.as_i64() as u64),
                &parsed,
            );

            let origin = self.origin();
            let items = self
                .render_page(query.viewer, &paged.items, &origin)
                .await?;

            Ok(Page {
                items,
                prev_cursor: paged.prev_cursor,
                next_cursor: paged.next_cursor,
            })
        })
    }
}

/// This spec's own implementation of `crate::accounts::ports::AccountCountsProvider`
/// (task 9.1) — supplies the real `statuses`/`last_status_at` sub-counts;
/// `followers`/`following` are always `0` (not this spec's own truth
/// source) — see this module's own doc comment for the full rationale.
pub struct AccountCountsContribution {
    pool: PgPool,
}

impl AccountCountsContribution {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl AccountCountsProvider for AccountCountsContribution {
    fn counts<'a>(
        &'a self,
        target: &'a AccountRef,
    ) -> Pin<Box<dyn Future<Output = Result<AccountCounts, AppError>> + Send + 'a>> {
        Box::pin(async move {
            let id = account_ref_id(target);
            let statuses = status_repository::count_for_actor(&self.pool, id).await?;
            let last_status_at =
                status_repository::last_created_at_for_actor(&self.pool, id).await?;
            Ok(AccountCounts {
                followers: 0,
                following: 0,
                statuses,
                last_status_at,
            })
        })
    }
}
