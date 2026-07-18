//! Startup configuration loading, merging, and validation (Config boundary).
//!
//! Scope: this module owns only *startup* configuration (Requirement 2.6) —
//! the values a process needs before it can begin serving: the server
//! domain, the database connection target, log settings, and (as of
//! actor-model's task 6.1) the actor-model signing-key encryption secret. It
//! reads a TOML file and environment variables, merges them with environment
//! variables taking precedence on conflicting fields (Requirement 2.2), and
//! validates the result into an immutable [`AppConfig`] or a [`ConfigError`]
//! that identifies exactly which fields are missing or malformed
//! (Requirements 2.3, 2.4).
//!
//! This module never reads or writes *operational* configuration (the
//! database-stored, admin-editable settings owned by a later spec) —
//! see Requirement 2.6.
//!
//! `database.url` is wrapped in [`Secret<String>`](secret::Secret) (task
//! 2.2, Requirement 2.5): it can embed credentials
//! (`postgres://user:pass@host/db`), so it must never print in plaintext
//! via `Debug`/`Display`/log output. `actor.kek` (task 6.1, actor-model's
//! Requirement 6.1) is wrapped the same way, for the same reason: it is a
//! Key-Encryption-Key, not a database credential, but it is exactly as
//! secret-bearing. `owner.password` and `oauth.token_hash_key`
//! (api-foundation's task 1.2, Requirements 2.2/3.6) extend this same
//! discipline to two more startup secrets: the shared passphrase a later
//! `OwnerGate` (task 4.1) compares in constant time to authenticate the
//! single-owner operator, and the keyed hashing material a later OAuth
//! repository layer (tasks 3.1-3.3) uses to hash client secrets,
//! authorization codes, and access tokens before persisting them — neither
//! may ever appear in plaintext via `Debug`/`Display`/log output either.
//!
//! media-pipeline's task 1.2 adds `media.*`: storage root, max upload size,
//! thumbnail target dimensions, supported content types, worker
//! concurrency, max retry attempts, and processing-job lease duration. None
//! of these are secret-bearing, so none are wrapped in `Secret<T>`; see
//! [`MediaConfig`]'s own doc comment for field-by-field detail and why every
//! field has a safe default.

use std::collections::HashMap;
use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

#[cfg(test)]
mod tests;

mod secret;

pub use secret::Secret;

/// Prefix applied to every environment variable this module reads, e.g.
/// `KAWASEMI_SERVER_DOMAIN`, `KAWASEMI_DATABASE_URL`.
const ENV_PREFIX: &str = "KAWASEMI_";

/// Default path to the startup TOML config file, relative to the process's
/// current working directory. Overridable via `KAWASEMI_CONFIG_PATH`, which
/// is treated as an ordinary path lookup, not a merged config field.
const DEFAULT_CONFIG_PATH: &str = "kawasemi.toml";

/// Fully validated, immutable startup configuration (Requirement 2.1).
#[derive(Debug, Clone, PartialEq)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub log: LogConfig,
    /// actor-model's startup secret (design.md's "起動設定に鍵暗号鍵（KEK）
    /// の `Secret<T>` 項目を1つ追加", Requirement 6.1). core-runtime only
    /// hosts this field (mirroring `database.url`'s precedent of a
    /// downstream-consumed `Secret<T>` startup item) — it does not itself
    /// know what a KEK is used for; `crate::actor::keys::cipher` is the
    /// consumer.
    pub actor: ActorConfig,
    /// api-foundation's owner-authentication startup secret (design.md's
    /// Modified Files: "起動設定にオーナー資格情報（`Secret<T>`）...の項目を
    /// 追加", Requirement 2.2). core-runtime only hosts this field; a later
    /// task's `OwnerGate` is the consumer (design.md's OwnerGate: "起動設定
    /// （`Secret<T>`）のオーナー資格情報を定数時間比較で照合").
    pub owner: OwnerConfig,
    /// api-foundation's token-hashing startup secret (design.md's Modified
    /// Files: "...とトークンハッシュ用素材（`Secret<T>`）の項目を追加",
    /// Requirement 3.6). core-runtime only hosts this field; a later task's
    /// OAuth repository layer is the consumer (design.md's Physical Data
    /// Model: `client_secret_hash`/`code_hash`/`token_hash`, "同一規約で
    /// ハッシュ化").
    pub oauth: OauthConfig,
    /// federation-core's startup settings: secure-mode flag, public-key
    /// cache TTL, and received-Activity retention window (task 5.4,
    /// `_Boundary: FederationModule, Bootstrap, AppState, Config_`,
    /// Requirements 7.3, 10.1, 11.1, 11.2). core-runtime only hosts this
    /// field; `crate::federation::module::build_federation_module` is the
    /// consumer. See [`FederationConfig`]'s own doc comment for why this
    /// struct does not also carry a delivery-retry-policy field despite
    /// task 5.4's task text naming "配送リトライ方針" alongside these three.
    pub federation: FederationConfig,
    /// media-pipeline's startup settings (task 1.2, design.md's Modified
    /// Files: "起動設定にメディア保管ルート・アップロード上限サイズ・
    /// サムネイル寸法・対応形式・ワーカー並行度/再試行上限・処理ジョブの
    /// リース期間...を追加", Requirements 1.4, 4.2, 5.2, 6.1). core-runtime
    /// only hosts this field; the eventual consumers (`LocalFsStore`,
    /// `MediaService`, `PureRustImageProcessor`, `ProcessingWorker`,
    /// `ProcessingJobQueue`) are wired up by later tasks (2.x-5.2), not this
    /// one. See [`MediaConfig`]'s own doc comment for why no field here is
    /// wrapped in `Secret<T>`.
    pub media: MediaConfig,
}

/// Server-facing startup settings.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerConfig {
    /// Public domain this instance serves under. Required (Requirement 2.3).
    pub domain: String,
    /// Address the HTTP listener binds to. Defaults to `0.0.0.0:3000`.
    pub bind_addr: SocketAddr,
    /// Grace period allowed for in-flight requests to finish during
    /// shutdown before the server force-stops (Requirement 1.4, consumed by
    /// a later task). Defaults to 30 seconds.
    pub shutdown_grace: Duration,
}

/// Database connection startup settings.
#[derive(Debug, Clone, PartialEq)]
pub struct DatabaseConfig {
    /// Database connection string. Required (Requirement 2.3). Wrapped in
    /// [`Secret`] because it can embed credentials
    /// (`postgres://user:pass@host/db`) and must not print in plaintext via
    /// `Debug`/`Display`/log output (Requirement 2.5).
    pub url: Secret<String>,
    /// Maximum number of pooled connections (consumed by task 4.1). Defaults
    /// to 10.
    pub max_connections: u32,
    /// Timeout for acquiring a connection from the pool (consumed by task
    /// 4.1). Defaults to 5 seconds.
    pub acquire_timeout: Duration,
}

/// actor-model's startup settings: currently just the Key-Encryption-Key
/// (KEK) that seals/opens each actor's persisted signing key at rest
/// (design.md "Modified Files": "起動設定に鍵暗号鍵（KEK）の `Secret<T>`
/// 項目を1つ追加"). A dedicated struct (rather than a bare field on
/// [`AppConfig`]) mirrors this module's existing per-concern grouping
/// (`ServerConfig`/`DatabaseConfig`/`LogConfig`) and gives the KEK its own
/// `actor.*` dotted-path namespace (`actor.kek` -> `KAWASEMI_ACTOR_KEK`),
/// leaving room for future actor-model startup settings without disturbing
/// [`AppConfig`]'s own field list again.
#[derive(Debug, Clone, PartialEq)]
pub struct ActorConfig {
    /// Key-Encryption-Key for `crate::actor::keys::cipher::KeyCipher`'s
    /// at-rest sealing of persisted signing keys. Required, like
    /// `database.url`/`server.domain` — a security-critical secret has no
    /// safe default to fall back to. Supplied as a 64-character hexadecimal
    /// string (256 bits, see [`validate_kek`]) and wrapped in [`Secret`] so
    /// it never prints in plaintext via `Debug`/`Display`/log output
    /// (mirrors `DatabaseConfig::url`'s masking convention, Requirement
    /// 2.5).
    pub kek: Secret<[u8; 32]>,
}

/// api-foundation's owner-authentication startup settings: currently just
/// the shared secret a later `OwnerGate` (task 4.1) compares in constant
/// time to authenticate the single human operator of this instance
/// (design.md "Modified Files": "起動設定にオーナー資格情報（`Secret<T>`）
/// ...の項目を追加"; OwnerGate's Responsibilities: "起動設定（`Secret<T>`）
/// のオーナー資格情報を定数時間比較で照合（一人鯖の単一オーナー前提）").
///
/// Design choice (single passphrase, not username+password): this is a
/// one-owner-per-instance server ("一人鯖前提"). design.md's OwnerGate is
/// explicit that after this credential is verified, the authenticated
/// owner's identity is resolved separately via actor-model's single-owner
/// accessor (`ActorDirectory::sole_owner()`), not from anything supplied
/// here — so the config value itself does not need to encode a username or
/// identity, only a shared secret the owner proves knowledge of. A single
/// `password` field is therefore sufficient and keeps this struct as
/// minimal as `ActorConfig`'s single `kek` field.
#[derive(Debug, Clone, PartialEq)]
pub struct OwnerConfig {
    /// Shared passphrase proving ownership. Required, like `database.url`/
    /// `actor.kek` — a security-critical secret has no safe default to fall
    /// back to. Wrapped in [`Secret`] so it never prints in plaintext via
    /// `Debug`/`Display`/log output (mirrors `ActorConfig::kek`'s masking
    /// convention, Requirement 2.5's discipline extended to this field).
    /// Validated only for a minimal non-triviality floor (see
    /// [`validate_owner_password`]) — unlike `actor.kek`/
    /// `oauth.token_hash_key`, a human-chosen passphrase has no natural
    /// fixed encoding to validate against.
    pub password: Secret<String>,
}

/// api-foundation's OAuth startup settings: currently just the keyed
/// hashing material a later OAuth repository layer (tasks 3.1-3.3) uses to
/// hash client secrets, authorization codes, and access tokens before
/// persisting them (design.md's Physical Data Model: `client_secret_hash`/
/// `code_hash`/`token_hash` columns, "同一規約でハッシュ化"; Requirement
/// 3.6: access token values must never reach diagnostic logs in plaintext).
/// core-runtime only hosts this field — it does not itself know what a
/// keyed hash is used for, mirroring `ActorConfig::kek`'s precedent of a
/// downstream-consumed `Secret<T>` startup item.
#[derive(Debug, Clone, PartialEq)]
pub struct OauthConfig {
    /// Keyed hashing material ("pepper") for OAuth client secrets/
    /// authorization codes/access tokens. Required, like `actor.kek` — no
    /// safe default exists for a secret whose entire purpose is making
    /// stored hashes unforgeable without it. Supplied as a 64-character
    /// hexadecimal string (256 bits, see [`validate_token_hash_key`]) and
    /// wrapped in [`Secret`] so it never prints in plaintext via
    /// `Debug`/`Display`/log output. Mirrors `actor.kek`'s exact
    /// shape/validation: both are fixed-length secret material for a keyed
    /// cryptographic primitive with no natural human-typable format beyond
    /// hex.
    pub token_hash_key: Secret<[u8; 32]>,
}

/// federation-core's startup settings (task 5.4, design.md's Data Contracts
/// & Integration: "設定: セキュアモードフラグ・配送リトライ方針・公開鍵
/// キャッシュ TTL（`federation.public_key_cache_ttl`、既定 24h）・受信
/// Activity 保持日数（`federation.received_activity_retention_days`、既定
/// 14 日）を core-runtime 起動設定に追加").
///
/// ## Why no delivery-retry-policy field
/// Task 5.4's own text also names "配送リトライ方針" (delivery retry
/// policy) as something to add to startup config, alongside these three
/// fields. Judgment call, documented here: the only components that
/// actually *consume* a retry policy — `DeliveryWorker::process_job`
/// (comparing an incremented attempt count against
/// `federation::outbound::queue::DEFAULT_MAX_DELIVERY_ATTEMPTS`) and
/// `backoff_delay` (reading `DEFAULT_DELIVERY_BASE_DELAY`/
/// `DEFAULT_DELIVERY_MAX_DELAY`) — reference those as bare module
/// constants with no constructor parameter to inject a different value
/// through (`src/federation/outbound/worker.rs`, `queue.rs`). Task 5.4's own
/// boundary explicitly forbids modifying `src/federation/outbound/*.rs`
/// ("already-implemented dependencies"), so there is no reachable injection
/// point this task can wire a config value into without violating that
/// boundary. Adding a `FederationConfig` field nothing reads would be a
/// dead/unwired config surface, which this task's own "no scope expansion
/// beyond wiring" constraint counsels against. The delivery retry policy
/// therefore remains exactly the already-existing compile-time defaults
/// documented on those constants (`DEFAULT_DELIVERY_BASE_DELAY = 30s`,
/// `DEFAULT_DELIVERY_MAX_DELAY = 6h`, `DEFAULT_MAX_DELIVERY_ATTEMPTS = 10`);
/// a future task revisiting `DeliveryWorker`'s own boundary is the right
/// place to add the constructor parameter this would need.
#[derive(Debug, Clone, PartialEq)]
pub struct FederationConfig {
    /// Whether authorized fetch (signed GET) is required for ActivityPub
    /// representation requests (Requirement 6.4). Defaults to `false`: a
    /// freshly configured instance should serve public AP documents
    /// without requiring every fetcher to pre-negotiate signing.
    pub secure_mode: bool,
    /// How long a resolved remote public key stays valid in
    /// `remote_public_keys` before the next verification re-fetches it
    /// (`federation.public_key_cache_ttl`, design.md's literal config key;
    /// Requirement 2.4). Defaults to 24 hours, mirroring
    /// `crate::federation::signatures::DEFAULT_PUBLIC_KEY_CACHE_TTL`
    /// (duplicated as a plain seconds constant here rather than importing
    /// that `time::Duration` constant, so this foundational, early-loaded
    /// module does not gain a dependency on `crate::federation`).
    pub public_key_cache_ttl: Duration,
    /// How many days a `received_activities` row is kept before the
    /// periodic pruning task deletes it
    /// (`federation.received_activity_retention_days`, design.md's literal
    /// config key; Requirement 7.4). Defaults to 14 days, mirroring
    /// `crate::federation::inbound::DEFAULT_RECEIVED_ACTIVITY_RETENTION`
    /// (see this field's sibling doc comment for why that constant is not
    /// imported directly).
    pub received_activity_retention_days: u32,
}

/// media-pipeline's startup settings (task 1.2, design.md's Modified Files:
/// "起動設定にメディア保管ルート・アップロード上限サイズ・サムネイル寸法・
/// 対応形式・ワーカー並行度/再試行上限・処理ジョブのリース期間
/// （`lease_duration`。クラッシュしたワーカーからジョブを再取得するまでの
/// 猶予。既定は想定処理時間を十分に上回る値、例: 5 分）を追加").
/// core-runtime only hosts these fields; it does not itself know how a
/// storage root or thumbnail dimension is used — `LocalFsStore` (task 2.2,
/// Requirement 5.2), `MediaService` (task 4.1, Requirement 1.4),
/// `PureRustImageProcessor` (task 2.3, Requirement 6.1), and
/// `ProcessingWorker`/`ProcessingJobQueue` (tasks 3.2/4.3, Requirement 4.2)
/// are the eventual consumers, mirroring `FederationConfig`'s precedent of
/// a downstream-consumed startup settings group hosted here without this
/// module depending on `crate::media`.
///
/// Unlike `ActorConfig`/`OwnerConfig`/`OauthConfig`, no field here is
/// secret-bearing (no credentials or key material), so nothing is wrapped
/// in [`Secret`]. Every field also has a safe default (mirroring
/// `FederationConfig`'s "defaults are provided so a minimal config still
/// boots" precedent): none of Requirements 1.4, 4.2, 5.2, or 6.1 mandate a
/// value with no safe fallback, unlike `database.url`/`actor.kek`/
/// `owner.password`/`oauth.token_hash_key`, which have no safe default to
/// fall back to. Consequently this struct — like `FederationConfig` —
/// contributes no `ConfigIssue::Missing` cases, only `Malformed` ones for
/// values present but not shaped as expected.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaConfig {
    /// Filesystem directory under which a later `LocalFsStore` (task 2.2,
    /// Requirement 5.2) persists original media and derivatives, keyed by a
    /// path derived deterministically from the media identifier (design.md
    /// Physical Data Model: `object_key`/`thumb_key`). Defaults to
    /// `media_storage`, a relative path resolved against the process's
    /// current working directory — mirroring [`DEFAULT_CONFIG_PATH`]'s own
    /// relative-path convention, so a freshly cloned instance boots without
    /// an operator having to pre-provision an absolute path.
    pub storage_root: PathBuf,
    /// Maximum accepted upload size in bytes; an upload exceeding this is
    /// rejected before storage (Requirement 1.4, consumed by a later
    /// `MediaService::accept_upload`). Defaults to 10 MiB
    /// (`10 * 1024 * 1024` bytes), a generous ceiling for this MVP's
    /// image-only scope (Requirement 10.3 — no video/audio to size for
    /// yet).
    pub max_upload_size_bytes: u64,
    /// Target thumbnail width in pixels, consumed by a later
    /// `PureRustImageProcessor::process_image` (Requirement 6.1). Defaults
    /// to 400, alongside [`Self::thumbnail_target_height`].
    pub thumbnail_target_width: u32,
    /// Target thumbnail height in pixels, consumed the same way as
    /// [`Self::thumbnail_target_width`] (Requirement 6.1). Defaults to 400.
    pub thumbnail_target_height: u32,
    /// Content types accepted by upload validation; anything else is
    /// rejected as an unsupported format (Requirement 1.4, consumed by a
    /// later `MediaService::accept_upload`). Supplied as a comma-separated
    /// list (matching this module's convention of scalar-string TOML/env
    /// values — see [`MergedSource::get`]'s doc comment for why array-typed
    /// TOML values are not supported here). Defaults to the four raster
    /// formats a later pure-Rust `MediaProcessor` (task 2.3, Requirements
    /// 10.2, 10.3) is expected to decode without native dependencies:
    /// `image/jpeg`, `image/png`, `image/gif`, `image/webp`.
    pub supported_formats: Vec<String>,
    /// Number of concurrent processing workers a later `MediaModule`
    /// wiring (task 5.2) spawns to consume the processing job queue
    /// (Requirement 4.2's exclusive per-job claim only matters once 1+
    /// workers may run concurrently). Defaults to 2, a modest concurrency
    /// befitting this project's single-server ("一人鯖") deployment
    /// target.
    pub worker_concurrency: u32,
    /// Maximum retry attempts for a processing job before it is moved to a
    /// failed state, consumed by a later
    /// `ProcessingJobQueue::fail_or_retry` (design.md's `max_attempts: u32`
    /// parameter; Requirement 4.5, referenced from this task's boundary via
    /// Requirement 4.2's exclusive-claim/retry semantics). Defaults to 5.
    pub max_retry_attempts: u32,
    /// Grace period after a processing job's `locked_at` before a worker
    /// crash is presumed and another worker may reclaim the job (design.md's
    /// `lease_duration` parameter to `ProcessingJobQueue::claim_due`;
    /// Requirement 4.2). Defaults to 5 minutes, matching the task text's own
    /// example of a value "well above the expected processing time"
    /// (処理ジョブのリース期間の既定は想定処理時間を十分に上回る値、例: 5
    /// 分).
    pub lease_duration: Duration,
}

/// Logging/diagnostics startup settings.
#[derive(Debug, Clone, PartialEq)]
pub struct LogConfig {
    /// Minimum log level to emit. Defaults to `info`.
    pub level: LogLevel,
    /// Whether to emit executed SQL as diagnostic-level output (Requirement
    /// 7.3, consumed by task 3.1). Defaults to `false`.
    pub sql_diagnostic: bool,
}

/// Supported log verbosity levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    fn parse(input: &str) -> Option<Self> {
        match input.trim().to_ascii_lowercase().as_str() {
            "trace" => Some(LogLevel::Trace),
            "debug" => Some(LogLevel::Debug),
            "info" => Some(LogLevel::Info),
            "warn" | "warning" => Some(LogLevel::Warn),
            "error" => Some(LogLevel::Error),
            _ => None,
        }
    }
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            LogLevel::Trace => "trace",
            LogLevel::Debug => "debug",
            LogLevel::Info => "info",
            LogLevel::Warn => "warn",
            LogLevel::Error => "error",
        };
        f.write_str(s)
    }
}

/// A single problem found with one configuration field: either it was
/// required and absent, or it was present but did not parse into the
/// expected shape. Kept distinct per Requirements 2.3 (missing) and 2.4
/// (malformed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigIssue {
    /// A required field was not supplied by either the TOML file or an
    /// environment variable. `field` is the dotted path, e.g. `server.domain`.
    Missing { field: String },
    /// A field was supplied but its value did not have the expected shape.
    /// `field` is the dotted path; `reason` is a human-readable diagnostic
    /// that intentionally never echoes secret-bearing raw values.
    Malformed { field: String, reason: String },
}

impl fmt::Display for ConfigIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigIssue::Missing { field } => {
                write!(f, "missing required field: {field}")
            }
            ConfigIssue::Malformed { field, reason } => {
                write!(f, "invalid field {field}: {reason}")
            }
        }
    }
}

/// Aggregated startup-configuration validation failure. Carries every
/// [`ConfigIssue`] found in a single pass so operators can fix all problems
/// at once instead of one-at-a-time (Requirements 2.3, 2.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    pub issues: Vec<ConfigIssue>,
}

impl ConfigError {
    fn new(issues: Vec<ConfigIssue>) -> Self {
        debug_assert!(
            !issues.is_empty(),
            "ConfigError must carry at least one issue"
        );
        ConfigError { issues }
    }

    /// Dotted field paths that were missing.
    pub fn missing_fields(&self) -> impl Iterator<Item = &str> {
        self.issues.iter().filter_map(|issue| match issue {
            ConfigIssue::Missing { field } => Some(field.as_str()),
            ConfigIssue::Malformed { .. } => None,
        })
    }

    /// Dotted field paths that were malformed.
    pub fn malformed_fields(&self) -> impl Iterator<Item = &str> {
        self.issues.iter().filter_map(|issue| match issue {
            ConfigIssue::Malformed { field, .. } => Some(field.as_str()),
            ConfigIssue::Missing { .. } => None,
        })
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "invalid startup configuration:")?;
        for issue in &self.issues {
            writeln!(f, "  - {issue}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ConfigError {}

/// Reads the startup TOML config file (if present) and process environment
/// variables, merges them with environment variables taking precedence, and
/// validates the result.
///
/// Missing config file is not itself an error: an operator may configure
/// entirely via environment variables. Required-field or malformed-field
/// problems are reported via [`ConfigError`] regardless of source.
pub fn load_config() -> Result<AppConfig, ConfigError> {
    let path =
        std::env::var("KAWASEMI_CONFIG_PATH").unwrap_or_else(|_| DEFAULT_CONFIG_PATH.to_string());
    let toml_text = std::fs::read_to_string(&path).ok();
    let env: HashMap<String, String> = std::env::vars()
        .filter(|(key, _)| key.starts_with(ENV_PREFIX))
        .collect();
    load_config_from(toml_text.as_deref(), &env)
}

/// Testable core of [`load_config`]: takes explicit TOML text and an
/// explicit environment-variable map instead of touching real process state,
/// so unit tests can exercise merge/validation behavior deterministically
/// and in parallel without mutating (and racing on) `std::env`.
fn load_config_from(
    toml_text: Option<&str>,
    env: &HashMap<String, String>,
) -> Result<AppConfig, ConfigError> {
    // Note: parse as `toml::Table` (the document root), not `toml::Value` —
    // `Value`'s `FromStr` parses a single value expression, not a full
    // multi-table document, and errors on top-level `[section]` headers.
    let toml_value: toml::Value = match toml_text {
        Some(text) => match text.parse::<toml::Table>() {
            Ok(table) => toml::Value::Table(table),
            Err(e) => {
                return Err(ConfigError::new(vec![ConfigIssue::Malformed {
                    field: "<config file>".to_string(),
                    reason: format!("could not parse TOML: {e}"),
                }]));
            }
        },
        None => toml::Value::Table(Default::default()),
    };
    let source = MergedSource {
        toml: toml_value,
        env,
    };

    let mut issues = Vec::new();

    let domain = required_string(&source, "server.domain", validate_domain, &mut issues);
    let bind_addr = optional(
        &source,
        "server.bind_addr",
        default_bind_addr(),
        |raw| raw.parse::<SocketAddr>().map_err(|e| e.to_string()),
        &mut issues,
    );
    let shutdown_grace = optional(
        &source,
        "server.shutdown_grace_secs",
        Duration::from_secs(30),
        parse_secs,
        &mut issues,
    );

    let url = required_string(&source, "database.url", validate_db_url, &mut issues);
    let max_connections = optional(
        &source,
        "database.max_connections",
        10u32,
        |raw| raw.parse::<u32>().map_err(|e| e.to_string()),
        &mut issues,
    );
    let acquire_timeout = optional(
        &source,
        "database.acquire_timeout_secs",
        Duration::from_secs(5),
        parse_secs,
        &mut issues,
    );

    let kek = required(&source, "actor.kek", validate_kek, &mut issues);

    let owner_password = required_string(
        &source,
        "owner.password",
        validate_owner_password,
        &mut issues,
    );
    let token_hash_key = required(
        &source,
        "oauth.token_hash_key",
        validate_token_hash_key,
        &mut issues,
    );

    let federation_secure_mode = optional(
        &source,
        "federation.secure_mode",
        false,
        parse_bool,
        &mut issues,
    );
    let federation_public_key_cache_ttl = optional(
        &source,
        "federation.public_key_cache_ttl",
        Duration::from_secs(DEFAULT_FEDERATION_PUBLIC_KEY_CACHE_TTL_SECS),
        parse_secs,
        &mut issues,
    );
    let federation_received_activity_retention_days = optional(
        &source,
        "federation.received_activity_retention_days",
        DEFAULT_FEDERATION_RECEIVED_ACTIVITY_RETENTION_DAYS,
        |raw| raw.parse::<u32>().map_err(|e| e.to_string()),
        &mut issues,
    );

    let media_storage_root = optional(
        &source,
        "media.storage_root",
        default_media_storage_root(),
        validate_media_storage_root,
        &mut issues,
    );
    let media_max_upload_size_bytes = optional(
        &source,
        "media.max_upload_size_bytes",
        DEFAULT_MEDIA_MAX_UPLOAD_SIZE_BYTES,
        parse_max_upload_size_bytes,
        &mut issues,
    );
    let media_thumbnail_target_width = optional(
        &source,
        "media.thumbnail_target_width",
        DEFAULT_MEDIA_THUMBNAIL_TARGET_WIDTH,
        parse_thumbnail_dimension,
        &mut issues,
    );
    let media_thumbnail_target_height = optional(
        &source,
        "media.thumbnail_target_height",
        DEFAULT_MEDIA_THUMBNAIL_TARGET_HEIGHT,
        parse_thumbnail_dimension,
        &mut issues,
    );
    let media_supported_formats = optional(
        &source,
        "media.supported_formats",
        default_media_supported_formats(),
        parse_supported_formats,
        &mut issues,
    );
    let media_worker_concurrency = optional(
        &source,
        "media.worker_concurrency",
        DEFAULT_MEDIA_WORKER_CONCURRENCY,
        parse_worker_concurrency,
        &mut issues,
    );
    let media_max_retry_attempts = optional(
        &source,
        "media.max_retry_attempts",
        DEFAULT_MEDIA_MAX_RETRY_ATTEMPTS,
        |raw| raw.parse::<u32>().map_err(|e| e.to_string()),
        &mut issues,
    );
    let media_lease_duration = optional(
        &source,
        "media.lease_duration_secs",
        Duration::from_secs(DEFAULT_MEDIA_LEASE_DURATION_SECS),
        parse_secs,
        &mut issues,
    );

    let level = optional(
        &source,
        "log.level",
        LogLevel::Info,
        |raw| {
            LogLevel::parse(raw).ok_or_else(|| format!("'{raw}' is not a recognized log level (expected one of trace, debug, info, warn, error)"))
        },
        &mut issues,
    );
    let sql_diagnostic = optional(
        &source,
        "log.sql_diagnostic",
        false,
        parse_bool,
        &mut issues,
    );

    if !issues.is_empty() {
        return Err(ConfigError::new(issues));
    }

    Ok(AppConfig {
        server: ServerConfig {
            domain: domain.expect("validated above: no issues means all required fields present"),
            bind_addr: bind_addr.expect("validated above"),
            shutdown_grace: shutdown_grace.expect("validated above"),
        },
        database: DatabaseConfig {
            url: Secret::new(
                url.expect("validated above: no issues means all required fields present"),
            ),
            max_connections: max_connections.expect("validated above"),
            acquire_timeout: acquire_timeout.expect("validated above"),
        },
        log: LogConfig {
            level: level.expect("validated above"),
            sql_diagnostic: sql_diagnostic.expect("validated above"),
        },
        actor: ActorConfig {
            kek: Secret::new(
                kek.expect("validated above: no issues means all required fields present"),
            ),
        },
        owner: OwnerConfig {
            password: Secret::new(
                owner_password
                    .expect("validated above: no issues means all required fields present"),
            ),
        },
        oauth: OauthConfig {
            token_hash_key: Secret::new(
                token_hash_key
                    .expect("validated above: no issues means all required fields present"),
            ),
        },
        federation: FederationConfig {
            secure_mode: federation_secure_mode.expect("validated above"),
            public_key_cache_ttl: federation_public_key_cache_ttl.expect("validated above"),
            received_activity_retention_days: federation_received_activity_retention_days
                .expect("validated above"),
        },
        media: MediaConfig {
            storage_root: media_storage_root.expect("validated above"),
            max_upload_size_bytes: media_max_upload_size_bytes.expect("validated above"),
            thumbnail_target_width: media_thumbnail_target_width.expect("validated above"),
            thumbnail_target_height: media_thumbnail_target_height.expect("validated above"),
            supported_formats: media_supported_formats.expect("validated above"),
            worker_concurrency: media_worker_concurrency.expect("validated above"),
            max_retry_attempts: media_max_retry_attempts.expect("validated above"),
            lease_duration: media_lease_duration.expect("validated above"),
        },
    })
}

/// Default for `federation.public_key_cache_ttl`, in seconds (24 hours).
/// Mirrors `crate::federation::signatures::DEFAULT_PUBLIC_KEY_CACHE_TTL` —
/// see [`FederationConfig::public_key_cache_ttl`]'s doc comment for why this
/// is a plain duplicated constant rather than an import.
const DEFAULT_FEDERATION_PUBLIC_KEY_CACHE_TTL_SECS: u64 = 24 * 60 * 60;

/// Default for `federation.received_activity_retention_days` (14 days).
/// Mirrors `crate::federation::inbound::DEFAULT_RECEIVED_ACTIVITY_RETENTION`
/// — see [`FederationConfig::received_activity_retention_days`]'s doc
/// comment for why this is a plain duplicated constant rather than an
/// import.
const DEFAULT_FEDERATION_RECEIVED_ACTIVITY_RETENTION_DAYS: u32 = 14;

/// Default for `media.max_upload_size_bytes` (10 MiB). See
/// [`MediaConfig::max_upload_size_bytes`]'s doc comment for rationale.
const DEFAULT_MEDIA_MAX_UPLOAD_SIZE_BYTES: u64 = 10 * 1024 * 1024;

/// Default for `media.thumbnail_target_width` (400px). See
/// [`MediaConfig::thumbnail_target_width`]'s doc comment.
const DEFAULT_MEDIA_THUMBNAIL_TARGET_WIDTH: u32 = 400;

/// Default for `media.thumbnail_target_height` (400px). See
/// [`MediaConfig::thumbnail_target_height`]'s doc comment.
const DEFAULT_MEDIA_THUMBNAIL_TARGET_HEIGHT: u32 = 400;

/// Default for `media.worker_concurrency` (2 workers). See
/// [`MediaConfig::worker_concurrency`]'s doc comment for rationale.
const DEFAULT_MEDIA_WORKER_CONCURRENCY: u32 = 2;

/// Default for `media.max_retry_attempts` (5 attempts). See
/// [`MediaConfig::max_retry_attempts`]'s doc comment.
const DEFAULT_MEDIA_MAX_RETRY_ATTEMPTS: u32 = 5;

/// Default for `media.lease_duration_secs`, in seconds (5 minutes). See
/// [`MediaConfig::lease_duration`]'s doc comment for why this specific
/// value (task 1.2's own example of "well above the expected processing
/// time").
const DEFAULT_MEDIA_LEASE_DURATION_SECS: u64 = 5 * 60;

/// Default for `media.storage_root`: `media_storage`, resolved relative to
/// the process's current working directory. See
/// [`MediaConfig::storage_root`]'s doc comment for why a relative default is
/// safe here (mirrors [`DEFAULT_CONFIG_PATH`]'s own convention).
fn default_media_storage_root() -> PathBuf {
    PathBuf::from("media_storage")
}

/// Default for `media.supported_formats`: the four raster formats a later
/// pure-Rust `MediaProcessor` is expected to decode without native
/// dependencies. See [`MediaConfig::supported_formats`]'s doc comment.
fn default_media_supported_formats() -> Vec<String> {
    vec![
        "image/jpeg".to_string(),
        "image/png".to_string(),
        "image/gif".to_string(),
        "image/webp".to_string(),
    ]
}

fn default_bind_addr() -> SocketAddr {
    "0.0.0.0:3000".parse().expect("valid hardcoded default")
}

fn parse_secs(raw: &str) -> Result<Duration, String> {
    raw.trim()
        .parse::<u64>()
        .map(Duration::from_secs)
        .map_err(|e| format!("'{raw}' is not a whole number of seconds: {e}"))
}

fn parse_bool(raw: &str) -> Result<bool, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        other => Err(format!(
            "'{other}' is not a recognized boolean (expected true/false)"
        )),
    }
}

/// Validates a bare server domain: non-empty, no whitespace, not a URL
/// (no scheme), and composed of dot-separated alphanumeric/hyphen labels.
fn validate_domain(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("must not be empty".to_string());
    }
    if trimmed.chars().any(char::is_whitespace) {
        return Err("must not contain whitespace".to_string());
    }
    if trimmed.contains("://") {
        return Err("must be a bare domain, not a URL (remove the scheme)".to_string());
    }
    let labels_valid = trimmed.split('.').all(|label| {
        !label.is_empty() && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    });
    if !labels_valid || (trimmed != "localhost" && !trimmed.contains('.')) {
        return Err(format!("'{trimmed}' is not a valid domain"));
    }
    Ok(trimmed.to_string())
}

/// Validates a database connection string. Deliberately does not echo the
/// raw value in error messages: a malformed `postgres://` URL may still
/// carry embedded credentials, and Requirement 2.5 (secret masking, task
/// 2.2) is easiest to uphold if this boundary never puts secret-bearing
/// values into diagnostic text in the first place.
fn validate_db_url(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("must not be empty".to_string());
    }
    if !(trimmed.starts_with("postgres://") || trimmed.starts_with("postgresql://")) {
        return Err("must start with postgres:// or postgresql://".to_string());
    }
    Ok(trimmed.to_string())
}

/// Validates a Key-Encryption-Key: exactly 64 hexadecimal characters
/// (case-insensitive), decoding to a fixed 256-bit byte array. Deliberately
/// never echoes the raw value in error messages — a malformed value is
/// still potentially secret-bearing input, mirroring
/// [`validate_db_url`]'s same discipline (Requirement 2.5's masking
/// convention extended to this field).
///
/// Delegates its parsing rule to [`validate_hex_256_bits`], shared with
/// [`validate_token_hash_key`]: both fields are fixed-length secret material
/// for a keyed cryptographic primitive with no natural human-typable format
/// beyond hex, so the validation logic itself has nothing field-specific
/// left to differ on — only these doc comments (and the dotted config path
/// each is registered under) do.
fn validate_kek(raw: &str) -> Result<[u8; 32], String> {
    validate_hex_256_bits(raw)
}

/// Validates OAuth token-hashing material: exactly 64 hexadecimal characters
/// (case-insensitive), decoding to a fixed 256-bit byte array. Deliberately
/// never echoes the raw value in error messages, mirroring
/// [`validate_kek`]'s same discipline (Requirement 3.6: this key exists
/// specifically so tokens are never stored/logged in plaintext, so it must
/// not itself leak into a diagnostic message on malformed input).
///
/// See [`validate_kek`]'s doc comment for why this shares
/// [`validate_hex_256_bits`] rather than duplicating the parsing loop.
fn validate_token_hash_key(raw: &str) -> Result<[u8; 32], String> {
    validate_hex_256_bits(raw)
}

/// Decodes exactly 64 hexadecimal characters (case-insensitive) into a
/// fixed 256-bit byte array. Shared parsing rule for [`validate_kek`] and
/// [`validate_token_hash_key`] (see their doc comments for why sharing this
/// helper, rather than importing one validator into the other's module,
/// keeps the two concerns decoupled while still avoiding duplicated logic).
///
/// Operates on `char`s (not raw bytes) so a malformed value containing
/// multi-byte UTF-8 characters cannot panic on a byte boundary that splits
/// one — it is simply reported as the wrong length or a non-hex character,
/// like any other malformed input.
fn validate_hex_256_bits(raw: &str) -> Result<[u8; 32], String> {
    let chars: Vec<char> = raw.trim().chars().collect();
    if chars.len() != 64 {
        return Err(format!(
            "must be exactly 64 hexadecimal characters (a 256-bit key), got {} character(s)",
            chars.len()
        ));
    }

    let mut bytes = [0u8; 32];
    for (i, pair) in chars.chunks(2).enumerate() {
        let hex_pair: String = pair.iter().collect();
        bytes[i] = u8::from_str_radix(&hex_pair, 16)
            .map_err(|_| "must contain only hexadecimal characters (0-9, a-f, A-F)".to_string())?;
    }
    Ok(bytes)
}

/// Validates the owner's shared passphrase: non-empty and at least 8
/// characters after trimming. Deliberately never echoes the raw value in
/// error messages, mirroring [`validate_db_url`]/[`validate_kek`]'s same
/// discipline — a malformed (e.g. too-short) password is still
/// secret-bearing input.
///
/// Unlike `actor.kek`/`oauth.token_hash_key`, a human-chosen passphrase has
/// no natural fixed encoding to validate structurally; the 8-character
/// floor only catches obviously-trivial misconfiguration (e.g. a
/// placeholder like `"x"` left in a config file) and is not a substitute
/// for the owner choosing a genuinely strong passphrase.
fn validate_owner_password(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("must not be empty".to_string());
    }
    if trimmed.chars().count() < 8 {
        return Err("must be at least 8 characters".to_string());
    }
    Ok(trimmed.to_string())
}

/// Validates `media.storage_root`: non-empty after trimming. Deliberately
/// permissive about shape (relative or absolute, existing or not — a later
/// `LocalFsStore` is responsible for creating/validating the directory at
/// use time); this validator only rejects the degenerate empty-string case,
/// mirroring [`validate_domain`]'s minimal non-emptiness floor.
fn validate_media_storage_root(raw: &str) -> Result<PathBuf, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("must not be empty".to_string());
    }
    Ok(PathBuf::from(trimmed))
}

/// Validates `media.max_upload_size_bytes`: a positive whole number of
/// bytes. Zero is rejected — a zero-byte ceiling would reject every upload,
/// which is never the intent of a size *limit* (Requirement 1.4).
fn parse_max_upload_size_bytes(raw: &str) -> Result<u64, String> {
    let value = raw
        .trim()
        .parse::<u64>()
        .map_err(|e| format!("'{raw}' is not a whole number of bytes: {e}"))?;
    if value == 0 {
        return Err("must be greater than 0".to_string());
    }
    Ok(value)
}

/// Validates a thumbnail target dimension (`media.thumbnail_target_width`/
/// `media.thumbnail_target_height`): a positive whole number of pixels.
/// Shared by both fields since the parsing rule is identical (Requirement
/// 6.1); only the dotted config path each is registered under differs.
fn parse_thumbnail_dimension(raw: &str) -> Result<u32, String> {
    let value = raw
        .trim()
        .parse::<u32>()
        .map_err(|e| format!("'{raw}' is not a whole number of pixels: {e}"))?;
    if value == 0 {
        return Err("must be greater than 0".to_string());
    }
    Ok(value)
}

/// Validates `media.supported_formats`: a comma-separated list of content
/// types, each trimmed of surrounding whitespace, with empty entries
/// dropped. At least one entry must remain — an empty accepted-format list
/// would reject every upload, which is never the intent of Requirement
/// 1.4's "unsupported format" check (that check exists to reject some
/// formats, not all of them).
fn parse_supported_formats(raw: &str) -> Result<Vec<String>, String> {
    let formats: Vec<String> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    if formats.is_empty() {
        return Err("must list at least one supported content type".to_string());
    }
    Ok(formats)
}

/// Validates `media.worker_concurrency`: a whole number of at least 1. Zero
/// workers would mean the processing job queue (Requirement 4.2) is never
/// consumed, which is never a valid startup intent — an operator who wants
/// processing paused should stop the process, not configure a
/// zero-concurrency worker pool.
fn parse_worker_concurrency(raw: &str) -> Result<u32, String> {
    let value = raw
        .trim()
        .parse::<u32>()
        .map_err(|e| format!("'{raw}' is not a whole number: {e}"))?;
    if value == 0 {
        return Err("must be at least 1".to_string());
    }
    Ok(value)
}

/// Merged view over a parsed TOML document and an environment-variable map,
/// resolving a dotted field path to its raw string value with environment
/// variables taking precedence over the TOML file (Requirement 2.2).
struct MergedSource<'a> {
    toml: toml::Value,
    env: &'a HashMap<String, String>,
}

impl MergedSource<'_> {
    /// Resolves `path` (e.g. `"server.domain"`) to its raw string
    /// representation, or `Ok(None)` if neither source supplies it.
    /// Returns `Err` only if the TOML value exists but has a shape (array,
    /// inline table, etc.) that cannot be represented as a scalar string.
    fn get(&self, path: &str) -> Result<Option<String>, String> {
        let env_key = to_env_key(path);
        if let Some(v) = self.env.get(&env_key) {
            return Ok(Some(v.clone()));
        }

        let mut current = &self.toml;
        for segment in path.split('.') {
            match current.get(segment) {
                Some(v) => current = v,
                None => return Ok(None),
            }
        }

        match current {
            toml::Value::String(s) => Ok(Some(s.clone())),
            toml::Value::Integer(i) => Ok(Some(i.to_string())),
            toml::Value::Float(f) => Ok(Some(f.to_string())),
            toml::Value::Boolean(b) => Ok(Some(b.to_string())),
            other => Err(format!("unsupported TOML value type: {}", other.type_str())),
        }
    }
}

fn to_env_key(path: &str) -> String {
    format!(
        "{ENV_PREFIX}{}",
        path.to_ascii_uppercase().replace('.', "_")
    )
}

/// Resolves and validates a required field of any type, recording a
/// [`ConfigIssue`] into `issues` (either `Missing` or `Malformed`) instead of
/// short-circuiting, so multiple problems can be reported from one
/// `load_config` call. Generic over the validated type `T` so both a plain
/// `String` field (via [`required_string`]) and a differently-shaped field
/// (e.g. `actor.kek`'s `[u8; 32]`, via [`validate_kek`]) share this one
/// implementation.
fn required<T>(
    source: &MergedSource,
    field: &str,
    validate: impl Fn(&str) -> Result<T, String>,
    issues: &mut Vec<ConfigIssue>,
) -> Option<T> {
    match source.get(field) {
        Ok(Some(raw)) => match validate(&raw) {
            Ok(v) => Some(v),
            Err(reason) => {
                issues.push(ConfigIssue::Malformed {
                    field: field.to_string(),
                    reason,
                });
                None
            }
        },
        Ok(None) => {
            issues.push(ConfigIssue::Missing {
                field: field.to_string(),
            });
            None
        }
        Err(reason) => {
            issues.push(ConfigIssue::Malformed {
                field: field.to_string(),
                reason,
            });
            None
        }
    }
}

/// [`required`] specialized for a plain `String` field, matching this
/// module's existing validators (`validate_domain`/`validate_db_url`), which
/// are plain `fn(&str) -> Result<String, String>` items.
fn required_string(
    source: &MergedSource,
    field: &str,
    validate: fn(&str) -> Result<String, String>,
    issues: &mut Vec<ConfigIssue>,
) -> Option<String> {
    required(source, field, validate, issues)
}

/// Resolves and validates an optional field, falling back to `default` when
/// absent from both sources. Malformed values (present but unparsable) are
/// still recorded as errors — only absence is tolerated.
fn optional<T>(
    source: &MergedSource,
    field: &str,
    default: T,
    parse: impl Fn(&str) -> Result<T, String>,
    issues: &mut Vec<ConfigIssue>,
) -> Option<T> {
    match source.get(field) {
        Ok(Some(raw)) => match parse(&raw) {
            Ok(v) => Some(v),
            Err(reason) => {
                issues.push(ConfigIssue::Malformed {
                    field: field.to_string(),
                    reason,
                });
                None
            }
        },
        Ok(None) => Some(default),
        Err(reason) => {
            issues.push(ConfigIssue::Malformed {
                field: field.to_string(),
                reason,
            });
            None
        }
    }
}
