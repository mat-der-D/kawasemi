//! `AccountService` (design.md "Service / サービス層" -> "AccountService /
//! InstanceService / CustomEmojiService"; Requirements 2.1, 3.1, 3.2, 3.3,
//! 4.1, 4.2, 4.4, 4.5; tasks 5.1/5.2/5.3, `Boundary: AccountService`):
//! resolves the single actor bound to a Bearer token into a CredentialAccount
//! ([`AccountService::verify_credentials`]), resolves an arbitrary
//! local/known-remote/needs-fetching identifier into an Account
//! ([`AccountService::show_account`]), (task 5.2) resolves an account's
//! Status page via the [`crate::accounts::ports::AccountStatusesProvider`]
//! delegation boundary ([`AccountService::list_statuses`]), and (task 5.3)
//! resolves a batch of target ids' relationship state via the
//! [`crate::accounts::ports::RelationshipStateProvider`] delegation
//! boundary, serialized with task 3.2's `RelationshipSerializer`
//! ([`AccountService::relationships`]).
//!
//! Scope: this module owns the operations tasks 5.1/5.2/5.3/5.4 name —
//! `verify_credentials`/`show_account`/`list_statuses`/`relationships`/
//! `update_credentials` — orchestrating already-implemented collaborators
//! (`ActorDirectory`, task 2.1-2.3's repositories, task 3.1's
//! `AccountSerializer`, task 3.2's `RelationshipSerializer`, task 4's
//! `RemoteAccountFetcher`, task 1.3's `AccountPortsRegistry`, and — new as of
//! task 5.4 — media-pipeline's `MediaService`).
//!
//! ## Task 5.4: `update_credentials` (Requirements 6.1, 6.2, 6.3, 6.5)
//! Follows design.md's "update_credentials（プロフィール更新）" sequence
//! diagram exactly: validate (field count/length limits, focus range) before
//! any side effect -> ingest avatar/header (if present) via `MediaService` ->
//! partial `AccountProfileRepository::upsert_profile` -> build and return the
//! updated CredentialAccount from the just-upserted profile (reusing
//! `AccountSerializer::build_credential_account`, the same builder
//! `verify_credentials` uses, rather than duplicating its field-mapping
//! logic). See [`UpdateCredentialsInput`] for the input shape judgment call
//! and [`validate_update_credentials`] for the exact limits chosen (both
//! documented there, and flagged in this task's own status report CONCERNS —
//! neither requirements.md's Requirement 6.3 excerpt nor design.md gives
//! concrete numbers for "フィールド数上限・各値の長さ上限").
//!
//! This task adds two new constructor fields to `AccountService` —
//! `media: Arc<MediaService<S>>` (the same `S: MediaStore` this service
//! already carries as `store`, so no new generic parameter is needed) and
//! `runtime: RuntimeContext` (needed for `upsert_profile`'s `now` parameter,
//! Requirement 6.5's "時刻は `RuntimeContext`" — this service had no
//! occasion to read the clock before this task) — both already named as
//! `AccountService`'s own planned dependencies in design.md's Components
//! table ("Repos, Fetcher, Serializers, Ports, ActorDirectory,
//! RuntimeContext"), just not wired in until this task actually needed them.
//! `AccountsModule`/`build_accounts_module` (`src/accounts.rs`) is extended
//! to thread a `media: Arc<MediaService<LocalFsStore>>` parameter through,
//! reusing `media_module.service()` at every call site
//! (`bootstrap.rs`/`test_harness.rs`/`federation/test_harness.rs`/
//! `server/tests.rs`/`state/tests.rs`) rather than constructing a second,
//! independent `MediaService`/`MediaConfig` — the exact "share, don't
//! duplicate" convention task 5.1's own doc comment already established for
//! `media_module.store()`.
//!
//! ## `AccountService<S: MediaStore, H: FederationHttpClient>`, not `Arc<dyn ..>`
//! Mirrors `crate::media::service::MediaService<S: MediaStore>`'s and
//! `crate::accounts::remote_fetcher::RemoteAccountFetcher<H: FederationHttpClient>`'s
//! identical rationale: neither `MediaStore` nor `FederationHttpClient` is
//! `dyn`-object-safe in this crate (both are `#[allow(async_fn_in_trait)]`-
//! shaped), so this service takes both as generic type parameters rather
//! than trait objects. `AccountsModule` (this file's own Composition Root
//! wiring) monomorphizes over this crate's one concrete production pair —
//! `LocalFsStore`/`ReqwestFederationHttpClient` — the same "one concrete
//! type per non-`dyn`-safe trait" convention `crate::media::MediaModule`/
//! `crate::federation::module::FederationModule` already established.
//!
//! ## Deliberate deviations from design.md's literal Service Interface
//! design.md's Service Interface sketch is:
//! ```text
//! pub async fn verify_credentials(&self, ctx: &RequestActorContext) -> Result<serde_json::Value, AppError>;
//! pub async fn show_account(&self, id: &str, viewer: Option<&RequestActorContext>) -> Result<serde_json::Value, AppError>;
//! ```
//! This module's actual signatures add one parameter to each, for the exact
//! same reason `AccountSerializer`'s own doc comment already documents for
//! its `build_account_local`/`build_credential_account` methods (`store: &impl
//! MediaStore, origin: &ForwardedOrigin`): this service is the one caller of
//! those methods, so whatever gap those methods have, this service inherits.
//! `store` does not need to be threaded through per call (this service holds
//! its own `S` by value, injected once at construction, mirroring
//! `MediaService<S>`'s identical "generic store held by value" shape) —
//! only `origin: &ForwardedOrigin` is a genuinely per-request value (resolved
//! from a request's forwarded-proxy headers) neither this service nor
//! `AccountsModule` can own ahead of time, so it is the one added parameter
//! on both methods. `show_account`'s `viewer: Option<&RequestActorContext>`
//! parameter is kept (as `_viewer`, currently unused) to match design.md's
//! literal signature exactly — Requirement 3's Account contract does not
//! vary by viewer in this MVP (Requirement 3.4's "任意認証" only means the
//! *token* is optional, not that the response shape changes), but a caller
//! (`AccountsEndpoints`, task 6) can already pass one through without this
//! service's own signature needing to change again once a future
//! requirement does need it.
//!
//! ## `created_at` (Requirement 1.1): closes the gap `serializer.rs` flagged
//! `AccountSerializer::view_local`/`build_account_local`/
//! `build_credential_account` all take an explicit `created_at:
//! OffsetDateTime` parameter because neither `ResolvedActor` nor
//! `AccountProfile` carries one (`serializer.rs`'s own doc comment,
//! "Deliberate deviations...", flags this exact gap and names its own likely
//! resolution: "a small, additive `ResolvedActor`/`ActorDirectory`
//! revalidation in actor-model"). This task closes that gap with
//! [`crate::actor::directory::ActorDirectory::actor_created_at`], a narrow,
//! additive method on `ActorDirectory` mirroring the *exact* precedent that
//! component's own doc comment already documents twice over
//! (`resolve_actor_by_id`, added by federation-core's task 4.3;
//! `sole_owner`, added by api-foundation's task 4.1 — both "narrow upstream
//! addition[s]" by a downstream spec's own task, not `ActorDirectory`'s
//! original task 5.2 scope). This is the third such addition, by this exact
//! spec's task 5.1, following the same shape: delegates to the
//! already-implemented `repository::find_by_id`, returns `Ok(None)` (not an
//! error) for absence, and is documented as this task's own narrow
//! resolution of a gap flagged by a sibling task rather than a silent guess
//! (e.g. `OffsetDateTime::now_utc()`, which `serializer.rs`'s own doc comment
//! already rules out as breaking this task's own "同一入力で決定的 JSON"
//! determinism requirement).
//!
//! ## Emoji candidates: `list_visible_emojis`, not a shortcode-targeted `resolve_emojis`
//! `AccountSerializer::view_local`/`view_remote` take an `emojis: &[CustomEmojiView]`
//! *candidate* slice and internally match referenced `:shortcode:` tokens in
//! `display_name`/`note` against it (`match_referenced_emojis`, private to
//! `serializer.rs`) — this service never needs to pre-extract which
//! shortcodes are actually referenced itself, only supply a candidate pool
//! that is a superset of whatever the profile/remote text references.
//! [`emoji_candidates`] supplies that pool via
//! [`crate::accounts::emoji_repository::list_visible_emojis`] (every
//! `visible_in_picker = TRUE` row, regardless of domain) rather than
//! `resolve_emojis(pool, shortcodes)` — extracting the referenced-shortcode
//! list itself is `serializer.rs`'s own private `extract_shortcodes`/
//! `shortcodes_in` helpers, neither `pub`, and this task must not edit
//! `serializer.rs` (a different task's, 3.1's, already-reviewed boundary) to
//! expose them. This is a documented, narrower-than-ideal trade-off (see
//! this task's own status report CONCERNS): an emoji shortcode referenced in
//! a profile's text but explicitly hidden from the picker
//! (`visible_in_picker = FALSE`) will not be found among these candidates
//! and so will be silently omitted from that account's `emojis` array,
//! whereas `resolve_emojis` would have found it (its own doc comment: "shortcode
//! resolvability is independent of picker visibility"). Every
//! picker-visible referenced emoji — the overwhelming common case — is
//! still resolved correctly; `match_referenced_emojis` only ever emits
//! entries actually referenced by the account's own text, so over-supplying
//! candidates here never leaks an unreferenced emoji into the output.
//!
//! ## Local/remote/needs-fetching identifier discipline (Requirements 3.1,
//! 3.2, 3.3; task text: "ローカル（`ActorDirectory`）/既知リモート/必要時
//! フェッチで解決")
//! design.md's own "accounts/:id 取得" flowchart draws exactly three
//! `Kind{id resolves to}` branches — local actor / known remote / unknown
//! (404) — with no fourth "fetch a brand-new remote" branch drawn
//! explicitly. Reconciling that with this task's own text (which does name
//! "必要時フェッチ", fetch-as-needed) and this task's explicit `Depends: 3.1,
//! 4` (naming `RemoteAccountFetcher`, task 4, as a real dependency — not
//! merely a transitively-satisfied one), [`AccountService::show_account`]
//! reads `id` as one of two shapes:
//! - A bare non-negative integer string (`id.parse::<i64>()` succeeds): an
//!   *internal* database id, minted from this instance's single shared
//!   `RuntimeContext::ids` generator for **both** `local_actors.id` and
//!   `remote_accounts.id` (see `RemoteAccountFetcher::fetch_and_upsert`'s own
//!   `self.runtime.ids.next_id()` call) — the same generator instance
//!   `ActorService::create_actor` mints local actor ids from, so a given
//!   numeric value is never independently reused across the two tables.
//!   This id is tried against `ActorDirectory::resolve_actor_by_id` first
//!   (Requirement 3.1's "ローカルアクターを指すとき"), then
//!   `RemoteAccountRepository::find_remote_by_id` (Requirement 3.2's "既知の
//!   リモートアカウントを指すとき" — a cache hit only; a numeric id alone
//!   carries no `actor_uri` to fetch a miss with). Neither matching is an
//!   error to fail through; the *absence* of a match in both is Requirement
//!   3.3's 404.
//! - Anything else (does not parse as a bare integer): treated as a remote
//!   `actor_uri` reference (e.g. `https://remote.example/users/alice`) and
//!   handed directly to `RemoteAccountFetcher::fetch_and_normalize` — which
//!   is itself already cache-first (Requirement 7.3: reuses a valid cached
//!   `remote_accounts` row keyed by that same `actor_uri` without a network
//!   call) and only fetches over the network on an actual cache miss/stale
//!   entry (Requirement 7.1) — closing the "必要時フェッチ" (fetch-as-needed)
//!   half of this task's text. `RemoteAccountFetcher::fetch_and_normalize`'s
//!   own doc comment already establishes that every one of its failure
//!   paths (transport failure, non-success upstream status, missing
//!   required property) maps to a caller-facing `404 Not Found`
//!   [`AppError`] specifically *for* this future caller (its own doc
//!   comment names "the future `AccountService::show_account`, task 5.1"
//!   verbatim) — so this service does not need a second not-found
//!   translation step here; Requirement 3.3 is already satisfied by
//!   propagating that `AppError` as-is via `?`.
//!
//! ## Feature Flag Protocol: not applicable
//! Brand-new internal component with no existing callers or previously
//! observable behavior to gate (mirrors every other freshly-added service in
//! this crate, e.g. `crate::actor::service::ActorService`'s own doc
//! comment). A standard RED -> GREEN -> REFACTOR cycle against a real
//! Postgres instance (via `spawn_test_app`) is this crate's established
//! verification method for this kind of module.

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::Value;
use sqlx::postgres::PgPool;
use time::OffsetDateTime;

use crate::accounts::emoji_repository::list_visible_emojis;
use crate::accounts::model::{AccountProfile, CustomEmojiView, ProfileField, ProfilePatch};
use crate::accounts::ports::{AccountPortsRegistry, StatusesQuery};
use crate::accounts::profile_repository::{find_profile, upsert_profile};
use crate::accounts::relationship_serializer::RelationshipSerializer;
use crate::accounts::remote_fetcher::RemoteAccountFetcher;
use crate::accounts::remote_repository::find_remote_by_id;
use crate::accounts::serializer::AccountSerializer;
use crate::actor::directory::ActorDirectory;
use crate::actor::model::ResolvedActor;
use crate::api::pagination::{ForwardedOrigin, Page, PageParams};
use crate::domain::{AccountRef, Id, Visibility};
use crate::error::AppError;
use crate::federation::signatures::FederationHttpClient;
use crate::media::service::{MediaService, UploadInput};
use crate::media::store::MediaStore;
use crate::media::{Focus, FocusRangeError};
use crate::oauth::model::RequestActorContext;
use crate::runtime::RuntimeContext;

/// The wire-level pagination/filter parameters `list_statuses` (task 5.2)
/// accepts, before an account has been resolved to an [`AccountRef`] or a
/// viewer to an `Option<Id>` — design.md's Service Interface names this
/// `StatusesQueryInput` in `list_statuses`'s own signature but does not
/// define its fields (unlike [`StatusesQuery`], whose fields design.md's
/// ports component spells out verbatim). This type supplies exactly the
/// fields `list_statuses` still needs from a caller once `target`/`viewer`
/// are excluded — `page` is carried through to [`StatusesQuery::page`]
/// **unparsed** (`PageParams`, not a decoded `ParsedPageParams<C>`):
/// `AccountService` has no way to know which concrete `Cursor` type
/// statuses-core's eventual real provider will page by, so decoding is left
/// to whichever provider is registered, exactly the same "recipient decodes"
/// discipline [`StatusesQuery::page`]'s own field type (`PageParams`, not a
/// generic `ParsedPageParams<C>`) already establishes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatusesQueryInput {
    pub page: PageParams,
    pub pinned: bool,
    pub only_media: bool,
    pub exclude_replies: bool,
    pub exclude_reblogs: bool,
}

/// One `fields_attributes` entry of an `update_credentials` request
/// (Requirement 6.1). Unlike [`ProfileField`] (this same shape, but
/// persisted), this input carries no `verified_at` — a caller cannot set a
/// field's verification timestamp directly through `update_credentials`
/// (verifying a field's link is a separate, not-yet-in-scope concern); every
/// field built from this input starts unverified (`verified_at: None`), the
/// same way a freshly added field always would.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileFieldInput {
    pub name: String,
    pub value: String,
}

/// A raw avatar/header upload embedded in an `update_credentials` request
/// (Requirement 6.2), mirroring [`UploadInput`]'s exact shape minus
/// `description` (Requirement 6.1's enumerated update_credentials fields do
/// not include a per-image caption) — `focus` is a raw `(f32, f32)`
/// coordinate pair, not an already-validated [`Focus`], for the identical
/// reason `media::service::UploadInput`'s own doc comment gives: this
/// service, not the caller, owns converting it through the fallible
/// [`Focus::new`] and reporting a range violation as a 422 (Requirement 6.3).
#[derive(Debug, Clone, PartialEq)]
pub struct MediaUploadInput {
    pub bytes: Vec<u8>,
    pub content_type: String,
    pub focus: Option<(f32, f32)>,
}

/// `update_credentials`'s wire-level input (design.md's Service Interface
/// names `UpdateCredentialsInput` in `update_credentials`'s own signature but
/// does not define its fields — the same situation task 5.2's own doc
/// comment already documents for `StatusesQueryInput`). Every field is
/// `Option`; `None` means "not present in this request, leave unchanged"
/// (mirroring [`ProfilePatch`]'s own discipline, which this type is
/// converted into almost verbatim by [`AccountService::update_credentials`]).
///
/// Field selection follows Requirement 6.1's own enumerated list verbatim:
/// `display_name`/`note`/`locked`/`bot`/`discoverable`/`fields_attributes`/
/// source `privacy`/`sensitive`/`language`, plus `avatar`/`header` (raw
/// upload bytes, Requirement 6.2). `source_privacy` is typed as the
/// already-canonical [`Visibility`] enum directly (not a raw string) — per
/// this task's own instructions, when the caller-facing field is already
/// `Option<Visibility>`, Requirement 6.3's "公開範囲の許容値" validation is
/// already satisfied by the type system (an invalid discriminant simply
/// cannot be constructed), so string-to-enum parsing and its failure mode
/// belong upstream, in whatever HTTP-layer deserializer task 6
/// (`AccountsEndpoints`) builds — not this task's boundary.
///
/// `avatar`/`header` have no "explicit clear" variant (unlike
/// [`ProfilePatch::avatar_media`]'s `Option<Option<Id>>`): Requirement 6.1's
/// enumerated fields describe *setting* an avatar/header via upload, not
/// clearing one back to the default image, and offering a clear-to-default
/// affordance here is not something this task's acceptance criteria ask for
/// — a documented, narrower-than-`ProfilePatch` judgment call (this task's
/// own status report CONCERNS).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UpdateCredentialsInput {
    pub display_name: Option<String>,
    pub note: Option<String>,
    pub locked: Option<bool>,
    pub bot: Option<bool>,
    pub discoverable: Option<bool>,
    pub fields_attributes: Option<Vec<ProfileFieldInput>>,
    pub source_privacy: Option<Visibility>,
    pub source_sensitive: Option<bool>,
    pub source_language: Option<Option<String>>,
    pub avatar: Option<MediaUploadInput>,
    pub header: Option<MediaUploadInput>,
}

/// Requirement 6.3's numeric validation limits. Neither requirements.md's
/// excerpt nor design.md's excerpt gives concrete numbers for "フィールド数
/// 上限・各値の長さ上限" — these are this task's own judgment call, chosen to
/// match Mastodon's own real, long-standing limits (documented as a CONCERN
/// in this task's own status report for reviewer confirmation) rather than
/// an arbitrary placeholder: Mastodon caps profile fields at 4 entries, each
/// name/value at 255 characters, `display_name` at 30 characters, and the
/// profile `note` (bio) at 500 characters.
pub const MAX_PROFILE_FIELDS: usize = 4;
pub const MAX_FIELD_NAME_LEN: usize = 255;
pub const MAX_FIELD_VALUE_LEN: usize = 255;
pub const MAX_DISPLAY_NAME_LEN: usize = 30;
pub const MAX_NOTE_LEN: usize = 500;

/// Maps a [`FocusRangeError`] to a caller-facing 422 [`AppError`], the exact
/// same mapping `media::service::focus_range_error_to_app_error` (private to
/// that module) already establishes — duplicated here at the same
/// call-shape rather than exposed cross-module, since the underlying range
/// check itself ([`Focus::new`]) is what's actually reused, not this
/// one-line error conversion.
fn focus_range_error_to_app_error(err: FocusRangeError) -> AppError {
    AppError::client(StatusCode::UNPROCESSABLE_ENTITY, err.to_string())
}

/// Validates an optional raw focus coordinate pair via the fallible
/// [`Focus::new`] (media-pipeline's own task 2.1 constructor) — the same
/// focus-range validation logic `MediaService::accept_upload` already uses,
/// reused here (not reimplemented) so `update_credentials` rejects an
/// out-of-range avatar/header focus *before* ever calling
/// [`MediaService::accept_upload`] (fail-fast, per this task's own
/// instructions: "all validations before any side effect").
fn validate_focus_if_present(coords: Option<(f32, f32)>) -> Result<(), AppError> {
    if let Some((x, y)) = coords {
        Focus::new(x, y).map_err(focus_range_error_to_app_error)?;
    }
    Ok(())
}

/// Requirement 6.3's fail-fast validation gate: field count/length limits and
/// avatar/header focus range, all checked (and any violation returned)
/// before [`AccountService::update_credentials`] performs any side effect
/// (media ingestion or profile upsert) — design.md's sequence diagram's own
/// "`Svc->>Svc: validate fields limits privacy focus`" step, run in full
/// before the diagram's own next step ("`alt avatar or header present`").
fn validate_update_credentials(input: &UpdateCredentialsInput) -> Result<(), AppError> {
    if let Some(display_name) = &input.display_name
        && display_name.chars().count() > MAX_DISPLAY_NAME_LEN
    {
        return Err(AppError::client(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("display_name must be at most {MAX_DISPLAY_NAME_LEN} characters"),
        ));
    }

    if let Some(note) = &input.note
        && note.chars().count() > MAX_NOTE_LEN
    {
        return Err(AppError::client(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("note must be at most {MAX_NOTE_LEN} characters"),
        ));
    }

    if let Some(fields) = &input.fields_attributes {
        if fields.len() > MAX_PROFILE_FIELDS {
            return Err(AppError::client(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("at most {MAX_PROFILE_FIELDS} profile fields are accepted"),
            ));
        }
        for field in fields {
            if field.name.is_empty() || field.name.chars().count() > MAX_FIELD_NAME_LEN {
                return Err(AppError::client(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!(
                        "profile field name must be non-empty and at most {MAX_FIELD_NAME_LEN} characters"
                    ),
                ));
            }
            if field.value.chars().count() > MAX_FIELD_VALUE_LEN {
                return Err(AppError::client(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!("profile field value must be at most {MAX_FIELD_VALUE_LEN} characters"),
                ));
            }
        }
    }

    if let Some(avatar) = &input.avatar {
        validate_focus_if_present(avatar.focus)?;
    }
    if let Some(header) = &input.header {
        validate_focus_if_present(header.focus)?;
    }

    Ok(())
}

/// Resolves the single actor bound to a Bearer token into a CredentialAccount
/// (`verify_credentials`, Requirement 2.1) and resolves an arbitrary
/// local/known-remote/needs-fetching identifier into an Account
/// (`show_account`, Requirements 3.1, 3.2, 3.3). See this module's doc
/// comment for the full reasoning behind every deviation from design.md's
/// literal Service Interface sketch.
pub struct AccountService<S: MediaStore, H: FederationHttpClient> {
    pool: PgPool,
    directory: Arc<ActorDirectory>,
    fetcher: RemoteAccountFetcher<H>,
    serializer: AccountSerializer,
    ports: AccountPortsRegistry,
    store: S,
    /// Task 5.4's addition: media-pipeline's `MediaService`, ingesting
    /// avatar/header uploads (Requirement 6.2). `Arc`-wrapped because this
    /// is the same shared handle `AccountsModule`/media's own
    /// `MediaModule::service()` already hands out to every other consumer
    /// (e.g. `MediaEndpoints`) — this service does not need its own
    /// independent instance. Held over the same `S: MediaStore` type
    /// parameter this service already carries as `store` (both are always
    /// the one concrete production `MediaStore`, `LocalFsStore`, in
    /// practice), so no new generic parameter is introduced.
    media: Arc<MediaService<S>>,
    /// Task 5.4's addition: needed for `upsert_profile`'s `now` parameter
    /// (Requirement 6.5: "時刻は `RuntimeContext`"). Already named as this
    /// service's own planned dependency in design.md's Components table;
    /// this task is simply the first one that actually needs to read the
    /// clock.
    runtime: RuntimeContext,
}

impl<S: MediaStore, H: FederationHttpClient> AccountService<S, H> {
    /// Builds a service from already-constructed collaborators — this
    /// constructor only bundles them, mirroring this crate's established
    /// "bundle, don't build" convention for a business-service layer (e.g.
    /// `MediaService::new`).
    ///
    /// `too_many_arguments` is suppressed for the same documented reason
    /// `AppState::new`/`AccountSerializer`'s own builders already suppress
    /// it (`src/state.rs`'s own doc comment: "inherent to this
    /// constructor's role, not a smell to refactor away") — one parameter
    /// per already-constructed collaborator this bundling constructor
    /// assembles, growing as tasks land, not something a params struct
    /// would actually remove the coupling of.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pool: PgPool,
        directory: Arc<ActorDirectory>,
        fetcher: RemoteAccountFetcher<H>,
        serializer: AccountSerializer,
        ports: AccountPortsRegistry,
        store: S,
        media: Arc<MediaService<S>>,
        runtime: RuntimeContext,
    ) -> Self {
        AccountService {
            pool,
            directory,
            fetcher,
            serializer,
            ports,
            store,
            media,
            runtime,
        }
    }

    /// Resolves `actor_id` to the three inputs a local Account/
    /// CredentialAccount view needs beyond counts/emojis: the owner-free
    /// [`ResolvedActor`], its [`AccountProfile`] (a safe, all-default value
    /// when no `account_profiles` row exists yet — task 2.1's own documented
    /// "not `find_profile`'s job to substitute" contract, so this service is
    /// the caller that performs the substitution), and its `created_at` (see
    /// this module's doc comment, "`created_at`"). Returns `Ok(None)` — not
    /// an error — when no local actor exists under `actor_id`, mirroring
    /// `ActorDirectory::resolve_actor_by_id`'s own "no error for absence"
    /// contract.
    async fn resolve_local(
        &self,
        actor_id: Id,
    ) -> Result<Option<(ResolvedActor, AccountProfile, OffsetDateTime)>, AppError> {
        let Some(actor) = self.directory.resolve_actor_by_id(actor_id).await? else {
            return Ok(None);
        };
        let created_at = self.directory.actor_created_at(actor_id).await?.expect(
            "an actor resolve_actor_by_id just resolved must also have a created_at row: \
                 both read the same local_actors row",
        );
        let profile = find_profile(&self.pool, actor_id)
            .await?
            .unwrap_or_else(|| AccountProfile::default_for(actor_id));
        Ok(Some((actor, profile, created_at)))
    }

    /// The emoji candidate pool every Account/CredentialAccount build passes
    /// to `AccountSerializer`. See this module's doc comment ("Emoji
    /// candidates") for why this is `list_visible_emojis`, not a
    /// shortcode-targeted `resolve_emojis` call.
    async fn emoji_candidates(&self) -> Result<Vec<CustomEmojiView>, AppError> {
        list_visible_emojis(&self.pool).await
    }

    /// Resolves the Bearer-token-bound actor (`ctx.actor_id`) into a
    /// CredentialAccount JSON (Requirement 2.1: "Bearer トークンに結びついた
    /// 単一アクターを CredentialAccount として返す"). Counts are the
    /// currently-registered `AccountCountsProvider`'s value — all-zero via
    /// `ZeroCountsProvider` until a downstream spec (social-graph/
    /// statuses-core) registers a real one (task 1.3's own documented
    /// default). See this module's doc comment for why `origin` is an added
    /// parameter beyond design.md's literal sketch.
    ///
    /// Fails with a caller-facing `401 Unauthorized` [`AppError`] if
    /// `ctx.actor_id` no longer resolves to a local actor (a token bound to
    /// an actor that has since been removed) — api-foundation's Bearer
    /// middleware (task 6, out of this task's boundary) is expected to have
    /// already validated the token itself; this is a defensive fallback for
    /// the narrower case where the token was valid but the actor it names no
    /// longer exists.
    pub async fn verify_credentials(
        &self,
        ctx: &RequestActorContext,
        origin: &ForwardedOrigin,
    ) -> Result<Value, AppError> {
        let (actor, profile, created_at) =
            self.resolve_local(ctx.actor_id).await?.ok_or_else(|| {
                AppError::client(
                    StatusCode::UNAUTHORIZED,
                    "this token is bound to an actor that no longer exists",
                )
            })?;
        let counts = self.ports.counts(&AccountRef::Local(ctx.actor_id)).await?;
        let emojis = self.emoji_candidates().await?;
        Ok(self.serializer.build_credential_account(
            &actor,
            &profile,
            created_at,
            &counts,
            &self.store,
            origin,
            &emojis,
        ))
    }

    /// Resolves `id` — local, known-remote, or a remote reference needing a
    /// fetch — into an Account JSON (Requirements 3.1, 3.2, 3.3). See this
    /// module's doc comment ("Local/remote/needs-fetching identifier
    /// discipline") for the full resolution order and ("Deliberate
    /// deviations") for why `_viewer`/`origin` differ from design.md's
    /// literal sketch.
    pub async fn show_account(
        &self,
        id: &str,
        _viewer: Option<&RequestActorContext>,
        origin: &ForwardedOrigin,
    ) -> Result<Value, AppError> {
        if let Ok(raw_id) = id.parse::<i64>() {
            let account_id = Id::from_i64(raw_id);

            if let Some((actor, profile, created_at)) = self.resolve_local(account_id).await? {
                let counts = self.ports.counts(&AccountRef::Local(account_id)).await?;
                let emojis = self.emoji_candidates().await?;
                return Ok(self.serializer.build_account_local(
                    &actor,
                    &profile,
                    created_at,
                    &counts,
                    &self.store,
                    origin,
                    &emojis,
                ));
            }

            if let Some(remote) = find_remote_by_id(&self.pool, account_id).await? {
                let counts = self.ports.counts(&AccountRef::Remote(account_id)).await?;
                let emojis = self.emoji_candidates().await?;
                return Ok(self
                    .serializer
                    .build_account_remote(&remote, &counts, &emojis));
            }

            return Err(AppError::client(
                StatusCode::NOT_FOUND,
                format!("account '{id}' was not found"),
            ));
        }

        // Not a bare internal id: a remote actor_uri reference. Cache-first,
        // network-fetch-on-miss/stale (Requirements 7.1, 7.3); every failure
        // path already maps to a caller-facing 404 (Requirement 3.3) —
        // `RemoteAccountFetcher::fetch_and_normalize`'s own doc comment.
        let remote = self.fetcher.fetch_and_normalize(id).await?;
        let counts = self.ports.counts(&AccountRef::Remote(remote.id)).await?;
        let emojis = self.emoji_candidates().await?;
        Ok(self
            .serializer
            .build_account_remote(&remote, &counts, &emojis))
    }

    /// Resolves `id` to an [`AccountRef`] — the same local/known-remote/
    /// needs-fetching resolution order [`Self::show_account`] uses (see this
    /// module's doc comment, "Local/remote/needs-fetching identifier
    /// discipline"), reusing [`Self::resolve_local`]/`find_remote_by_id`/
    /// `fetcher.fetch_and_normalize` directly. Unlike `show_account`, this
    /// never builds an Account JSON: [`Self::list_statuses`] only needs a
    /// valid target reference to hand to `AccountStatusesProvider`, not a
    /// full serialized view. Fails with the same caller-facing `404`
    /// [`AppError`] as `show_account` for an id matching neither a local
    /// actor nor a known/fetchable remote account.
    async fn resolve_account_ref(&self, id: &str) -> Result<AccountRef, AppError> {
        if let Ok(raw_id) = id.parse::<i64>() {
            let account_id = Id::from_i64(raw_id);

            if self.resolve_local(account_id).await?.is_some() {
                return Ok(AccountRef::Local(account_id));
            }

            if find_remote_by_id(&self.pool, account_id).await?.is_some() {
                return Ok(AccountRef::Remote(account_id));
            }

            return Err(AppError::client(
                StatusCode::NOT_FOUND,
                format!("account '{id}' was not found"),
            ));
        }

        let remote = self.fetcher.fetch_and_normalize(id).await?;
        Ok(AccountRef::Remote(remote.id))
    }

    /// Resolves `id` to its [`AccountRef`] (404 for anything else),
    /// interprets `query`'s pagination/filter parameters and `viewer`'s
    /// identity into a [`StatusesQuery`], and delegates the actual Status
    /// page to the currently registered `AccountStatusesProvider`
    /// (Requirements 4.1, 4.2, 4.4, 4.5; design.md's "AccountService"
    /// Service Interface, `list_statuses`). While no real provider is
    /// registered, [`AccountPortsRegistry`]'s built-in
    /// [`crate::accounts::ports::EmptyStatusesProvider`] default already
    /// returns an empty [`Page`] without touching the database or network
    /// (Requirement 4.3) — this method still responds `Ok`, not an error, in
    /// that case, since an empty page is itself the correct response, not a
    /// failure.
    pub async fn list_statuses(
        &self,
        id: &str,
        query: StatusesQueryInput,
        viewer: Option<&RequestActorContext>,
    ) -> Result<Page<serde_json::Value>, AppError> {
        let target = self.resolve_account_ref(id).await?;
        let statuses_query = StatusesQuery {
            target,
            viewer: viewer.map(|ctx| ctx.actor_id),
            page: query.page,
            pinned: query.pinned,
            only_media: query.only_media,
            exclude_replies: query.exclude_replies,
            exclude_reblogs: query.exclude_reblogs,
        };
        self.ports.list_statuses(&statuses_query).await
    }

    /// Resolves each of `ids` (Requirement 5.1) to an [`AccountRef`] —
    /// reusing [`Self::resolve_account_ref`]'s exact local/known-remote/
    /// needs-fetching discipline, the same one [`Self::list_statuses`]
    /// already reuses — then queries the currently registered
    /// [`crate::accounts::ports::RelationshipStateProvider`] (via
    /// [`AccountPortsRegistry::relationships`]) for `ctx.actor_id`'s (the
    /// viewer, the Bearer-token-bound actor — same source
    /// [`Self::verify_credentials`] reads) relationship to every resolved
    /// target, and serializes each returned
    /// [`crate::accounts::model::RelationshipView`] via
    /// [`RelationshipSerializer::build_relationship`] into a JSON array, in
    /// the same order as the resolved targets (Requirements 5.1, 5.2, 5.3).
    ///
    /// While no real provider is registered, the built-in
    /// [`crate::accounts::ports::NoRelationshipProvider`] default already
    /// returns the Requirement 5.4 "no relationship" value (every boolean
    /// flag `false`, every count 0, `note` empty) for every resolved target
    /// — this method still responds `Ok`, not an error, in that case.
    ///
    /// `RelationshipSerializer` is instantiated locally rather than held as
    /// a constructor-injected field: unlike `AccountSerializer` (which needs
    /// a server domain) it is a zero-field, stateless unit struct (its own
    /// doc comment: "carries no state and performs no I/O"), so there is no
    /// collaborator state for this service to own or thread through
    /// construction.
    ///
    /// ## Unresolvable ids: skipped, not a batch-wide 404
    /// Neither Requirement 5 nor design.md's `AccountService.relationships`
    /// bullet specifies what happens when one id among several does not
    /// resolve to any known local/remote/fetchable account — unlike
    /// Requirements 3.3/4's explicit single-id 404 contract for
    /// `show_account`/`list_statuses`. Mastodon's own `relationships`
    /// endpoint does not fail an entire batch over one bad id; it simply
    /// omits ids it cannot resolve from the response. This method follows
    /// that precedent: an id for which [`Self::resolve_account_ref`] returns
    /// an `Err` is silently omitted from `ids` before the provider is ever
    /// queried, rather than aborting the whole request with a batch-wide
    /// error. This is consistent with Requirement 5.4's "既定は関係なし"
    /// spirit (a nonexistent target trivially has no relationship to
    /// report), but it is a judgment call, not a literal requirement —
    /// flagged in this task's status report CONCERNS for reviewer
    /// confirmation, since a stricter reading could instead argue for a
    /// batch-wide 404 the way `show_account`/`list_statuses` apply to a
    /// single id.
    pub async fn relationships(
        &self,
        ctx: &RequestActorContext,
        ids: &[String],
    ) -> Result<Value, AppError> {
        let mut targets = Vec::with_capacity(ids.len());
        for id in ids {
            if let Ok(account_ref) = self.resolve_account_ref(id).await {
                targets.push(account_ref);
            }
        }

        let views = self.ports.relationships(ctx.actor_id, &targets).await?;

        let relationship_serializer = RelationshipSerializer::new();
        let array = views
            .iter()
            .map(|view| relationship_serializer.build_relationship(view))
            .collect();
        Ok(Value::Array(array))
    }

    /// Ingests a single avatar/header upload for `actor_id` via
    /// [`MediaService::accept_upload`] (Requirement 6.2), returning the newly
    /// created [`crate::media::model::Media`]'s [`Id`] to use as the
    /// [`ProfilePatch`]'s `avatar_media`/`header_media` value.
    /// `description`/further `focus` validation is not repeated here:
    /// `focus` was already validated by [`validate_update_credentials`]
    /// before this method is ever called (fail-fast), and
    /// `MediaService::accept_upload`'s own internal (redundant but harmless)
    /// re-validation of the same coordinate pair can only ever agree.
    async fn ingest_profile_media(
        &self,
        actor_id: Id,
        upload: MediaUploadInput,
    ) -> Result<Id, AppError> {
        let media = self
            .media
            .accept_upload(
                actor_id,
                UploadInput {
                    bytes: upload.bytes,
                    content_type: upload.content_type,
                    description: None,
                    focus: upload.focus,
                },
            )
            .await?;
        Ok(media.id)
    }

    /// Updates the Bearer-token-bound actor's (`ctx.actor_id`) profile
    /// (Requirements 6.1, 6.2, 6.3, 6.5): validates `input` in full before
    /// any side effect ([`validate_update_credentials`]), ingests any
    /// avatar/header upload present via [`Self::ingest_profile_media`],
    /// applies the resulting [`ProfilePatch`] via
    /// [`crate::accounts::profile_repository::upsert_profile`] (a partial
    /// update — every `None` field on `input` leaves its stored value
    /// unchanged), and returns the updated CredentialAccount built from the
    /// just-upserted profile — reusing
    /// [`AccountSerializer::build_credential_account`], the exact same
    /// builder [`Self::verify_credentials`] uses, rather than duplicating
    /// its field-mapping logic (design.md's sequence diagram: "validate
    /// fields limits privacy focus" -> "ingest media as profile image" (if
    /// present) -> "upsert account profile patch only" -> "credential
    /// account").
    ///
    /// Because [`crate::accounts::profile_repository::upsert_profile`]
    /// actually persists to `account_profiles`, the result is automatically
    /// visible to a subsequent [`Self::verify_credentials`]/[`Self::show_account`]
    /// call against the same actor (Requirement 6.5) — no separate cache to
    /// invalidate.
    ///
    /// Fails with the same caller-facing `401 Unauthorized` [`AppError`]
    /// [`Self::verify_credentials`] uses if `ctx.actor_id` no longer resolves
    /// to a local actor — this is checked *after* the profile upsert
    /// succeeds (the upsert itself only ever touches `account_profiles`
    /// keyed by `actor_id`, with no foreign-key dependency on
    /// `local_actors` that would fail first), so a stale token naming a
    /// since-removed actor still fails safely rather than silently writing
    /// an orphaned profile row and only then erroring.
    pub async fn update_credentials(
        &self,
        ctx: &RequestActorContext,
        input: UpdateCredentialsInput,
        origin: &ForwardedOrigin,
    ) -> Result<Value, AppError> {
        validate_update_credentials(&input)?;

        let avatar_media = match input.avatar {
            Some(upload) => Some(Some(self.ingest_profile_media(ctx.actor_id, upload).await?)),
            None => None,
        };
        let header_media = match input.header {
            Some(upload) => Some(Some(self.ingest_profile_media(ctx.actor_id, upload).await?)),
            None => None,
        };
        let fields = input.fields_attributes.map(|fields| {
            fields
                .into_iter()
                .map(|field| ProfileField {
                    name: field.name,
                    value: field.value,
                    verified_at: None,
                })
                .collect()
        });

        let patch = ProfilePatch {
            display_name: input.display_name,
            note: input.note,
            avatar_media,
            header_media,
            fields,
            locked: input.locked,
            bot: input.bot,
            discoverable: input.discoverable,
            source_privacy: input.source_privacy,
            source_sensitive: input.source_sensitive,
            source_language: input.source_language,
        };

        let now = self.runtime.clock.now();
        let profile = upsert_profile(&self.pool, ctx.actor_id, patch, now).await?;

        let actor = self
            .directory
            .resolve_actor_by_id(ctx.actor_id)
            .await?
            .ok_or_else(|| {
                AppError::client(
                    StatusCode::UNAUTHORIZED,
                    "this token is bound to an actor that no longer exists",
                )
            })?;
        let created_at = self.directory.actor_created_at(ctx.actor_id).await?.expect(
            "an actor resolve_actor_by_id just resolved must also have a created_at row: \
                 both read the same local_actors row",
        );
        let counts = self.ports.counts(&AccountRef::Local(ctx.actor_id)).await?;
        let emojis = self.emoji_candidates().await?;
        Ok(self.serializer.build_credential_account(
            &actor,
            &profile,
            created_at,
            &counts,
            &self.store,
            origin,
            &emojis,
        ))
    }
}
