//! `InstanceService` (design.md "Service / サービス層" -> "AccountService /
//! InstanceService / CustomEmojiService"; Requirements 8.1, 8.2; task 5.5,
//! `Boundary: InstanceService`): loads the operational `instance_settings`
//! singleton (task 2.4's `InstanceSettingsRepository::load_instance_settings`)
//! and combines it with this server's own real upload constraints (task
//! 3.3's `InstanceSerializer::build_instance_v2` + `ServerCapabilities`) into
//! the Mastodon-compatible Instance(v2) JSON.
//!
//! Scope: this module owns exactly design.md's Service Interface signature
//! `pub async fn instance_v2(&self) -> Result<serde_json::Value, AppError>`
//! for the `InstanceService` component. It does not read `instance_settings`
//! itself beyond calling
//! [`crate::accounts::settings_repository::load_instance_settings`] (task
//! 2.4, already implemented, unmodified by this task), does not define the
//! Instance(v2) JSON shape itself (task 3.3's
//! [`crate::accounts::instance_serializer::InstanceSerializer`], already
//! implemented and unmodified by this task, owns that mapping), and mounts
//! no HTTP endpoint (`AccountsEndpoints`, task 6, out of this task's
//! boundary).
//!
//! ## Collaborators, held once at construction — not rebuilt per call
//! Mirrors `AccountService`'s own "bundle, don't build twice" constructor
//! convention (`src/accounts/account_service.rs::AccountService::new`):
//! [`InstanceService`] holds a `PgPool` clone (cheap — a connection-pool
//! handle, not a new connection), an [`InstanceSerializer`] (holds only this
//! instance's own `domain` string), and a [`ServerCapabilities`] snapshot
//! already derived from this server's `MediaConfig` at construction time via
//! [`ServerCapabilities::from_media_config`] — task 3.3's own documented
//! "real usage" constructor (`instance_serializer.rs`'s own doc comment:
//! "task 5.5's `InstanceService` will call this against the live
//! `AppState`'s `MediaConfig`"). Because `MediaConfig`'s upload constraints
//! (`supported_formats`/`max_upload_size_bytes`) are read once at process
//! startup and never change afterward, building `ServerCapabilities` once at
//! construction — rather than re-reading a live `MediaConfig` on every
//! [`InstanceService::instance_v2`] call — never observes a stale value:
//! there is no "live" `MediaConfig` this server ever mutates after startup
//! for that value to go stale against in the first place.
//!
//! [`crate::accounts::build_accounts_module`] (`src/accounts.rs`) gains a new
//! `media_config: MediaConfig` parameter (Requirement 8.4) to build this
//! snapshot from — every call site (`bootstrap.rs`/`test_harness.rs`/
//! `federation/test_harness.rs`/`server/tests.rs`/`state/tests.rs`) already
//! has its own `config.media` value in scope (the same `AppConfig.media`
//! `AccountService`'s own `media: Arc<MediaService<LocalFsStore>>` parameter
//! is ultimately derived from), so this is a clone of an already-validated
//! value, not a second, independently-parsed `MediaConfig`.
//!
//! ## Feature Flag Protocol: not applicable
//! Brand-new internal component with no existing callers or previously
//! observable behavior to gate (mirrors `AccountService`'s own identical doc
//! comment). A standard RED -> GREEN -> REFACTOR cycle against a real
//! Postgres instance (via `spawn_test_app`) is this crate's established
//! verification method for this kind of module.

#[cfg(test)]
mod tests;

use sqlx::postgres::PgPool;

use crate::accounts::instance_serializer::{InstanceSerializer, ServerCapabilities};
use crate::accounts::settings_repository::load_instance_settings;
use crate::error::AppError;

/// Loads operational settings + real server constraints and synthesizes the
/// Instance(v2) JSON contract (Requirements 8.1, 8.2). See this module's doc
/// comment for the full collaborator/construction rationale.
pub struct InstanceService {
    pool: PgPool,
    serializer: InstanceSerializer,
    caps: ServerCapabilities,
}

impl InstanceService {
    /// Builds a service from already-constructed collaborators — this
    /// constructor only bundles them, mirroring `AccountService::new`'s
    /// identical "bundle, don't build" convention.
    pub fn new(pool: PgPool, serializer: InstanceSerializer, caps: ServerCapabilities) -> Self {
        InstanceService {
            pool,
            serializer,
            caps,
        }
    }

    /// Loads the operational `instance_settings` singleton (task 2.4's
    /// repository — safely all-defaulted even with zero rows, Requirement
    /// 8.3) and combines it with this instance's own `caps`/`domain` into the
    /// Instance(v2) JSON contract (design.md's literal Service Interface
    /// signature; Requirements 8.1, 8.2, 8.4).
    pub async fn instance_v2(&self) -> Result<serde_json::Value, AppError> {
        let settings = load_instance_settings(&self.pool).await?;
        Ok(self.serializer.build_instance_v2(&settings, &self.caps))
    }
}
