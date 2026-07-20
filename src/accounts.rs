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

pub mod model;
pub mod ports;

pub use model::{
    AccountCounts, AccountProfile, AccountView, AccountViewFields, Acct, CredentialSource,
    CustomEmojiView, InstanceSettings, ProfileField, ProfilePatch, RelationshipView, RemoteAccount,
};
pub use ports::{
    AccountCountsProvider, AccountPortsRegistry, AccountStatusesProvider, EmptyStatusesProvider,
    NoRelationshipProvider, RelationshipStateProvider, StatusesQuery, ZeroCountsProvider,
};

/// The Composition Root's accounts-and-instance module bundle (design.md's
/// "Runtime / 配線層" -> `AccountsModule（wiring）`, Requirements 10.1, 10.5;
/// task 1.4, `Boundary: AccountsModule`). Held by `AppState` (task 1.4's
/// `src/state.rs` change) the same way `crate::media::MediaModule`/
/// `crate::federation::FederationModule`/`crate::oauth::OauthModule` already
/// are.
///
/// At this wiring-only stage the bundle holds exactly task 1.3's
/// [`AccountPortsRegistry`] — the one piece design.md's `AccountsModule`
/// component explicitly names as this task's own responsibility ("委譲 port
/// を既定実装で初期化して...レジストリに格納"). Repositories/serializers/
/// services (design.md's other `AccountsModule`-adjacent components) do not
/// exist yet (tasks 2.x-5.x); when they land, they extend this struct the
/// same incremental way `MediaModule` grew from `store`+`service` alone.
///
/// This type does not construct its own dependencies — [`build_accounts_module`]
/// does that (mirrors `MediaModule`'s/`FederationModule`'s identical
/// "bundle, don't build" contract).
#[derive(Clone)]
pub struct AccountsModule {
    ports: AccountPortsRegistry,
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
}

/// Assembles the accounts-and-instance module bundle with every delegation
/// port defaulted to its built-in safe default (task 1.4, Requirements 10.1,
/// 10.5): [`AccountPortsRegistry::new`] — no DB pool, runtime context, or
/// config is threaded through here (unlike `crate::media::build_media_module`),
/// because nothing this task constructs touches the database, the clock, or
/// startup configuration yet (no repositories/services exist at this
/// wiring-only stage — see [`AccountsModule`]'s own doc comment for what a
/// later task will extend this constructor to accept once it does). Shared
/// by `src/bootstrap.rs` (production) and `src/test_harness.rs`
/// (`spawn_test_app`), mirroring `crate::media::build_media_module`'s "one
/// composition function, two callers" precedent.
pub fn build_accounts_module() -> AccountsModule {
    AccountsModule {
        ports: AccountPortsRegistry::new(),
    }
}
