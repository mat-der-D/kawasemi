//! `CustomEmojiSerializer` (design.md "Service / サービス層" ->
//! "RelationshipSerializer / InstanceSerializer / CustomEmojiSerializer";
//! Requirements 9.2, 9.4; task 3.4, `Boundary: CustomEmojiSerializer`): maps
//! a [`CustomEmojiView`] onto the Mastodon-compatible `custom_emojis` JSON
//! contract.
//!
//! Scope: this module owns exactly the mapping from an already-resolved
//! [`CustomEmojiView`] to CustomEmoji JSON. It does not read
//! `custom_emojis` from the database itself (that is
//! `CustomEmojiRepository`'s job, task 2.3, already implemented), does not
//! implement `CustomEmojiService` (task 5.6, the eventual caller that lists
//! visible emojis via the repository and hands each one to this module),
//! and registers no contract-harness golden (task 3.5) — this module's own
//! tests only prove the mapping itself: every Requirement 9.2 field present
//! with the right type, and that the produced JSON shares its
//! representation with what `AccountSerializer` already emits for each
//! `emojis` entry (Requirement 9.4).
//!
//! ## Shared representation with Account's `emojis` (Requirement 9.4) — how
//! Requirement 9.2 says CustomEmoji `shall` contain "少なくとも `shortcode` /
//! `url` / `static_url` / `visible_in_picker` / `category`"; Requirement 9.4
//! additionally requires that when `AccountSerializer` builds an account's
//! `emojis` array, it "shall ... `custom_emojis` と同一の読み取りモデル・同一
//! の CustomEmoji 表現を用いる" (use the *same* CustomEmoji representation).
//! `src/accounts/serializer.rs` (task 3.1, already implemented and
//! reviewed) already defined exactly this JSON shape as
//! [`crate::accounts::serializer::CustomEmojiJson`] — a plain
//! `#[derive(Serialize)]` struct with precisely Requirement 9.2's five
//! fields, already `pub` (struct and every field), and already re-exported
//! at `crate::accounts::CustomEmojiJson` (see `src/accounts.rs`'s `pub use
//! serializer::{... CustomEmojiJson ...}`). This module therefore *imports
//! and reuses that exact type* rather than defining a second, same-shaped
//! struct that could silently drift from it — a second definition would
//! violate "同一表現を共有" the moment one of the two structs' field list
//! changed without the other noticing, even if both happened to serialize
//! identically today. No change to `serializer.rs` was needed to reuse it:
//! [`CustomEmojiJson`](crate::accounts::serializer::CustomEmojiJson) was
//! already `pub` with every field `pub` before this task started; only its
//! private mapping function `emoji_to_json` (not the type) is
//! module-private in `serializer.rs`, so this module defines its own
//! equally-trivial, total `CustomEmojiView -> CustomEmojiJson` mapping
//! ([`to_custom_emoji_json`]) rather than reaching into another module's
//! private function — the *type* (the actual representation Requirement 9.4
//! cares about) is what is shared, and it is shared by direct reuse, not by
//! parallel redefinition.
//!
//! ## Typed struct + `Serialize`, not a hand-built `serde_json::json!` value
//! Follows `src/accounts/serializer.rs`'s (task 3.1) and
//! `src/accounts/relationship_serializer.rs`'s (task 3.2) established
//! precedent: [`to_custom_emoji_json`]/[`custom_emoji_to_json`] mirror
//! `relationship_serializer.rs`'s `to_relationship_json`/
//! `relationship_to_json` pair exactly — the former is a pure, total
//! mapping; the latter is a thin `serde_json::to_value` wrapper (task 3.5
//! will reuse it verbatim when it registers the golden).
//!
//! ## No local/remote branching, no injected config
//! Like [`crate::accounts::relationship_serializer::RelationshipSerializer`]
//! (and unlike `AccountSerializer`, which needs a server domain and a
//! `MediaStore`/`ForwardedOrigin`), a [`CustomEmojiView`] is already a
//! single flat, fully-resolved domain value (design.md's model doc:
//! "`CustomEmojiView` は `shortcode`/`url`/`static_url`/`visible_in_picker`/
//! `category`") with no field that needs an external default supplied at
//! render time — every field maps straight across. [`CustomEmojiSerializer`]
//! is therefore a zero-field unit struct purely for interface parity with
//! design.md's literal Service Interface sketch (`pub fn
//! build_custom_emoji(&self, emoji: &CustomEmojiView) ->
//! serde_json::Value;`) and with `AccountSerializer`'s/
//! `RelationshipSerializer`'s/`InstanceSerializer`'s method-on-a-serializer
//! shape that `CustomEmojiService` (task 5.6) will hold — it carries no
//! state and performs no I/O.

#[cfg(test)]
mod tests;

use serde_json::Value;

use crate::accounts::model::CustomEmojiView;
use crate::accounts::serializer::CustomEmojiJson;

/// Projects a [`CustomEmojiView`] into [`CustomEmojiJson`] — a pure, total
/// mapping with no branching: every field of `view` maps straight across to
/// its same-named [`CustomEmojiJson`] field (Requirement 9.2). This is the
/// exact same target type `AccountSerializer` (task 3.1) already produces
/// for each entry of an account's `emojis` array (Requirement 9.4) — see
/// this module's doc comment ("Shared representation with Account's
/// `emojis`").
pub fn to_custom_emoji_json(view: &CustomEmojiView) -> CustomEmojiJson {
    CustomEmojiJson {
        shortcode: view.shortcode.clone(),
        url: view.url.clone(),
        static_url: view.static_url.clone(),
        visible_in_picker: view.visible_in_picker,
        category: view.category.clone(),
    }
}

/// [`to_custom_emoji_json`], converted to a plain [`serde_json::Value`]
/// (matching `relationship_serializer.rs::relationship_to_json`'s and
/// `serializer.rs::account_to_json`'s convention).
pub fn custom_emoji_to_json(view: &CustomEmojiView) -> Value {
    serde_json::to_value(to_custom_emoji_json(view))
        .expect("CustomEmojiJson always serializes to JSON")
}

/// Maps a [`CustomEmojiView`] onto the CustomEmoji JSON contract
/// (Requirements 9.2, 9.4). See this module's doc comment ("No local/remote
/// branching, no injected config") for why this holds no state.
#[derive(Debug, Clone, Copy, Default)]
pub struct CustomEmojiSerializer;

impl CustomEmojiSerializer {
    /// Builds a new serializer. Takes no configuration — see this module's
    /// doc comment.
    pub fn new() -> Self {
        CustomEmojiSerializer
    }

    /// Builds the CustomEmoji JSON for `view` (Requirement 9.2, 9.4).
    pub fn build_custom_emoji(&self, view: &CustomEmojiView) -> Value {
        custom_emoji_to_json(view)
    }
}
