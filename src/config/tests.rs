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

/// A valid `actor.kek`: 64 hex characters (256 bits). Shared by every test
/// below that needs config to load successfully (or that deliberately
/// varies a different field), so only the field(s) actually under test
/// differ between cases.
const VALID_KEK_HEX: &str = "ab00cd11ef22ab00cd11ef22ab00cd11ef22ab00cd11ef22ab00cd11ef22abcd";

/// A valid `owner.password`: an arbitrary passphrase meeting
/// [`super::validate_owner_password`]'s minimum-length rule. Shared the same
/// way [`VALID_KEK_HEX`] is.
const VALID_OWNER_PASSWORD: &str = "correct-horse-battery-staple";

/// A valid `oauth.token_hash_key`: 64 hex characters (256 bits), distinct
/// from [`VALID_KEK_HEX`] so tests can tell the two fields apart if one were
/// accidentally read in place of the other. Shared the same way
/// [`VALID_KEK_HEX`] is.
const VALID_TOKEN_HASH_KEY_HEX: &str =
    "11aa22bb33cc44dd11aa22bb33cc44dd11aa22bb33cc44dd11aa22bb33cc44dd";

const VALID_TOML: &str = r#"
[server]
domain = "toml.example"
bind_addr = "127.0.0.1:4000"
shutdown_grace_secs = 15

[database]
url = "postgres://toml-user:toml-pass@localhost/toml_db"
max_connections = 7
acquire_timeout_secs = 3

[actor]
kek = "ab00cd11ef22ab00cd11ef22ab00cd11ef22ab00cd11ef22ab00cd11ef22abcd"

[owner]
password = "toml-owner-passphrase"

[oauth]
token_hash_key = "11aa22bb33cc44dd11aa22bb33cc44dd11aa22bb33cc44dd11aa22bb33cc44dd"

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
        ("KAWASEMI_ACTOR_KEK", VALID_KEK_HEX),
        ("KAWASEMI_OWNER_PASSWORD", VALID_OWNER_PASSWORD),
        ("KAWASEMI_OAUTH_TOKEN_HASH_KEY", VALID_TOKEN_HASH_KEY_HEX),
    ]);
    let config = load_config_from(None, &overrides).expect("env-only config should be sufficient");

    assert_eq!(config.server.domain, "env-only.example");
    assert_eq!(
        config.database.url.expose_secret().as_str(),
        "postgres://user:pass@localhost/db"
    );
    // Defaults kick in for everything not supplied.
    assert_eq!(config.database.max_connections, 10);
    assert_eq!(config.log.level, LogLevel::Info);
    assert!(!config.log.sql_diagnostic);
}

#[test]
fn missing_required_domain_aborts_with_identified_field() {
    // Requirement 2.3: missing required field (domain) is reported and
    // identifies which field is missing.
    let overrides = env(&[
        ("KAWASEMI_DATABASE_URL", "postgres://user:pass@localhost/db"),
        ("KAWASEMI_ACTOR_KEK", VALID_KEK_HEX),
        ("KAWASEMI_OWNER_PASSWORD", VALID_OWNER_PASSWORD),
        ("KAWASEMI_OAUTH_TOKEN_HASH_KEY", VALID_TOKEN_HASH_KEY_HEX),
    ]);
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
    let overrides = env(&[
        ("KAWASEMI_SERVER_DOMAIN", "example.com"),
        ("KAWASEMI_ACTOR_KEK", VALID_KEK_HEX),
        ("KAWASEMI_OWNER_PASSWORD", VALID_OWNER_PASSWORD),
        ("KAWASEMI_OAUTH_TOKEN_HASH_KEY", VALID_TOKEN_HASH_KEY_HEX),
    ]);
    let err = load_config_from(None, &overrides)
        .expect_err("database url missing from both TOML and env must fail");

    let missing: Vec<&str> = err.missing_fields().collect();
    assert_eq!(missing, vec!["database.url"]);
}

#[test]
fn missing_required_kek_aborts_with_identified_field() {
    // Requirement 6.1 (actor-model): the startup KEK is required, like
    // `server.domain`/`database.url` — a missing value is reported, not
    // silently defaulted to a weak/fixed key.
    let overrides = env(&[
        ("KAWASEMI_SERVER_DOMAIN", "example.com"),
        ("KAWASEMI_DATABASE_URL", "postgres://user:pass@localhost/db"),
        ("KAWASEMI_OWNER_PASSWORD", VALID_OWNER_PASSWORD),
        ("KAWASEMI_OAUTH_TOKEN_HASH_KEY", VALID_TOKEN_HASH_KEY_HEX),
    ]);
    let err = load_config_from(None, &overrides).expect_err("missing KEK must fail");

    let missing: Vec<&str> = err.missing_fields().collect();
    assert_eq!(missing, vec!["actor.kek"]);
    assert!(err.malformed_fields().next().is_none());
}

#[test]
fn missing_required_owner_password_aborts_with_identified_field() {
    // Mirrors missing_required_kek_aborts_with_identified_field: the owner
    // credential is required, like the other startup secrets — a missing
    // value is reported, not silently defaulted (there is no safe default
    // for "the passphrase that proves you are the server owner").
    let overrides = env(&[
        ("KAWASEMI_SERVER_DOMAIN", "example.com"),
        ("KAWASEMI_DATABASE_URL", "postgres://user:pass@localhost/db"),
        ("KAWASEMI_ACTOR_KEK", VALID_KEK_HEX),
        ("KAWASEMI_OAUTH_TOKEN_HASH_KEY", VALID_TOKEN_HASH_KEY_HEX),
    ]);
    let err = load_config_from(None, &overrides).expect_err("missing owner password must fail");

    let missing: Vec<&str> = err.missing_fields().collect();
    assert_eq!(missing, vec!["owner.password"]);
    assert!(err.malformed_fields().next().is_none());
}

#[test]
fn missing_required_token_hash_key_aborts_with_identified_field() {
    let overrides = env(&[
        ("KAWASEMI_SERVER_DOMAIN", "example.com"),
        ("KAWASEMI_DATABASE_URL", "postgres://user:pass@localhost/db"),
        ("KAWASEMI_ACTOR_KEK", VALID_KEK_HEX),
        ("KAWASEMI_OWNER_PASSWORD", VALID_OWNER_PASSWORD),
    ]);
    let err = load_config_from(None, &overrides).expect_err("missing token hash key must fail");

    let missing: Vec<&str> = err.missing_fields().collect();
    assert_eq!(missing, vec!["oauth.token_hash_key"]);
    assert!(err.malformed_fields().next().is_none());
}

#[test]
fn multiple_missing_required_fields_are_all_reported() {
    let err = load_config_from(None, &HashMap::new())
        .expect_err("empty config must fail on all required fields");

    let mut missing: Vec<&str> = err.missing_fields().collect();
    missing.sort_unstable();
    assert_eq!(
        missing,
        vec![
            "actor.kek",
            "database.url",
            "oauth.token_hash_key",
            "owner.password",
            "server.domain",
        ]
    );
}

#[test]
fn malformed_domain_aborts_with_identified_field() {
    // Requirement 2.4: malformed field value is reported and identifies
    // which field is invalid, distinct from "missing".
    let overrides = env(&[
        ("KAWASEMI_SERVER_DOMAIN", "not a domain with spaces"),
        ("KAWASEMI_DATABASE_URL", "postgres://user:pass@localhost/db"),
        ("KAWASEMI_ACTOR_KEK", VALID_KEK_HEX),
        ("KAWASEMI_OWNER_PASSWORD", VALID_OWNER_PASSWORD),
        ("KAWASEMI_OAUTH_TOKEN_HASH_KEY", VALID_TOKEN_HASH_KEY_HEX),
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
        ("KAWASEMI_ACTOR_KEK", VALID_KEK_HEX),
        ("KAWASEMI_OWNER_PASSWORD", VALID_OWNER_PASSWORD),
        ("KAWASEMI_OAUTH_TOKEN_HASH_KEY", VALID_TOKEN_HASH_KEY_HEX),
    ]);
    let err = load_config_from(None, &overrides).expect_err("non-postgres URL must fail");

    let malformed: Vec<&str> = err.malformed_fields().collect();
    assert_eq!(malformed, vec!["database.url"]);
    // The raw (potentially credential-bearing) value must never be echoed
    // into the diagnostic message.
    assert!(!err.to_string().contains("supersecret"));
}

#[test]
fn malformed_kek_wrong_length_is_reported_as_malformed_not_missing() {
    let overrides = env(&[
        ("KAWASEMI_SERVER_DOMAIN", "example.com"),
        ("KAWASEMI_DATABASE_URL", "postgres://user:pass@localhost/db"),
        ("KAWASEMI_ACTOR_KEK", "not-64-hex-chars"),
        ("KAWASEMI_OWNER_PASSWORD", VALID_OWNER_PASSWORD),
        ("KAWASEMI_OAUTH_TOKEN_HASH_KEY", VALID_TOKEN_HASH_KEY_HEX),
    ]);
    let err = load_config_from(None, &overrides).expect_err("wrong-length KEK must fail");

    let malformed: Vec<&str> = err.malformed_fields().collect();
    assert_eq!(malformed, vec!["actor.kek"]);
    assert!(err.missing_fields().next().is_none());
}

#[test]
fn malformed_kek_non_hex_characters_is_reported_as_malformed() {
    let non_hex_kek = "zz".repeat(32); // 64 chars, but not valid hex
    let overrides = env(&[
        ("KAWASEMI_SERVER_DOMAIN", "example.com"),
        ("KAWASEMI_DATABASE_URL", "postgres://user:pass@localhost/db"),
        ("KAWASEMI_ACTOR_KEK", non_hex_kek.as_str()),
        ("KAWASEMI_OWNER_PASSWORD", VALID_OWNER_PASSWORD),
        ("KAWASEMI_OAUTH_TOKEN_HASH_KEY", VALID_TOKEN_HASH_KEY_HEX),
    ]);
    let err = load_config_from(None, &overrides).expect_err("non-hex KEK must fail");

    let malformed: Vec<&str> = err.malformed_fields().collect();
    assert_eq!(malformed, vec!["actor.kek"]);
}

#[test]
fn malformed_kek_does_not_leak_the_raw_value_in_the_error_message() {
    let overrides = env(&[
        ("KAWASEMI_SERVER_DOMAIN", "example.com"),
        ("KAWASEMI_DATABASE_URL", "postgres://user:pass@localhost/db"),
        ("KAWASEMI_ACTOR_KEK", "not-a-valid-kek-value-at-all"),
        ("KAWASEMI_OWNER_PASSWORD", VALID_OWNER_PASSWORD),
        ("KAWASEMI_OAUTH_TOKEN_HASH_KEY", VALID_TOKEN_HASH_KEY_HEX),
    ]);
    let err = load_config_from(None, &overrides).expect_err("malformed KEK must fail");

    assert!(!err.to_string().contains("not-a-valid-kek-value-at-all"));
}

#[test]
fn valid_kek_hex_decodes_to_the_expected_bytes() {
    let overrides = env(&[
        ("KAWASEMI_SERVER_DOMAIN", "example.com"),
        ("KAWASEMI_DATABASE_URL", "postgres://user:pass@localhost/db"),
        ("KAWASEMI_ACTOR_KEK", VALID_KEK_HEX),
        ("KAWASEMI_OWNER_PASSWORD", VALID_OWNER_PASSWORD),
        ("KAWASEMI_OAUTH_TOKEN_HASH_KEY", VALID_TOKEN_HASH_KEY_HEX),
    ]);
    let config = load_config_from(None, &overrides).expect("valid KEK must load");

    let expected: [u8; 32] = [
        0xab, 0x00, 0xcd, 0x11, 0xef, 0x22, 0xab, 0x00, 0xcd, 0x11, 0xef, 0x22, 0xab, 0x00, 0xcd,
        0x11, 0xef, 0x22, 0xab, 0x00, 0xcd, 0x11, 0xef, 0x22, 0xab, 0x00, 0xcd, 0x11, 0xef, 0x22,
        0xab, 0xcd,
    ];
    assert_eq!(config.actor.kek.expose_secret(), &expected);
}

#[test]
fn kek_debug_output_does_not_leak_the_key_material() {
    let overrides = env(&[
        ("KAWASEMI_SERVER_DOMAIN", "example.com"),
        ("KAWASEMI_DATABASE_URL", "postgres://user:pass@localhost/db"),
        ("KAWASEMI_ACTOR_KEK", VALID_KEK_HEX),
        ("KAWASEMI_OWNER_PASSWORD", VALID_OWNER_PASSWORD),
        ("KAWASEMI_OAUTH_TOKEN_HASH_KEY", VALID_TOKEN_HASH_KEY_HEX),
    ]);
    let config = load_config_from(None, &overrides).expect("valid KEK must load");

    let formatted = format!("{config:?}");
    assert!(!formatted.contains(VALID_KEK_HEX));
    assert!(!formatted.contains("ab00cd11"));
}

#[test]
fn malformed_bind_addr_is_reported_as_malformed_not_missing() {
    let overrides = env(&[
        ("KAWASEMI_SERVER_DOMAIN", "example.com"),
        ("KAWASEMI_DATABASE_URL", "postgres://user:pass@localhost/db"),
        ("KAWASEMI_ACTOR_KEK", VALID_KEK_HEX),
        ("KAWASEMI_OWNER_PASSWORD", VALID_OWNER_PASSWORD),
        ("KAWASEMI_OAUTH_TOKEN_HASH_KEY", VALID_TOKEN_HASH_KEY_HEX),
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
        ("KAWASEMI_ACTOR_KEK", VALID_KEK_HEX),
        ("KAWASEMI_OWNER_PASSWORD", VALID_OWNER_PASSWORD),
        ("KAWASEMI_OAUTH_TOKEN_HASH_KEY", VALID_TOKEN_HASH_KEY_HEX),
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
        ("KAWASEMI_ACTOR_KEK", VALID_KEK_HEX),
        ("KAWASEMI_OWNER_PASSWORD", VALID_OWNER_PASSWORD),
        ("KAWASEMI_OAUTH_TOKEN_HASH_KEY", VALID_TOKEN_HASH_KEY_HEX),
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
        ("KAWASEMI_ACTOR_KEK", VALID_KEK_HEX),
        ("KAWASEMI_OWNER_PASSWORD", VALID_OWNER_PASSWORD),
        ("KAWASEMI_OAUTH_TOKEN_HASH_KEY", VALID_TOKEN_HASH_KEY_HEX),
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
    // correct kind for each. KEK is supplied validly so it does not add a
    // third, unrelated issue to this test's assertions.
    let overrides = env(&[
        ("KAWASEMI_DATABASE_URL", "not-a-url"),
        ("KAWASEMI_ACTOR_KEK", VALID_KEK_HEX),
        ("KAWASEMI_OWNER_PASSWORD", VALID_OWNER_PASSWORD),
        ("KAWASEMI_OAUTH_TOKEN_HASH_KEY", VALID_TOKEN_HASH_KEY_HEX),
    ]);
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
        config.database.url.expose_secret().as_str(),
        "postgres://toml-user:toml-pass@localhost/toml_db"
    );
    assert_eq!(config.database.max_connections, 7);
    assert_eq!(config.database.acquire_timeout, Duration::from_secs(3));
    assert_eq!(
        config.owner.password.expose_secret().as_str(),
        "toml-owner-passphrase"
    );
    assert_eq!(
        config.oauth.token_hash_key.expose_secret(),
        &[
            0x11, 0xaa, 0x22, 0xbb, 0x33, 0xcc, 0x44, 0xdd, 0x11, 0xaa, 0x22, 0xbb, 0x33, 0xcc,
            0x44, 0xdd, 0x11, 0xaa, 0x22, 0xbb, 0x33, 0xcc, 0x44, 0xdd, 0x11, 0xaa, 0x22, 0xbb,
            0x33, 0xcc, 0x44, 0xdd,
        ]
    );
    assert_eq!(config.log.level, LogLevel::Debug);
    assert!(config.log.sql_diagnostic);
}

#[test]
fn config_error_display_mentions_every_issue() {
    let err = load_config_from(None, &HashMap::new()).expect_err("empty config must fail");
    let rendered = err.to_string();
    assert!(rendered.contains("server.domain"));
    assert!(rendered.contains("database.url"));
    assert!(rendered.contains("actor.kek"));
    assert!(rendered.contains("owner.password"));
    assert!(rendered.contains("oauth.token_hash_key"));
}

#[test]
fn malformed_owner_password_too_short_is_reported_as_malformed_not_missing() {
    let overrides = env(&[
        ("KAWASEMI_SERVER_DOMAIN", "example.com"),
        ("KAWASEMI_DATABASE_URL", "postgres://user:pass@localhost/db"),
        ("KAWASEMI_ACTOR_KEK", VALID_KEK_HEX),
        ("KAWASEMI_OWNER_PASSWORD", "short"),
        ("KAWASEMI_OAUTH_TOKEN_HASH_KEY", VALID_TOKEN_HASH_KEY_HEX),
    ]);
    let err = load_config_from(None, &overrides).expect_err("too-short owner password must fail");

    let malformed: Vec<&str> = err.malformed_fields().collect();
    assert_eq!(malformed, vec!["owner.password"]);
    assert!(err.missing_fields().next().is_none());
}

#[test]
fn malformed_owner_password_empty_is_reported_as_malformed() {
    let overrides = env(&[
        ("KAWASEMI_SERVER_DOMAIN", "example.com"),
        ("KAWASEMI_DATABASE_URL", "postgres://user:pass@localhost/db"),
        ("KAWASEMI_ACTOR_KEK", VALID_KEK_HEX),
        ("KAWASEMI_OWNER_PASSWORD", "   "),
        ("KAWASEMI_OAUTH_TOKEN_HASH_KEY", VALID_TOKEN_HASH_KEY_HEX),
    ]);
    let err = load_config_from(None, &overrides).expect_err("blank owner password must fail");

    let malformed: Vec<&str> = err.malformed_fields().collect();
    assert_eq!(malformed, vec!["owner.password"]);
}

#[test]
fn malformed_owner_password_does_not_leak_the_raw_value_in_the_error_message() {
    let overrides = env(&[
        ("KAWASEMI_SERVER_DOMAIN", "example.com"),
        ("KAWASEMI_DATABASE_URL", "postgres://user:pass@localhost/db"),
        ("KAWASEMI_ACTOR_KEK", VALID_KEK_HEX),
        ("KAWASEMI_OWNER_PASSWORD", "sh0rt!!"), // distinctive, but < 8 chars
        ("KAWASEMI_OAUTH_TOKEN_HASH_KEY", VALID_TOKEN_HASH_KEY_HEX),
    ]);
    let err = load_config_from(None, &overrides).expect_err("too-short owner password must fail");

    assert!(!err.to_string().contains("sh0rt!!"));
}

#[test]
fn valid_owner_password_round_trips_through_expose_secret() {
    let overrides = env(&[
        ("KAWASEMI_SERVER_DOMAIN", "example.com"),
        ("KAWASEMI_DATABASE_URL", "postgres://user:pass@localhost/db"),
        ("KAWASEMI_ACTOR_KEK", VALID_KEK_HEX),
        ("KAWASEMI_OWNER_PASSWORD", VALID_OWNER_PASSWORD),
        ("KAWASEMI_OAUTH_TOKEN_HASH_KEY", VALID_TOKEN_HASH_KEY_HEX),
    ]);
    let config = load_config_from(None, &overrides).expect("valid owner password must load");

    assert_eq!(
        config.owner.password.expose_secret().as_str(),
        VALID_OWNER_PASSWORD
    );
}

#[test]
fn owner_password_debug_output_does_not_leak_the_password() {
    let overrides = env(&[
        ("KAWASEMI_SERVER_DOMAIN", "example.com"),
        ("KAWASEMI_DATABASE_URL", "postgres://user:pass@localhost/db"),
        ("KAWASEMI_ACTOR_KEK", VALID_KEK_HEX),
        ("KAWASEMI_OWNER_PASSWORD", VALID_OWNER_PASSWORD),
        ("KAWASEMI_OAUTH_TOKEN_HASH_KEY", VALID_TOKEN_HASH_KEY_HEX),
    ]);
    let config = load_config_from(None, &overrides).expect("valid owner password must load");

    let formatted = format!("{config:?}");
    assert!(!formatted.contains(VALID_OWNER_PASSWORD));
}

#[test]
fn malformed_token_hash_key_wrong_length_is_reported_as_malformed_not_missing() {
    let overrides = env(&[
        ("KAWASEMI_SERVER_DOMAIN", "example.com"),
        ("KAWASEMI_DATABASE_URL", "postgres://user:pass@localhost/db"),
        ("KAWASEMI_ACTOR_KEK", VALID_KEK_HEX),
        ("KAWASEMI_OWNER_PASSWORD", VALID_OWNER_PASSWORD),
        ("KAWASEMI_OAUTH_TOKEN_HASH_KEY", "not-64-hex-chars"),
    ]);
    let err =
        load_config_from(None, &overrides).expect_err("wrong-length token hash key must fail");

    let malformed: Vec<&str> = err.malformed_fields().collect();
    assert_eq!(malformed, vec!["oauth.token_hash_key"]);
    assert!(err.missing_fields().next().is_none());
}

#[test]
fn malformed_token_hash_key_non_hex_characters_is_reported_as_malformed() {
    let non_hex_key = "zz".repeat(32); // 64 chars, but not valid hex
    let overrides = env(&[
        ("KAWASEMI_SERVER_DOMAIN", "example.com"),
        ("KAWASEMI_DATABASE_URL", "postgres://user:pass@localhost/db"),
        ("KAWASEMI_ACTOR_KEK", VALID_KEK_HEX),
        ("KAWASEMI_OWNER_PASSWORD", VALID_OWNER_PASSWORD),
        ("KAWASEMI_OAUTH_TOKEN_HASH_KEY", non_hex_key.as_str()),
    ]);
    let err = load_config_from(None, &overrides).expect_err("non-hex token hash key must fail");

    let malformed: Vec<&str> = err.malformed_fields().collect();
    assert_eq!(malformed, vec!["oauth.token_hash_key"]);
}

#[test]
fn malformed_token_hash_key_does_not_leak_the_raw_value_in_the_error_message() {
    let overrides = env(&[
        ("KAWASEMI_SERVER_DOMAIN", "example.com"),
        ("KAWASEMI_DATABASE_URL", "postgres://user:pass@localhost/db"),
        ("KAWASEMI_ACTOR_KEK", VALID_KEK_HEX),
        ("KAWASEMI_OWNER_PASSWORD", VALID_OWNER_PASSWORD),
        (
            "KAWASEMI_OAUTH_TOKEN_HASH_KEY",
            "not-a-valid-token-hash-key-at-all",
        ),
    ]);
    let err = load_config_from(None, &overrides).expect_err("malformed token hash key must fail");

    assert!(
        !err.to_string()
            .contains("not-a-valid-token-hash-key-at-all")
    );
}

#[test]
fn valid_token_hash_key_hex_decodes_to_the_expected_bytes() {
    let overrides = env(&[
        ("KAWASEMI_SERVER_DOMAIN", "example.com"),
        ("KAWASEMI_DATABASE_URL", "postgres://user:pass@localhost/db"),
        ("KAWASEMI_ACTOR_KEK", VALID_KEK_HEX),
        ("KAWASEMI_OWNER_PASSWORD", VALID_OWNER_PASSWORD),
        ("KAWASEMI_OAUTH_TOKEN_HASH_KEY", VALID_TOKEN_HASH_KEY_HEX),
    ]);
    let config = load_config_from(None, &overrides).expect("valid token hash key must load");

    let expected: [u8; 32] = [
        0x11, 0xaa, 0x22, 0xbb, 0x33, 0xcc, 0x44, 0xdd, 0x11, 0xaa, 0x22, 0xbb, 0x33, 0xcc, 0x44,
        0xdd, 0x11, 0xaa, 0x22, 0xbb, 0x33, 0xcc, 0x44, 0xdd, 0x11, 0xaa, 0x22, 0xbb, 0x33, 0xcc,
        0x44, 0xdd,
    ];
    assert_eq!(config.oauth.token_hash_key.expose_secret(), &expected);
}

#[test]
fn token_hash_key_debug_output_does_not_leak_the_key_material() {
    let overrides = env(&[
        ("KAWASEMI_SERVER_DOMAIN", "example.com"),
        ("KAWASEMI_DATABASE_URL", "postgres://user:pass@localhost/db"),
        ("KAWASEMI_ACTOR_KEK", VALID_KEK_HEX),
        ("KAWASEMI_OWNER_PASSWORD", VALID_OWNER_PASSWORD),
        ("KAWASEMI_OAUTH_TOKEN_HASH_KEY", VALID_TOKEN_HASH_KEY_HEX),
    ]);
    let config = load_config_from(None, &overrides).expect("valid token hash key must load");

    let formatted = format!("{config:?}");
    assert!(!formatted.contains(VALID_TOKEN_HASH_KEY_HEX));
    assert!(!formatted.contains("11aa22bb"));
}

// --- media-pipeline task 1.2: `media.*` startup config ---
//
// Unlike `actor.kek`/`owner.password`/`oauth.token_hash_key`, no `media.*`
// field is required (see `MediaConfig`'s own doc comment in `config.rs` for
// why every field has a safe default) — so, following `FederationConfig`'s
// own precedent, there is no "missing required media field" test here: it
// would have nothing to assert beyond a fabricated requirement this spec
// does not have.

#[test]
fn media_config_defaults_when_not_supplied() {
    // Requirements 1.4, 4.2, 5.2, 6.1: every `media.*` field is defaultable,
    // and a minimal config (only the genuinely required fields from other
    // specs) must still boot with sensible media defaults.
    let overrides = env(&[
        ("KAWASEMI_SERVER_DOMAIN", "example.com"),
        ("KAWASEMI_DATABASE_URL", "postgres://user:pass@localhost/db"),
        ("KAWASEMI_ACTOR_KEK", VALID_KEK_HEX),
        ("KAWASEMI_OWNER_PASSWORD", VALID_OWNER_PASSWORD),
        ("KAWASEMI_OAUTH_TOKEN_HASH_KEY", VALID_TOKEN_HASH_KEY_HEX),
    ]);
    let config = load_config_from(None, &overrides).expect("media defaults alone must load");

    assert_eq!(config.media.storage_root, PathBuf::from("media_storage"));
    assert_eq!(config.media.max_upload_size_bytes, 10 * 1024 * 1024);
    assert_eq!(config.media.thumbnail_target_width, 400);
    assert_eq!(config.media.thumbnail_target_height, 400);
    assert_eq!(
        config.media.supported_formats,
        vec![
            "image/jpeg".to_string(),
            "image/png".to_string(),
            "image/gif".to_string(),
            "image/webp".to_string(),
        ]
    );
    assert_eq!(config.media.worker_concurrency, 2);
    assert_eq!(config.media.max_retry_attempts, 5);
    assert_eq!(config.media.lease_duration, Duration::from_secs(5 * 60));
}

#[test]
fn media_config_loads_explicit_values_from_toml() {
    // Requirements 1.4, 4.2, 5.2, 6.1: every field can be explicitly
    // supplied and overrides its default.
    let toml = format!(
        r#"
[server]
domain = "example.com"

[database]
url = "postgres://user:pass@localhost/db"

[actor]
kek = "{VALID_KEK_HEX}"

[owner]
password = "{VALID_OWNER_PASSWORD}"

[oauth]
token_hash_key = "{VALID_TOKEN_HASH_KEY_HEX}"

[media]
storage_root = "/srv/kawasemi/media"
max_upload_size_bytes = 5242880
thumbnail_target_width = 320
thumbnail_target_height = 240
supported_formats = "image/png, image/webp"
worker_concurrency = 4
max_retry_attempts = 8
lease_duration_secs = 600
"#
    );
    let config = load_config_from(Some(&toml), &HashMap::new())
        .expect("fully specified media TOML must load");

    assert_eq!(
        config.media.storage_root,
        PathBuf::from("/srv/kawasemi/media")
    );
    assert_eq!(config.media.max_upload_size_bytes, 5_242_880);
    assert_eq!(config.media.thumbnail_target_width, 320);
    assert_eq!(config.media.thumbnail_target_height, 240);
    assert_eq!(
        config.media.supported_formats,
        vec!["image/png".to_string(), "image/webp".to_string()]
    );
    assert_eq!(config.media.worker_concurrency, 4);
    assert_eq!(config.media.max_retry_attempts, 8);
    assert_eq!(config.media.lease_duration, Duration::from_secs(600));
}

#[test]
fn media_env_var_overrides_toml_for_same_field() {
    // Requirement 2.2's env-over-TOML precedence, extended to `media.*`.
    let toml = format!(
        r#"
[server]
domain = "example.com"

[database]
url = "postgres://user:pass@localhost/db"

[actor]
kek = "{VALID_KEK_HEX}"

[owner]
password = "{VALID_OWNER_PASSWORD}"

[oauth]
token_hash_key = "{VALID_TOKEN_HASH_KEY_HEX}"

[media]
storage_root = "/from/toml"
"#
    );
    let overrides = env(&[("KAWASEMI_MEDIA_STORAGE_ROOT", "/from/env")]);
    let config = load_config_from(Some(&toml), &overrides)
        .expect("media config with one env override must load");

    assert_eq!(config.media.storage_root, PathBuf::from("/from/env"));
}

#[test]
fn malformed_media_storage_root_empty_is_reported_as_malformed() {
    let overrides = env(&[
        ("KAWASEMI_SERVER_DOMAIN", "example.com"),
        ("KAWASEMI_DATABASE_URL", "postgres://user:pass@localhost/db"),
        ("KAWASEMI_ACTOR_KEK", VALID_KEK_HEX),
        ("KAWASEMI_OWNER_PASSWORD", VALID_OWNER_PASSWORD),
        ("KAWASEMI_OAUTH_TOKEN_HASH_KEY", VALID_TOKEN_HASH_KEY_HEX),
        ("KAWASEMI_MEDIA_STORAGE_ROOT", "   "),
    ]);
    let err = load_config_from(None, &overrides).expect_err("blank storage root must fail");

    let malformed: Vec<&str> = err.malformed_fields().collect();
    assert_eq!(malformed, vec!["media.storage_root"]);
    assert!(err.missing_fields().next().is_none());
}

#[test]
fn malformed_media_max_upload_size_bytes_zero_is_reported_as_malformed() {
    // Requirement 1.4: a zero-byte ceiling would reject every upload, which
    // is never the intent of a size limit.
    let overrides = env(&[
        ("KAWASEMI_SERVER_DOMAIN", "example.com"),
        ("KAWASEMI_DATABASE_URL", "postgres://user:pass@localhost/db"),
        ("KAWASEMI_ACTOR_KEK", VALID_KEK_HEX),
        ("KAWASEMI_OWNER_PASSWORD", VALID_OWNER_PASSWORD),
        ("KAWASEMI_OAUTH_TOKEN_HASH_KEY", VALID_TOKEN_HASH_KEY_HEX),
        ("KAWASEMI_MEDIA_MAX_UPLOAD_SIZE_BYTES", "0"),
    ]);
    let err = load_config_from(None, &overrides).expect_err("zero upload size must fail");

    let malformed: Vec<&str> = err.malformed_fields().collect();
    assert_eq!(malformed, vec!["media.max_upload_size_bytes"]);
}

#[test]
fn malformed_media_max_upload_size_bytes_non_numeric_is_reported_as_malformed() {
    let overrides = env(&[
        ("KAWASEMI_SERVER_DOMAIN", "example.com"),
        ("KAWASEMI_DATABASE_URL", "postgres://user:pass@localhost/db"),
        ("KAWASEMI_ACTOR_KEK", VALID_KEK_HEX),
        ("KAWASEMI_OWNER_PASSWORD", VALID_OWNER_PASSWORD),
        ("KAWASEMI_OAUTH_TOKEN_HASH_KEY", VALID_TOKEN_HASH_KEY_HEX),
        ("KAWASEMI_MEDIA_MAX_UPLOAD_SIZE_BYTES", "huge"),
    ]);
    let err = load_config_from(None, &overrides).expect_err("non-numeric upload size must fail");

    let malformed: Vec<&str> = err.malformed_fields().collect();
    assert_eq!(malformed, vec!["media.max_upload_size_bytes"]);
}

#[test]
fn malformed_media_thumbnail_dimensions_zero_are_reported_as_malformed() {
    // Requirement 6.1: a zero-pixel thumbnail target is degenerate.
    let overrides = env(&[
        ("KAWASEMI_SERVER_DOMAIN", "example.com"),
        ("KAWASEMI_DATABASE_URL", "postgres://user:pass@localhost/db"),
        ("KAWASEMI_ACTOR_KEK", VALID_KEK_HEX),
        ("KAWASEMI_OWNER_PASSWORD", VALID_OWNER_PASSWORD),
        ("KAWASEMI_OAUTH_TOKEN_HASH_KEY", VALID_TOKEN_HASH_KEY_HEX),
        ("KAWASEMI_MEDIA_THUMBNAIL_TARGET_WIDTH", "0"),
        ("KAWASEMI_MEDIA_THUMBNAIL_TARGET_HEIGHT", "0"),
    ]);
    let err = load_config_from(None, &overrides).expect_err("zero thumbnail dimensions must fail");

    let mut malformed: Vec<&str> = err.malformed_fields().collect();
    malformed.sort_unstable();
    assert_eq!(
        malformed,
        vec![
            "media.thumbnail_target_height",
            "media.thumbnail_target_width",
        ]
    );
}

#[test]
fn malformed_media_supported_formats_empty_is_reported_as_malformed() {
    // Requirement 1.4: an empty accepted-format list would reject every
    // upload, which is never the intent of the "unsupported format" check.
    let overrides = env(&[
        ("KAWASEMI_SERVER_DOMAIN", "example.com"),
        ("KAWASEMI_DATABASE_URL", "postgres://user:pass@localhost/db"),
        ("KAWASEMI_ACTOR_KEK", VALID_KEK_HEX),
        ("KAWASEMI_OWNER_PASSWORD", VALID_OWNER_PASSWORD),
        ("KAWASEMI_OAUTH_TOKEN_HASH_KEY", VALID_TOKEN_HASH_KEY_HEX),
        ("KAWASEMI_MEDIA_SUPPORTED_FORMATS", "  , , "),
    ]);
    let err = load_config_from(None, &overrides).expect_err("empty format list must fail");

    let malformed: Vec<&str> = err.malformed_fields().collect();
    assert_eq!(malformed, vec!["media.supported_formats"]);
}

#[test]
fn media_supported_formats_are_split_and_trimmed_from_comma_separated_list() {
    let overrides = env(&[
        ("KAWASEMI_SERVER_DOMAIN", "example.com"),
        ("KAWASEMI_DATABASE_URL", "postgres://user:pass@localhost/db"),
        ("KAWASEMI_ACTOR_KEK", VALID_KEK_HEX),
        ("KAWASEMI_OWNER_PASSWORD", VALID_OWNER_PASSWORD),
        ("KAWASEMI_OAUTH_TOKEN_HASH_KEY", VALID_TOKEN_HASH_KEY_HEX),
        (
            "KAWASEMI_MEDIA_SUPPORTED_FORMATS",
            " image/jpeg ,image/png,  image/gif  ",
        ),
    ]);
    let config = load_config_from(None, &overrides).expect("valid format list must load");

    assert_eq!(
        config.media.supported_formats,
        vec![
            "image/jpeg".to_string(),
            "image/png".to_string(),
            "image/gif".to_string(),
        ]
    );
}

#[test]
fn malformed_media_worker_concurrency_zero_is_reported_as_malformed() {
    // Requirement 4.2: a zero-worker pool would mean the processing job
    // queue is never consumed, which is never a valid startup intent.
    let overrides = env(&[
        ("KAWASEMI_SERVER_DOMAIN", "example.com"),
        ("KAWASEMI_DATABASE_URL", "postgres://user:pass@localhost/db"),
        ("KAWASEMI_ACTOR_KEK", VALID_KEK_HEX),
        ("KAWASEMI_OWNER_PASSWORD", VALID_OWNER_PASSWORD),
        ("KAWASEMI_OAUTH_TOKEN_HASH_KEY", VALID_TOKEN_HASH_KEY_HEX),
        ("KAWASEMI_MEDIA_WORKER_CONCURRENCY", "0"),
    ]);
    let err = load_config_from(None, &overrides).expect_err("zero worker concurrency must fail");

    let malformed: Vec<&str> = err.malformed_fields().collect();
    assert_eq!(malformed, vec!["media.worker_concurrency"]);
}

#[test]
fn malformed_media_max_retry_attempts_non_numeric_is_reported_as_malformed() {
    let overrides = env(&[
        ("KAWASEMI_SERVER_DOMAIN", "example.com"),
        ("KAWASEMI_DATABASE_URL", "postgres://user:pass@localhost/db"),
        ("KAWASEMI_ACTOR_KEK", VALID_KEK_HEX),
        ("KAWASEMI_OWNER_PASSWORD", VALID_OWNER_PASSWORD),
        ("KAWASEMI_OAUTH_TOKEN_HASH_KEY", VALID_TOKEN_HASH_KEY_HEX),
        ("KAWASEMI_MEDIA_MAX_RETRY_ATTEMPTS", "many"),
    ]);
    let err = load_config_from(None, &overrides).expect_err("non-numeric retry attempts must fail");

    let malformed: Vec<&str> = err.malformed_fields().collect();
    assert_eq!(malformed, vec!["media.max_retry_attempts"]);
}

#[test]
fn malformed_media_lease_duration_non_numeric_is_reported_as_malformed() {
    // Requirement 4.2: `lease_duration` (task text: "処理ジョブのリース期間")
    // must reject a non-numeric value the same way other `_secs` duration
    // fields do (mirrors `malformed_numeric_fields_are_reported_as_malformed`
    // above for `server.shutdown_grace_secs`).
    let overrides = env(&[
        ("KAWASEMI_SERVER_DOMAIN", "example.com"),
        ("KAWASEMI_DATABASE_URL", "postgres://user:pass@localhost/db"),
        ("KAWASEMI_ACTOR_KEK", VALID_KEK_HEX),
        ("KAWASEMI_OWNER_PASSWORD", VALID_OWNER_PASSWORD),
        ("KAWASEMI_OAUTH_TOKEN_HASH_KEY", VALID_TOKEN_HASH_KEY_HEX),
        ("KAWASEMI_MEDIA_LEASE_DURATION_SECS", "forever"),
    ]);
    let err = load_config_from(None, &overrides).expect_err("non-numeric lease duration must fail");

    let malformed: Vec<&str> = err.malformed_fields().collect();
    assert_eq!(malformed, vec!["media.lease_duration_secs"]);
}

#[test]
fn media_lease_duration_default_is_well_above_a_typical_processing_duration() {
    // Task 1.2's own text: the lease duration default must be "well above
    // the expected processing time" so a healthy worker never has its job
    // reclaimed out from under it. This is a coarse sanity bound (not a
    // measured processing benchmark), matching the spirit of the task's own
    // "例: 5 分" (e.g. 5 minutes) example while staying resilient to a future
    // default tuning within the same order of magnitude.
    let overrides = env(&[
        ("KAWASEMI_SERVER_DOMAIN", "example.com"),
        ("KAWASEMI_DATABASE_URL", "postgres://user:pass@localhost/db"),
        ("KAWASEMI_ACTOR_KEK", VALID_KEK_HEX),
        ("KAWASEMI_OWNER_PASSWORD", VALID_OWNER_PASSWORD),
        ("KAWASEMI_OAUTH_TOKEN_HASH_KEY", VALID_TOKEN_HASH_KEY_HEX),
    ]);
    let config = load_config_from(None, &overrides).expect("media defaults alone must load");

    assert!(
        config.media.lease_duration >= Duration::from_secs(60),
        "lease_duration default ({:?}) is not well above a typical few-second image processing job",
        config.media.lease_duration
    );
}
