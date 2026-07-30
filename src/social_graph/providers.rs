//! `BlockPolicyImpl` / `RelProviderImpl` / `FilterQuery` (design.md "Port
//! Impl / 委譲実装層" -> `#### BlockPolicyImpl / RelProviderImpl /
//! AccountCountsProviderImpl / FilterQuery`, design.md lines ~544-574;
//! Requirements 6.1, 6.2, 6.3, 6.4 (task 4.2, `Boundary: BlockPolicyImpl`)
//! and 8.2, 8.3, 8.4, 9.1, 9.2, 9.3, 9.4 (task 4.3, `Boundary:
//! RelProviderImpl, FilterQuery`)): supplies federation-core's
//! destination-aware `BlockPolicy` delegation boundary
//! (`crate::federation::inbound::block_policy::BlockPolicy`) with a real,
//! `blocks`-table-backed judgment, replacing that module's own always-`false`
//! `NoopBlockPolicy` default ([`BlockPolicyImpl`]); supplies
//! accounts-and-instance's `RelationshipStateProvider` delegation boundary
//! with a real, this spec's own relationship-tables-backed implementation,
//! replacing `NoRelationshipProvider` ([`RelProviderImpl`]); exposes a
//! query-only façade over the same tables' filter sets for timelines/
//! notifications ([`FilterQuery`]); and supplies accounts-and-instance's
//! `AccountCountsProvider` delegation boundary with a real,
//! `follows`-table-backed implementation of the `followers`/`following`
//! fields, replacing `ZeroCountsProvider`'s always-zero default for those
//! two fields ([`AccountCountsProviderImpl`]).
//!
//! ## Scope
//! Task 4.2 (already committed) owns exactly [`BlockPolicyImpl`] and the
//! narrow local-actor resolution it needs for the destination side of a
//! judgment (see "Resolving the destination" below). Task 4.3 (already
//! committed) additively owns exactly [`RelProviderImpl`] and [`FilterQuery`]
//! (see this module's doc comment, "RelProviderImpl / FilterQuery", further
//! down). Task 4.4 (this task) additively owns exactly
//! [`AccountCountsProviderImpl`] (see this module's doc comment,
//! "AccountCountsProviderImpl", further down) — per design.md's File
//! Structure Plan this file is the designated home for all four Port Impls.
//! None of these tasks touch `src/social_graph/inbound.rs` (out of
//! boundary), register any of these four implementations against their real
//! registry/bootstrap (task 5.2's `SocialGraphModule` wiring boundary), or
//! touch `src/social_graph/repository.rs` beyond the minimal, additive
//! `repository::is_blocked` (task 4.2) / `repository::reblogs_hidden_targets`
//! (task 4.3) queries each task's own persistence-layer counterpart needs
//! (see those functions' own doc comments); task 4.4 needs no new
//! repository query — `repository::count_followers`/`count_following`
//! (task 1.3) already exist.
//!
//! ## Resolving the signer: reuses `inbound.rs`'s `ActorUriResolver` port
//! [`BlockPolicyImpl`] is generic over `AU: ActorUriResolver` (task 4.1,
//! `crate::social_graph::inbound::ActorUriResolver`, re-exported as
//! `crate::social_graph::ActorUriResolver`) to turn the verified signer's
//! `actor_uri` into this spec's own `AccountRef` — local or remote, exactly
//! the same port `SocialGraphInboundHandler` already depends on for the same
//! problem (this module's own doc comment explains why that port exists
//! rather than reusing `crate::statuses`'s differently-shaped one; not
//! repeated here). Reusing it here avoids a second, parallel local/remote
//! actor-URI resolution implementation for the same problem; task 4.1's own
//! `Boundary` note in tasks.md explicitly names this port as available for
//! reuse by later tasks.
//!
//! An unresolvable signer (e.g. a transient failure fetching a genuinely
//! remote actor document — [`ActorUriResolver::resolve_account_ref`] has no
//! "unknown, but not an error" outcome; it either resolves or returns `Err`)
//! propagates as a genuine `Err` from [`BlockPolicyImpl::is_blocked`], rather
//! than being silently mapped to `Ok(false)`. This is a deliberate,
//! fail-closed choice: the signer was already HTTP-Signature-verified before
//! this method is ever called (`InboxService::process_verified`'s own pipeline
//! order — block judgment happens after signature verification, so the
//! signer's actor document was already fetched once to verify the signature),
//! so a resolution failure here indicates a genuine transient problem (e.g. a
//! network hiccup on the fetch-and-normalize path), not "this spec has never
//! heard of this signer" — and letting that failure surface as a request
//! error is safer than silently treating an unresolvable signer as
//! definitely-not-blocked and letting the Activity through.
//!
//! ## Resolving the destination: a narrow, local-only lookup, not the full
//! `ActorUriResolver`
//! By contrast, `local_recipient`'s own `actor_uri` (for
//! [`LocalRecipientContext::Actor`]) is, per federation-core's own contract,
//! *always* one of this instance's own local actors (design.md: "宛先が個別
//! アクター向け...宛先ローカルアクターがブロック中なら真を返し" — this
//! judgment is only ever asked from the destination local actor's own
//! perspective). [`BlockPolicyImpl`] therefore resolves it directly against
//! `ActorDirectory` (mirroring `inbound.rs::ProdActorUriResolver::
//! local_handle`'s identical `https://{domain}/users/{handle}` prefix-strip
//! logic, duplicated here rather than exported from `inbound.rs` — that
//! module is out of this task's boundary, and the logic is a few lines) —
//! never `AU`'s genuinely-remote fallback path. This keeps the common-case,
//! per-signed-request-called fast path (every individual-inbox delivery
//! reaches this method) from ever attempting a needless remote fetch for a
//! URI that is, by contract, always local.
//!
//! When the destination `actor_uri` does not resolve to any currently known
//! local actor (a URI shaped like this instance's own actor URLs, but naming
//! an actor this instance no longer has — e.g. deleted between the router
//! resolving the delivery path and this call), [`BlockPolicyImpl::is_blocked`]
//! answers `Ok(false)` rather than erroring. Although the
//! `LocalRecipientContext::Actor` contract says this should always resolve,
//! treating a resolution miss here as a hard error would mean a rare,
//! already-benign race (the destination actor is gone; there is nothing left
//! to protect with a block judgment either way) turns into a 500 for the
//! *entire* signed request instead of the request simply proceeding as
//! "no one to be blocked from here" — matching this module's/design.md's
//! stated preference elsewhere for `Ok(false)` over an error when the
//! judgment target genuinely cannot exist in this spec's own `blocks` table
//! (see the `false`-not-error precedent already established for
//! [`LocalRecipientContext::SharedInbox`] below, and
//! `crate::federation::inbound::block_policy`'s own "never bulk-reject"
//! rationale for the same underlying principle: a resolution gap here should
//! never turn into rejecting more than strictly justified by an actual
//! `blocks` row).
//!
//! ## `LocalRecipientContext::SharedInbox`: always `false`, no query
//! Per federation-core's own contract (`crate::federation::inbound::
//! block_policy`'s doc comment, "Why destination-aware") a shared-inbox
//! delivery has no single resolved destination local actor yet, so bulk-
//! rejecting at this point would incorrectly drop the Activity for local
//! actors who never blocked the signer. [`BlockPolicyImpl::is_blocked`]
//! answers `Ok(false)` for this variant unconditionally, before ever touching
//! `self.pool` — this spec's own Follow/Accept/Reject/Block/Undo Activities
//! are in any case always addressed to an individual actor inbox, never a
//! shared inbox (design.md's own note on this point), so this branch is a
//! contract-completeness requirement rather than a scenario this spec's own
//! Activities actually exercise.
//!
//! ## Block lifecycle (Requirement 6.4): no caching
//! [`BlockPolicyImpl::is_blocked`] issues a fresh `repository::is_blocked`
//! query against live DB state on every call — no cached/stale
//! `blocked`/`not-blocked` verdict is ever kept across calls. Once a `blocks`
//! row is deleted (unblock), the very next `is_blocked` call for that pair
//! observes it and answers `false` again, falling out structurally from
//! querying live state rather than from any explicit "invalidate on unblock"
//! step.

#[cfg(test)]
mod tests;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use sqlx::PgPool;

use crate::accounts::model::{AccountCounts, RelationshipView};
use crate::accounts::ports::{AccountCountsProvider, RelationshipStateProvider};
use crate::actor::{ActorDirectory, Handle};
use crate::domain::{AccountRef, Id};
use crate::error::AppError;
use crate::federation::inbound::block_policy::{BlockPolicy, LocalRecipientContext};
use crate::runtime::RuntimeContext;
use crate::social_graph::inbound::ActorUriResolver;
use crate::social_graph::relationship_mapper::RelationshipMapper;
use crate::social_graph::repository;

/// federation-core's `BlockPolicy` delegation boundary, backed by this
/// spec's own `blocks` table (Requirements 6.1, 6.2, 6.3, 6.4). See this
/// module's doc comment for the full per-variant contract.
#[derive(Clone)]
pub struct BlockPolicyImpl<AU>
where
    AU: ActorUriResolver,
{
    pool: PgPool,
    domain: String,
    directory: Arc<ActorDirectory>,
    actor_uris: AU,
}

impl<AU> BlockPolicyImpl<AU>
where
    AU: ActorUriResolver,
{
    /// Builds a `BlockPolicyImpl` bound to `pool` (this spec's own `blocks`
    /// table read), `domain` (this instance's own configured server domain —
    /// must match `ActorUrls`'s own domain exactly, mirrors
    /// `inbound::ProdActorUriResolver::new`'s identical requirement, used
    /// only for the destination-side local-actor-URL shortcut), `directory`
    /// (the destination-side local-actor lookup — see this module's doc
    /// comment, "Resolving the destination"), and `actor_uris` (the
    /// signer-side resolution port — see this module's doc comment,
    /// "Resolving the signer").
    pub fn new(
        pool: PgPool,
        domain: impl Into<String>,
        directory: Arc<ActorDirectory>,
        actor_uris: AU,
    ) -> Self {
        Self {
            pool,
            domain: domain.into(),
            directory,
            actor_uris,
        }
    }

    /// Extracts `{handle}` from `actor_uri` when it matches this instance's
    /// own `https://{domain}/users/{handle}` shape
    /// ([`crate::federation::urls::ActorUrls::actor_url`]'s exact
    /// construction) — mirrors `inbound::ProdActorUriResolver::
    /// local_handle`'s identical logic (this module's doc comment,
    /// "Resolving the destination").
    fn local_handle(&self, actor_uri: &str) -> Option<Handle> {
        let prefix = format!("https://{}/users/", self.domain);
        actor_uri
            .strip_prefix(prefix.as_str())
            .filter(|rest| !rest.is_empty() && !rest.contains('/'))
            .and_then(|rest| Handle::new(rest).ok())
    }

    /// Resolves `actor_uri` (a [`LocalRecipientContext::Actor`]'s own
    /// destination URI) to this instance's own local [`AccountRef`], or
    /// `Ok(None)` when it does not currently name one of this instance's own
    /// local actors (this module's doc comment, "Resolving the destination":
    /// treated as a benign miss, not an error).
    async fn resolve_local_recipient(
        &self,
        actor_uri: &str,
    ) -> Result<Option<AccountRef>, AppError> {
        let Some(handle) = self.local_handle(actor_uri) else {
            return Ok(None);
        };
        let resolved = self.directory.resolve_actor_by_handle(&handle).await?;
        Ok(resolved.map(|actor| AccountRef::Local(actor.id)))
    }
}

impl<AU> BlockPolicy for BlockPolicyImpl<AU>
where
    AU: ActorUriResolver,
{
    /// Requirements 6.1, 6.2, 6.3, 6.4. See this module's doc comment for the
    /// full per-variant contract (`Actor` -> live `blocks`-row check from the
    /// destination local actor's own perspective; `SharedInbox` -> always
    /// `false`, no query).
    async fn is_blocked(
        &self,
        actor_uri: &str,
        local_recipient: LocalRecipientContext,
    ) -> Result<bool, AppError> {
        let LocalRecipientContext::Actor {
            actor_uri: recipient_uri,
        } = local_recipient
        else {
            // SharedInbox: destination not yet resolved -- never bulk-reject
            // (this module's doc comment, "LocalRecipientContext::
            // SharedInbox"; federation-core's own contract).
            return Ok(false);
        };

        let Some(destination) = self.resolve_local_recipient(&recipient_uri).await? else {
            // Destination URI does not currently name a known local actor
            // (this module's doc comment, "Resolving the destination").
            return Ok(false);
        };

        let signer = self.actor_uris.resolve_account_ref(actor_uri).await?;

        repository::is_blocked(&self.pool, &destination, &signer).await
    }
}

// -- RelProviderImpl / FilterQuery (design.md same heading; Requirements ---
// -- 8.2, 8.3, 8.4, 9.1, 9.2, 9.3, 9.4; task 4.3, `Boundary: RelProviderImpl,
// -- FilterQuery`) -------------------------------------------------------
//
// `RelProviderImpl` supplies accounts-and-instance's `RelationshipStateProvider`
// delegation boundary (`crate::accounts::ports::RelationshipStateProvider`)
// with a real implementation backed by this spec's own relationship tables,
// replacing that module's always-"no relationship" `NoRelationshipProvider`
// default. `viewer: Id` (the trait's own parameter shape) is always this
// instance's own authenticated local actor -- `crate::accounts::ports`'s own
// doc comment names this boundary "閲覧者アクター", and every existing
// caller in this crate that populates an analogous `viewer_id: Id` for a
// social-graph-owned operation (`FollowService`/`MuteService`/`BlockService`,
// tasks 3.1/3.3/3.4) always wraps it as `AccountRef::Local(viewer_id)`, never
// `Remote` -- so `RelProviderImpl::relationships` does the same rather than
// accepting an `AccountRef` directly (matching the trait's actual, already-
// fixed signature, which is out of this task's boundary to change).
//
// `FilterQuery` is a thin, query-only façade over `RelationshipRepository`'s
// already-implemented (task 1.3) filter-set queries
// (`blocked_targets`/`blocked_by`/`muted_targets`/`following_targets`) plus
// this task's own minimal, additive `repository::reblogs_hidden_targets`
// extension (see that function's own doc comment) -- for timelines/
// notifications to consume (Requirements 9.1, 9.2, 9.3). It implements no
// filter-*application* logic of its own (Requirement 9.4: "フィルタ適用処
// 理そのものは実装せず") -- every method here only reads and returns
// account-reference lists/sets, never drops/keeps a caller's own items.

/// Supplies accounts-and-instance's [`RelationshipStateProvider`] delegation
/// boundary with a real implementation (Requirements 8.2, 8.3, 8.4). See
/// this module's doc comment ("RelProviderImpl / FilterQuery") for the
/// `viewer: Id` -> `AccountRef::Local` assumption this relies on.
#[derive(Clone)]
pub struct RelProviderImpl {
    pool: PgPool,
    runtime: RuntimeContext,
}

impl RelProviderImpl {
    /// Builds a `RelProviderImpl` bound to `pool` (this spec's own
    /// relationship tables, via `RelationshipRepository::load_states`) and
    /// `runtime` (`Clock` injection for `load_states`'s expiry-aware `now`
    /// -- never a direct wall-clock read, this crate's determinism rule).
    pub fn new(pool: PgPool, runtime: RuntimeContext) -> Self {
        Self { pool, runtime }
    }
}

impl RelationshipStateProvider for RelProviderImpl {
    /// Requirements 8.2, 8.3, 8.4: resolves `viewer` to
    /// `AccountRef::Local(viewer)`, batch-loads every target's relationship
    /// state via `RelationshipRepository::load_states` (already expiry-aware
    /// for mutes, and already returns one state per `targets` entry in
    /// `targets`' own order -- see that function's own doc comment), and
    /// maps each state through the already-implemented (task 2.4)
    /// `RelationshipMapper::to_view`. The output's order therefore matches
    /// `targets`' order by construction, with no re-sorting/re-association
    /// needed on this method's own part.
    fn relationships<'a>(
        &'a self,
        viewer: Id,
        targets: &'a [AccountRef],
    ) -> Pin<Box<dyn Future<Output = Result<Vec<RelationshipView>, AppError>> + Send + 'a>> {
        Box::pin(async move {
            let viewer_ref = AccountRef::Local(viewer);
            let now = self.runtime.clock.now();
            let states = repository::load_states(&self.pool, &viewer_ref, targets, now).await?;
            Ok(states
                .iter()
                .map(|state| RelationshipMapper.to_view(state))
                .collect())
        })
    }
}

/// The block/blocked-by/mute(expiry-aware)/notification-mute sets a viewer
/// currently has (Requirement 9.1) -- [`FilterQuery::blocked_set`]'s return
/// type. Each field is a plain list of the matching accounts; no filter
/// application (dropping/keeping a caller's own items) happens here or
/// anywhere in this module (Requirement 9.4).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RelationshipSets {
    /// Accounts `viewer` has blocked.
    pub blocked: Vec<AccountRef>,
    /// Accounts that have blocked `viewer`.
    pub blocked_by: Vec<AccountRef>,
    /// Accounts `viewer` currently mutes (expired mutes already excluded,
    /// Requirement 9.3).
    pub muted: Vec<AccountRef>,
    /// The subset of `muted` muted with `notifications = true`.
    pub muted_notifications: Vec<AccountRef>,
}

/// A thin, query-only façade over `RelationshipRepository`'s filter-set
/// queries, for timelines/notifications to consume (Requirements 9.1, 9.2,
/// 9.3). See this module's doc comment ("RelProviderImpl / FilterQuery")
/// for why this implements no filter-application logic of its own
/// (Requirement 9.4).
#[derive(Clone)]
pub struct FilterQuery {
    pool: PgPool,
    runtime: RuntimeContext,
}

impl FilterQuery {
    /// Builds a `FilterQuery` bound to `pool` (this spec's own relationship
    /// tables) and `runtime` (`Clock` injection for the mute-expiry-aware
    /// `now` `blocked_set` needs).
    pub fn new(pool: PgPool, runtime: RuntimeContext) -> Self {
        Self { pool, runtime }
    }

    /// Blocked / blocked-by / muted / muted-with-notifications sets for
    /// `viewer` (Requirement 9.1). `muted`/`muted_notifications` already
    /// exclude expired mutes, per `RelationshipRepository::muted_targets`'s
    /// own `now`-driven filter (Requirement 9.3) -- this method performs no
    /// expiry check of its own, it only supplies the current `now`.
    pub async fn blocked_set(&self, viewer: &AccountRef) -> Result<RelationshipSets, AppError> {
        let now = self.runtime.clock.now();
        let blocked = repository::blocked_targets(&self.pool, viewer).await?;
        let blocked_by = repository::blocked_by(&self.pool, viewer).await?;
        let muted = repository::muted_targets(&self.pool, viewer, now, false).await?;
        let muted_notifications = repository::muted_targets(&self.pool, viewer, now, true).await?;
        Ok(RelationshipSets {
            blocked,
            blocked_by,
            muted,
            muted_notifications,
        })
    }

    /// `viewer`'s established follow-target set (Requirement 9.2), for home
    /// timeline construction.
    pub async fn following_set(&self, viewer: &AccountRef) -> Result<Vec<AccountRef>, AppError> {
        repository::following_targets(&self.pool, viewer).await
    }

    /// `viewer`'s follow targets with reblog display disabled
    /// (`show_reblogs = false`) -- task 4.3's own `reblogs_hidden` filter
    /// set, for boost-display suppression in home timeline construction.
    pub async fn reblogs_hidden_set(
        &self,
        viewer: &AccountRef,
    ) -> Result<Vec<AccountRef>, AppError> {
        repository::reblogs_hidden_targets(&self.pool, viewer).await
    }
}

// -- AccountCountsProviderImpl (design.md same heading; Requirement 8.2; ---
// -- task 4.4, `Boundary: AccountCountsProviderImpl`) -----------------------
//
// `AccountCountsProviderImpl` supplies accounts-and-instance's
// `AccountCountsProvider` delegation boundary (`crate::accounts::ports::
// AccountCountsProvider`) with a real, `follows`-table-backed implementation
// for exactly the `followers`/`following` fields of `AccountCounts`,
// replacing that module's always-zero `ZeroCountsProvider` default for those
// two fields. `statuses`/`last_status_at` stay at `ZeroCountsProvider`'s own
// documented zero/`None` values -- this spec does not own post counts
// (Boundary Commitments: "投稿数 / `last_status_at` は本 spec 範囲外で既定
// 0 / None"; that pair is statuses-core's own delegation, supplied
// separately).

/// Supplies accounts-and-instance's [`AccountCountsProvider`] delegation
/// boundary with a real implementation of its `followers`/`following`
/// fields (Requirement 8.2, Boundary Commitments). See this module's doc
/// comment ("AccountCountsProviderImpl") for why `statuses`/
/// `last_status_at` are left at their zero/`None` defaults.
#[derive(Clone)]
pub struct AccountCountsProviderImpl {
    pool: PgPool,
}

impl AccountCountsProviderImpl {
    /// Builds an `AccountCountsProviderImpl` bound to `pool` (this spec's
    /// own `follows` table, via `repository::count_followers`/
    /// `repository::count_following`).
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl AccountCountsProvider for AccountCountsProviderImpl {
    /// Requirement 8.2, Boundary Commitments: counts `target`'s followers
    /// (`repository::count_followers`, `target` as `followee`) and following
    /// (`repository::count_following`, `target` as `follower`) from
    /// established `follows` rows; `statuses`/`last_status_at` stay at
    /// accounts-and-instance's own zero/`None` defaults, matching
    /// `ZeroCountsProvider`'s documented shape for the fields this spec does
    /// not own.
    fn counts<'a>(
        &'a self,
        target: &'a AccountRef,
    ) -> Pin<Box<dyn Future<Output = Result<AccountCounts, AppError>> + Send + 'a>> {
        Box::pin(async move {
            let followers = repository::count_followers(&self.pool, target).await?;
            let following = repository::count_following(&self.pool, target).await?;
            Ok(AccountCounts {
                followers,
                following,
                statuses: 0,
                last_status_at: None,
            })
        })
    }
}
