//! `SearchResultSerializer` (design.md "Serialization / シリアライズ層" ->
//! "#### TagSerializer / SearchResultSerializer"; Requirements 1.1, 1.2,
//! 1.4, 1.5; task 4.1, `Boundary: SearchResultSerializer`): assembles the
//! Mastodon-compatible SearchResults JSON envelope (`accounts`/`statuses`/
//! `hashtags`) from already-rendered per-item JSON values.
//!
//! Scope: this module owns exactly [`SearchResultSerializer`] and its
//! `build_search_results`/`new` methods — design.md's own literal Service
//! Interface excerpt (`pub fn build_search_results(&self, accounts:
//! Vec<serde_json::Value>, statuses: Vec<serde_json::Value>, hashtags:
//! Vec<serde_json::Value>) -> serde_json::Value;`). It does not resolve
//! search matches into JSON itself — `accounts`/`statuses` are expected to
//! already be accounts-and-instance's/statuses-core's own upstream Account/
//! Status JSON contracts (`SearchHydrator`'s job, task 4.2, strictly
//! upstream of and outside this module's boundary) and `hashtags` is
//! expected to already be [`super::tag_serializer::TagSerializer`]-built Tag
//! JSON (this same task, `crate::search::tag_serializer`).
//!
//! ## No re-serialization of upstream `accounts`/`statuses` (Requirement
//! 1.2)
//! This spec does not own, and must not redefine, the Account/Status JSON
//! contracts (requirements.md's own Introduction: "本 spec は Account /
//! Status の JSON エンティティ契約を再定義しない"). [`build_search_results`]
//! therefore treats its `accounts`/`statuses` parameters as opaque,
//! already-final `serde_json::Value`s: each element is placed into the
//! output array by reference/move only (`Vec<Value>` -> `Value::Array`, via
//! `serde_json::json!`'s array-literal-from-`Vec` behavior), never
//! destructured, re-keyed, or passed back through any serializer. This is
//! provable structurally — the function signature itself takes
//! already-`Value`-typed `Vec`s and has no access to whatever
//! `AccountView`/`StatusRenderInput`-shaped domain type produced them, so
//! there is nothing for it to reshape even if it wanted to — and this
//! module's own unit tests additionally assert byte-identical embedding
//! (`assert_eq!` against the exact input `Value`) to catch any future
//! accidental reshaping.
//!
//! ## Empty-type array-not-null discipline (Requirement 1.4)
//! `accounts`/`statuses`/`hashtags` are always present as JSON arrays in
//! the output, `[]` when their input `Vec` is empty. This falls out of
//! `serde_json::json!`'s own `Vec<Value>` -> JSON-array serialization (an
//! empty `Vec` serializes as `[]`, never `null`) rather than needing any
//! `Option`/explicit-empty-check special-casing in this module — the same
//! "empty `Vec`, not `Option<Vec>`" discipline `crate::search::model`'s own
//! doc comment establishes for `SearchMatches`' fields, carried through
//! here at the JSON-rendering boundary.
//!
//! `SearchService`'s own job (task 5.1, out of this task's boundary) is to
//! ensure that a `type`-scoped request (Requirement 2.2, "指定された種別の
//! みを検索し、他の種別の結果を空配列で返す") actually passes an empty `Vec`
//! for the unscoped types before calling [`build_search_results`] — this
//! module has no `type` concept of its own and simply renders whatever
//! three `Vec`s it is handed.

#[cfg(test)]
mod tests;

use serde_json::{Value, json};

/// Assembles the SearchResults JSON envelope (`accounts`/`statuses`/
/// `hashtags`; Requirements 1.1, 1.4). Stateless — see this module's doc
/// comment for why it still exposes an instance method (`&self`) rather
/// than a free function, mirroring design.md's own literal
/// `SearchResultSerializer` Service Interface.
#[derive(Debug, Clone, Copy, Default)]
pub struct SearchResultSerializer;

impl SearchResultSerializer {
    /// Builds a `SearchResultSerializer`. Takes no configuration — unlike
    /// [`super::tag_serializer::TagSerializer`], this module never builds a
    /// URL or otherwise needs a server domain.
    pub fn new() -> Self {
        SearchResultSerializer
    }

    /// Assembles the SearchResults JSON envelope from already-rendered
    /// per-item JSON: `accounts` (accounts-and-instance Account JSON,
    /// Requirement 1.2), `statuses` (statuses-core Status JSON, Requirement
    /// 1.2), and `hashtags` ([`super::tag_serializer::TagSerializer`]-built
    /// Tag JSON, Requirement 1.3). Every field is always a JSON array,
    /// `[]` when its input is empty, never `null` (Requirement 1.1, 1.4).
    /// See this module's doc comment for why no element is ever
    /// re-serialized or reshaped.
    pub fn build_search_results(
        &self,
        accounts: Vec<Value>,
        statuses: Vec<Value>,
        hashtags: Vec<Value>,
    ) -> Value {
        json!({
            "accounts": accounts,
            "statuses": statuses,
            "hashtags": hashtags,
        })
    }
}
