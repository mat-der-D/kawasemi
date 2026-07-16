//! Unit tests for the `ContractHarness` boundary (task 8.1).
//!
//! Covers [`assert_golden`]'s matching/mismatching behavior with
//! location-specific diff reporting (Requirements 9.1, 9.2), the internal
//! write/read round trip the `UPDATE_GOLDEN_ENV`-triggered baseline-update
//! path relies on, and [`register_fixture`]/[`load_fixture`] round-tripping
//! (Requirement 9.5).
//!
//! Deliberately does **not** toggle [`UPDATE_GOLDEN_ENV`] itself: this
//! crate's unit tests all run in one shared process by default (`cargo
//! test`'s usual parallel-thread execution), so mutating a process-global
//! environment variable here could race with any other test in this same
//! binary that calls [`assert_golden`] expecting a mismatch panic. The
//! env-var-triggered dispatch is instead exercised once, in isolation, by
//! `tests/contract_harness_it.rs` (its own separate process/binary, and the
//! only test function in that file) — see that file's doc comment.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;

use super::*;

/// Builds a process-unique temp file path for a golden fixture, mirroring
/// `src/test_harness.rs`'s own `unique_schema_name` "counter + nanos"
/// convention so concurrently-running tests never collide.
fn unique_temp_path(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("kawasemi_contract_test_{label}_{nanos}_{seq}.json"))
}

/// Best-effort cleanup of a temp golden file this test wrote, regardless of
/// whether the test body panicked (mirrors `TestApp`'s own `Drop`-as-
/// best-effort convention in `src/test_harness.rs`).
struct TempFileGuard(PathBuf);
impl Drop for TempFileGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// Extracts a human-readable message from a caught panic payload (as
/// returned by `std::panic::catch_unwind`), for asserting on
/// [`assert_golden`]'s diff-report content.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else {
        "<panic payload was not a string>".to_string()
    }
}

#[test]
fn assert_golden_passes_silently_when_actual_matches_golden() {
    let path = unique_temp_path("match");
    let _guard = TempFileGuard(path.clone());
    fs::write(&path, r#"{"a":1,"b":{"c":2}}"#).expect("writing the test golden must succeed");

    let path_str = path.to_str().expect("temp path must be valid UTF-8");
    assert_golden(path_str, &json!({"a": 1, "b": {"c": 2}}));
}

#[test]
fn assert_golden_reports_exact_location_of_a_scalar_mismatch() {
    let path = unique_temp_path("scalar_mismatch");
    let _guard = TempFileGuard(path.clone());
    fs::write(&path, r#"{"attributes":{"username":"alice"}}"#)
        .expect("writing the test golden must succeed");
    let path_str = path
        .to_str()
        .expect("temp path must be valid UTF-8")
        .to_string();

    let result = std::panic::catch_unwind(|| {
        assert_golden(&path_str, &json!({"attributes": {"username": "bob"}}));
    });

    let err = result.expect_err("a scalar mismatch must panic");
    let message = panic_message(&*err);
    assert!(
        message.contains("$.attributes.username"),
        "message did not pinpoint the mismatch location: {message}"
    );
    assert!(
        message.contains("\"alice\"") && message.contains("\"bob\""),
        "message did not include both expected and actual values: {message}"
    );
}

#[test]
fn assert_golden_reports_nested_array_index_location() {
    let path = unique_temp_path("array_mismatch");
    let _guard = TempFileGuard(path.clone());
    fs::write(&path, r#"{"tags":["a","b","c"]}"#).expect("writing the test golden must succeed");
    let path_str = path
        .to_str()
        .expect("temp path must be valid UTF-8")
        .to_string();

    let result = std::panic::catch_unwind(|| {
        assert_golden(&path_str, &json!({"tags": ["a", "x", "c"]}));
    });

    let err = result.expect_err("an array element mismatch must panic");
    let message = panic_message(&*err);
    assert!(
        message.contains("$.tags[1]"),
        "message did not pinpoint the array index location: {message}"
    );
}

#[test]
fn assert_golden_reports_missing_key_location() {
    let path = unique_temp_path("missing_key");
    let _guard = TempFileGuard(path.clone());
    fs::write(&path, r#"{"a":1,"b":2}"#).expect("writing the test golden must succeed");
    let path_str = path
        .to_str()
        .expect("temp path must be valid UTF-8")
        .to_string();

    let result = std::panic::catch_unwind(|| {
        assert_golden(&path_str, &json!({"a": 1}));
    });

    let err = result.expect_err("a missing key must panic");
    let message = panic_message(&*err);
    assert!(message.contains("$.b"), "message was: {message}");
    assert!(message.contains("missing key"), "message was: {message}");
}

#[test]
fn assert_golden_reports_unexpected_key_location() {
    let path = unique_temp_path("extra_key");
    let _guard = TempFileGuard(path.clone());
    fs::write(&path, r#"{"a":1}"#).expect("writing the test golden must succeed");
    let path_str = path
        .to_str()
        .expect("temp path must be valid UTF-8")
        .to_string();

    let result = std::panic::catch_unwind(|| {
        assert_golden(&path_str, &json!({"a": 1, "extra": true}));
    });

    let err = result.expect_err("an unexpected key must panic");
    let message = panic_message(&*err);
    assert!(message.contains("$.extra"), "message was: {message}");
    assert!(message.contains("unexpected key"), "message was: {message}");
}

#[test]
fn assert_golden_reports_array_length_mismatch() {
    let path = unique_temp_path("array_len");
    let _guard = TempFileGuard(path.clone());
    fs::write(&path, r#"{"items":[1,2,3]}"#).expect("writing the test golden must succeed");
    let path_str = path
        .to_str()
        .expect("temp path must be valid UTF-8")
        .to_string();

    let result = std::panic::catch_unwind(|| {
        assert_golden(&path_str, &json!({"items": [1, 2]}));
    });

    let err = result.expect_err("an array length mismatch must panic");
    let message = panic_message(&*err);
    assert!(
        message.contains("array length mismatch"),
        "message was: {message}"
    );
    assert!(message.contains("$.items"), "message was: {message}");
}

#[test]
fn assert_golden_panics_with_update_env_guidance_when_golden_file_is_missing() {
    // Never created — proves the missing-golden path without needing
    // cleanup.
    let path = unique_temp_path("missing_file_never_created");
    let path_str = path
        .to_str()
        .expect("temp path must be valid UTF-8")
        .to_string();

    let result = std::panic::catch_unwind(|| {
        assert_golden(&path_str, &json!({"a": 1}));
    });

    let err = result.expect_err("a missing golden file must panic");
    let message = panic_message(&*err);
    assert!(
        message.contains(UPDATE_GOLDEN_ENV),
        "message did not mention the update-golden escape hatch: {message}"
    );
}

#[test]
fn write_golden_then_read_golden_round_trips_content() {
    // Exercises the same primitives `assert_golden`'s
    // `UPDATE_GOLDEN_ENV`-triggered branch calls, without touching the
    // process-global env var itself (see this module's doc comment).
    let path = unique_temp_path("write_read_roundtrip");
    let _guard = TempFileGuard(path.clone());
    let value = json!({"nested": {"n": 42}, "list": [1, 2, 3]});

    write_golden(&path, &value);
    let loaded = read_golden(&path, path.to_str().expect("temp path must be valid UTF-8"));

    assert_eq!(loaded, value);
}

#[test]
fn register_fixture_then_load_fixture_round_trips_the_captured_exchange() {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let name = format!("contract_harness_round_trip_test_{nanos}_{seq}");

    struct FixtureGuard(String);
    impl Drop for FixtureGuard {
        fn drop(&mut self) {
            let _ = fs::remove_file(fixture_path(&self.0));
        }
    }
    let _guard = FixtureGuard(name.clone());

    let captured = CapturedExchange {
        method: "GET".to_string(),
        path: "/api/v1/apps/verify_credentials".to_string(),
        request_body: None,
        status: 200,
        response_body: json!({"name": "test-client", "website": null}),
    };

    register_fixture(&name, captured.clone());
    let loaded = load_fixture(&name);

    assert_eq!(loaded, captured);
}

#[test]
fn register_fixture_preserves_a_present_request_body() {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let name = format!("contract_harness_round_trip_with_body_{nanos}_{seq}");

    struct FixtureGuard(String);
    impl Drop for FixtureGuard {
        fn drop(&mut self) {
            let _ = fs::remove_file(fixture_path(&self.0));
        }
    }
    let _guard = FixtureGuard(name.clone());

    let captured = CapturedExchange {
        method: "POST".to_string(),
        path: "/api/v1/apps".to_string(),
        request_body: Some(json!({"client_name": "test client"})),
        status: 200,
        response_body: json!({"client_id": "abc"}),
    };

    register_fixture(&name, captured.clone());
    let loaded = load_fixture(&name);

    assert_eq!(loaded, captured);
    assert_eq!(
        loaded.request_body,
        Some(json!({"client_name": "test client"}))
    );
}

#[test]
#[should_panic(expected = "bare identifier")]
fn register_fixture_rejects_names_with_path_separators() {
    register_fixture(
        "../escape",
        CapturedExchange {
            method: "GET".to_string(),
            path: "/x".to_string(),
            request_body: None,
            status: 200,
            response_body: json!({}),
        },
    );
}

#[test]
#[should_panic(expected = "bare identifier")]
fn load_fixture_rejects_names_with_path_separators() {
    let _ = load_fixture("nested/escape");
}
