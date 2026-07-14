//! Integration test for the `ContractHarness` boundary (task 9.5,
//! Requirements 9.1-9.5), proving [`register_fixture`]/[`load_fixture`]
//! function end to end as an *acceptance-criterion* mechanism (Requirement
//! 9.5) rather than just a standalone read/write round trip.
//!
//! `src/contract/tests.rs` already unit-tests that `register_fixture` then
//! `load_fixture` round-trips a hand-built [`CapturedExchange`] byte for
//! byte (Requirement 9.5's storage mechanics). What it does *not* cover,
//! and what this file adds, is the actual *usage pattern* the harness's own
//! module doc comment describes: a registered fixture's `response_body` fed
//! "straight into `assert_golden` as the expected side" — i.e. a real
//! (here, synthetic-standin-per-task-8.1's-boundary) client capture
//! becomes the golden that a live, deterministic-boundary-generated
//! response is then held to. That is what makes fixture registration an
//! *acceptance criterion* and not just a storage cubby.
//!
//! What this proves, end to end, with a sample resource (never a real
//! Mastodon entity contract — Account/Status/... remain out of this
//! spec's boundary per task 8.1's notes):
//! - (9.5) A [`CapturedExchange`] registered via `register_fixture` is
//!   retrieved unchanged via `load_fixture`.
//! - (9.3, 9.1, 9.5) The loaded fixture's `response_body` establishes a
//!   golden that a *second, independently-`spawn_test_app`-booted*
//!   instance's live output — generated purely from its own
//!   deterministic `RuntimeContext.ids`/`.clock`/`.rng` — satisfies,
//!   proving the fixture functions as a real acceptance gate against live
//!   determinism, not just a static string comparison.
//! - (9.2, 9.5) A deliberate divergence between the live output and the
//!   fixture-derived golden is caught by `assert_golden` with a
//!   location-pinpointed report, proving the fixture-as-acceptance-
//!   criterion path also enforces contract drift detection, not just a
//!   success path.
//!
//! This file sets [`UPDATE_GOLDEN_ENV`] itself (to establish the golden
//! from the fixture's `response_body`), which is safe here for the same
//! reason `tests/contract_harness_it.rs` documents: each `tests/*.rs` file
//! compiles to its own separate process, and this file contains exactly
//! one test function, so no concurrently-running test in this same process
//! can be thrown off by the env var being set mid-run.

use kawasemi::contract::{
    CapturedExchange, UPDATE_GOLDEN_ENV, assert_golden, load_fixture, register_fixture,
};
use kawasemi::runtime::RuntimeContext;
use kawasemi::test_harness::spawn_test_app;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Builds a synthetic "sample resource" JSON purely to exercise the
/// contract harness's fixture-as-acceptance-criterion mechanics against a
/// real `RuntimeContext` — not a stand-in for any real Mastodon entity
/// contract (mirrors `tests/contract_harness_it.rs`'s own helper of the
/// same shape; kept as a private per-file copy per this crate's existing
/// convention of not sharing test-only helpers across integration test
/// binaries).
fn sample_resource_json(runtime: &RuntimeContext) -> Value {
    let id = runtime.ids.next_id();
    let created_at = runtime.clock.now();
    let mut nonce_bytes = [0u8; 8];
    runtime.rng.fill_bytes(&mut nonce_bytes);
    json!({
        "id": id.as_i64(),
        "created_at": created_at.unix_timestamp(),
        "nonce": hex_encode(&nonce_bytes),
    })
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Builds a process-and-call-unique fixture name so this test never
/// collides with concurrently-running processes exercising the same
/// `tests/fixtures/` directory (mirrors `src/contract/tests.rs`'s own
/// "counter + nanos" convention).
fn unique_fixture_name(label: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("contract_harness_fixture_it_{label}_{nanos}_{seq}")
}

/// Removes [`UPDATE_GOLDEN_ENV`] from this process's environment
/// regardless of how the calling test exits (mirrors `TestApp`'s own
/// `Drop`-as-best-effort convention and `tests/contract_harness_it.rs`'s
/// identical guard).
struct EnvVarGuard(&'static str);
impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        unsafe {
            std::env::remove_var(self.0);
        }
    }
}

/// Best-effort cleanup of the fixture file this test registers, regardless
/// of whether the test body panicked.
struct FixtureGuard(String);
impl Drop for FixtureGuard {
    fn drop(&mut self) {
        // `register_fixture`'s storage path is not exported (only
        // `register_fixture`/`load_fixture` are public API), so mirror its
        // known layout (`tests/fixtures/<name>.json`, relative to this
        // crate's manifest dir) directly rather than reach into the crate's
        // private `fixture_path` helper from an external integration test.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(format!("{}.json", self.0));
        let _ = std::fs::remove_file(path);
    }
}

#[tokio::test]
async fn fixture_registered_from_a_captured_exchange_functions_as_the_golden_acceptance_criterion()
{
    let app_a = spawn_test_app().await;
    let sample_json = sample_resource_json(&app_a.runtime);

    // (9.5) Register a captured exchange — standing in for a real standard
    // client's request/response, per task 8.1's boundary — carrying the
    // sample resource as its response body.
    let fixture_name = unique_fixture_name("accept_criterion");
    let _fixture_guard = FixtureGuard(fixture_name.clone());
    let captured = CapturedExchange {
        method: "GET".to_string(),
        path: "/api/v1/sample_resource".to_string(),
        request_body: None,
        status: 200,
        response_body: sample_json.clone(),
    };
    register_fixture(&fixture_name, captured.clone());

    // (9.5) The registration round-trips unchanged through the public
    // extension point.
    let loaded = load_fixture(&fixture_name);
    assert_eq!(
        loaded, captured,
        "a registered fixture must be retrievable unchanged via load_fixture"
    );

    // (9.5, 9.1) Establish a golden from the *loaded fixture's*
    // response_body — exactly the "feed response_body straight into
    // assert_golden as the expected side" pattern src/contract.rs's module
    // doc comment describes as how a registered fixture becomes an
    // acceptance criterion.
    let golden_path = std::env::temp_dir()
        .join(format!(
            "kawasemi_contract_harness_fixture_it_{}.json",
            std::process::id()
        ))
        .to_str()
        .expect("temp path must be valid UTF-8")
        .to_string();
    {
        let _env_guard = EnvVarGuard(UPDATE_GOLDEN_ENV);
        unsafe {
            std::env::set_var(UPDATE_GOLDEN_ENV, "1");
        }
        assert_golden(&golden_path, &loaded.response_body);
    }
    assert!(
        std::env::var(UPDATE_GOLDEN_ENV).is_err(),
        "the update-golden env var must not leak past its guarded scope"
    );

    // (9.5, 9.3) A second, independently-spawned instance's live output —
    // generated purely from its own deterministic RuntimeContext — must
    // satisfy the fixture-derived golden: the registered fixture is acting
    // as a real acceptance gate against live determinism, not a static
    // string compare against itself.
    let app_b = spawn_test_app().await;
    let live_json_b = sample_resource_json(&app_b.runtime);
    assert_golden(&golden_path, &live_json_b);

    // (9.5, 9.2) A deliberate divergence from the fixture-derived golden is
    // still caught, with the report pinpointing exactly where the
    // acceptance criterion was violated — the fixture path enforces drift
    // detection too, not just a success path.
    let mut diverged = live_json_b.clone();
    diverged["nonce"] = json!("0000000000000000");
    let golden_path_for_panic = golden_path.clone();
    let result = std::panic::catch_unwind(move || {
        assert_golden(&golden_path_for_panic, &diverged);
    });
    let err = result.expect_err(
        "a value diverging from the fixture-derived golden must cause assert_golden to panic",
    );
    let message = if let Some(message) = err.downcast_ref::<String>() {
        message.clone()
    } else if let Some(message) = err.downcast_ref::<&str>() {
        (*message).to_string()
    } else {
        String::from("<panic payload was not a string>")
    };
    assert!(
        message.contains("$.nonce"),
        "mismatch report did not pinpoint the diverged field's location: {message}"
    );

    let _ = std::fs::remove_file(&golden_path);

    app_a.cleanup().await;
    app_b.cleanup().await;
}
