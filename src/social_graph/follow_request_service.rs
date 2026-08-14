//! `FollowRequestService` (design.md "Service / サービス層" -> "#### FollowService
//! / FollowRequestService / MuteService / BlockService"; Requirements 2.2,
//! 2.3, 2.4; task 3.2, `Boundary: FollowRequestService`): aggregates the
//! inbound-pending-follow-request business operations an owner reviewing
//! who wants to follow them needs — the paginated pending-inbound-request
//! list (2.2), authorize (promote the pending request to an established
//! follow + Accept(Follow) delivery, 2.3), and reject (drop the pending
//! request + Reject(Follow) delivery, 2.4). Mirrors task 3.1's
//! [`crate::social_graph::follow_service::FollowService`] closely (same
//! sibling-service shape, same collaborators) — see that module's own doc
//! comment for the base rationale this module does not repeat, and see
//! below for what differs.
//!
//! ## Scope
//! This module owns exactly [`FollowRequestService`] and its three public
//! methods, [`FollowRequestService::list_requests`]/
//! [`FollowRequestService::authorize_request`]/
//! [`FollowRequestService::reject_request`] (design.md's File Structure
//! Plan: `follow_request_service.rs`). `FollowService`/`MuteService`/
//! `BlockService` are separate files/tasks — this task implements only the
//! three methods task 3.2's own Requirements list (2.2-2.4) and boundary
//! (`FollowRequestService`) name. No HTTP surface (scope/auth/404
//! status-code discipline) lives here either — that is task 5.1's boundary
//! (`SocialGraphEndpoints`), not named in task 3.2's own Requirements list.
//! This service only ever deals with **inbound** pending requests (an owner
//! reviewing who wants to follow them) — it never touches outbound-pending
//! logic, which is `FollowService`'s own territory (task 3.1).
//!
//! ## Deliberate deviations from design.md's literal Service Interface
//! design.md's sketch (lines ~508-510) writes:
//! ```text
//! pub async fn list_requests(&self, viewer: &RequestActorContext, page: PageParams) -> Result<Page<serde_json::Value>, AppError>;
//! pub async fn authorize_request(&self, viewer: &RequestActorContext, requester: &str) -> Result<serde_json::Value, AppError>;
//! pub async fn reject_request(&self, viewer: &RequestActorContext, requester: &str) -> Result<serde_json::Value, AppError>;
//! ```
//! This module's actual signatures take `owner_id: Id` instead of
//! `viewer: &RequestActorContext`, for the exact same reason (and citing the
//! exact same precedent, `interaction_service.rs::reblog`/`unreblog`)
//! `follow_service.rs`'s own doc comment already documents for
//! `FollowService::follow`/`unfollow` — `RequestActorContext -> Id`
//! extraction is task 5.1's (endpoints) responsibility, not this service's.
//! `requester: &str` is kept exactly as design.md specifies (parsed the same
//! numeric-id-first way `FollowService::resolve_target` parses `target`).
//!
//! ## `list_requests`: embedding real Account JSON via a concrete
//! `AccountService` handle (CONCERN)
//! Requirement 2.2 asks for "当該アクター宛の保留中フォローリクエストの送信元
//! アカウント一覧" — the pending requests' *source accounts*, not raw
//! `FollowRequest` rows. Building a Mastodon-shaped Account JSON is
//! accounts-and-instance's own, already-implemented contract
//! ([`crate::accounts::serializer::AccountSerializer`]/
//! [`crate::accounts::account_service::AccountService::show_account`]) —
//! this module must not reinvent that shape. `design.md`'s own
//! `FollowRequestService` "Key Dependencies" table row does not list
//! `AccountService`/`AccountSerializer` explicitly (only `RelationshipRepository,
//! ActivityBuilder, Transitions, Pagination`), but that same table's row for
//! sibling services is documented elsewhere (`repository.rs`, `tasks.md`'s
//! Implementation Notes) as a curated, non-exhaustive summary rather than a
//! literal exhaustive dependency list — and a real, already-reviewed
//! precedent for exactly this situation (a downstream spec needing to embed
//! full Account JSON from several layers away from a live HTTP request, no
//! per-request `ForwardedOrigin` available) already exists in this crate:
//! `crate::statuses::account_provider::AccountStatusesProviderImpl` holds a
//! concrete `Arc<AccountService<LocalFsStore, ReqwestFederationHttpClient>>`
//! handle plus a configured `domain: String`, and synthesizes a fixed
//! `https://{domain}` origin via `ForwardedOrigin::resolve` (that module's own
//! doc comment, "Rendering without a live request's own forwarded origin").
//! This module follows that identical, already-reviewed shape rather than
//! inventing a new one: [`FollowRequestService`] holds the same concrete
//! `Arc<AccountService<LocalFsStore, ReqwestFederationHttpClient>>` +
//! `domain: String` pair, and [`FollowRequestService::list_requests`] calls
//! `AccountService::show_account` once per pending request's requester,
//! against that same synthesized origin. This carries the identical CONCERN
//! `AccountStatusesProviderImpl`'s own doc comment already flags for its
//! own embedded-Account URLs: behind a reverse proxy presenting a different
//! external scheme/host than this instance's own configured `domain`, this
//! list's embedded accounts' media/URL fields will not reflect the proxy's
//! externally-visible origin. Flagged here for reviewer confirmation, not
//! silently shipped as fully general.
//!
//! ## `authorize_request`/`reject_request`: 404 for "nothing pending",
//! sourced from `Transitions`'s own widened return value
//! [`crate::social_graph::transitions::Transitions::promote_pending`]/
//! [`crate::social_graph::transitions::Transitions::drop_pending`] are each
//! individually idempotent no-ops (`Ok(None)`) when no matching pending
//! request exists — appropriate for their own original callers (the
//! not-yet-implemented `InboundHandler`, which must never error on a
//! duplicate/late Accept or Reject), but Requirement 2's own implicit
//! "特定のフォローリクエストの承認/拒否" cannot authorize/reject something
//! that does not exist, so this service surfaces that `None` case as a
//! `404`-shaped [`AppError`] (mirrors `follow_service.rs`'s own "target not
//! found" 404 convention for the analogous situation). See
//! `transitions.rs`'s own doc comment ("Task 3.2 additions") for why
//! `promote_pending`/`drop_pending`'s return type was widened from
//! `Result<(), AppError>` to `Result<Option<String>, AppError>` to make this
//! possible in the same atomic step that consumes the pending row (recovering
//! the original inbound Follow's own Activity id, which
//! [`crate::social_graph::activity_builder::ActivityBuilder::build_accept`]/
//! [`build_reject`] both require as their `follow_activity_id` parameter).
//!
//! ## Generic shape mirrors `FollowService`
//! `FollowRequestService<AL, AR, D, LS, HS>` mirrors
//! `FollowService<AL, AR, D, LS, HS>`'s established shape exactly — see that
//! module's own doc comment ("Generic shape mirrors `InteractionService`")
//! for the full rationale, which applies here unchanged.
//!
//! ## Where this module's tests live
//! There is no `follow_request_service/tests.rs`. Every one of this module's
//! tests drives [`FollowRequestService`] against real `actor`/
//! `remote_accounts`/`follow_requests` rows, so all of them require a
//! running instance (`crate::test_harness`'s own `spawn_test_app`) and live
//! in `tests/social_graph_follow_request_service_it.rs` — placed there by
//! `.kiro/specs/test-placement-migration` task 5.2 so that steering
//! `structure.md`'s test layout rule ("DB込みの実起動インスタンスを要する検証
//! は `tests/` 直下の `*_it.rs` に置く") holds in fact and not only on paper.
//! A module with no `tests.rs` therefore means "no pure unit test applies
//! here", not "untested".

use std::sync::Arc;

use axum::http::StatusCode;
use sqlx::PgPool;

use crate::accounts::account_service::AccountService;
use crate::accounts::relationship_serializer::RelationshipSerializer;
use crate::accounts::remote_repository;
use crate::api::origin::self_origin;
use crate::api::pagination::{ForwardedOrigin, Page, PageParams};
use crate::domain::{AccountRef, Id};
use crate::error::{AppError, ErrorKind};
use crate::federation::LocalActorLookup as DeliveryLocalActorLookup;
use crate::federation::signatures::ReqwestFederationHttpClient;
use crate::federation::{DeliveryRequest, DeliveryService, DeliverySink, Recipient};
use crate::media::LocalFsStore;
use crate::runtime::RuntimeContext;
use crate::social_graph::activity_builder::{ActivityBuilder, LocalActorLookup, RemoteActorLookup};
use crate::social_graph::relationship_mapper::RelationshipMapper;
use crate::social_graph::repository::{self, RelationshipState};
use crate::social_graph::transitions::Transitions;

fn account_not_found(target: &str) -> AppError {
    AppError::client(
        StatusCode::NOT_FOUND,
        format!("account '{target}' was not found"),
    )
}

fn no_pending_request(target: &str) -> AppError {
    AppError::client(
        StatusCode::NOT_FOUND,
        format!("no pending follow request from '{target}'"),
    )
}

/// `requester`'s resolved identity and ready-to-deliver [`Recipient`] — this
/// service's own private output of [`FollowRequestService::
/// resolve_requester`], mirroring `follow_service.rs::ResolvedTarget` minus
/// the lock-state field this service never needs (approval necessity was
/// already judged when the pending row was first recorded).
struct ResolvedRequester {
    account: AccountRef,
    recipient: Recipient,
}

/// The inbound-pending-follow-request business-service layer (design.md's
/// exact `FollowRequestService`, Requirements 2.2-2.4). See this module's
/// doc comment for the full deviation rationale.
pub struct FollowRequestService<AL, AR, D, LS, HS>
where
    AL: LocalActorLookup,
    AR: RemoteActorLookup,
    D: DeliveryLocalActorLookup,
    LS: DeliverySink,
    HS: DeliverySink,
{
    pool: PgPool,
    runtime: RuntimeContext,
    local: AL,
    activity_builder: ActivityBuilder<AL, AR>,
    transitions: Transitions,
    delivery: Arc<DeliveryService<D, LS, HS>>,
    accounts: Arc<AccountService<LocalFsStore, ReqwestFederationHttpClient>>,
    domain: String,
}

impl<AL, AR, D, LS, HS> FollowRequestService<AL, AR, D, LS, HS>
where
    AL: LocalActorLookup,
    AR: RemoteActorLookup,
    D: DeliveryLocalActorLookup,
    LS: DeliverySink,
    HS: DeliverySink,
{
    /// Builds a `FollowRequestService` bound to `pool`/`runtime` (mirrors
    /// `FollowService::new`'s identical rationale), `local`/`activity_builder`/
    /// `transitions`/`delivery` (same collaborators, same roles, as
    /// `FollowService`), `accounts` (Account-embed rendering for
    /// [`Self::list_requests`], `crate::accounts::build_accounts_module`'s own
    /// `AccountService` handle — see this module's doc comment, "`list_requests`:
    /// embedding real Account JSON"), and `domain` (this instance's own
    /// configured server domain, for the same synthesized-origin rationale).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pool: PgPool,
        runtime: RuntimeContext,
        local: AL,
        activity_builder: ActivityBuilder<AL, AR>,
        transitions: Transitions,
        delivery: Arc<DeliveryService<D, LS, HS>>,
        accounts: Arc<AccountService<LocalFsStore, ReqwestFederationHttpClient>>,
        domain: impl Into<String>,
    ) -> Self {
        Self {
            pool,
            runtime,
            local,
            activity_builder,
            transitions,
            delivery,
            accounts,
            domain: domain.into(),
        }
    }

    /// Synthesizes a fixed `https://{domain}` origin for [`Self::list_requests`]'s
    /// embedded Account JSON rendering — see this module's doc comment
    /// ("`list_requests`: embedding real Account JSON") for why no real
    /// per-request `ForwardedOrigin` is available this deep, and why this is
    /// the same fallback `AccountStatusesProviderImpl::origin` already uses.
    fn origin(&self) -> ForwardedOrigin {
        self_origin(&self.domain)
    }

    /// Resolves `requester_id` (an already-parsed internal numeric account
    /// id) to its [`AccountRef`]/ready-to-deliver [`Recipient`],
    /// local-first then remote-cache — mirrors `FollowService::
    /// resolve_target`'s identical local/remote/404 discipline (this
    /// module's doc comment does not repeat that rationale; see
    /// `follow_service.rs`'s own doc comment, "`target: &str` resolution",
    /// for the full one). No lock-state resolution is needed here: approval
    /// necessity was already judged (by `FollowApprovalPolicy`, task 2.1) at
    /// the moment the pending request this service consumes was first
    /// recorded.
    async fn resolve_requester(&self, requester_id: Id) -> Result<ResolvedRequester, AppError> {
        match self.local.resolve_handle(requester_id).await {
            Ok(handle) => {
                return Ok(ResolvedRequester {
                    account: AccountRef::Local(requester_id),
                    recipient: Recipient::Local(handle),
                });
            }
            Err(err) if err.kind == ErrorKind::Client => {
                // Not a local actor -- fall through to the remote-cache
                // check below, mirroring `FollowService::resolve_target`'s
                // identical branch.
            }
            Err(err) => return Err(err),
        }

        if let Some(remote) = remote_repository::find_remote_by_id(&self.pool, requester_id).await?
        {
            // See `follow_service.rs`'s doc comment ("Remote delivery's
            // `inbox`") for why this interim actor-uri-plus-`/inbox`
            // convention, rather than a persisted inbox field, is used here.
            let inbox = format!("{}/inbox", remote.actor_uri);
            return Ok(ResolvedRequester {
                account: AccountRef::Remote(requester_id),
                recipient: Recipient::Remote {
                    inbox,
                    shared_inbox: None,
                },
            });
        }

        Err(account_not_found(&requester_id.as_i64().to_string()))
    }

    /// Loads `viewer`'s single relationship state to `target` — mirrors
    /// `follow_service.rs::FollowService::load_state`'s identical thin
    /// wrapper around [`repository::load_states`]'s batched interface.
    async fn load_state(
        &self,
        viewer: &AccountRef,
        target: &AccountRef,
        now: time::OffsetDateTime,
    ) -> Result<RelationshipState, AppError> {
        let mut states =
            repository::load_states(&self.pool, viewer, std::slice::from_ref(target), now).await?;
        Ok(states
            .pop()
            .expect("load_states returns exactly one state per requested target"))
    }

    /// Maps `state` to its Relationship JSON response via the already-
    /// implemented [`RelationshipMapper`] + accounts-and-instance's
    /// `RelationshipSerializer` — mirrors `follow_service.rs`'s identical
    /// helper.
    fn build_relationship(&self, state: &RelationshipState) -> serde_json::Value {
        let view = RelationshipMapper.to_view(state);
        RelationshipSerializer::new().build_relationship(&view)
    }

    /// Returns `owner_id`'s pending **inbound** follow requests (Requirement
    /// 2.2), one Account JSON per requester, in the same page shape
    /// `repository::list_inbound_requests` already produces. See this
    /// module's doc comment ("`list_requests`: embedding real Account
    /// JSON") for why each requester is rendered via the real,
    /// already-implemented `AccountService::show_account` rather than a
    /// reinvented shape.
    pub async fn list_requests(
        &self,
        owner_id: Id,
        page: PageParams,
    ) -> Result<Page<serde_json::Value>, AppError> {
        let owner_ref = AccountRef::Local(owner_id);
        let requests_page =
            repository::list_inbound_requests(&self.pool, &owner_ref, &page).await?;

        let origin = self.origin();
        let mut items = Vec::with_capacity(requests_page.items.len());
        for req in &requests_page.items {
            let requester_id = match req.requester {
                AccountRef::Local(id) | AccountRef::Remote(id) => id,
            };
            let account_json = self
                .accounts
                .show_account(&requester_id.as_i64().to_string(), None, &origin)
                .await?;
            items.push(account_json);
        }

        Ok(Page {
            items,
            prev_cursor: requests_page.prev_cursor,
            next_cursor: requests_page.next_cursor,
        })
    }

    /// Authorizes (promotes) `owner_id`'s pending inbound follow request from
    /// `requester_target`: establishes the follow (`Transitions::
    /// promote_pending`) and delivers `Accept(Follow)` (`ActivityBuilder::
    /// build_accept`) to the requester via the common `DeliveryService` path,
    /// regardless of the requester's locality, before returning the updated
    /// relationship (Requirement 2.3). 404s when no such pending request
    /// exists — see this module's doc comment ("`authorize_request`/
    /// `reject_request`: 404 for 'nothing pending'").
    pub async fn authorize_request(
        &self,
        owner_id: Id,
        requester_target: &str,
    ) -> Result<serde_json::Value, AppError> {
        let requester_id = requester_target
            .parse::<i64>()
            .map(Id::from_i64)
            .map_err(|_| account_not_found(requester_target))?;

        let resolved = self.resolve_requester(requester_id).await?;
        let owner_ref = AccountRef::Local(owner_id);

        let Some(follow_activity_id) = self
            .transitions
            .promote_pending(&resolved.account, &owner_ref)
            .await?
        else {
            return Err(no_pending_request(requester_target));
        };

        let accept = self
            .activity_builder
            .build_accept(&owner_ref, &follow_activity_id, &resolved.account)
            .await?;

        let sender = self.local.resolve_handle(owner_id).await?;
        self.delivery
            .deliver(DeliveryRequest {
                activity: accept,
                sender,
                recipients: vec![resolved.recipient],
            })
            .await?;

        let now = self.runtime.clock.now();
        let state = self.load_state(&owner_ref, &resolved.account, now).await?;
        Ok(self.build_relationship(&state))
    }

    /// Rejects `owner_id`'s pending inbound follow request from
    /// `requester_target`: drops the pending request (`Transitions::
    /// drop_pending`, never establishing a follow) and delivers
    /// `Reject(Follow)` (`ActivityBuilder::build_reject`) to the requester
    /// via the common `DeliveryService` path, before returning the updated
    /// relationship (Requirement 2.4). 404s when no such pending request
    /// exists — see this module's doc comment ("`authorize_request`/
    /// `reject_request`: 404 for 'nothing pending'").
    pub async fn reject_request(
        &self,
        owner_id: Id,
        requester_target: &str,
    ) -> Result<serde_json::Value, AppError> {
        let requester_id = requester_target
            .parse::<i64>()
            .map(Id::from_i64)
            .map_err(|_| account_not_found(requester_target))?;

        let resolved = self.resolve_requester(requester_id).await?;
        let owner_ref = AccountRef::Local(owner_id);

        let Some(follow_activity_id) = self
            .transitions
            .drop_pending(&resolved.account, &owner_ref)
            .await?
        else {
            return Err(no_pending_request(requester_target));
        };

        let reject = self
            .activity_builder
            .build_reject(&owner_ref, &follow_activity_id, &resolved.account)
            .await?;

        let sender = self.local.resolve_handle(owner_id).await?;
        self.delivery
            .deliver(DeliveryRequest {
                activity: reject,
                sender,
                recipients: vec![resolved.recipient],
            })
            .await?;

        let now = self.runtime.clock.now();
        let state = self.load_state(&owner_ref, &resolved.account, now).await?;
        Ok(self.build_relationship(&state))
    }
}
