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
//! secret-bearing.

use std::collections::HashMap;
use std::fmt;
use std::net::SocketAddr;
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
    })
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
/// Operates on `char`s (not raw bytes) so a malformed value containing
/// multi-byte UTF-8 characters cannot panic on a byte boundary that splits
/// one — it is simply reported as the wrong length or a non-hex character,
/// like any other malformed input.
fn validate_kek(raw: &str) -> Result<[u8; 32], String> {
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
