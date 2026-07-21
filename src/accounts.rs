//! Accounts domain module (accounts-and-instance spec, `src/accounts.rs` +
//! `src/accounts/`, mirroring the module-with-submodule convention
//! established by `src/media.rs`/`src/media/`, `src/federation.rs`/
//! `src/federation/`, and `src/oauth.rs`/`src/oauth/`).
//!
//! Scope so far:
//! - Task 1.1 (`Boundary: migration`, no Rust code): `migrations/
//!   0006_accounts.sql` — `account_profiles` / `remote_accounts` /
//!   `custom_emojis` / `instance_settings`.
//! - Task 1.2 (`Boundary: model`): the domain value types design.md's
//!   model component names — [`model::AccountView`],
//!   [`model::ProfileField`], [`model::CredentialSource`],
//!   [`model::AccountProfile`], [`model::ProfilePatch`],
//!   [`model::RemoteAccount`], [`model::CustomEmojiView`],
//!   [`model::RelationshipView`], [`model::AccountCounts`], and
//!   [`model::InstanceSettings`] — plus [`model::Acct`], a small helper
//!   type carrying the local/remote `acct` string-rendering discipline (see
//!   `model.rs`'s own doc comment, "Why `Acct` exists"). `AccountRef`/
//!   `Visibility` are not redefined here — both are imported from
//!   `crate::domain` (core-runtime's canonical shared primitives module)
//!   — see [`model`].
//!
//! - Task 1.3 (`Boundary: ports`): the downstream-owned-information
//!   delegation boundary — [`ports::AccountStatusesProvider`] /
//!   [`ports::RelationshipStateProvider`] / [`ports::AccountCountsProvider`],
//!   their built-in default implementations ([`ports::EmptyStatusesProvider`]
//!   / [`ports::NoRelationshipProvider`] / [`ports::ZeroCountsProvider`]),
//!   and the swap-in delegation registry ([`ports::AccountPortsRegistry`])
//!   — see [`ports`].
//!
//! - Task 1.4 (`Boundary: AccountsModule`): the Composition Root wiring
//!   skeleton — [`AccountsModule`], the module-bundle wrapper `AppState`
//!   (task 1.4's own `src/state.rs` change) now holds one of, built once by
//!   [`build_accounts_module`] and never mutated afterward (mirrors every
//!   other module bundle's own "bundle, don't build twice" contract — see
//!   `crate::media::MediaModule`'s doc comment). At this stage the bundle
//!   holds exactly task 1.3's [`AccountPortsRegistry`], defaulted via
//!   [`AccountPortsRegistry::new`] (every slot the built-in default
//!   implementation) — no repositories (task 2.x), no serializers (task
//!   3.x), no services (task 4.x/5.x), and no real handler logic exist yet,
//!   per this task's own boundary ("ハンドラは後続で実装"). `src/server.rs`
//!   mounts the accounts/instance/custom_emojis routes onto explicit `501
//!   Not Implemented` placeholder handlers (mirroring
//!   `crate::media::media_router`'s own "separate `.merge()`-able group"
//!   precedent) — no per-request state beyond what `AppState` already
//!   carries is needed for a placeholder handler, so no
//!   `impl FromRef<AppState> for ...` bridge exists for this task (unlike
//!   `MediaEndpointsState<LocalFsStore>`'s bridge, which real media handlers
//!   need for their own service/store state) — see `src/server.rs`'s
//!   `accounts_router` doc comment for the full reasoning. This module is
//!   not wired into `crate::server` beyond that placeholder mount point —
//!   real HTTP surface is task group 6's own boundary
//!   (`_Boundary: AccountsEndpoints, AccountsModule_`).
//!
//! - Tasks 2.1-2.4 (`Boundary: AccountProfileRepository` /
//!   `RemoteAccountRepository` / `CustomEmojiRepository` /
//!   `InstanceSettingsRepository`): the data layer — see
//!   [`profile_repository`], [`remote_repository`], [`emoji_repository`],
//!   [`settings_repository`].
//!
//! - Task 3.1 (`Boundary: AccountSerializer`): maps a local actor
//!   (`ResolvedActor` + [`AccountProfile`]) or a [`RemoteAccount`] onto the
//!   unified Account/CredentialAccount JSON contract — see [`serializer`].
//!
//! - Task 3.2 (`Boundary: RelationshipSerializer`): maps a
//!   [`model::RelationshipView`] onto the `relationships` JSON contract —
//!   see [`relationship_serializer`].
//!
//! - Task 3.3 (`Boundary: InstanceSerializer`): synthesizes the Instance(v2)
//!   JSON contract from [`model::InstanceSettings`] and a real
//!   media-pipeline-derived [`instance_serializer::ServerCapabilities`]
//!   snapshot — see [`instance_serializer`].
//!
//! - Task 3.4 (`Boundary: CustomEmojiSerializer`): maps a
//!   [`model::CustomEmojiView`] onto the CustomEmoji JSON contract, reusing
//!   [`serializer::CustomEmojiJson`] (task 3.1's already-`pub` type) so the
//!   representation is shared, not re-derived, with Account's `emojis`
//!   entries (Requirement 9.4) — see [`custom_emoji_serializer`].
//!
//! - Task 4 (`Boundary: RemoteAccountFetcher`): fetches an ActivityPub actor
//!   document for a not-yet-cached or stale `actor_uri` via
//!   `FederationHttpClient`, safely normalizes it (unknown extension
//!   properties never fail normalization; missing required properties do)
//!   into a [`model::RemoteAccount`], and upserts it through
//!   [`remote_repository`]'s already-implemented cache — reusing a fresh
//!   cache entry without any network call — see [`remote_fetcher`].
//!
//! - Task 5.1 (`Boundary: AccountService`): the first two operations of the
//!   eventual `AccountService` business layer —
//!   [`account_service::AccountService::verify_credentials`] (Bearer-token-
//!   bound actor -> CredentialAccount) and
//!   [`account_service::AccountService::show_account`] (local/known-remote/
//!   needs-fetching identifier -> Account, 404 for anything else) —
//!   orchestrating [`profile_repository`]/[`remote_repository`]/
//!   [`emoji_repository`]/[`serializer`]/[`remote_fetcher`]/[`ports`] plus
//!   actor-model's `ActorDirectory` (gaining one narrow, additive method,
//!   `ActorDirectory::actor_created_at`, this task's own resolution of the
//!   `created_at` gap `serializer.rs` flagged) — see [`account_service`].
//!   `list_statuses`/`relationships`/`update_credentials` (tasks 5.2/5.3/5.4)
//!   do not exist on this type yet. This task also extends
//!   [`build_accounts_module`]/[`AccountsModule`] to construct and hold the
//!   real, `LocalFsStore`/`ReqwestFederationHttpClient`-monomorphized
//!   `AccountService` — the first time this bundle holds anything beyond
//!   task 1.3's `AccountPortsRegistry`. No HTTP surface (`AccountsEndpoints`,
//!   task 6) mounts these operations yet.
//!
//! - Task 5.4 (`Boundary: AccountService`, continued): adds
//!   [`account_service::AccountService::update_credentials`] (validate ->
//!   ingest avatar/header via media-pipeline's `MediaService` -> partial
//!   `account_profiles` upsert -> updated CredentialAccount) — see
//!   [`account_service`]'s own doc comment. [`build_accounts_module`] gains a
//!   new `media: Arc<MediaService<LocalFsStore>>` parameter, threaded from
//!   the caller's own already-built `MediaModule::service()` handle (every
//!   call site — `bootstrap.rs`/`test_harness.rs`/
//!   `federation/test_harness.rs`/`server/tests.rs`/`state/tests.rs` — builds
//!   its `MediaModule` before its `AccountsModule` already, so this is a
//!   reused `Arc` clone, never a second, independently-configured
//!   `MediaService`/`MediaConfig`).
//!
//! - Task 5.5 (`Boundary: InstanceService`): the `InstanceService`
//!   business-layer sibling to `AccountService` —
//!   [`instance_service::InstanceService::instance_v2`] loads the
//!   operational `instance_settings` singleton (task 2.4's
//!   `InstanceSettingsRepository::load_instance_settings`) and combines it
//!   with a [`instance_serializer::ServerCapabilities`] snapshot (derived
//!   once, at construction, from this instance's own `MediaConfig`) via task
//!   3.3's already-implemented `InstanceSerializer::build_instance_v2` — see
//!   [`instance_service`]. [`build_accounts_module`] gains a new
//!   `media_config: MediaConfig` parameter (every call site already has its
//!   own `config.media` value in scope, the same value `MediaService` is
//!   itself built from) to build that snapshot from, and now also
//!   constructs and holds the real `InstanceService` alongside
//!   `AccountService`.

use std::sync::Arc;

use sqlx::postgres::PgPool;

use crate::actor::ActorDirectory;
use crate::config::MediaConfig;
use crate::federation::signatures::ReqwestFederationHttpClient;
use crate::media::local_fs::LocalFsStore;
use crate::media::service::MediaService;
use crate::runtime::RuntimeContext;

pub mod account_service;
pub mod custom_emoji_serializer;
pub mod emoji_repository;
pub mod instance_serializer;
pub mod instance_service;
pub mod model;
pub mod ports;
pub mod profile_repository;
pub mod relationship_serializer;
pub mod remote_fetcher;
pub mod remote_repository;
pub mod serializer;
pub mod settings_repository;

pub use account_service::{
    AccountService, MediaUploadInput, ProfileFieldInput, StatusesQueryInput, UpdateCredentialsInput,
};
pub use custom_emoji_serializer::{
    CustomEmojiSerializer, custom_emoji_to_json, to_custom_emoji_json,
};
pub use instance_serializer::{
    ConfigurationJson, ContactJson, InstanceJson, InstanceSerializer, MediaAttachmentsConfigJson,
    RegistrationsJson, RuleJson, ServerCapabilities, UsageJson, UsageUsersJson, instance_to_json,
    to_instance_json,
};
pub use instance_service::InstanceService;
pub use model::{
    AccountCounts, AccountProfile, AccountView, AccountViewFields, Acct, CredentialSource,
    CustomEmojiView, InstanceSettings, ProfileField, ProfilePatch, RelationshipView, RemoteAccount,
};
pub use ports::{
    AccountCountsProvider, AccountPortsRegistry, AccountStatusesProvider, EmptyStatusesProvider,
    NoRelationshipProvider, RelationshipStateProvider, StatusesQuery, ZeroCountsProvider,
};
pub use relationship_serializer::{
    RelationshipJson, RelationshipSerializer, relationship_to_json, to_relationship_json,
};
pub use remote_fetcher::{DEFAULT_REMOTE_ACCOUNT_CACHE_TTL, RemoteAccountFetcher};
pub use serializer::{
    AccountFieldJson, AccountJson, AccountSerializer, CredentialAccountJson, CredentialSourceJson,
    CustomEmojiJson, RoleJson, account_to_json, to_account_json,
};

/// The Composition Root's accounts-and-instance module bundle (design.md's
/// "Runtime / 配線層" -> `AccountsModule（wiring）`, Requirements 10.1, 10.5;
/// task 1.4, `Boundary: AccountsModule`). Held by `AppState` (task 1.4's
/// `src/state.rs` change) the same way `crate::media::MediaModule`/
/// `crate::federation::FederationModule`/`crate::oauth::OauthModule` already
/// are.
///
/// At this wiring-only stage the bundle holds task 1.3's
/// [`AccountPortsRegistry`] — the one piece design.md's `AccountsModule`
/// component explicitly names as task 1.4's own responsibility ("委譲 port
/// を既定実装で初期化して...レジストリに格納") — plus, as of task 5.1, the
/// real, `LocalFsStore`/`ReqwestFederationHttpClient`-monomorphized
/// [`account_service::AccountService`] (design.md's `AccountsModule`
/// Responsibilities: "各リポジトリ/サービス/シリアライザを構築し..."). This
/// mirrors `MediaModule`'s own incremental growth from `store` alone (task
/// 2.2) to `store`+`service` (task 4.1) as later tasks landed real
/// components on top of the initial wiring skeleton.
///
/// This type does not construct its own dependencies — [`build_accounts_module`]
/// does that (mirrors `MediaModule`'s/`FederationModule`'s identical
/// "bundle, don't build" contract).
#[derive(Clone)]
pub struct AccountsModule {
    ports: AccountPortsRegistry,
    service: Arc<AccountService<LocalFsStore, ReqwestFederationHttpClient>>,
    instance: Arc<InstanceService>,
}

impl AccountsModule {
    /// The shared delegation-ports registry (task 1.3): downstream specs
    /// (statuses-core/social-graph) retrieve this same handle through
    /// `AppState::accounts().ports()` to call `set_statuses_provider`/
    /// `set_relationship_provider`/`set_counts_provider` on the *live*
    /// registry already inside a running `AppState`, exactly the swap-in
    /// contract [`AccountPortsRegistry`]'s own doc comment describes
    /// ("Registry shape": `&self`, not `&mut self`, so this works even
    /// though `AppState` itself is immutable-after-construction).
    /// `AccountPortsRegistry` is cheap to clone (three `Arc`s), so this
    /// returns an owned clone rather than a reference — callers that swap in
    /// a provider are calling `set_*` on the exact same interior `RwLock`
    /// slots regardless of which cloned handle they hold.
    pub fn ports(&self) -> AccountPortsRegistry {
        self.ports.clone()
    }

    /// The shared `AccountService` handle (task 5.1): a future
    /// `AccountsEndpoints` (task 6) derives its own endpoint state from this
    /// same handle, the same way `crate::media::MediaModule::service`'s
    /// callers do — an `Arc` clone (cheap, one atomic increment), not a
    /// freshly constructed service.
    pub fn service(&self) -> Arc<AccountService<LocalFsStore, ReqwestFederationHttpClient>> {
        Arc::clone(&self.service)
    }

    /// The shared `InstanceService` handle (task 5.5): a future
    /// `AccountsEndpoints` (task 6) derives its own `GET /api/v2/instance`
    /// endpoint state from this same handle, the same way
    /// [`AccountsModule::service`]'s callers do — an `Arc` clone (cheap, one
    /// atomic increment), not a freshly constructed service.
    pub fn instance(&self) -> Arc<InstanceService> {
        Arc::clone(&self.instance)
    }
}

/// Assembles the accounts-and-instance module bundle (task 5.1, Requirements
/// 2.1, 3.1, 3.2, 3.3, 10.1, 10.5): defaults task 1.3's [`AccountPortsRegistry`]
/// to its built-in safe implementations, and builds the real
/// [`account_service::AccountService`] — monomorphized over this crate's one
/// concrete production `MediaStore`/`FederationHttpClient` pair
/// (`LocalFsStore`/`ReqwestFederationHttpClient`, mirroring
/// `crate::media::build_media_module`'s/`crate::federation::build_federation_module`'s
/// identical "one concrete type per non-`dyn`-safe trait" convention) —
/// around `pool`/`runtime`/`domain`/`directory`/`http_client`/`store`/`media`,
/// every one of which the caller (`src/bootstrap.rs`'s production path,
/// `src/test_harness.rs`'s `spawn_test_app`) already constructs for its own
/// other module bundles (actor-model's `ActorDirectory`, federation-core's
/// `ReqwestFederationHttpClient`, media-pipeline's `LocalFsStore`/
/// `MediaService`) and simply shares here rather than this function
/// constructing a second, independent instance of any of them. `media`
/// (task 5.4, Requirement 6.2) is always the caller's own already-built
/// `MediaModule::service()` handle — an `Arc` clone, not a second
/// `MediaService`/`MediaConfig` this function parses/constructs itself. No
/// background task to spawn (unlike `media`/`federation`'s own
/// `build_*_module` — this bundle owns no resident worker).
///
/// `media_config` (task 5.5, Requirement 8.4) is the caller's own already-
/// validated `AppConfig.media` value (a clone, not a second independently
/// parsed `MediaConfig`) — used once, here, to derive the
/// [`instance_serializer::ServerCapabilities`] snapshot [`InstanceService`]
/// is constructed with (see [`instance_service`]'s own doc comment for why a
/// once-at-construction snapshot never goes stale against a `MediaConfig`
/// this server never mutates after startup).
///
/// `too_many_arguments` is suppressed for the same documented reason
/// `AppState::new`/`AccountService::new`'s own builders already suppress it
/// (`src/state.rs`'s own doc comment: "inherent to this constructor's role,
/// not a smell to refactor away") — one parameter per already-constructed
/// collaborator this Composition Root function assembles, growing as tasks
/// land, not something a params struct would actually remove the coupling
/// of.
#[allow(clippy::too_many_arguments)]
pub fn build_accounts_module(
    pool: PgPool,
    runtime: RuntimeContext,
    domain: impl Into<String>,
    directory: Arc<ActorDirectory>,
    http_client: Arc<ReqwestFederationHttpClient>,
    store: LocalFsStore,
    media: Arc<MediaService<LocalFsStore>>,
    media_config: MediaConfig,
) -> AccountsModule {
    let domain = domain.into();
    let ports = AccountPortsRegistry::new();
    let fetcher = RemoteAccountFetcher::new(
        pool.clone(),
        http_client,
        runtime.clone(),
        DEFAULT_REMOTE_ACCOUNT_CACHE_TTL,
    );
    let serializer = AccountSerializer::new(domain.clone());
    let service = Arc::new(AccountService::new(
        pool.clone(),
        directory,
        fetcher,
        serializer,
        ports.clone(),
        store,
        media,
        runtime,
    ));
    let instance_serializer = InstanceSerializer::new(domain);
    let caps = ServerCapabilities::from_media_config(&media_config);
    let instance = Arc::new(InstanceService::new(pool, instance_serializer, caps));
    AccountsModule {
        ports,
        service,
        instance,
    }
}
