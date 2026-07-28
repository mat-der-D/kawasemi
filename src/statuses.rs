//! Statuses domain module (statuses-core spec, `src/statuses.rs` +
//! `src/statuses/`, mirroring the module-with-submodule convention
//! established by `src/media.rs`/`src/media/` and `src/accounts.rs`/
//! `src/accounts/`).
//!
//! Scope so far:
//! - Task 1.1 (`Boundary: migration`, no Rust code): `migrations/
//!   0007_statuses.sql` — `statuses` / `status_edits` / `status_media` /
//!   `favourites` / `bookmarks` / `pins` / `polls` / `poll_options` /
//!   `poll_votes` / `status_idempotency_keys` / `tags` / `status_tags`.
//! - Task 1.2 (`Boundary: model`): the domain value types this task's own
//!   instruction enumerates — [`model::Status`], [`model::StatusEdit`],
//!   [`model::Poll`], [`model::PollOption`], [`model::PollVote`],
//!   [`model::IdempotencyRecord`], and [`model::Tag`]. `Visibility` is
//!   imported from `crate::domain` (core-runtime's canonical shared
//!   primitives module) rather than redefined — see [`model`]'s own doc
//!   comment for why `AccountRef` is not additionally imported here.
//!
//! - Task 2.1 (`Boundary: StatusRepository, TagRepository`): [`status_repository`]
//!   (`Status`/`StatusEdit` persistence: insert, visible-scope fetch,
//!   ancestor/descendant traversal, delete with its two explicit
//!   self-referential cleanup steps, edit-apply + history, atomic counter
//!   updates) and [`tag_repository`] (hashtag persistence against `tags` /
//!   `status_tags`, and its tag<->status read boundary).
//!
//! - Task 2.2 (`Boundary: InteractionRepository`): [`interaction_repository`]
//!   (favourite/bookmark/pin record/revoke/exists against `favourites` /
//!   `bookmarks` / `pins`, the bookmark list's own creation-order cursor,
//!   and reblog's read-only duplicate-check against `statuses` — reblog
//!   record/revoke itself stays in [`status_repository`], see
//!   [`interaction_repository`]'s own doc comment).
//!
//! - Task 2.3 (`Boundary: PollRepository, IdempotencyStore`): [`poll_repository`]
//!   (`Poll`/`PollOption`/`PollVote` persistence: poll/option insertion, vote
//!   recording with deadline/range/single-vs-multiple/duplicate validation,
//!   and aggregate tally retrieval against `polls` / `poll_options` /
//!   `poll_votes`) and [`idempotency`] (the `Idempotency-Key` ledger:
//!   `(actor_id, key)` -> `status_id` lookup and resend resolution against
//!   `status_idempotency_keys`).
//!
//! - Task 3.1 (`Boundary: VisibilityPolicy, RelationshipQuery(port)`):
//!   [`visibility`] (the single visibility judgment
//!   [`visibility::is_visible`] that retrieval/context/interaction
//!   visibility checks are meant to funnel through, plus the
//!   [`visibility::RelationshipQuery`] delegation-port contract and its
//!   safe default [`visibility::NoRelationshipQuery`]). Does not yet wire
//!   `status_repository`'s queries to call [`visibility::is_visible`] —
//!   that remains `status_repository`'s own documented temporary stand-in
//!   until a later task replaces it.
//!
//! - Task 3.2 (`Boundary: Addressing`): [`addressing`] (the single
//!   `Visibility` -> `to`/`cc`/recipient derivation,
//!   [`addressing::derive_addressing`] / [`addressing::derive_recipients`],
//!   that local origination and remote delivery both funnel through — see
//!   [`addressing`]'s own doc comment for the `ActorRef` type it defines
//!   and the `to`/`cc` placement convention it follows).
//!
//! - Task 3.3 (`Boundary: StatusSerializer, PollSerializer`): [`serializer`]
//!   ([`serializer::status_to_json`]/[`serializer::poll_to_json`]: the
//!   Mastodon-compatible Status/Poll JSON contract, Account/media rendering
//!   delegated to `crate::accounts::serializer`/`crate::media::serializer`,
//!   contract-harness goldens under `tests/golden/statuses/` — see
//!   [`serializer`]'s own doc comment for the pre-resolved-input carrier
//!   types it defines and why).
//!
//! - Task 4.1 (`Boundary: StatusActivityBuilder`): [`activity_builder`]
//!   ([`activity_builder::StatusActivityBuilder`]: generates the six
//!   canonical post-related Activities — `Create(Note)` / `Announce` /
//!   `Like` / `Delete` / `Update` / `Undo(Announce|Like)` — plus the
//!   Mastodon-compatible vote wire form `Create{Note, name=...}`, and hands
//!   each one, with its `Addressing`-derived recipients, to
//!   `DeliveryService::deliver` unmodified — see [`activity_builder`]'s own
//!   doc comment for the `UndoKind`/`ActorHandleLookup` gap-fills and its
//!   deliberate deviations from design.md's literal Service Interface).
//!
//! - Task 5.1 (`Boundary: StatusService`): [`status_service`]
//!   ([`status_service::StatusService`]: create/show/context/delete/edit/
//!   history/source orchestration — idempotency check, empty-post
//!   rejection, media-ownership verification, poll/media exclusivity
//!   validation, mention/tag/emoji extraction, visibility-filtered
//!   retrieval/context routed through `visibility::is_visible` rather than
//!   `status_repository`'s own provisional stand-in, and Create/Delete/
//!   Update dispatch via `StatusActivityBuilder` — see [`status_service`]'s
//!   own doc comment for its documented boundary decisions on poll
//!   handling, remote-mention resolution, and the edit/history fields the
//!   schema cannot fully retain).
//!
//! - Task 5.2 (`Boundary: InteractionService`): [`interaction_service`]
//!   ([`interaction_service::InteractionService`]: reblog/favourite/
//!   bookmark/pin orchestration — visibility gate, duplicate prevention,
//!   counter update, and Announce/Like/Undo dispatch for reblog/favourite,
//!   local-only state for bookmark/pin — see [`interaction_service`]'s own
//!   doc comment for its documented boundary decisions).
//!
//! - Task 5.3 (`Boundary: PollService`): [`poll_service`]
//!   ([`poll_service::PollService`]: poll get/vote orchestration — a poll's
//!   owning status visibility gate, delegating deadline/range/single-vs-
//!   multiple/duplicate validation to `poll_repository::record_vote`,
//!   tally reflection, and vote-Activity dispatch via
//!   `StatusActivityBuilder::deliver_vote` (Requirements 13.2-13.6) — see
//!   [`poll_service`]'s own doc comment for why poll *creation*
//!   (Requirement 13.1) stays out of this task's scope and
//!   `status_service.rs` is left untouched).
//!
//! - Task 6.1 (`Boundary: InboundHandlers`): [`inbound_handlers`]
//!   ([`inbound_handlers::CreateNoteHandler`]/[`inbound_handlers::AnnounceHandler`]/
//!   [`inbound_handlers::LikeHandler`]/[`inbound_handlers::DeleteHandler`]/
//!   [`inbound_handlers::UpdateHandler`]/[`inbound_handlers::UndoHandler`] —
//!   federation-core's `InboundActivityHandler` implemented for the six
//!   post-related inbound Activity kinds, each calling the exact same
//!   repository functions the corresponding local-origin service already
//!   calls (Requirement 14.5) — plus
//!   [`inbound_handlers::register_status_handlers`], which registers all six
//!   against an `InboundActivityDispatcher`. Adds two small additive
//!   widenings this task's own boundary permits: `status_repository::find_by_uri`
//!   (a thin `pub` uri-keyed lookup, mirroring `find_by_id`'s existing
//!   precedent) and `status_service::extract_content_tokens`/`ExtractedTokens::hashtags`
//!   widened to `pub(crate)` (so inbound `Create(Note)` hashtag persistence
//!   reuses the exact same extraction function local-origin `create_status`
//!   already uses) — see [`inbound_handlers`]'s own doc comment for the full
//!   rationale, including its documented cross-spec dependency on
//!   accounts-and-instance's already-implemented `RemoteAccountFetcher` for
//!   resolving a remote actor's `actor_uri` to a stable `Id`.
//!
//!   Still no HTTP surface, and `register_status_handlers` is not yet wired
//!   into `crate::state::AppState`/`crate::bootstrap`/`crate::server` (task
//!   7.2's boundary) — this module remains a standalone, independently
//!   unit-testable set of handlers with no live caller yet. See design.md's
//!   "File Structure Plan" for the full planned module set.
//!
//! - Task 6.2 (`Boundary: StatusIngestService`, `_Depends: 6.1_`):
//!   [`ingest_service`] ([`ingest_service::StatusIngestService`]: a remote
//!   Note "URL/document → Status" ingestion entry point callable from
//!   outside federation-core's inbound Activity dispatch, e.g. a future
//!   search spec's `RemoteResolver`). Reuses
//!   [`inbound_handlers::ingest_note_object`] (a `pub(crate)` function
//!   extracted from `CreateNoteHandler`'s own inbound `Create(Note)` path by
//!   this task) verbatim, so this entry point produces identical results to
//!   the inbound-dispatch path for the same input (Requirement 14.5). Not
//!   wired into `AppState`/bootstrap/any live HTTP path or the `search` spec
//!   itself (which does not exist yet in this codebase) — see
//!   [`ingest_service`]'s own doc comment for the full contract.

//! - Task 7.1 (`Boundary: StatusEndpoints`, `_Depends: 5.1, 5.2, 5.3_`):
//!   [`endpoints`] (the 19 HTTP handlers design.md's API Contract table
//!   names for statuses/reblog/favourite/bookmark/pin/context/history/
//!   source and polls, plus [`endpoints::StatusesEndpointsState`], the
//!   still-generic router-local state bundle these handlers close over —
//!   see [`endpoints`]'s own doc comment for why it stays generic, unlike
//!   `AccountsEndpointsState`/`MediaEndpointsState`, and for the
//!   `Status -> StatusRenderInput` assembly glue this task had to write from
//!   scratch). Not mounted on `crate::server`/`crate::state::AppState` yet —
//!   task 7.2's boundary.
//!
//! - Task 7.2 (`Boundary: StatusesModule, server, bootstrap, config`,
//!   `_Depends: 6.1, 7.1_`): this module's own [`StatusesModule`] (built by
//!   [`build_statuses_module`]) picks this crate's one concrete production
//!   type per generic port (`crate::actor::ActorDirectory` for
//!   `ActorHandleLookup`/`LocalActorLookup`/`MentionLookup`,
//!   `crate::statuses::visibility::NoRelationshipQuery` for
//!   `RelationshipQuery` — social-graph has not landed — and federation-core's
//!   concrete `DeliveryService` instantiation for `StatusActivityBuilder`'s
//!   delivery port), constructs one [`activity_builder::StatusActivityBuilder`]
//!   per service (each sharing the same `Arc<ConcreteDeliveryService>` —
//!   see this file's own doc comment on [`ConcreteDeliveryService`] for why
//!   `activity_builder.rs`'s `delivery` field was widened to `Arc` for
//!   this), and bundles the three resulting services. [`ProdRemoteActorResolver`]
//!   supplies `inbound_handlers::RemoteActorResolver`'s first real
//!   production implementation (see its own doc comment for the local-actor
//!   shortcut and `tokio::spawn`-based `Send` adapter this requires).
//!   `src/bootstrap.rs`/`src/test_harness.rs` call [`build_statuses_module`]
//!   after federation-core's own module is built (needing its
//!   `delivery_service()`), pass a registration closure that calls
//!   [`inbound_handlers::register_status_handlers`] into
//!   `crate::federation::build_federation_module`'s own new
//!   `register_downstream` parameter, and store the result on
//!   `crate::state::AppState`; `src/server.rs` mounts every route
//!   `endpoints.rs` (task 7.1) defined, monomorphized over this module's
//!   concrete type aliases.

pub mod account_provider;
pub mod activity_builder;
pub mod addressing;
pub mod endpoints;
pub mod idempotency;
pub mod inbound_handlers;
pub mod ingest_service;
pub mod interaction_repository;
pub mod interaction_service;
pub mod model;
pub mod notification_sink;
pub mod poll_repository;
pub mod poll_service;
pub mod serializer;
pub mod status_repository;
pub mod status_service;
pub mod tag_repository;
pub mod visibility;

use std::future::Future;
use std::sync::Arc;

use axum::http::StatusCode;
use sqlx::PgPool;

use crate::accounts::RemoteAccountFetcher;
use crate::accounts::account_service::AccountService;
use crate::accounts::ports::AccountPortsRegistry;
use crate::actor::{ActorDirectory, Handle};
use crate::domain::Id;
use crate::error::AppError;
use crate::federation::signatures::ReqwestFederationHttpClient;
use crate::federation::urls::ActorUrls;
use crate::federation::{
    ConcreteBlockPolicy, ConcreteDeliveryService, ConcreteReceivedActivityStore, ConcreteVerifier,
    DbDeliveryQueue, HttpDeliverySink, InboundActivityDispatcher, LocalDeliverySink,
};
use crate::media::local_fs::LocalFsStore;
use crate::runtime::RuntimeContext;
use crate::statuses::activity_builder::StatusActivityBuilder;
use crate::statuses::endpoints::StatusesEndpointsState;
use crate::statuses::inbound_handlers::{RemoteActorResolver, StatusInboundDeps};
use crate::statuses::interaction_service::InteractionService;
use crate::statuses::notification_sink::NotificationSinkRegistry;
use crate::statuses::poll_service::PollService;
use crate::statuses::status_service::StatusService;
use crate::statuses::visibility::NoRelationshipQuery;

pub use model::{IdempotencyRecord, Poll, PollOption, PollVote, Status, StatusEdit, Tag};

/// This crate's one concrete `DeliverySink` (in-process) instantiation for
/// `StatusActivityBuilder`'s `L` parameter — matches
/// `crate::federation::ConcreteDeliveryService`'s own `L` argument exactly
/// (see that type's own doc comment, `src/federation/module.rs`), so the
/// `Arc<ConcreteDeliveryService>` `FederationModule::delivery_service`
/// exposes can be passed to [`build_statuses_module`] unmodified.
pub type ConcreteLocalSink =
    LocalDeliverySink<ConcreteVerifier, ConcreteBlockPolicy, ConcreteReceivedActivityStore>;

/// This crate's one concrete `DeliverySink` (HTTP/queue-backed) instantiation
/// for `StatusActivityBuilder`'s `H` parameter — matches
/// `crate::federation::ConcreteDeliveryService`'s own `H` argument exactly.
pub type ConcreteHttpSink = HttpDeliverySink<DbDeliveryQueue, ActorDirectory>;

/// The one concrete `StatusActivityBuilder` instantiation this instance
/// mounts every service with: `ActorDirectory` for both `ActorHandleLookup`
/// (`A`) and (matching `ConcreteDeliveryService`'s own `D`) `LocalActorLookup`,
/// [`ConcreteLocalSink`]/[`ConcreteHttpSink`] for `L`/`H`.
pub type ConcreteStatusActivityBuilder =
    StatusActivityBuilder<ActorDirectory, ActorDirectory, ConcreteLocalSink, ConcreteHttpSink>;

/// The one concrete `StatusService` instantiation this instance mounts (see
/// this file's doc comment, task 7.2): `ActorDirectory` for `A`/`D`/`M`
/// (`ActorHandleLookup`/`LocalActorLookup`/`MentionLookup` are all
/// implemented on `ActorDirectory` — see `activity_builder.rs`/
/// `status_service.rs`'s own `impl` blocks), [`ConcreteLocalSink`]/
/// [`ConcreteHttpSink`] for `L`/`H`, and
/// [`visibility::NoRelationshipQuery`] for `R` (social-graph has not landed
/// — task 3.1's own safe default).
pub type ConcreteStatusService = StatusService<
    ActorDirectory,
    ActorDirectory,
    ConcreteLocalSink,
    ConcreteHttpSink,
    NoRelationshipQuery,
    ActorDirectory,
>;

/// The one concrete `InteractionService` instantiation this instance mounts
/// — see [`ConcreteStatusService`]'s own doc comment for the identical
/// per-parameter rationale (this service has no `M`/`MentionLookup`
/// parameter of its own).
pub type ConcreteInteractionService = InteractionService<
    ActorDirectory,
    ActorDirectory,
    ConcreteLocalSink,
    ConcreteHttpSink,
    NoRelationshipQuery,
>;

/// The one concrete `PollService` instantiation this instance mounts — see
/// [`ConcreteStatusService`]'s own doc comment for the identical
/// per-parameter rationale.
pub type ConcretePollService = PollService<
    ActorDirectory,
    ActorDirectory,
    ConcreteLocalSink,
    ConcreteHttpSink,
    NoRelationshipQuery,
>;

/// The one concrete `StatusesEndpointsState` instantiation this instance
/// mounts every statuses/polls/bookmarks route with (task 7.1's own
/// `StatusesEndpointsState<A, D, L, H, R, M>`, still generic — this task
/// picks the concrete type arguments `endpoints.rs`'s own doc comment says
/// no earlier task had picked yet). `src/server.rs`'s `FromRef<AppState>`
/// bridge derives this from `AppState` directly.
pub type ConcreteStatusesEndpointsState = StatusesEndpointsState<
    ActorDirectory,
    ActorDirectory,
    ConcreteLocalSink,
    ConcreteHttpSink,
    NoRelationshipQuery,
    ActorDirectory,
>;

/// Adapts accounts-and-instance's already-implemented
/// `RemoteAccountFetcher<ReqwestFederationHttpClient>` to this spec's own
/// [`inbound_handlers::RemoteActorResolver`] port for real production use —
/// `inbound_handlers.rs`'s own doc comment ("Resolving `actor_uri -> Id`")
/// explicitly assigns supplying this implementation to this task
/// (`_Boundary: StatusesModule, server, bootstrap, config_`).
///
/// Two responsibilities, in this order:
///
/// 1. **Local-actor shortcut** (this crate's "ローカル最適化パス" convention,
///    steering's own phrase, applied here to actor-identity resolution the
///    same way `DeliveryService` already applies it to physical delivery).
///    Every inbound handler resolves the acting actor from
///    `ctx.signer.actor_uri` (`inbound_handlers.rs`'s own doc comment,
///    security rationale) — including for a purely local, in-process
///    delivery loop-back, where `LocalDeliverySink` builds a *synthetic*
///    `VerifiedSigner` from the **sending** local actor's own identity, never
///    a real HTTP-signed claim (`src/federation/outbound/sink.rs`'s own doc
///    comment: "`sender: &Handle`... builds a synthetic `VerifiedSigner`").
///    Without this shortcut, *every* local-to-local interaction (a status
///    mentioning another local actor, a local reblog/favourite of another
///    local actor's post) would force a real outbound HTTP fetch of this
///    instance's own actor document merely to re-derive an [`Id`] this
///    instance already knows synchronously — at best wasteful, at worst
///    outright broken in an environment where `ActorUrls` builds a
///    `https://{domain}/...` URI this process cannot itself reach over real
///    DNS/TLS (e.g. `crate::test_harness::spawn_test_app`'s fixed internal
///    test domain). Since [`ActorUrls::actor_url`] always builds exactly
///    `https://{domain}/users/{handle}`, this resolver recognizes that exact
///    shape against its own configured `domain` and resolves it directly via
///    [`ActorDirectory::resolve_actor_by_handle`] — no network call at all.
/// 2. **Genuinely remote fallback**: any other `actor_uri` shape (or a
///    same-shape URI that does not resolve to a currently-registered local
///    actor) falls through to [`RemoteAccountFetcher::fetch_and_normalize`],
///    wrapped in [`tokio::spawn`]. `RemoteAccountFetcher<H>::fetch_and_normalize`
///    cannot, as written, satisfy [`RemoteActorResolver`]'s own `Send`-future
///    requirement for a *generic* `H: FederationHttpClient` — `H::fetch`'s
///    `async fn` carries no `Send` bound in the trait itself
///    (`inbound_handlers.rs`'s own doc comment explains this in detail) — so
///    this resolver spawns the fetch onto its own `tokio` task (a
///    fully-concrete `RemoteAccountFetcher<ReqwestFederationHttpClient>`
///    instantiation, not a further-generic one) and awaits the `JoinHandle`
///    instead of `.await`-ing the fetcher's future directly in place.
pub struct ProdRemoteActorResolver {
    domain: String,
    directory: Arc<ActorDirectory>,
    fetcher: Arc<RemoteAccountFetcher<ReqwestFederationHttpClient>>,
}

impl ProdRemoteActorResolver {
    /// Builds a resolver bound to `domain` (this instance's own configured
    /// server domain, for the local-actor shortcut — must match
    /// `crate::config::ServerConfig::domain`/`ActorUrls`'s own domain
    /// exactly), `directory` (the local-actor lookup the shortcut resolves
    /// through), and `fetcher` (the genuinely-remote fallback).
    pub fn new(
        domain: impl Into<String>,
        directory: Arc<ActorDirectory>,
        fetcher: Arc<RemoteAccountFetcher<ReqwestFederationHttpClient>>,
    ) -> Self {
        Self {
            domain: domain.into(),
            directory,
            fetcher,
        }
    }

    /// Extracts `{handle}` from `actor_uri` when it matches this instance's
    /// own `https://{domain}/users/{handle}` shape ([`ActorUrls::actor_url`]'s
    /// exact construction) — see this type's own doc comment ("Local-actor
    /// shortcut"). Rejects a shape match whose extracted remainder itself
    /// contains a `/` (e.g. `.../users/alice/inbox`) so this shortcut only
    /// ever matches a bare actor URL, never one of its own sub-resources.
    fn local_handle(&self, actor_uri: &str) -> Option<Handle> {
        let prefix = format!("https://{}/users/", self.domain);
        actor_uri
            .strip_prefix(prefix.as_str())
            .filter(|rest| !rest.is_empty() && !rest.contains('/'))
            .and_then(|rest| Handle::new(rest).ok())
    }
}

impl RemoteActorResolver for ProdRemoteActorResolver {
    fn resolve_remote_actor(
        &self,
        actor_uri: &str,
    ) -> impl Future<Output = Result<Id, AppError>> + Send {
        let local_handle = self.local_handle(actor_uri);
        let directory = Arc::clone(&self.directory);
        let fetcher = Arc::clone(&self.fetcher);
        let actor_uri = actor_uri.to_string();
        async move {
            if let Some(handle) = local_handle
                && let Some(resolved) = directory.resolve_actor_by_handle(&handle).await?
            {
                return Ok(resolved.id);
                // Otherwise: shaped like one of our own actor URLs, but no
                // currently registered local actor matches — fall through to
                // the genuinely-remote path below rather than failing
                // outright here (a stale/foreign lookalike URI is not this
                // shortcut's problem to diagnose).
            }

            let joined = tokio::spawn(async move { fetcher.fetch_and_normalize(&actor_uri).await });
            match joined.await {
                Ok(result) => result.map(|account| account.id),
                Err(join_err) => Err(AppError::server(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    join_err,
                )),
            }
        }
    }
}

/// The statuses-core module bundle (design.md's exact `StatusesModule`
/// component; task 7.2, Requirements 4.3, 14.1): the three business
/// services [`build_statuses_module`] assembles, shared behind cheap-clone
/// `Arc`s the same way every other `*Module` bundle in this crate is (see
/// `crate::media::MediaModule`/`crate::accounts::AccountsModule`'s own
/// identical "bundle, don't build; accessors return `Arc::clone`"
/// precedent). `src/server.rs`'s `FromRef<AppState> for
/// StatusesEndpointsState<...>` bridge derives every mounted statuses
/// endpoint's own router-local state from these three handles.
pub struct StatusesModule {
    status_service: Arc<ConcreteStatusService>,
    interaction_service: Arc<ConcreteInteractionService>,
    poll_service: Arc<ConcretePollService>,
    notifications: NotificationSinkRegistry,
}

impl StatusesModule {
    /// The shared `StatusService` handle.
    pub fn status_service(&self) -> Arc<ConcreteStatusService> {
        Arc::clone(&self.status_service)
    }

    /// The shared `InteractionService` handle.
    pub fn interaction_service(&self) -> Arc<ConcreteInteractionService> {
        Arc::clone(&self.interaction_service)
    }

    /// The shared `PollService` handle.
    pub fn poll_service(&self) -> Arc<ConcretePollService> {
        Arc::clone(&self.poll_service)
    }

    /// The shared [`NotificationSinkRegistry`] handle (task 9.2) — cheap to
    /// clone (mirrors `crate::accounts::AccountsModule::ports()`'s identical
    /// shape). A future notifications spec's own bootstrap calls
    /// `.set_sink(...)` on the clone returned here to swap in its real
    /// `NotificationEventSink` implementation, reaching every `emit` call
    /// site this module's `status_service`/`interaction_service` already
    /// hold without touching either service's call sites.
    pub fn notification_sink_registry(&self) -> NotificationSinkRegistry {
        self.notifications.clone()
    }
}

/// Assembles the statuses-core module bundle (task 7.2, Requirements 4.3,
/// 14.1): builds one [`ActorDirectory`] per generic-port slot this module's
/// services need (a thin, stateless `PgPool` wrapper — see
/// `crate::actor::directory`'s own doc comment — so constructing several
/// independent instances from the same `pool` is cheap and correct, not a
/// second, divergent directory), one [`ConcreteStatusActivityBuilder`] per
/// service (each cloning the same `delivery` `Arc` — see
/// `activity_builder.rs`'s own doc comment on why that field is now an
/// `Arc`), and the three services themselves, each defaulted to
/// [`NoRelationshipQuery`] (social-graph has not landed).
///
/// `delivery` is `Arc<ConcreteDeliveryService>` — the exact type
/// `crate::federation::FederationModule::delivery_service` returns a
/// reference to — so callers (`src/bootstrap.rs`, `src/test_harness.rs`)
/// pass `Arc::clone(federation_module.delivery_service())` directly, after
/// `federation::build_federation_module` has already run.
pub fn build_statuses_module(
    pool: PgPool,
    runtime: RuntimeContext,
    domain: impl Into<String>,
    delivery: Arc<ConcreteDeliveryService>,
) -> StatusesModule {
    let domain = domain.into();
    let urls = ActorUrls::new(domain.clone());
    // One shared registry (task 9.2) — `status_service`/`interaction_service`
    // each hold a clone of this *same* instance so a single future
    // `set_sink` call (via `StatusesModule::notification_sink_registry`)
    // reaches every emit call site at once, defaulting to `NoopSink` until
    // then.
    let notifications = NotificationSinkRegistry::new();

    let status_builder = ConcreteStatusActivityBuilder::new(
        urls.clone(),
        runtime.ids.clone(),
        ActorDirectory::new(pool.clone()),
        Arc::clone(&delivery),
    );
    let interaction_builder = ConcreteStatusActivityBuilder::new(
        urls.clone(),
        runtime.ids.clone(),
        ActorDirectory::new(pool.clone()),
        Arc::clone(&delivery),
    );
    let poll_builder = ConcreteStatusActivityBuilder::new(
        urls.clone(),
        runtime.ids.clone(),
        ActorDirectory::new(pool.clone()),
        delivery,
    );

    let status_service = Arc::new(StatusService::new(
        pool.clone(),
        runtime.clone(),
        domain.clone(),
        urls.clone(),
        status_builder,
        NoRelationshipQuery,
        ActorDirectory::new(pool.clone()),
        notifications.clone(),
    ));

    let interaction_service = Arc::new(InteractionService::new(
        pool.clone(),
        runtime.clone(),
        urls.clone(),
        interaction_builder,
        ActorDirectory::new(pool.clone()),
        NoRelationshipQuery,
        notifications.clone(),
    ));

    let poll_service = Arc::new(PollService::new(
        pool.clone(),
        runtime,
        urls,
        poll_builder,
        ActorDirectory::new(pool),
        NoRelationshipQuery,
    ));

    StatusesModule {
        status_service,
        interaction_service,
        poll_service,
        notifications,
    }
}

/// Supplies this spec's own real implementations of the two
/// accounts-and-instance-owned delegation ports (task 9.1, `_Boundary:
/// AccountStatusesProviderImpl, AccountCountsContribution_`, Requirements
/// 6.1, 7.1) into `ports` — a live `crate::accounts::ports::AccountPortsRegistry`
/// handle (`crate::accounts::AccountsModule::ports()`, cheap to clone: see
/// that registry's own doc comment) — replacing its built-in
/// `EmptyStatusesProvider`/`ZeroCountsProvider` defaults (design.md's own
/// wording: "既定実装（空ページ / 0）を...本 spec の実装へ差し替える").
///
/// Callers (`src/bootstrap.rs`, `src/test_harness.rs`) call this once,
/// after both `crate::accounts::build_accounts_module` (for `ports`/
/// `accounts`) and `crate::media::build_media_module` (for `media_store`)
/// have already run — mirrors [`register_downstream_handlers`]'s own
/// "assemble this spec's own real implementation of a downstream-owned
/// port, then hand it to that port's own registration point" shape, just
/// registering directly (`set_statuses_provider`/`set_counts_provider`
/// take `&self`, see `AccountPortsRegistry`'s own doc comment) rather than
/// via a deferred closure — no equivalent single, later "the dispatcher is
/// under construction right now" choke point exists on the accounts-and-
/// instance side the way `InboundActivityDispatcher::register` created for
/// `register_downstream_handlers`.
pub fn register_account_ports(
    pool: PgPool,
    runtime: RuntimeContext,
    domain: impl Into<String>,
    ports: AccountPortsRegistry,
    accounts: Arc<AccountService<LocalFsStore, ReqwestFederationHttpClient>>,
    media_store: LocalFsStore,
) {
    let statuses_provider = account_provider::AccountStatusesProviderImpl::new(
        pool.clone(),
        runtime,
        domain,
        accounts,
        media_store,
    );
    ports.set_statuses_provider(Arc::new(statuses_provider));
    ports.set_counts_provider(Arc::new(account_provider::AccountCountsContribution::new(
        pool,
    )));
}

/// Builds the registration closure `crate::federation::build_federation_module`'s
/// own `register_downstream` parameter expects (task 7.2, Requirement 14.1):
/// registers all six post-related inbound handlers
/// ([`inbound_handlers::register_status_handlers`]) against the live
/// dispatcher, using [`ProdRemoteActorResolver`] as the production
/// `RemoteActorResolver`. Callers (`src/bootstrap.rs`, `src/test_harness.rs`)
/// build this closure's captured `resolver` from a `RemoteAccountFetcher`
/// constructed the same way `crate::accounts::build_accounts_module`'s own
/// call site does (its own, separately-constructed `ReqwestFederationHttpClient`
/// — never the same `Arc` `FederationModule`'s own client uses), *before*
/// calling `build_federation_module` (registration must happen inside that
/// function, before its own `InboxService::new` call — see
/// `src/federation/module.rs`'s own doc comment).
pub fn register_downstream_handlers(
    pool: PgPool,
    runtime: RuntimeContext,
    resolver: Arc<ProdRemoteActorResolver>,
) -> impl FnOnce(&mut InboundActivityDispatcher) {
    move |dispatcher: &mut InboundActivityDispatcher| {
        inbound_handlers::register_status_handlers(
            dispatcher,
            StatusInboundDeps {
                pool,
                runtime,
                remote_actors: resolver,
            },
        );
    }
}
