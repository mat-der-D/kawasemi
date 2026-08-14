//! Notifications domain module (notifications spec, made up of
//! `src/notifications.rs` and `src/notifications/`, mirroring the
//! module-with-submodule convention established by `src/media.rs`/
//! `src/media/`, `src/accounts.rs`/`src/accounts/`,
//! `src/statuses.rs`/`src/statuses/`, and
//! `src/social_graph.rs`/`src/social_graph/`).
//!
//! Scope so far:
//! - Task 1.1 (`Boundary: NotificationRepository`, no Rust code):
//!   `migrations/0009_notifications.sql` — the `notifications` table, its
//!   dedup partial unique index (未消去限定), and its recipient cursor
//!   index.
//! - Task 1.2 (`Boundary: model`): the domain value types design.md's
//!   "model" component names — [`model::NotificationType`],
//!   [`model::Notification`], and [`model::NotificationEvent`]. `Id`/
//!   `AccountRef` are not redefined here: both are imported from
//!   `crate::domain` (core-runtime's canonical shared primitives module,
//!   mirroring `src/statuses/model.rs`'s and `src/social_graph/model.rs`'s
//!   identical precedent) — see [`model`]'s own doc comment.
//! - Task 1.3 (`Boundary: NotificationRepository`): the notification's own
//!   persistence — [`repository::insert_dedup`] (dedup-idempotent insert),
//!   [`repository::list`] (recipient-scoped/dismissed-excluded/cursor-
//!   paginated/type-and-account-filtered), [`repository::find_for_recipient`]
//!   (single fetch), [`repository::dismiss`], [`repository::clear`], plus
//!   [`repository::InsertOutcome`]/[`repository::ListFilter`] — against
//!   `notifications` (`migrations/0009_notifications.sql`, task 1.1). See
//!   [`repository`]'s own doc comment.
//! - Task 2.1 (`Boundary: NotificationSerializer`): the Notification JSON
//!   outer shell — [`serializer::to_notification_json`]/
//!   [`serializer::notification_to_json`], delegating the `account`/
//!   `status` embeds to accounts-and-instance's/statuses-core's own
//!   serializers rather than redefining either contract. See
//!   [`serializer`]'s own doc comment.
//! - Task 2.2 (`Boundary: ports`): the delegation seams —
//!   [`ports::NotificationEventSink`] (upstream event receipt, default
//!   [`ports::NoopSink`]) and [`ports::NotificationDeliverySink`]
//!   (downstream delivery of a persisted notification, same default), plus
//!   the swap-in registry handle [`ports::NotificationPortsRegistry`] a
//!   later task (4.2) stores inside `AppState`. See [`ports`]'s own doc
//!   comment.
//! - Task 2.3 (`Boundary: NotificationFilter`): the generation-stage
//!   block/notification-mute suppression decision —
//!   [`filter::NotificationFilter::should_suppress`], a thin consumer of
//!   social-graph's `FilterQuery::blocked_set` that reimplements no
//!   relationship state or expiry logic of its own. See [`filter`]'s own
//!   doc comment.
//! - Task 2.4 (`Boundary: NotificationGenerator`): the single notification
//!   generation point — [`generator::NotificationGenerator::generate`],
//!   which sequences recipient-local-check → [`filter::NotificationFilter`]
//!   → kind-agnostic event-to-notification mapping →
//!   [`repository::insert_dedup`] → new-only
//!   [`ports::NotificationDeliverySink`] hand-off, per design.md's own
//!   sequence diagram. Implements no filtering/dedup/delivery logic of its
//!   own — every collaborator it sequences was already built by an earlier
//!   task. See [`generator`]'s own doc comment.
//! - Task 3.1 (`Boundary: NotificationEventSink`): the real (non-`NoopSink`)
//!   [`ports::NotificationEventSink`] implementation —
//!   [`event_sink::GeneratorEventSink`], which routes a canonical
//!   [`NotificationEvent`] straight into
//!   [`generator::NotificationGenerator::generate`] — plus the bridge that
//!   makes that routing reachable from the two upstream emitters that
//!   already exist, [`event_sink::StatusesEventSinkAdapter`], which
//!   converts `statuses::notification_sink`'s placeholder event/kind types
//!   into this module's own and delegates onward. Registering either into
//!   a live `AppState`/`statuses::notification_sink::NotificationSinkRegistry`
//!   was left to task 4.2 — see [`event_sink`]'s own doc comment.
//! - Task 4.1 (`Boundary: NotificationEndpoints`): the four HTTP handlers
//!   ([`endpoints::list_notifications`]/[`endpoints::show_notification`]/
//!   [`endpoints::clear_notifications`]/[`endpoints::dismiss_notification`])
//!   and their router-local state bundle
//!   ([`endpoints::NotificationEndpointsState`]) — see [`endpoints`]'s own
//!   doc comment. Not yet mounted onto any real router or declared as part
//!   of this module's own tree (task 4.1's own explicit boundary excluded
//!   `pub mod endpoints;` from this file) until this task (4.2) added it
//!   below.
//! - Task 4.2 (`Boundary: NotificationModule`): this file's own composition
//!   point — [`build_notification_module`] assembles this spec's own
//!   [`filter::NotificationFilter`]/[`generator::NotificationGenerator`],
//!   registers the real (non-`NoopSink`) event sink pair task 3.1 built
//!   (both into this module's own [`ports::NotificationPortsRegistry`] and,
//!   critically, into the *upstream*-owned
//!   `statuses::notification_sink::NotificationSinkRegistry` every existing
//!   local-/remote-origin emit call site already holds a clone of —
//!   replacing its built-in `NoopSink` default, Requirement 5.4), leaves
//!   [`ports::NotificationPortsRegistry`]'s `delivery_sink` slot at its
//!   default [`ports::NoopSink`] (Requirement 5.5 — a future streaming/
//!   web-push spec's own bootstrap swaps this in later), and bundles the
//!   result as [`NotificationModule`], which `src/state.rs` now stores and
//!   `src/server.rs`'s `FromRef<AppState> for
//!   endpoints::NotificationEndpointsState` bridge derives every mounted
//!   notification endpoint's own state from. `pub mod endpoints;` (added
//!   below) is this task's own minimal addition making task 4.1's
//!   already-reviewed module reachable from the crate's module tree at all.
//!   See this file's own "module wiring" section below for
//!   [`NotificationModule`]/[`build_notification_module`]'s full doc
//!   comments, and `src/state.rs`/`src/bootstrap.rs`/`src/server.rs`'s own
//!   doc comments at each of their call sites.
//!
//! ## Tests
//! This module's own wiring tests — that the notification routes are
//! actually mounted and that an emitted event traverses every seam
//! [`build_notification_module`] connects — require a real running instance
//! and live in `tests/notifications_module_it.rs`, moved there from this
//! module's former `tests` submodule by
//! `.kiro/specs/test-placement-migration` task 4.2.

pub mod endpoints;
pub mod event_sink;
pub mod filter;
pub mod generator;
pub mod model;
pub mod ports;
pub mod repository;
pub mod serializer;
pub mod service;

pub use event_sink::{GeneratorEventSink, StatusesEventSinkAdapter};
pub use filter::NotificationFilter;
pub use generator::{GenerateOutcome, NotificationGenerator};
pub use model::{Notification, NotificationEvent, NotificationType};
pub use ports::{
    NoopSink, NotificationDeliverySink, NotificationEventSink, NotificationPortsRegistry,
};
pub use repository::{InsertOutcome, ListFilter};
pub use serializer::{
    NotificationJson, NotificationRenderInput, SerializeContext, notification_to_json,
    to_notification_json,
};
pub use service::NotificationService;

// ---- Task 4.2 (Boundary: NotificationModule): module wiring -------------

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use sqlx::PgPool;

use crate::accounts::account_service::AccountService;
use crate::error::AppError;
use crate::federation::signatures::ReqwestFederationHttpClient;
use crate::media::LocalFsStore;
use crate::runtime::RuntimeContext;
use crate::social_graph::FilterQuery;
use crate::statuses::notification_sink::NotificationSinkRegistry;

/// Bridges [`NotificationGenerator`]'s fixed, construction-time
/// `Arc<dyn NotificationDeliverySink>` dependency onto
/// [`NotificationPortsRegistry`]'s own swappable `delivery_sink` slot (see
/// that type's own doc comment, "Registry shape: one handle, two
/// independent replaceable slots"), so a downstream streaming/web-push
/// spec's future `NotificationPortsRegistry::set_delivery_sink` call —
/// against the very same registry instance [`NotificationModule::ports`]
/// hands back, since `AppState` stores only one instance of it — takes
/// effect for this already-constructed [`NotificationGenerator`] too, not
/// only for a hypothetical caller that happens to read the registry
/// directly. Without this indirection, the registry's own `delivery_sink`
/// slot living inside `AppState` would be functionally inert: nothing in
/// this crate would ever consult it, and "下流が後で差し替え可能"
/// (design.md's own completion text for this task) would not actually hold.
struct DeliverySinkBridge {
    registry: NotificationPortsRegistry,
}

impl NotificationDeliverySink for DeliverySinkBridge {
    fn deliver<'a>(
        &'a self,
        notification: &'a Notification,
    ) -> Pin<Box<dyn Future<Output = Result<(), AppError>> + Send + 'a>> {
        Box::pin(async move { self.registry.deliver(notification).await })
    }
}

/// The notifications module bundle (design.md's exact `NotificationModule`
/// component; task 4.2, `Boundary: NotificationModule`, Requirements 5.4,
/// 5.5, 9.1): the shared [`NotificationService`] handle `src/server.rs`'s
/// `FromRef<AppState> for endpoints::NotificationEndpointsState` bridge
/// derives every mounted notification endpoint's own state from, plus the
/// [`NotificationPortsRegistry`] handle a future streaming/web-push spec's
/// own bootstrap reaches (mirroring
/// `crate::accounts::AccountsModule::ports`'s identical "bundle, don't
/// build; accessors return `Arc`/cheap clones" shape) to register its real
/// [`NotificationDeliverySink`] implementation, replacing the default
/// [`NoopSink`] this task initializes it with. Built by
/// [`build_notification_module`].
pub struct NotificationModule {
    service: Arc<NotificationService>,
    ports: NotificationPortsRegistry,
}

impl NotificationModule {
    /// The shared `NotificationService` handle.
    pub fn service(&self) -> Arc<NotificationService> {
        Arc::clone(&self.service)
    }

    /// The shared [`NotificationPortsRegistry`] handle — cheap to clone
    /// (mirrors `crate::accounts::AccountsModule::ports()`'s identical
    /// shape). A future streaming/web-push spec's own bootstrap calls
    /// `.set_delivery_sink(...)` on the clone returned here to swap in its
    /// real [`NotificationDeliverySink`] implementation, reaching the
    /// already-constructed [`NotificationGenerator`] this module wires (via
    /// [`DeliverySinkBridge`]) without needing to rebuild this module.
    pub fn ports(&self) -> NotificationPortsRegistry {
        self.ports.clone()
    }
}

/// Assembles the [`NotificationModule`] bundle (task 4.2, Requirements 5.4,
/// 5.5, 9.1): builds this spec's own repository-backed
/// [`filter::NotificationFilter`] (from social-graph's [`FilterQuery`], the
/// same construction `social_graph::providers::FilterQuery::new` documents),
/// the single [`generator::NotificationGenerator`] generation point (its
/// delivery sink bridged through a freshly built
/// [`NotificationPortsRegistry`] via [`DeliverySinkBridge`], defaulting to
/// [`NoopSink`] until a future streaming/web-push spec's own bootstrap
/// replaces it — Requirement 5.5), and the real (non-`NoopSink`)
/// [`event_sink::GeneratorEventSink`]/[`event_sink::StatusesEventSinkAdapter`]
/// pair (task 3.1). This function registers that real event sink into
/// *both* this module's own [`NotificationPortsRegistry`] (kept in sync,
/// even though nothing else in this crate currently reads that particular
/// slot — see that registry's own doc comment: "the swap-in registry handle
/// a later task (4.2) inserts into AppState") and — the actually-consumed
/// wiring point — `notification_sink_registry`, the *upstream*-owned
/// `crate::statuses::notification_sink::NotificationSinkRegistry`
/// `StatusService`/`InteractionService`/`inbound_handlers.rs`/
/// `social_graph::transitions` already hold a clone of, replacing its
/// built-in `NoopSink` default (Requirement 5.4's "上流既定 no-op を差し替
/// え").
///
/// `accounts`/`media_store` are the exact handles [`NotificationService`]
/// needs for its own `account`/`status` embed rendering (mirrors
/// `crate::timelines::build_timelines_module`'s identical "share, don't
/// rebuild" call convention) — callers (`src/bootstrap.rs`,
/// `src/test_harness.rs`, `src/federation/test_harness.rs`) pass
/// `accounts_module.service()`/`media_module.store().clone()` directly,
/// after those modules are already built. `notification_sink_registry` must
/// be the *same* instance already handed to
/// `crate::statuses::build_statuses_module`/
/// `crate::social_graph::register_downstream_handlers` (task 10.2's own
/// shared-registry convention, see `crate::statuses::build_statuses_module`'s
/// own doc comment "notifications") — a clone reaches the identical shared
/// slot, so this call's own `set_sink` replaces the default every existing
/// local-/remote-origin emit call site already holds a handle to, without
/// touching any of their call sites.
pub fn build_notification_module(
    pool: PgPool,
    runtime: RuntimeContext,
    domain: impl Into<String>,
    accounts: Arc<AccountService<LocalFsStore, ReqwestFederationHttpClient>>,
    media_store: LocalFsStore,
    notification_sink_registry: NotificationSinkRegistry,
) -> NotificationModule {
    let filter = NotificationFilter::new(FilterQuery::new(pool.clone(), runtime.clone()));

    let ports = NotificationPortsRegistry::new();
    let delivery_bridge: Arc<dyn NotificationDeliverySink> = Arc::new(DeliverySinkBridge {
        registry: ports.clone(),
    });

    let generator = Arc::new(NotificationGenerator::new(
        pool.clone(),
        filter,
        delivery_bridge,
        runtime.clone(),
    ));

    // Requirement 5.4: replaces the upstream-owned NotificationSinkRegistry's
    // built-in NoopSink default with the real event sink (task 3.1),
    // reaching every local-/remote-origin emit call site that already holds
    // a clone of `notification_sink_registry`.
    let event_sink: Arc<dyn NotificationEventSink> =
        Arc::new(GeneratorEventSink::new(Arc::clone(&generator)));
    ports.set_event_sink(Arc::clone(&event_sink));
    notification_sink_registry.set_sink(Arc::new(StatusesEventSinkAdapter::new(event_sink)));

    let service = Arc::new(NotificationService::new(
        pool,
        runtime,
        domain,
        accounts,
        media_store,
    ));

    NotificationModule { service, ports }
}
