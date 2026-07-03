//! Unit tests for startup config loading/merging/validation.
//!
//! These call the crate-private `load_config_from` (explicit TOML text +
//! explicit env map) rather than the public `load_config` (which reads real
//! process env and a real file path), so tests are deterministic and safe
//! to run in parallel — no mutation of shared `std::env` state.

use super::*;

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

const VALID_TOML: &str = r#"
[server]
domain = "toml.example"
bind_addr = "127.0.0.1:4000"
shutdown_grace_secs = 15

[database]
url = "postgres://toml-user:toml-pass@localhost/toml_db"
max_connections = 7
acquire_timeout_secs = 3

[log]
level = "debug"
sql_diagnostic = true
"#;

#[test]
fn env_var_overrides_toml_for_same_field() {
    // Requirement 2.2: when the same item is set in both TOML and an env
    // var, the env var wins.
    let overrides = env(&[("KAWASEMI_SERVER_DOMAIN", "env.example")]);
    let config = load_config_from(Some(VALID_TOML), &overrides)
        .expect("valid config with one env override should load");

    assert_eq!(config.server.domain, "env.example");
    // Unrelated fields still come from TOML, proving this was a merge, not
    // a full env-only load.
    assert_eq!(config.database.max_connections, 7);
    assert_eq!(config.log.level, LogLevel::Debug);
}

#[test]
fn env_only_config_with_no_toml_file_is_valid() {
    // Requirement 2.1: both sources are read; a missing TOML file is fine
    // as long as env vars supply the required fields.
    let overrides = env(&[
        ("KAWASEMI_SERVER_DOMAIN", "env-only.example"),
        ("KAWASEMI_DATABASE_URL", "postgres://user:pass@localhost/db"),
    ]);
    let config = load_config_from(None, &overrides).expect("env-only config should be sufficient");

    assert_eq!(config.server.domain, "env-only.example");
    assert_eq!(config.database.url, "postgres://user:pass@localhost/db");
    // Defaults kick in for everything not supplied.
    assert_eq!(config.database.max_connections, 10);
    assert_eq!(config.log.level, LogLevel::Info);
    assert!(!config.log.sql_diagnostic);
}

#[test]
fn missing_required_domain_aborts_with_identified_field() {
    // Requirement 2.3: missing required field (domain) is reported and
    // identifies which field is missing.
    let overrides = env(&[("KAWASEMI_DATABASE_URL", "postgres://user:pass@localhost/db")]);
    let err = load_config_from(None, &overrides)
        .expect_err("domain missing from both TOML and env must fail");

    let missing: Vec<&str> = err.missing_fields().collect();
    assert_eq!(missing, vec!["server.domain"]);
    assert!(err.malformed_fields().next().is_none());
    assert!(err.to_string().contains("server.domain"));
}

#[test]
fn missing_required_database_url_aborts_with_identified_field() {
    // Requirement 2.3, second required field.
    let overrides = env(&[("KAWASEMI_SERVER_DOMAIN", "example.com")]);
    let err = load_config_from(None, &overrides)
        .expect_err("database url missing from both TOML and env must fail");

    let missing: Vec<&str> = err.missing_fields().collect();
    assert_eq!(missing, vec!["database.url"]);
}

#[test]
fn multiple_missing_required_fields_are_all_reported() {
    let err = load_config_from(None, &HashMap::new())
        .expect_err("empty config must fail on both required fields");

    let mut missing: Vec<&str> = err.missing_fields().collect();
    missing.sort_unstable();
    assert_eq!(missing, vec!["database.url", "server.domain"]);
}

#[test]
fn malformed_domain_aborts_with_identified_field() {
    // Requirement 2.4: malformed field value is reported and identifies
    // which field is invalid, distinct from "missing".
    let overrides = env(&[
        ("KAWASEMI_SERVER_DOMAIN", "not a domain with spaces"),
        ("KAWASEMI_DATABASE_URL", "postgres://user:pass@localhost/db"),
    ]);
    let err = load_config_from(None, &overrides).expect_err("malformed domain must fail");

    let malformed: Vec<&str> = err.malformed_fields().collect();
    assert_eq!(malformed, vec!["server.domain"]);
    assert!(err.missing_fields().next().is_none());
}

#[test]
fn malformed_database_url_aborts_with_identified_field_and_no_secret_leak() {
    let overrides = env(&[
        ("KAWASEMI_SERVER_DOMAIN", "example.com"),
        (
            "KAWASEMI_DATABASE_URL",
            "mysql://user:supersecret@localhost/db",
        ),
    ]);
    let err = load_config_from(None, &overrides).expect_err("non-postgres URL must fail");

    let malformed: Vec<&str> = err.malformed_fields().collect();
    assert_eq!(malformed, vec!["database.url"]);
    // The raw (potentially credential-bearing) value must never be echoed
    // into the diagnostic message.
    assert!(!err.to_string().contains("supersecret"));
}

#[test]
fn malformed_bind_addr_is_reported_as_malformed_not_missing() {
    let overrides = env(&[
        ("KAWASEMI_SERVER_DOMAIN", "example.com"),
        ("KAWASEMI_DATABASE_URL", "postgres://user:pass@localhost/db"),
        ("KAWASEMI_SERVER_BIND_ADDR", "not-an-address"),
    ]);
    let err = load_config_from(None, &overrides).expect_err("malformed bind_addr must fail");

    let malformed: Vec<&str> = err.malformed_fields().collect();
    assert_eq!(malformed, vec!["server.bind_addr"]);
}

#[test]
fn malformed_log_level_is_reported_as_malformed() {
    let overrides = env(&[
        ("KAWASEMI_SERVER_DOMAIN", "example.com"),
        ("KAWASEMI_DATABASE_URL", "postgres://user:pass@localhost/db"),
        ("KAWASEMI_LOG_LEVEL", "verbose"),
    ]);
    let err = load_config_from(None, &overrides).expect_err("unknown log level must fail");

    let malformed: Vec<&str> = err.malformed_fields().collect();
    assert_eq!(malformed, vec!["log.level"]);
}

#[test]
fn malformed_sql_diagnostic_flag_is_reported_as_malformed() {
    let overrides = env(&[
        ("KAWASEMI_SERVER_DOMAIN", "example.com"),
        ("KAWASEMI_DATABASE_URL", "postgres://user:pass@localhost/db"),
        ("KAWASEMI_LOG_SQL_DIAGNOSTIC", "maybe"),
    ]);
    let err = load_config_from(None, &overrides).expect_err("non-boolean flag must fail");

    let malformed: Vec<&str> = err.malformed_fields().collect();
    assert_eq!(malformed, vec!["log.sql_diagnostic"]);
}

#[test]
fn malformed_numeric_fields_are_reported_as_malformed() {
    let overrides = env(&[
        ("KAWASEMI_SERVER_DOMAIN", "example.com"),
        ("KAWASEMI_DATABASE_URL", "postgres://user:pass@localhost/db"),
        ("KAWASEMI_DATABASE_MAX_CONNECTIONS", "not-a-number"),
        ("KAWASEMI_SERVER_SHUTDOWN_GRACE_SECS", "soon"),
    ]);
    let err = load_config_from(None, &overrides).expect_err("non-numeric fields must fail");

    let mut malformed: Vec<&str> = err.malformed_fields().collect();
    malformed.sort_unstable();
    assert_eq!(
        malformed,
        vec!["database.max_connections", "server.shutdown_grace_secs"]
    );
}

#[test]
fn missing_and_malformed_are_distinguished_in_a_single_pass() {
    // domain missing + database.url malformed, reported together with the
    // correct kind for each.
    let overrides = env(&[("KAWASEMI_DATABASE_URL", "not-a-url")]);
    let err = load_config_from(None, &overrides).expect_err("mixed issues must fail");

    let missing: Vec<&str> = err.missing_fields().collect();
    let malformed: Vec<&str> = err.malformed_fields().collect();
    assert_eq!(missing, vec!["server.domain"]);
    assert_eq!(malformed, vec!["database.url"]);
}

#[test]
fn fully_specified_toml_loads_without_env_overrides() {
    let config = load_config_from(Some(VALID_TOML), &HashMap::new())
        .expect("fully specified TOML alone should be sufficient");

    assert_eq!(config.server.domain, "toml.example");
    assert_eq!(
        config.server.bind_addr,
        "127.0.0.1:4000".parse::<SocketAddr>().unwrap()
    );
    assert_eq!(config.server.shutdown_grace, Duration::from_secs(15));
    assert_eq!(
        config.database.url,
        "postgres://toml-user:toml-pass@localhost/toml_db"
    );
    assert_eq!(config.database.max_connections, 7);
    assert_eq!(config.database.acquire_timeout, Duration::from_secs(3));
    assert_eq!(config.log.level, LogLevel::Debug);
    assert!(config.log.sql_diagnostic);
}

#[test]
fn config_error_display_mentions_every_issue() {
    let err = load_config_from(None, &HashMap::new()).expect_err("empty config must fail");
    let rendered = err.to_string();
    assert!(rendered.contains("server.domain"));
    assert!(rendered.contains("database.url"));
}
