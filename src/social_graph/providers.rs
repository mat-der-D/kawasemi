//! `BlockPolicyImpl` (design.md "Port Impl / 委譲実装層" ->
//! `#### BlockPolicyImpl / RelProviderImpl / AccountCountsProviderImpl /
//! FilterQuery`, design.md lines ~544-574; Requirements 6.1, 6.2, 6.3, 6.4;
//! task 4.2, `Boundary: BlockPolicyImpl`): supplies federation-core's
//! destination-aware `BlockPolicy` delegation boundary
//! (`crate::federation::inbound::block_policy::BlockPolicy`) with a real,
//! `blocks`-table-backed judgment, replacing that module's own always-`false`
//! `NoopBlockPolicy` default.
//!
//! ## Scope
//! Owns exactly [`BlockPolicyImpl`] and the narrow local-actor resolution it
//! needs for the destination side of a judgment (see "Resolving the
//! destination" below). Per design.md's File Structure Plan this file is
//! also the intended home for `RelProviderImpl`/`AccountCountsProviderImpl`/
//! `FilterQuery` (tasks 4.3/4.4, out of this task's `Boundary:
//! BlockPolicyImpl` scope) — this task adds no placeholder/stub for any of
//! those three, only [`BlockPolicyImpl`] itself. Does not touch
//! `src/social_graph/inbound.rs` (out of this task's boundary), does not
//! register [`BlockPolicyImpl`] against federation-core's actual `BlockPolicy`
//! registry/bootstrap (task 5.2's `SocialGraphModule` wiring boundary), and
//! does not touch `src/social_graph/repository.rs` beyond the one minimal,
//! additive `repository::is_blocked` existence query this task's own
//! persistence-layer counterpart needs (see that function's own doc comment).
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

use std::sync::Arc;

use sqlx::PgPool;

use crate::actor::{ActorDirectory, Handle};
use crate::domain::AccountRef;
use crate::error::AppError;
use crate::federation::inbound::block_policy::{BlockPolicy, LocalRecipientContext};
use crate::social_graph::inbound::ActorUriResolver;
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
