//! `CustomEmojiService` (design.md "Service / サービス層" -> "AccountService /
//! InstanceService / CustomEmojiService"; Requirement 9.1; task 5.6,
//! `Boundary: CustomEmojiService`): lists visible custom emojis for
//! `GET /api/v1/custom_emojis`.
//!
//! Scope: this module owns exactly design.md's Service Interface signature
//! `pub async fn list_custom_emojis(&self) -> Result<serde_json::Value, AppError>`
//! for the `CustomEmojiService` component. It does not read `custom_emojis`
//! itself beyond calling
//! [`crate::accounts::emoji_repository::list_visible_emojis`] (task 2.3,
//! already implemented, unmodified by this task), does not define the
//! CustomEmoji JSON shape itself (task 3.4's
//! [`crate::accounts::custom_emoji_serializer::CustomEmojiSerializer`],
//! already implemented and unmodified by this task, owns that mapping), and
//! mounts no HTTP endpoint (`AccountsEndpoints`, task 6, out of this task's
//! boundary).
//!
//! ## Collaborators, held once at construction — not rebuilt per call
//! Mirrors `InstanceService`'s own "bundle, don't build twice" constructor
//! convention (`src/accounts/instance_service.rs::InstanceService::new`):
//! [`CustomEmojiService`] holds a `PgPool` clone (cheap — a connection-pool
//! handle, not a new connection) and a [`CustomEmojiSerializer`]. Unlike
//! `InstanceService`, this is the simplest of the three sibling services —
//! no `MediaConfig`/`ServerCapabilities`/`RuntimeContext` are needed, since
//! [`crate::accounts::model::CustomEmojiView`] is already a single flat,
//! fully-resolved value with no field requiring an injected default at
//! render time (see `custom_emoji_serializer.rs`'s own doc comment, "No
//! local/remote branching, no injected config").
//!
//! ## Feature Flag Protocol: not applicable
//! Brand-new internal component with no existing callers or previously
//! observable behavior to gate (mirrors `AccountService`'s/
//! `InstanceService`'s own identical doc comment). A standard
//! RED -> GREEN -> REFACTOR cycle against a real Postgres instance (via
//! `spawn_test_app`) is this crate's established verification method for
//! this kind of module.

#[cfg(test)]
mod tests;

use sqlx::postgres::PgPool;

use crate::accounts::custom_emoji_serializer::CustomEmojiSerializer;
use crate::accounts::emoji_repository::list_visible_emojis;
use crate::error::AppError;

/// Lists visible custom emojis (Requirement 9.1). See this module's doc
/// comment for the full collaborator/construction rationale.
pub struct CustomEmojiService {
    pool: PgPool,
    serializer: CustomEmojiSerializer,
}

impl CustomEmojiService {
    /// Builds a service from already-constructed collaborators — this
    /// constructor only bundles them, mirroring `AccountService::new`'s /
    /// `InstanceService::new`'s identical "bundle, don't build" convention.
    pub fn new(pool: PgPool, serializer: CustomEmojiSerializer) -> Self {
        CustomEmojiService { pool, serializer }
    }

    /// Lists every `visible_in_picker = TRUE` custom emoji (task 2.3's
    /// repository) as a CustomEmoji JSON array (task 3.4's serializer),
    /// satisfying design.md's literal Service Interface signature
    /// (Requirement 9.1). An empty repository result maps to an empty JSON
    /// array, never an error.
    pub async fn list_custom_emojis(&self) -> Result<serde_json::Value, AppError> {
        let emojis = list_visible_emojis(&self.pool).await?;
        let json: Vec<serde_json::Value> = emojis
            .iter()
            .map(|emoji| self.serializer.build_custom_emoji(emoji))
            .collect();
        Ok(serde_json::Value::Array(json))
    }
}
