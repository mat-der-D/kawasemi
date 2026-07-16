//! Integration test for the `ContractHarness` boundary (task 8.1,
//! Requirements 9.1-9.3), proving the harness composes correctly with
//! core-runtime's deterministic non-determinism boundary
//! (`RuntimeContext::deterministic`, exposed here via
//! [`spawn_test_app`](kawasemi::test_harness::spawn_test_app)).
//!
//! This file deliberately contains exactly one test function. It is the
//! one place in this crate that exercises [`kawasemi::contract`]'s
//! [`UPDATE_GOLDEN_ENV`](kawasemi::contract::UPDATE_GOLDEN_ENV)-triggered
//! baseline-write branch by actually setting that process environment
//! variable — safe here specifically *because* `tests/*.rs` files each
//! compile to their own separate process, and this file has no second test
//! function that could run concurrently in the same process and be thrown
//! off by the env var being set mid-run (see
//! `src/contract/tests.rs`'s own doc comment, which explains why its unit
//! tests avoid this same env var entirely).
//!
//! What this proves, end to end:
//! - (9.3) Two independently-`spawn_test_app`-booted instances — both
//!   always built from the same fixed `DeterministicSeed`
//!   (`src/test_harness.rs`) — produce byte-for-byte identical JSON for a
//!   synthetic sample payload built purely from their own
//!   `RuntimeContext.ids`/`RuntimeContext.clock`/`RuntimeContext.rng`.
//!   (This sample payload is *not* a real Mastodon entity — no individual
//!   entity contract is owned here, per this task's boundary.)
//! - (9.1) `assert_golden` established from one instance's output is
//!   reproduced by the other instance's output.
//! - (9.2) A deliberately mutated field triggers a mismatch report that
//!   pinpoints the exact JSON location that changed.

use kawasemi::contract::{UPDATE_GOLDEN_ENV, assert_golden};
use kawasemi::runtime::RuntimeContext;
use kawasemi::test_harness::spawn_test_app;
use serde_json::{Value, json};

/// Builds a synthetic "sample resource" JSON purely to exercise the
/// contract harness's determinism guarantee against a real
/// `RuntimeContext` — not a stand-in for any real Mastodon entity contract
/// (Account/Status/... remain out of this task's boundary).
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

/// Removes [`UPDATE_GOLDEN_ENV`] from this process's environment
/// regardless of how the calling test exits (mirrors `TestApp`'s own
/// `Drop`-as-best-effort convention), so a later assertion failure in this
/// same test can't leave the env var set for anything else this (single-
/// test) binary might otherwise run.
struct EnvVarGuard(&'static str);
impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        unsafe {
            std::env::remove_var(self.0);
        }
    }
}

#[tokio::test]
async fn same_deterministic_boundary_reproduces_the_same_golden_across_independent_instances() {
    let app_a = spawn_test_app().await;
    let app_b = spawn_test_app().await;

    let json_a = sample_resource_json(&app_a.runtime);
    let json_b = sample_resource_json(&app_b.runtime);
    assert_eq!(
        json_a, json_b,
        "spawn_test_app's fixed DeterministicSeed must reproduce identical sample JSON \
         across independently-booted instances"
    );

    let golden_path = std::env::temp_dir()
        .join(format!(
            "kawasemi_contract_harness_it_{}.json",
            std::process::id()
        ))
        .to_str()
        .expect("temp path must be valid UTF-8")
        .to_string();

    // Establish the golden from instance A's output via the
    // UPDATE_GOLDEN_ENV escape hatch (Requirement 9.1's "fix a golden"
    // half) rather than hand-authoring expected byte values for an
    // opaque, seed-derived id/clock/rng sequence.
    {
        let _env_guard = EnvVarGuard(UPDATE_GOLDEN_ENV);
        unsafe {
            std::env::set_var(UPDATE_GOLDEN_ENV, "1");
        }
        assert_golden(&golden_path, &json_a);
    }
    assert!(
        std::env::var(UPDATE_GOLDEN_ENV).is_err(),
        "the update-golden env var must not leak past its guarded scope"
    );

    // (9.1, 9.3) Instance B's independently-generated JSON reproduces the
    // exact same golden established from instance A.
    assert_golden(&golden_path, &json_b);

    // (9.2) A deliberately mutated value triggers a location-specific
    // mismatch report.
    let mut mutated = json_b.clone();
    mutated["nonce"] = json!("0000000000000000");
    let golden_path_for_panic = golden_path.clone();
    let result = std::panic::catch_unwind(move || {
        assert_golden(&golden_path_for_panic, &mutated);
    });
    let err = result.expect_err("a deliberately mutated field must cause assert_golden to panic");
    let message = if let Some(message) = err.downcast_ref::<String>() {
        message.clone()
    } else if let Some(message) = err.downcast_ref::<&str>() {
        (*message).to_string()
    } else {
        String::from("<panic payload was not a string>")
    };
    assert!(
        message.contains("$.nonce"),
        "mismatch report did not pinpoint the mutated field's location: {message}"
    );

    let _ = std::fs::remove_file(&golden_path);

    app_a.cleanup().await;
    app_b.cleanup().await;
}
