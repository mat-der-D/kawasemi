//! Entity contract test harness (api-foundation `ContractHarness`
//! boundary, task 8.1, Requirements 9.1-9.5).
//!
//! Scope: this module owns the *generic* golden/snapshot comparison
//! machinery ([`assert_golden`]) and the real-client-capture registration
//! extension point ([`CapturedExchange`] / [`register_fixture`] /
//! [`load_fixture`]) that later feature specs (accounts-and-instance,
//! statuses-core, ...) build their own entity JSON contracts on top of
//! (Requirement 9.4). It does not define, generate, or assert on any
//! individual entity's JSON shape — Account/Status/Notification/... are
//! Out of Boundary here, exactly as design.md's "This Spec Owns" /
//! "Out of Boundary" split states. Every example JSON this module's own
//! tests use is a synthetic stand-in built only to prove the harness
//! mechanics, never a real Mastodon entity contract.
//!
//! ## Design note: reconciling design.md's file plan with this crate's
//! module conventions
//! design.md's File Structure Plan places this component at
//! `src/testing/contract.rs` (a `testing/` wrapper directory around a
//! single file). This task instead adds a plain top-level `src/contract.rs`:
//! no sibling file under a `testing/` directory exists or is planned by any
//! other task, so a wrapper module would hold exactly one child and add
//! indirection without benefit — every other cross-cutting component in
//! this crate (`api/`, `oauth/`) is a directory only when it actually holds
//! multiple files. The two documented Service Interface functions
//! (`assert_golden`, `register_fixture`) are implemented with exactly the
//! signatures design.md specifies.
//!
//! ## Why `serde_json` moved from `[dev-dependencies]` to `[dependencies]`
//! `src/oauth/token_endpoint.rs` documents a deliberate prior decision to
//! avoid `serde_json::Value` in production code specifically *because*
//! `serde_json` was only a dev-dependency. That constraint does not carry
//! over here: this module's entire purpose is comparing/persisting
//! `serde_json::Value`, its public functions are meant to be called from
//! other crates'-worth of integration tests (`tests/*.rs` binaries, and
//! later specs' own `tests/*_it.rs`), and Rust's `#[cfg(test)]` only
//! compiles for *this* crate's own `cargo test` runs — an integration test
//! binary depends on this crate built *without* `--cfg test`, so gating
//! this module behind `#[cfg(test)]` would make it invisible to exactly the
//! callers it exists for. Promoting the already-present `serde_json`
//! dev-dependency to a regular one (same pinned version, `Cargo.toml`) is
//! therefore the minimal correct fix, not a new dependency.
//!
//! ## Golden files vs. registered fixtures: two storage conventions
//! - [`assert_golden`]'s `golden_path` is caller-controlled (resolved
//!   relative to `CARGO_MANIFEST_DIR` unless already absolute): the calling
//!   spec's own test authors own and check in that file directly (e.g.
//!   `tests/golden/accounts/show_public.json`), so this module never
//!   invents a directory layout for them.
//! - [`register_fixture`]'s `name` is a bare identifier, not a path: real
//!   client captures are written by this module under one fixed directory
//!   it does own ([`FIXTURES_DIR`]), so later specs have a single place to
//!   look regardless of which spec captured a given fixture.
//!
//! ## Determinism (Requirement 9.3)
//! This module does not itself touch [`crate::runtime::RuntimeContext`] —
//! JSON diffing and file I/O have nothing non-deterministic about them.
//! Requirement 9.3's "reproducible golden" guarantee instead comes from
//! *how* a calling contract test produces `actual_json` before handing it
//! to [`assert_golden`]: by generating it from a
//! [`crate::test_harness::spawn_test_app`]-booted instance, whose
//! `RuntimeContext` is always [`crate::runtime::RuntimeContext::deterministic`]
//! with a fixed seed (see `src/test_harness.rs`'s own doc comment,
//! "Deterministic injection"). `tests/contract_harness_it.rs` proves
//! exactly that this module composes correctly with that guarantee: two
//! independently-`spawn_test_app`-booted instances produce byte-for-byte
//! identical JSON for a synthetic sample payload, and [`assert_golden`]
//! treats one instance's output as a golden the other instance's output
//! then reproduces.

#[cfg(test)]
mod tests;

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Environment variable that, when set to any non-empty value, makes
/// [`assert_golden`] overwrite the golden file at `golden_path` with
/// `actual_json` instead of comparing against it — the same "record a new
/// baseline" escape hatch established golden-testing tools provide (e.g.
/// `cargo insta`'s `INSTA_UPDATE`), so a spec author can regenerate a
/// golden after a deliberate, reviewed contract change instead of hand
/// editing JSON.
pub const UPDATE_GOLDEN_ENV: &str = "KAWASEMI_UPDATE_GOLDEN";

/// Fixed directory (relative to `CARGO_MANIFEST_DIR`) [`register_fixture`]
/// writes into and [`load_fixture`] reads from.
const FIXTURES_DIR: &str = "tests/fixtures";

/// A single real standard-client request/response exchange, captured
/// outside this crate (e.g. from a client's own debug log or a packet
/// capture) and registered as a contract acceptance criterion (Requirement
/// 9.5). Deliberately minimal: just enough to record what was sent and
/// received and later compare a generated response against it (including
/// by feeding [`Self::response_body`] straight into [`assert_golden`] as
/// the expected side) — this module does not interpret HTTP semantics
/// beyond these fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapturedExchange {
    /// HTTP method of the captured request (e.g. `"GET"`).
    pub method: String,
    /// Request path (and query string, if any) of the captured request.
    pub path: String,
    /// Captured request body, if the exchange had one (e.g. a POST body);
    /// `None` for methods/exchanges that carried no body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_body: Option<Value>,
    /// Captured HTTP status code of the response.
    pub status: u16,
    /// Captured response JSON body — the value later contract tests treat
    /// as the acceptance criterion.
    pub response_body: Value,
}

/// One location-specific difference between a golden's expected JSON and
/// an actual JSON value (Requirement 9.2). `path` is a JSON-pointer-style
/// location (e.g. `$.attributes.username`, `$.tags[2]`) identifying exactly
/// where `expected` and `actual` diverge; `detail` describes how.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Mismatch {
    path: String,
    detail: String,
}

/// Compares `actual_json` against the golden JSON stored at `golden_path`
/// (Requirement 9.1), reporting any difference with its exact location in
/// the JSON tree (Requirement 9.2).
///
/// `golden_path` is resolved relative to `CARGO_MANIFEST_DIR` unless it is
/// already absolute (letting a caller's own test point at a location
/// outside this crate's tree if it ever needs to, e.g. a temp directory in
/// this module's own tests).
///
/// If [`UPDATE_GOLDEN_ENV`] is set to a non-empty value, this function
/// writes `actual_json` to `golden_path` (creating parent directories as
/// needed) instead of comparing, establishing/overwriting the baseline.
///
/// # Panics
/// - If the golden file does not exist and [`UPDATE_GOLDEN_ENV`] is not
///   set (with guidance on how to create one).
/// - If the golden file's content is not valid JSON.
/// - If `actual_json` differs from the golden, with every mismatch's
///   location and detail listed.
pub fn assert_golden(golden_path: &str, actual_json: &Value) {
    let resolved = resolve_manifest_relative(golden_path);

    if update_golden_requested() {
        write_golden(&resolved, actual_json);
        return;
    }

    let expected = read_golden(&resolved, golden_path);
    let mismatches = diff(&expected, actual_json);
    if !mismatches.is_empty() {
        panic!("{}", format_report(golden_path, &resolved, &mismatches));
    }
}

/// Registers `captured` as a named fixture (Requirement 9.5): serializes it
/// as pretty-printed JSON under [`FIXTURES_DIR`] (relative to
/// `CARGO_MANIFEST_DIR`) as `<name>.json`, creating the directory if it does
/// not exist yet. A later contract test retrieves it via [`load_fixture`].
///
/// `name` must be a bare identifier (no path separators, not `.`/`..`) —
/// this function owns the storage location, unlike [`assert_golden`]'s
/// caller-controlled `golden_path`.
///
/// # Panics
/// If `name` is not a bare identifier, or if writing the fixture file
/// fails.
pub fn register_fixture(name: &str, captured: CapturedExchange) {
    let path = fixture_path(name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap_or_else(|err| {
            panic!(
                "contract harness: failed to create fixtures directory {}: {err}",
                parent.display()
            )
        });
    }
    let pretty = serde_json::to_string_pretty(&captured)
        .expect("CapturedExchange always serializes to JSON");
    fs::write(&path, format!("{pretty}\n")).unwrap_or_else(|err| {
        panic!(
            "contract harness: failed to write fixture {:?} to {}: {err}",
            name,
            path.display()
        )
    });
}

/// Loads a fixture previously written by [`register_fixture`] under the
/// same `name`. Not part of design.md's illustrative Service Interface, but
/// necessary for the harness to be a *usable* extension point rather than a
/// write-only sink: a later spec's own contract test needs a way to read
/// back a registered real-client capture to use as its acceptance
/// criterion.
///
/// # Panics
/// If no fixture is registered under `name`, or its content is not valid
/// JSON matching [`CapturedExchange`]'s shape.
pub fn load_fixture(name: &str) -> CapturedExchange {
    let path = fixture_path(name);
    let raw = fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "contract harness: fixture {name:?} could not be read from {}: {err}",
            path.display()
        )
    });
    serde_json::from_str(&raw).unwrap_or_else(|err| {
        panic!(
            "contract harness: fixture {name:?} at {} is not a valid CapturedExchange: {err}",
            path.display()
        )
    })
}

/// Returns `true` if [`UPDATE_GOLDEN_ENV`] is set to any non-empty value.
fn update_golden_requested() -> bool {
    std::env::var(UPDATE_GOLDEN_ENV)
        .map(|value| !value.is_empty())
        .unwrap_or(false)
}

/// Resolves `relative` against `CARGO_MANIFEST_DIR`, unless `relative` is
/// already an absolute path (in which case it is returned unchanged).
fn resolve_manifest_relative(relative: &str) -> PathBuf {
    let candidate = Path::new(relative);
    if candidate.is_absolute() {
        return candidate.to_path_buf();
    }
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR must be set when running under cargo build/test");
    Path::new(&manifest_dir).join(relative)
}

/// Builds the fixture file path for `name`, validating it is a bare
/// identifier first (never a path fragment supplied by
/// [`register_fixture`]/[`load_fixture`] callers).
fn fixture_path(name: &str) -> PathBuf {
    assert!(
        !name.is_empty()
            && name != "."
            && name != ".."
            && !name.contains('/')
            && !name.contains('\\'),
        "contract harness: fixture name must be a bare identifier with no path separators, got {name:?}"
    );
    resolve_manifest_relative(FIXTURES_DIR).join(format!("{name}.json"))
}

/// Writes `actual_json` as the new golden content at `resolved`, creating
/// parent directories as needed.
fn write_golden(resolved: &Path, actual_json: &Value) {
    if let Some(parent) = resolved.parent() {
        fs::create_dir_all(parent).unwrap_or_else(|err| {
            panic!(
                "contract harness: failed to create golden directory {}: {err}",
                parent.display()
            )
        });
    }
    let pretty =
        serde_json::to_string_pretty(actual_json).expect("serde_json::Value always serializes");
    fs::write(resolved, format!("{pretty}\n")).unwrap_or_else(|err| {
        panic!(
            "contract harness: failed to write golden file {}: {err}",
            resolved.display()
        )
    });
}

/// Reads and parses the golden file at `resolved`. `golden_path` (the
/// original, unresolved argument) is threaded through purely so the panic
/// message shows what the caller passed, not just the resolved absolute
/// path.
fn read_golden(resolved: &Path, golden_path: &str) -> Value {
    let raw = fs::read_to_string(resolved).unwrap_or_else(|err| {
        panic!(
            "contract harness: golden file {golden_path:?} (resolved: {}) could not be read: \
             {err}. If this is a new contract, create it by re-running with \
             {UPDATE_GOLDEN_ENV}=1 set.",
            resolved.display()
        )
    });
    serde_json::from_str(&raw).unwrap_or_else(|err| {
        panic!("contract harness: golden file {golden_path:?} is not valid JSON: {err}")
    })
}

/// Recursively compares `expected` against `actual`, collecting every
/// mismatch with its exact location (Requirement 9.2).
fn diff(expected: &Value, actual: &Value) -> Vec<Mismatch> {
    let mut mismatches = Vec::new();
    let mut path = Vec::new();
    diff_at(&mut path, expected, actual, &mut mismatches);
    mismatches
}

fn diff_at(path: &mut Vec<String>, expected: &Value, actual: &Value, out: &mut Vec<Mismatch>) {
    if expected == actual {
        return;
    }

    match (expected, actual) {
        (Value::Object(expected_map), Value::Object(actual_map)) => {
            for (key, expected_value) in expected_map {
                path.push(format!(".{key}"));
                match actual_map.get(key) {
                    Some(actual_value) => diff_at(path, expected_value, actual_value, out),
                    None => out.push(Mismatch {
                        path: render_path(path),
                        detail: format!("missing key (expected {})", compact(expected_value)),
                    }),
                }
                path.pop();
            }
            for (key, actual_value) in actual_map {
                if !expected_map.contains_key(key) {
                    path.push(format!(".{key}"));
                    out.push(Mismatch {
                        path: render_path(path),
                        detail: format!("unexpected key (got {})", compact(actual_value)),
                    });
                    path.pop();
                }
            }
        }
        (Value::Array(expected_items), Value::Array(actual_items)) => {
            if expected_items.len() != actual_items.len() {
                out.push(Mismatch {
                    path: render_path(path),
                    detail: format!(
                        "array length mismatch: expected {} item(s), got {}",
                        expected_items.len(),
                        actual_items.len()
                    ),
                });
            }
            let common = expected_items.len().min(actual_items.len());
            for (index, (expected_item, actual_item)) in expected_items
                .iter()
                .zip(actual_items.iter())
                .enumerate()
                .take(common)
            {
                path.push(format!("[{index}]"));
                diff_at(path, expected_item, actual_item, out);
                path.pop();
            }
        }
        _ => out.push(Mismatch {
            path: render_path(path),
            detail: format!("expected {}, got {}", compact(expected), compact(actual)),
        }),
    }
}

/// Renders `path` (a stack of `.key`/`[index]` segments) as a single
/// JSON-pointer-style location string, e.g. `$.attributes.tags[2]`.
fn render_path(path: &[String]) -> String {
    let mut rendered = String::from("$");
    for segment in path {
        rendered.push_str(segment);
    }
    rendered
}

/// Compact single-line JSON rendering of `value`, for embedding in a
/// mismatch detail message.
fn compact(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<unserializable>".to_string())
}

/// Formats the full mismatch report [`assert_golden`] panics with: a header
/// naming the golden file (both as the caller passed it and its resolved
/// absolute path) and mismatch count, followed by one bullet per
/// [`Mismatch`] (Requirement 9.2).
fn format_report(golden_path: &str, resolved: &Path, mismatches: &[Mismatch]) -> String {
    let mut report = format!(
        "contract harness: golden mismatch for {golden_path:?} (resolved: {}): {} mismatch(es)\n",
        resolved.display(),
        mismatches.len()
    );
    for mismatch in mismatches {
        let _ = writeln!(report, "  - {}: {}", mismatch.path, mismatch.detail);
    }
    report
}
