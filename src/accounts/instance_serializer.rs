//! `InstanceSerializer` (design.md "Service / サービス層" ->
//! "RelationshipSerializer / InstanceSerializer / CustomEmojiSerializer";
//! Requirements 8.1, 8.2, 8.3, 8.4; task 3.3, `Boundary: InstanceSerializer`):
//! synthesizes the Mastodon-compatible Instance(v2) JSON contract from
//! already-loaded operational settings ([`InstanceSettings`], task 2.4's
//! `InstanceSettingsRepository::load_instance_settings`), a caller-supplied
//! real-constraint snapshot ([`ServerCapabilities`]), and this instance's
//! own domain/build-time constants.
//!
//! Scope: this module owns exactly the mapping from already-resolved inputs
//! to Instance(v2) JSON. It does not read `instance_settings` from the
//! database itself (that is `InstanceSettingsRepository`'s job, task 2.4,
//! already implemented), does not implement `InstanceService` (task 5.5, the
//! eventual caller that loads `InstanceSettings` via the repository and
//! hands it — together with a `ServerCapabilities` built from the running
//! `MediaConfig` — to this module), and registers no contract-harness golden
//! (task 3.5).
//!
//! ## Typed structs + `Serialize`, not a hand-built `serde_json::json!` value
//! Follows `src/accounts/serializer.rs`'s (task 3.1) and
//! `src/accounts/relationship_serializer.rs`'s (task 3.2) established
//! precedent: [`InstanceJson`]/[`UsageJson`]/[`UsageUsersJson`]/
//! [`ConfigurationJson`]/[`MediaAttachmentsConfigJson`]/[`RegistrationsJson`]/
//! [`ContactJson`]/[`RuleJson`] are plain `#[derive(Serialize)]` structs
//! mirroring Requirement 8.1's field list field-by-field, not a
//! `json!{...}` literal a field could silently go missing from.
//! [`to_instance_json`]/[`instance_to_json`] mirror `serializer.rs`'s
//! `to_account_json`/`account_to_json` pair: the former is a pure, total
//! mapping; the latter is a thin `serde_json::to_value` wrapper (task 3.5
//! will reuse it verbatim when it registers the golden).
//!
//! ## Field provenance (Requirement 8.1, 8.2, 8.3; design.md's InstanceSerializer
//! Responsibilities lists this exact split)
//! - `title`/`description`/`contact`/`rules`/`registrations`/`thumbnail`/
//!   `languages`: [`InstanceSettings`] (DB-backed, task 2.4's repository
//!   already guarantees every field is present — with a safe default when
//!   unset — even on a fresh database with zero rows; see
//!   `settings_repository.rs`'s own doc comment, "Default-merge strategy").
//!   This module performs no additional default-substitution on top of that
//!   — an already-fully-populated [`InstanceSettings`] is exactly what its
//!   [`InstanceJson`] fields map straight across from (Requirement 8.2,
//!   8.3).
//! - `domain`: not part of [`InstanceSettings`] (design.md's model doc
//!   explicitly excludes `version`/`source_url`/`usage` from
//!   `InstanceSettings`, and `domain` is likewise absent from that type's
//!   field list — it is this server's own startup configuration, not an
//!   operator-editable operational setting). [`InstanceSerializer::new`]
//!   takes it the same way [`crate::accounts::serializer::AccountSerializer::new`]
//!   takes its own `domain` parameter — this instance's bare server domain
//!   (`crate::config::AppConfig::server.domain`, e.g. `"example.social"`),
//!   supplied once at construction rather than threaded through every call.
//! - `version`: build-time constant `env!("CARGO_PKG_VERSION")`, exactly as
//!   the task text specifies — read at compile time from `Cargo.toml`'s
//!   `[package].version` (currently `"0.1.0"`), never persisted or computed
//!   at runtime, so it is trivially deterministic across calls and rebuilds
//!   of the same source tree.
//! - `source_url`: a fixed, deterministic constant
//!   ([`SOURCE_URL`]) — this codebase's own git remote
//!   (`git remote -v` -> `.../mat-der-D/kawasemi`, verified against this
//!   working tree at implementation time), rendered as an `https://github.com/...`
//!   URL. design.md only specifies "ビルド時定数（固定のリポジトリ URL 定数）"
//!   without naming an exact literal or a `Cargo.toml` field to derive it
//!   from (this crate's `Cargo.toml` has no `repository`/`homepage` key to
//!   read via `env!` either), so this module picks the one concrete,
//!   verifiable value available — this project's actual repository URL —
//!   rather than a placeholder domain that would not actually resolve to
//!   this project's source.
//! - `usage.users.active_month`: a fixed MVP placeholder
//!   ([`ACTIVE_MONTH_USERS_PLACEHOLDER`]). This is spec-sanctioned, not a
//!   deferred-work shortcut: the task text explicitly calls for "MVP 固定値
//!   プレースホルダ", and design.md's own InstanceSerializer field-provenance
//!   list gives the same illustrative value this module uses verbatim
//!   ("MVP では固定値（例: `1`）を返すプレースホルダとする。真の集計...は本
//!   spec の対象外とし、ゴールデン契約（8.5）を決定的に固定できることを優先
//!   する。"). A real active-user-count aggregation has no owning component
//!   anywhere in this spec or any spec it depends on (accounts-and-instance's
//!   own design.md Non-Goals list has no such item either — it is simply
//!   out of every spec's scope so far), so inventing a real computation here
//!   would mean querying tables/logic this module has no business owning.
//!   `1` (this server's single owner — steering's "一人鯖") is a defensible,
//!   deterministic, always-reproducible value; the point of this field is
//!   the golden's own determinism (8.5), not accuracy this MVP cannot yet
//!   provide.
//! - `configuration`: see "`configuration` alignment with media-pipeline's
//!   real constraints" below.
//!
//! ## `configuration` alignment with media-pipeline's real constraints
//! (Requirement 8.4: "`configuration`...を、本サーバーの実際の制約と整合する
//! 値として返す")
//! design.md's InstanceSerializer Responsibilities section is the only place
//! that names `configuration`'s concrete sourcing for this task
//! ("`configuration` は media-pipeline の上限等と整合させる") — it does not
//! call for `statuses`/`polls`/`accounts` sub-objects the way Requirement
//! 8.4's own prose generically lists as typical Mastodon `configuration`
//! categories ("`statuses` / `media_attachments` / `polls` / `accounts` 等").
//! Those three categories have no owning implementation anywhere in this
//! codebase yet — `statuses-core` (Status body/limits) and `polls`/
//! `accounts`-limits components are not implemented by any spec merged so
//! far (this spec's own design.md Non-Goals: "投稿（Status）本体の取得・
//! CRUD" is explicitly out of scope) — so inventing numeric limits for them
//! here would be exactly the "align with the server's actual constraints"
//! requirement's *opposite*: numbers with no real backing constraint,
//! guaranteed to drift from whatever those specs eventually implement.
//! This module therefore populates `configuration.media_attachments` only —
//! the one category this codebase actually enforces today — and leaves
//! `statuses`/`polls`/`accounts` for whichever future task/spec first
//! introduces a real, enforced constraint for them (mirroring this spec's
//! own established "committed default behind a delegation boundary until a
//! real owner registers" pattern, e.g. `ports.rs`'s `AccountStatusesProvider`/
//! `RelationshipStateProvider`).
//!
//! `configuration.media_attachments` sources exactly the two upload
//! constraints media-pipeline's own `MediaService::accept_upload`
//! (`src/media/service.rs::validate_format`/`validate_size`) actually
//! enforces, both already startup-configurable via
//! [`crate::config::MediaConfig`] (`src/config.rs`, read once at boot,
//! shared by every module through `AppState`):
//! - `supported_mime_types`: [`crate::config::MediaConfig::supported_formats`]
//!   — the exact content-type allow-list `validate_format` checks against
//!   (an upload with a `content_type` outside this list is rejected before
//!   storage).
//! - `image_size_limit`: [`crate::config::MediaConfig::max_upload_size_bytes`]
//!   — the exact byte ceiling `validate_size` enforces.
//!
//! No `image_matrix_limit`/`video_size_limit`/`video_frame_rate_limit`/
//! `video_matrix_limit` fields are emitted, even though real Mastodon's own
//! `media_attachments` object includes them: media-pipeline is an
//! image-only MVP with no video support at all
//! (`crate::config::MediaConfig::max_upload_size_bytes`'s own doc comment:
//! "この MVP の画像専用スコープ...no video/audio to size for yet") and no
//! pixel-dimension upload limit exists anywhere in `src/media/` (only a
//! *thumbnail target* width/height,
//! [`crate::config::MediaConfig::thumbnail_target_width`]/
//! `thumbnail_target_height`, which bounds a generated derivative, not an
//! accepted upload) — emitting those fields with an invented number would
//! violate this exact requirement's "align with the real constraint" intent
//! for the sake of superficially matching Mastodon's full field list.
//!
//! [`ServerCapabilities`] is this module's own definition of design.md's
//! literal Service Interface sketch parameter (`caps: &ServerCapabilities`)
//! — design.md names the type but does not define its fields anywhere
//! (`model.rs`, task 1.2's boundary, does not mention it), so, mirroring
//! `AccountSerializer`'s own precedent of defining small caller-supplied
//! parameter types local to the serializer that needs them, this module
//! defines it scoped to exactly the two values above.
//! [`ServerCapabilities::from_media_config`] is the intended real-usage
//! constructor (task 5.5's `InstanceService` will call it against the live
//! `AppState`'s `MediaConfig`); [`ServerCapabilities::new`] exists for tests
//! that want to construct one directly without a full `MediaConfig` value.
//!
//! ## `contact`/`rules`: honest, undecorated mappings from stored data
//! - `contact`: real Mastodon's v2 `contact` object nests a full `account`
//!   Account entity. This module has no `ResolvedActor`/`AccountSerializer`
//!   wiring available (design.md's literal `build_instance_v2` signature
//!   takes only `settings`/`caps`, no actor-resolution dependency), and
//!   [`InstanceSettings::contact_account_id`] is only an `Option<Id>` — this
//!   module renders that id as [`ContactJson::account_id`] rather than
//!   fabricating a nested Account object it cannot actually resolve (which
//!   would misrepresent unresolved/placeholder data as a real account).
//!   Resolving `contact_account_id` into a full nested Account (if a future
//!   task decides that gap needs closing) is `InstanceService`'s call (task
//!   5.5), not this pure serializer's.
//! - `rules`: real Mastodon's `rules` entries carry a stable `id` alongside
//!   `text`. [`InstanceSettings::rules`] (design.md's model doc, and
//!   `migrations/0006_accounts.sql`'s `rules JSONB` column) stores only
//!   `Vec<String>` — no per-rule id is persisted anywhere. This module
//!   synthesizes a 1-based positional id (`"1"`, `"2"`, ...) per rule in
//!   stored order — deterministic for a given `Vec<String>` (Requirement
//!   8.5's determinism), and matches the common Mastodon-server convention
//!   of small sequential rule ids — rather than leaving `rules` id-less
//!   (which the real Instance(v2) contract requires) or inventing a
//!   persisted-id scheme this task's boundary (`InstanceSerializer` only,
//!   not `model.rs`/`migrations/`) has no authority to add.

#[cfg(test)]
mod tests;

use serde::Serialize;
use serde_json::Value;

use crate::accounts::model::InstanceSettings;
use crate::config::MediaConfig;
use crate::domain::Id;

/// The fixed MVP placeholder for `usage.users.active_month`. See this
/// module's doc comment ("Field provenance") for why a fixed value is
/// spec-sanctioned here, not a shortcut.
const ACTIVE_MONTH_USERS_PLACEHOLDER: i64 = 1;

/// The fixed, deterministic `source_url` constant. See this module's doc
/// comment ("Field provenance") for why this exact value was chosen.
const SOURCE_URL: &str = "https://github.com/mat-der-D/kawasemi";

/// `configuration.media_attachments`' JSON shape (Requirement 8.4). See this
/// module's doc comment ("`configuration` alignment...") for why only these
/// two fields are populated.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MediaAttachmentsConfigJson {
    pub supported_mime_types: Vec<String>,
    pub image_size_limit: u64,
}

/// `configuration`'s JSON shape (Requirement 8.1, 8.4).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConfigurationJson {
    pub media_attachments: MediaAttachmentsConfigJson,
}

/// `usage.users`' JSON shape (Requirement 8.1).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UsageUsersJson {
    pub active_month: i64,
}

/// `usage`'s JSON shape (Requirement 8.1).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UsageJson {
    pub users: UsageUsersJson,
}

/// `registrations`' JSON shape (Requirement 8.1, 8.2), sourced from
/// [`InstanceSettings::registrations_enabled`]/`registrations_approval_required`/
/// `registrations_message`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RegistrationsJson {
    pub enabled: bool,
    pub approval_required: bool,
    pub message: Option<String>,
}

/// `contact`'s JSON shape (Requirement 8.1, 8.2). See this module's doc
/// comment ("`contact`/`rules`: honest, undecorated mappings") for why
/// `account_id` (not a nested Account object) is what this module can
/// actually supply.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ContactJson {
    pub email: String,
    pub account_id: Option<Id>,
}

/// One `rules` array entry (Requirement 8.1, 8.2). See this module's doc
/// comment for why `id` is a synthesized 1-based position, not a persisted
/// value.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RuleJson {
    pub id: String,
    pub text: String,
}

/// The Mastodon-compatible Instance(v2) JSON contract (Requirement 8.1).
/// Field order matches Requirement 8.1's own listing.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InstanceJson {
    pub domain: String,
    pub title: String,
    pub version: String,
    pub source_url: String,
    pub description: String,
    pub usage: UsageJson,
    pub thumbnail: Option<String>,
    pub languages: Vec<String>,
    pub configuration: ConfigurationJson,
    pub registrations: RegistrationsJson,
    pub contact: ContactJson,
    pub rules: Vec<RuleJson>,
}

/// This server's real, enforced upload constraints (Requirement 8.4) — the
/// definition of design.md's literal `caps: &ServerCapabilities` Service
/// Interface parameter. See this module's doc comment ("`configuration`
/// alignment...") for the full scoping rationale.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerCapabilities {
    pub media_supported_mime_types: Vec<String>,
    pub media_image_size_limit: u64,
}

impl ServerCapabilities {
    /// Builds a [`ServerCapabilities`] from explicit values (for tests that
    /// do not want to construct a full [`MediaConfig`]).
    pub fn new(media_supported_mime_types: Vec<String>, media_image_size_limit: u64) -> Self {
        ServerCapabilities {
            media_supported_mime_types,
            media_image_size_limit,
        }
    }

    /// Builds a [`ServerCapabilities`] from the running [`MediaConfig`] —
    /// the real usage this module exists for (task 5.5's `InstanceService`
    /// will call this against the live `AppState`'s media configuration),
    /// so `configuration.media_attachments` always reflects whatever this
    /// server's actual startup configuration enforces, never a value that
    /// can drift from it.
    pub fn from_media_config(config: &MediaConfig) -> Self {
        ServerCapabilities {
            media_supported_mime_types: config.supported_formats.clone(),
            media_image_size_limit: config.max_upload_size_bytes,
        }
    }
}

fn usage_json() -> UsageJson {
    UsageJson {
        users: UsageUsersJson {
            active_month: ACTIVE_MONTH_USERS_PLACEHOLDER,
        },
    }
}

fn configuration_to_json(caps: &ServerCapabilities) -> ConfigurationJson {
    ConfigurationJson {
        media_attachments: MediaAttachmentsConfigJson {
            supported_mime_types: caps.media_supported_mime_types.clone(),
            image_size_limit: caps.media_image_size_limit,
        },
    }
}

fn registrations_to_json(settings: &InstanceSettings) -> RegistrationsJson {
    RegistrationsJson {
        enabled: settings.registrations_enabled,
        approval_required: settings.registrations_approval_required,
        message: settings.registrations_message.clone(),
    }
}

fn contact_to_json(settings: &InstanceSettings) -> ContactJson {
    ContactJson {
        email: settings.contact_email.clone(),
        account_id: settings.contact_account_id,
    }
}

/// Synthesizes 1-based positional rule ids in stored order (Requirement
/// 8.1, 8.2, 8.5's determinism). See this module's doc comment.
fn rules_to_json(rules: &[String]) -> Vec<RuleJson> {
    rules
        .iter()
        .enumerate()
        .map(|(index, text)| RuleJson {
            id: (index + 1).to_string(),
            text: text.clone(),
        })
        .collect()
}

/// Builds the Instance(v2) JSON contract (Requirements 8.1, 8.2, 8.3, 8.4) —
/// a pure, total mapping from `domain` (this server's own startup config),
/// an already-fully-populated `settings` (task 2.4's repository guarantee),
/// and `caps` (this server's real upload constraints). See this module's
/// doc comment for the full field-provenance breakdown.
pub fn to_instance_json(
    domain: &str,
    settings: &InstanceSettings,
    caps: &ServerCapabilities,
) -> InstanceJson {
    InstanceJson {
        domain: domain.to_string(),
        title: settings.title.clone(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        source_url: SOURCE_URL.to_string(),
        description: settings.description.clone(),
        usage: usage_json(),
        thumbnail: settings.thumbnail.clone(),
        languages: settings.languages.clone(),
        configuration: configuration_to_json(caps),
        registrations: registrations_to_json(settings),
        contact: contact_to_json(settings),
        rules: rules_to_json(&settings.rules),
    }
}

/// [`to_instance_json`], converted to a plain [`serde_json::Value`]
/// (matching `accounts/serializer.rs::account_to_json`'s convention).
pub fn instance_to_json(
    domain: &str,
    settings: &InstanceSettings,
    caps: &ServerCapabilities,
) -> Value {
    serde_json::to_value(to_instance_json(domain, settings, caps))
        .expect("InstanceJson always serializes to JSON")
}

/// Maps operational settings + real server constraints onto the Instance(v2)
/// JSON contract (Requirements 8.1-8.4). Holds this instance's own `domain`
/// (constructor-supplied, like `AccountSerializer::new`) — see this module's
/// doc comment ("Field provenance") for why `domain` is not part of
/// `InstanceSettings`.
#[derive(Debug, Clone)]
pub struct InstanceSerializer {
    domain: String,
}

impl InstanceSerializer {
    /// Builds a serializer for `domain` (this instance's own bare server
    /// domain, e.g. `"example.social"` — `crate::config::AppConfig::server.domain`).
    pub fn new(domain: impl Into<String>) -> Self {
        InstanceSerializer {
            domain: domain.into(),
        }
    }

    /// Builds the Instance(v2) JSON (Requirements 8.1, 8.2, 8.3, 8.4).
    pub fn build_instance_v2(
        &self,
        settings: &InstanceSettings,
        caps: &ServerCapabilities,
    ) -> Value {
        instance_to_json(&self.domain, settings, caps)
    }
}
