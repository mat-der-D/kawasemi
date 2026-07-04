//! Startup configuration loading, merging, and validation (Requirement 2).
//!
//! Configuration is assembled from two sources: a TOML file and process
//! environment variables. Where the same setting is present in both, the
//! environment variable wins (Requirement 2.2). This module owns only
//! *startup* configuration (server domain, database connection, logging);
//! *operational* configuration stored in the database belongs to a later
//! spec and is never read or written here (Requirement 2.6).
//!
//! Secret-bearing values (currently `DatabaseConfig::url`) are held as a
//! plain `String` in this task. A subsequent, dependent task introduces a
//! `Secret<T>` wrapper (`src/config/secret.rs`) and applies it to these
//! fields so they are never exposed via `Debug`/`Display` (Requirement 2.5).
//!
//! `load_config()` (the real, IO-backed entry point) is not yet called
//! from anywhere: wiring it into the startup sequence is task 7.4's job
//! (Bootstrap composition root). Until then this module is exercised only
//! by its own unit tests, so the module is allowed to be otherwise unused.

#![allow(dead_code)]

use std::collections::HashMap;
use std::fmt;
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

/// Prefix used for every environment variable recognized as a startup
/// configuration override.
pub const ENV_PREFIX: &str = "KAWASEMI_";

/// Environment variable naming the TOML file to read. Defaults to
/// `kawasemi.toml` in the process's current working directory.
pub const CONFIG_PATH_ENV: &str = "KAWASEMI_CONFIG_PATH";

const DEFAULT_CONFIG_PATH: &str = "kawasemi.toml";

const DEFAULT_BIND_ADDR: &str = "0.0.0.0:8080";
const DEFAULT_SHUTDOWN_GRACE_SECS: u64 = 30;
const DEFAULT_DB_MAX_CONNECTIONS: u32 = 10;
const DEFAULT_DB_ACQUIRE_TIMEOUT_SECS: u64 = 5;
const DEFAULT_LOG_LEVEL: &str = "info";
const DEFAULT_LOG_SQL_DIAGNOSTICS: bool = false;

/// Fully validated, immutable startup configuration (Requirement 2.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub log: LogConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    /// The public domain this server is reachable at. Required.
    pub domain: String,
    pub bind_addr: SocketAddr,
    pub shutdown_grace: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseConfig {
    /// The PostgreSQL connection string. Required.
    pub url: String,
    pub max_connections: u32,
    pub acquire_timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogConfig {
    pub level: LogLevel,
    pub sql_diagnostics: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    fn parse(raw: &str) -> Option<Self> {
        match raw.to_ascii_lowercase().as_str() {
            "trace" => Some(Self::Trace),
            "debug" => Some(Self::Debug),
            "info" => Some(Self::Info),
            "warn" => Some(Self::Warn),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

/// A single problem found while assembling configuration: either a
/// required field missing from every source, or a field whose value does
/// not conform to the expected format. Carrying the field name lets the
/// diagnostic message pinpoint exactly what needs fixing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigIssue {
    pub field: &'static str,
    pub kind: ConfigIssueKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigIssueKind {
    Missing,
    Invalid(String),
}

impl fmt::Display for ConfigIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            ConfigIssueKind::Missing => write!(f, "{} is missing", self.field),
            ConfigIssueKind::Invalid(reason) => write!(f, "{} is invalid: {reason}", self.field),
        }
    }
}

/// Startup configuration could not be assembled. Carries one entry per
/// problem found, distinguishing missing required items from malformed
/// ones, so operators can fix everything in a single pass
/// (Requirement 2.3, 2.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError(pub Vec<ConfigIssue>);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "invalid startup configuration:")?;
        for issue in &self.0 {
            writeln!(f, "  - {issue}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ConfigError {}

/// Reads the TOML file (if any) and environment variables from the real
/// process environment, merges them (environment wins), and validates the
/// result.
pub fn load_config() -> Result<AppConfig, ConfigError> {
    let config_path = std::env::var(CONFIG_PATH_ENV).unwrap_or_else(|_| DEFAULT_CONFIG_PATH.to_string());
    let toml_cfg = read_toml_file(Path::new(&config_path))?;
    let env: HashMap<String, String> = std::env::vars()
        .filter(|(key, _)| key.starts_with(ENV_PREFIX))
        .collect();
    build_config(toml_cfg, &env)
}

/// Intermediate representation of the TOML file. Every field is optional
/// here; absence is only an error once merged with the environment and
/// checked against the required-field rules in [`build_config`].
#[derive(Debug, Default, serde::Deserialize)]
struct TomlConfig {
    #[serde(default)]
    server: TomlServer,
    #[serde(default)]
    database: TomlDatabase,
    #[serde(default)]
    log: TomlLog,
}

#[derive(Debug, Default, serde::Deserialize)]
struct TomlServer {
    domain: Option<String>,
    bind_addr: Option<String>,
    shutdown_grace_secs: Option<u64>,
}

#[derive(Debug, Default, serde::Deserialize)]
struct TomlDatabase {
    url: Option<String>,
    max_connections: Option<u32>,
    acquire_timeout_secs: Option<u64>,
}

#[derive(Debug, Default, serde::Deserialize)]
struct TomlLog {
    level: Option<String>,
    sql_diagnostics: Option<bool>,
}

fn read_toml_file(path: &Path) -> Result<TomlConfig, ConfigError> {
    match std::fs::read_to_string(path) {
        Ok(contents) => toml::from_str(&contents).map_err(|err| {
            ConfigError(vec![ConfigIssue {
                field: "config_file",
                kind: ConfigIssueKind::Invalid(format!("failed to parse TOML: {err}")),
            }])
        }),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(TomlConfig::default()),
        Err(err) => Err(ConfigError(vec![ConfigIssue {
            field: "config_file",
            kind: ConfigIssueKind::Invalid(format!("failed to read file: {err}")),
        }])),
    }
}

/// Returns the environment override for `key`, if present and non-empty.
fn env_str<'a>(env: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    env.get(key).map(|value| value.as_str())
}

/// Merges an environment override (if present) with the TOML value.
/// The environment variable always wins (Requirement 2.2).
fn merged_string(env: &HashMap<String, String>, key: &str, toml_value: Option<String>) -> Option<String> {
    match env_str(env, key) {
        Some(value) => Some(value.to_string()),
        None => toml_value,
    }
}

/// Builds and validates an [`AppConfig`] from an already-parsed TOML
/// document and a map of `KAWASEMI_`-prefixed environment variables. Kept
/// separate from [`load_config`] so the merge and validation rules can be
/// unit-tested without touching real files or the real process
/// environment.
fn build_config(toml_cfg: TomlConfig, env: &HashMap<String, String>) -> Result<AppConfig, ConfigError> {
    let mut issues = Vec::new();

    // --- server.domain (required) ---
    let domain = match merged_string(env, "KAWASEMI_SERVER_DOMAIN", toml_cfg.server.domain) {
        None => {
            issues.push(ConfigIssue { field: "server.domain", kind: ConfigIssueKind::Missing });
            None
        }
        Some(raw) if raw.trim().is_empty() => {
            issues.push(ConfigIssue {
                field: "server.domain",
                kind: ConfigIssueKind::Invalid("must not be empty".to_string()),
            });
            None
        }
        Some(raw) => Some(raw),
    };

    // --- server.bind_addr (optional, defaulted) ---
    let bind_addr_raw = merged_string(env, "KAWASEMI_SERVER_BIND_ADDR", toml_cfg.server.bind_addr)
        .unwrap_or_else(|| DEFAULT_BIND_ADDR.to_string());
    let bind_addr = match bind_addr_raw.parse::<SocketAddr>() {
        Ok(addr) => Some(addr),
        Err(err) => {
            issues.push(ConfigIssue {
                field: "server.bind_addr",
                kind: ConfigIssueKind::Invalid(format!("not a valid socket address: {err}")),
            });
            None
        }
    };

    // --- server.shutdown_grace_secs (optional, defaulted) ---
    let shutdown_grace_raw = match env_str(env, "KAWASEMI_SERVER_SHUTDOWN_GRACE_SECS") {
        Some(value) => Some(value.to_string()),
        None => toml_cfg.server.shutdown_grace_secs.map(|secs| secs.to_string()),
    };
    let shutdown_grace = match shutdown_grace_raw {
        None => Some(Duration::from_secs(DEFAULT_SHUTDOWN_GRACE_SECS)),
        Some(raw) => match raw.parse::<u64>() {
            Ok(secs) => Some(Duration::from_secs(secs)),
            Err(err) => {
                issues.push(ConfigIssue {
                    field: "server.shutdown_grace_secs",
                    kind: ConfigIssueKind::Invalid(format!("not a valid non-negative integer: {err}")),
                });
                None
            }
        },
    };

    // --- database.url (required) ---
    let db_url = match merged_string(env, "KAWASEMI_DATABASE_URL", toml_cfg.database.url) {
        None => {
            issues.push(ConfigIssue { field: "database.url", kind: ConfigIssueKind::Missing });
            None
        }
        Some(raw) if raw.trim().is_empty() => {
            issues.push(ConfigIssue {
                field: "database.url",
                kind: ConfigIssueKind::Invalid("must not be empty".to_string()),
            });
            None
        }
        Some(raw) if !(raw.starts_with("postgres://") || raw.starts_with("postgresql://")) => {
            issues.push(ConfigIssue {
                field: "database.url",
                kind: ConfigIssueKind::Invalid(
                    "must start with postgres:// or postgresql://".to_string(),
                ),
            });
            None
        }
        Some(raw) => Some(raw),
    };

    // --- database.max_connections (optional, defaulted) ---
    let max_connections_raw = match env_str(env, "KAWASEMI_DATABASE_MAX_CONNECTIONS") {
        Some(value) => Some(value.to_string()),
        None => toml_cfg.database.max_connections.map(|n| n.to_string()),
    };
    let max_connections = match max_connections_raw {
        None => Some(DEFAULT_DB_MAX_CONNECTIONS),
        Some(raw) => match raw.parse::<u32>() {
            Ok(0) => {
                issues.push(ConfigIssue {
                    field: "database.max_connections",
                    kind: ConfigIssueKind::Invalid("must be greater than zero".to_string()),
                });
                None
            }
            Ok(n) => Some(n),
            Err(err) => {
                issues.push(ConfigIssue {
                    field: "database.max_connections",
                    kind: ConfigIssueKind::Invalid(format!("not a valid positive integer: {err}")),
                });
                None
            }
        },
    };

    // --- database.acquire_timeout_secs (optional, defaulted) ---
    let acquire_timeout_raw = match env_str(env, "KAWASEMI_DATABASE_ACQUIRE_TIMEOUT_SECS") {
        Some(value) => Some(value.to_string()),
        None => toml_cfg.database.acquire_timeout_secs.map(|secs| secs.to_string()),
    };
    let acquire_timeout = match acquire_timeout_raw {
        None => Some(Duration::from_secs(DEFAULT_DB_ACQUIRE_TIMEOUT_SECS)),
        Some(raw) => match raw.parse::<u64>() {
            Ok(0) => {
                issues.push(ConfigIssue {
                    field: "database.acquire_timeout_secs",
                    kind: ConfigIssueKind::Invalid("must be greater than zero".to_string()),
                });
                None
            }
            Ok(secs) => Some(Duration::from_secs(secs)),
            Err(err) => {
                issues.push(ConfigIssue {
                    field: "database.acquire_timeout_secs",
                    kind: ConfigIssueKind::Invalid(format!("not a valid positive integer: {err}")),
                });
                None
            }
        },
    };

    // --- log.level (optional, defaulted) ---
    let log_level_raw = merged_string(env, "KAWASEMI_LOG_LEVEL", toml_cfg.log.level)
        .unwrap_or_else(|| DEFAULT_LOG_LEVEL.to_string());
    let log_level = match LogLevel::parse(&log_level_raw) {
        Some(level) => Some(level),
        None => {
            issues.push(ConfigIssue {
                field: "log.level",
                kind: ConfigIssueKind::Invalid(format!(
                    "must be one of trace, debug, info, warn, error (got {log_level_raw:?})"
                )),
            });
            None
        }
    };

    // --- log.sql_diagnostics (optional, defaulted) ---
    let sql_diagnostics_raw = match env_str(env, "KAWASEMI_LOG_SQL_DIAGNOSTICS") {
        Some(value) => Some(value.to_string()),
        None => toml_cfg.log.sql_diagnostics.map(|flag| flag.to_string()),
    };
    let sql_diagnostics = match sql_diagnostics_raw {
        None => Some(DEFAULT_LOG_SQL_DIAGNOSTICS),
        Some(raw) => match raw.parse::<bool>() {
            Ok(flag) => Some(flag),
            Err(err) => {
                issues.push(ConfigIssue {
                    field: "log.sql_diagnostics",
                    kind: ConfigIssueKind::Invalid(format!("not a valid boolean (true/false): {err}")),
                });
                None
            }
        },
    };

    if !issues.is_empty() {
        return Err(ConfigError(issues));
    }

    Ok(AppConfig {
        server: ServerConfig {
            domain: domain.expect("validated above: no issues means domain is Some"),
            bind_addr: bind_addr.expect("validated above: no issues means bind_addr is Some"),
            shutdown_grace: shutdown_grace.expect("validated above: no issues means shutdown_grace is Some"),
        },
        database: DatabaseConfig {
            url: db_url.expect("validated above: no issues means db_url is Some"),
            max_connections: max_connections
                .expect("validated above: no issues means max_connections is Some"),
            acquire_timeout: acquire_timeout
                .expect("validated above: no issues means acquire_timeout is Some"),
        },
        log: LogConfig {
            level: log_level.expect("validated above: no issues means log_level is Some"),
            sql_diagnostics: sql_diagnostics
                .expect("validated above: no issues means sql_diagnostics is Some"),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn valid_toml() -> TomlConfig {
        TomlConfig {
            server: TomlServer {
                domain: Some("toml.example.org".to_string()),
                bind_addr: None,
                shutdown_grace_secs: None,
            },
            database: TomlDatabase {
                url: Some("postgres://toml/db".to_string()),
                max_connections: None,
                acquire_timeout_secs: None,
            },
            log: TomlLog { level: None, sql_diagnostics: None },
        }
    }

    #[test]
    fn env_var_overrides_toml_value_for_same_setting() {
        let toml_cfg = valid_toml();
        let env = env_of(&[("KAWASEMI_SERVER_DOMAIN", "env.example.org")]);

        let cfg = build_config(toml_cfg, &env).expect("should build with domain from either source");

        assert_eq!(cfg.server.domain, "env.example.org");
        // Sanity: the TOML-only database.url still comes through untouched.
        assert_eq!(cfg.database.url, "postgres://toml/db");
    }

    #[test]
    fn env_var_overrides_toml_value_for_database_url_too() {
        let toml_cfg = valid_toml();
        let env = env_of(&[("KAWASEMI_DATABASE_URL", "postgres://env/db")]);

        let cfg = build_config(toml_cfg, &env).expect("should build");

        assert_eq!(cfg.database.url, "postgres://env/db");
    }

    #[test]
    fn toml_only_value_is_used_when_no_env_override_present() {
        let toml_cfg = valid_toml();
        let env = env_of(&[]);

        let cfg = build_config(toml_cfg, &env).expect("should build purely from toml");

        assert_eq!(cfg.server.domain, "toml.example.org");
        assert_eq!(cfg.database.url, "postgres://toml/db");
    }

    #[test]
    fn missing_required_fields_abort_with_named_issues() {
        let toml_cfg = TomlConfig::default();
        let env = env_of(&[]);

        let err = build_config(toml_cfg, &env).expect_err("both required fields are absent");

        assert!(
            err.0.iter().any(|issue| issue.field == "server.domain"
                && issue.kind == ConfigIssueKind::Missing),
            "expected a Missing issue for server.domain, got {:?}",
            err.0
        );
        assert!(
            err.0.iter().any(|issue| issue.field == "database.url"
                && issue.kind == ConfigIssueKind::Missing),
            "expected a Missing issue for database.url, got {:?}",
            err.0
        );
    }

    #[test]
    fn malformed_bind_addr_aborts_with_invalid_issue_distinct_from_missing() {
        let mut toml_cfg = valid_toml();
        toml_cfg.server.bind_addr = Some("not-a-socket-address".to_string());
        let env = env_of(&[]);

        let err = build_config(toml_cfg, &env).expect_err("bind_addr is malformed");

        let issue = err
            .0
            .iter()
            .find(|issue| issue.field == "server.bind_addr")
            .expect("expected an issue for server.bind_addr");
        match &issue.kind {
            ConfigIssueKind::Invalid(_) => {}
            ConfigIssueKind::Missing => panic!("expected Invalid, not Missing, for a malformed value"),
        }
    }

    #[test]
    fn malformed_database_url_scheme_aborts_with_invalid_issue() {
        let mut toml_cfg = valid_toml();
        toml_cfg.database.url = Some("mysql://wrong-scheme/db".to_string());
        let env = env_of(&[]);

        let err = build_config(toml_cfg, &env).expect_err("database.url has the wrong scheme");

        let issue = err
            .0
            .iter()
            .find(|issue| issue.field == "database.url")
            .expect("expected an issue for database.url");
        assert!(matches!(issue.kind, ConfigIssueKind::Invalid(_)));
    }

    #[test]
    fn malformed_log_level_aborts_with_invalid_issue() {
        let mut toml_cfg = valid_toml();
        toml_cfg.log.level = Some("verbose".to_string());
        let env = env_of(&[]);

        let err = build_config(toml_cfg, &env).expect_err("log.level is not a recognized value");

        let issue = err
            .0
            .iter()
            .find(|issue| issue.field == "log.level")
            .expect("expected an issue for log.level");
        assert!(matches!(issue.kind, ConfigIssueKind::Invalid(_)));
    }

    #[test]
    fn defaults_are_applied_when_optional_fields_are_absent() {
        let toml_cfg = valid_toml();
        let env = env_of(&[]);

        let cfg = build_config(toml_cfg, &env).expect("should build with defaults");

        assert_eq!(cfg.server.bind_addr, DEFAULT_BIND_ADDR.parse::<SocketAddr>().unwrap());
        assert_eq!(cfg.server.shutdown_grace, Duration::from_secs(DEFAULT_SHUTDOWN_GRACE_SECS));
        assert_eq!(cfg.database.max_connections, DEFAULT_DB_MAX_CONNECTIONS);
        assert_eq!(cfg.database.acquire_timeout, Duration::from_secs(DEFAULT_DB_ACQUIRE_TIMEOUT_SECS));
        assert_eq!(cfg.log.level, LogLevel::Info);
        assert_eq!(cfg.log.sql_diagnostics, DEFAULT_LOG_SQL_DIAGNOSTICS);
    }

    #[test]
    fn multiple_issues_are_all_reported_together() {
        let toml_cfg = TomlConfig::default();
        let mut env = env_of(&[]);
        env.insert("KAWASEMI_LOG_LEVEL".to_string(), "not-a-level".to_string());

        let err = build_config(toml_cfg, &env).expect_err("multiple problems present at once");

        assert!(err.0.len() >= 3, "expected multiple issues, got {:?}", err.0);
    }
}
