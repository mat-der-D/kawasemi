//! Unit tests for [`super::SearchResultSerializer`] (task 4.1 completion
//! definition: "空種別が `[]` になり、Account/Status が上流出力のまま格納さ
//! れ...ゴールデンが決定的に再現される契約テストが通る"; Requirements 1.1,
//! 1.2, 1.4, 1.5).

use serde_json::json;

use super::SearchResultSerializer;
use crate::search::model::TagView;
use crate::search::tag_serializer::TagSerializer;

fn serializer() -> SearchResultSerializer {
    SearchResultSerializer::new()
}

/// A hand-built stand-in for accounts-and-instance's own Account JSON
/// contract (opaque to this module -- see this module's doc comment,
/// "No re-serialization").
fn stand_in_account_json() -> serde_json::Value {
    json!({
        "id": "1",
        "username": "alice",
        "acct": "alice",
        "display_name": "Alice",
    })
}

/// A hand-built stand-in for statuses-core's own Status JSON contract.
fn stand_in_status_json() -> serde_json::Value {
    json!({
        "id": "42",
        "content": "<p>hello fediverse</p>",
        "account": stand_in_account_json(),
    })
}

fn hashtag_json() -> serde_json::Value {
    TagSerializer::new("kawasemi.example").build_tag(&TagView {
        name: "fediverse".to_string(),
        url: "/tags/fediverse".to_string(),
        history: Vec::new(),
    })
}

/// Requirement 1.1: the SearchResults envelope carries exactly `accounts`/
/// `statuses`/`hashtags`.
#[test]
fn assembles_the_three_result_arrays() {
    let json = serializer().build_search_results(
        vec![stand_in_account_json()],
        vec![stand_in_status_json()],
        vec![hashtag_json()],
    );
    assert!(json["accounts"].is_array());
    assert!(json["statuses"].is_array());
    assert!(json["hashtags"].is_array());
    assert_eq!(json.as_object().unwrap().len(), 3);
}

/// Requirement 1.4: an absent/empty type is `[]`, never `null`, for every
/// one of the three fields independently.
#[test]
fn empty_types_render_as_empty_arrays_not_null() {
    let json = serializer().build_search_results(Vec::new(), Vec::new(), Vec::new());
    assert_eq!(json["accounts"], json!([]));
    assert_eq!(json["statuses"], json!([]));
    assert_eq!(json["hashtags"], json!([]));
    assert_ne!(json["accounts"], serde_json::Value::Null);
    assert_ne!(json["statuses"], serde_json::Value::Null);
    assert_ne!(json["hashtags"], serde_json::Value::Null);
}

/// Requirement 1.4, mixed case: only the populated type(s) carry elements,
/// the rest independently stay `[]`.
#[test]
fn one_populated_type_does_not_affect_the_others_empty_array_discipline() {
    let json =
        serializer().build_search_results(vec![stand_in_account_json()], Vec::new(), Vec::new());
    assert_eq!(json["accounts"].as_array().unwrap().len(), 1);
    assert_eq!(json["statuses"], json!([]));
    assert_eq!(json["hashtags"], json!([]));
}

/// Requirement 1.2: upstream Account/Status JSON is embedded byte-for-byte,
/// never re-serialized or reshaped by this module.
#[test]
fn embeds_upstream_account_and_status_json_verbatim() {
    let account = stand_in_account_json();
    let status = stand_in_status_json();
    let json =
        serializer().build_search_results(vec![account.clone()], vec![status.clone()], Vec::new());
    assert_eq!(json["accounts"][0], account);
    assert_eq!(json["statuses"][0], status);
}

/// The same three inputs fed through two independently-constructed
/// serializers produce byte-identical JSON (Requirement 1.5).
#[test]
fn same_input_reproduces_identical_json_across_independent_serializer_instances() {
    let a = SearchResultSerializer::new().build_search_results(
        vec![stand_in_account_json()],
        vec![stand_in_status_json()],
        vec![hashtag_json()],
    );
    let b = SearchResultSerializer::new().build_search_results(
        vec![stand_in_account_json()],
        vec![stand_in_status_json()],
        vec![hashtag_json()],
    );
    assert_eq!(a, b);
}

// ---- Requirement 1.5: contract-harness golden registration ----
//
// Registers SearchResults JSON goldens via `crate::contract::assert_golden`
// (mirrors `src/search/tag_serializer/tests.rs`'s identical precedent, in
// turn mirroring `src/notifications/serializer/tests.rs`'s/
// `src/accounts/serializer/tests.rs`'s): a pure serializer needs no
// `RuntimeContext` boundary for determinism -- literal fixtures already
// satisfy Requirement 1.5's "決定的に再現可能".

#[test]
fn empty_search_results_json_matches_the_registered_contract_golden() {
    let json = serializer().build_search_results(Vec::new(), Vec::new(), Vec::new());
    crate::contract::assert_golden("tests/golden/search/search_results_empty.json", &json);
}

#[test]
fn populated_search_results_json_matches_the_registered_contract_golden() {
    let json = serializer().build_search_results(
        vec![stand_in_account_json()],
        vec![stand_in_status_json()],
        vec![hashtag_json()],
    );
    crate::contract::assert_golden("tests/golden/search/search_results_populated.json", &json);
}
