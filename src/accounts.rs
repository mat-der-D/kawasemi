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
//!   No delegation ports (`AccountStatusesProvider` /
//!   `RelationshipStateProvider` / `AccountCountsProvider`, task 1.3), no
//!   `AccountsModule` wiring (task 1.4), no repositories (task 2.x), no
//!   serializers (task 3.x), no services, and no HTTP surface exist yet —
//!   this module is not wired into `crate::state::AppState`/
//!   `crate::bootstrap`/`crate::server` (that starts at task 1.4). See
//!   design.md's "File Structure Plan" for the full planned module set.

pub mod model;

pub use model::{
    AccountCounts, AccountProfile, AccountView, AccountViewFields, Acct, CredentialSource,
    CustomEmojiView, InstanceSettings, ProfileField, ProfilePatch, RelationshipView, RemoteAccount,
};
