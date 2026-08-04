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
//! ## Rendering glue: reuses repositories/serializer, does not reuse
//! `crate::statuses::endpoints`'s own private assembly methods
//! `crate::statuses::endpoints::StatusesEndpointsState`'s own
//! `render_status_json`/`resolve_common`/`leaf_render_input` (task 7.1)
//! already assemble a bare [`Status`] into `serializer::status_to_json`'s
//! input shape — but they are private methods on a router-local, still-
//! generic (`<A, D, L, H, R, M>`) state bundle this module has no reason to
//! parameterize over (this provider needs no `StatusActivityBuilder`/
//! delivery port at all — it only ever *reads*). This module therefore
//! writes its own small, self-contained equivalent
//! ([`AccountStatusesProviderImpl::render`]/`leaf_render_input`), reusing
//! the exact same underlying repositories/serializer functions
//! `endpoints.rs` itself reuses (`interaction_repository`/`tag_repository`/
//! `media_repository`/`poll_repository`/`serializer::status_to_json`/
//! `poll_to_json`/`AccountService::show_account`) — the same "small helper
//! duplication across sibling modules is this crate's own documented
//! convention" this spec's own Implementation Notes already invoke for
//! `UndoKind` (task 4.1) and `format_time` (task 7.1), applied here to a
//! larger assembly function for the same reason: no `pub(crate)` surface
//! exists yet to share it instead, and widening `endpoints.rs`'s private
//! methods to `pub(crate)` (adding six more generic parameters' worth of
//! surface this module would have to satisfy) is a larger, out-of-scope
//! refactor this task's own boundary (`AccountStatusesProviderImpl,
//! AccountCountsContribution`) does not ask for.
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
use crate::statuses::model::Poll;
use crate::statuses::model::Status;
use crate::statuses::poll_repository;
use crate::statuses::poll_repository::PollTally;
use crate::statuses::render_assembler::{
    EmojiResolution, PollResolver, RenderContext, StatusRenderAssembler,
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

    /// Resolves `status` (owned) into its full Mastodon-compatible JSON
    /// representation.
    ///
    /// Boost-target resolution stays here: this provider re-checks the
    /// target against its own [`RelationshipQueryRegistry`]-backed
    /// visibility judgment, keyed to the *target's* author rather than the
    /// booster's, and a target that fails is dropped entirely rather than
    /// partially rendered.
    async fn render(
        &self,
        viewer: Option<Id>,
        status: Status,
        origin: &ForwardedOrigin,
    ) -> Result<Value, AppError> {
        let reblog_target = match status.reblog_of_id {
            Some(target_id) => match status_repository::find_by_id(&self.pool, target_id).await? {
                Some(target) if self.visible_to(&target, viewer).await? => Some(target),
                _ => None,
            },
            None => None,
        };
        let polls = RequiredPolls {
            pool: self.pool.clone(),
        };
        let ctx = RenderContext {
            viewer,
            now: self.runtime.clock.now(),
            origin,
            muted: None,
            polls: &polls,
            emojis: EmojiResolution {
                content: false,
                poll_options: false,
            },
        };
        self.assembler()
            .assemble_one(&status, reblog_target.as_ref(), &ctx)
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
    fn resolve_many<'a>(
        &'a self,
        poll_ids: &'a [Id],
        viewer: Option<Id>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<(Id, Poll, PollTally)>, AppError>> + Send + 'a>>
    {
        Box::pin(async move {
            let mut out = Vec::with_capacity(poll_ids.len());
            for &poll_id in poll_ids {
                let poll = poll_repository::find_poll_by_id(&self.pool, poll_id)
                    .await?
                    .ok_or_else(not_found)?;
                let tally = poll_repository::tally(&self.pool, poll_id, viewer).await?;
                out.push((poll_id, poll, tally));
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
            let mut items = Vec::with_capacity(paged.items.len());
            for status in paged.items {
                items.push(self.render(query.viewer, status, &origin).await?);
            }

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
