//! `RelationshipSerializer` (design.md "Service / サービス層" ->
//! "RelationshipSerializer / InstanceSerializer / CustomEmojiSerializer";
//! Requirements 5.1, 5.2, 5.4; task 3.2, `Boundary: RelationshipSerializer`):
//! maps a [`RelationshipView`] onto the Mastodon-compatible `relationships`
//! JSON contract.
//!
//! Scope: this module owns exactly the mapping from an already-resolved
//! [`RelationshipView`] to Relationship JSON. It does not resolve
//! relationship state itself (that is `RelationshipStateProvider`'s job,
//! task 1.3 — `crate::accounts::ports::RelationshipStateProvider`, whose
//! built-in default, `NoRelationshipProvider`, already produces the exact
//! "no relationship" [`RelationshipView`] shape this module serializes),
//! does not implement `AccountService::relationships` (task 5.3, the
//! eventual caller that queries the provider and hands each resulting
//! `RelationshipView` to this module), and registers no contract-harness
//! golden (task 3.5) — this module's own tests only prove the mapping
//! itself: every Requirement 5.2 field present, and the Requirement 5.4
//! "no relationship" default (all booleans `false`, all counts/`languages`
//! empty, `note` empty) round-trips to JSON unchanged.
//!
//! ## Typed struct + `Serialize`, not a hand-built `serde_json::json!` value
//! Follows `src/accounts/serializer.rs`'s (task 3.1, `AccountSerializer`)
//! and `src/media/serializer.rs`'s established precedent: [`RelationshipJson`]
//! is a plain `#[derive(Serialize)]` struct mirroring Requirement 5.2's field
//! list field-by-field (in the exact order Requirement 5.2 lists them, which
//! is also [`RelationshipView`]'s own field order), not a `json!{...}`
//! literal a field could silently go missing from. [`to_relationship_json`]/
//! [`relationship_to_json`] mirror `serializer.rs`'s `to_account_json`/
//! `account_to_json` pair exactly: the former is a pure, total
//! `RelationshipView -> RelationshipJson` mapping; the latter is a thin
//! `serde_json::to_value` wrapper for contract testing (task 3.5 will reuse
//! it verbatim when it registers the golden).
//!
//! ## No local/remote branching, no injected config
//! Unlike `AccountSerializer` (which needs a server domain for default
//! avatar/header URLs and a `MediaStore`/`ForwardedOrigin` for local media
//! resolution), a `RelationshipView` is already a single flat, fully-
//! resolved domain value (design.md's model doc: "`RelationshipView` は Req
//! 5.2 の全フラグ") with no local/remote distinction and no field that needs
//! an external default supplied at render time — every field maps straight
//! across. [`RelationshipSerializer`] is therefore a zero-field unit struct
//! purely for interface parity with design.md's literal Service Interface
//! sketch (`pub fn build_relationship(&self, view: &RelationshipView) ->
//! serde_json::Value;`) and with `AccountSerializer`'s/(future)
//! `InstanceSerializer`'s method-on-a-serializer shape that `AccountService`
//! (task 5.x) will hold alongside them — it carries no state and performs no
//! I/O.
//!
//! ## `id` serialization
//! [`RelationshipView::id`] is `crate::domain::Id`, whose own `Serialize`
//! impl (`src/domain/primitives.rs`) already renders as a decimal string
//! (Mastodon-compatible API convention for numeric ids) — this module does
//! not re-implement that, it just uses the field's own `Serialize` behavior
//! via `#[derive(Serialize)]`, the same way [`crate::accounts::serializer::AccountJson::id`]
//! does.

#[cfg(test)]
mod tests;

use serde::Serialize;
use serde_json::Value;

use crate::accounts::model::RelationshipView;

/// The Mastodon-compatible `relationships` entity JSON contract (Requirement
/// 5.2: "少なくとも `id` / `following` / `showing_reblogs` / `notifying` /
/// `languages` / `followed_by` / `blocking` / `blocked_by` / `muting` /
/// `muting_notifications` / `requested` / `requested_by` / `domain_blocking`
/// / `endorsed` / `note`"). Field order matches Requirement 5.2's own
/// listing (and [`RelationshipView`]'s own field order).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RelationshipJson {
    pub id: crate::domain::Id,
    pub following: bool,
    pub showing_reblogs: bool,
    pub notifying: bool,
    pub languages: Vec<String>,
    pub followed_by: bool,
    pub blocking: bool,
    pub blocked_by: bool,
    pub muting: bool,
    pub muting_notifications: bool,
    pub requested: bool,
    pub requested_by: bool,
    pub domain_blocking: bool,
    pub endorsed: bool,
    pub note: String,
}

/// Projects a [`RelationshipView`] into [`RelationshipJson`] — a pure, total
/// mapping with no branching: every field of `view` maps straight across to
/// its same-named [`RelationshipJson`] field (Requirement 5.1, 5.2). When
/// `view` already holds the Requirement 5.4 "no relationship" default (as
/// `NoRelationshipProvider`, task 1.3, always produces), the resulting JSON
/// has every boolean `false`, `languages` an empty array, and `note` an
/// empty string.
pub fn to_relationship_json(view: &RelationshipView) -> RelationshipJson {
    RelationshipJson {
        id: view.id,
        following: view.following,
        showing_reblogs: view.showing_reblogs,
        notifying: view.notifying,
        languages: view.languages.clone(),
        followed_by: view.followed_by,
        blocking: view.blocking,
        blocked_by: view.blocked_by,
        muting: view.muting,
        muting_notifications: view.muting_notifications,
        requested: view.requested,
        requested_by: view.requested_by,
        domain_blocking: view.domain_blocking,
        endorsed: view.endorsed,
        note: view.note.clone(),
    }
}

/// [`to_relationship_json`], converted to a plain [`serde_json::Value`]
/// (matching `accounts/serializer.rs::account_to_json`'s and
/// `media/serializer.rs::to_json`'s convention).
pub fn relationship_to_json(view: &RelationshipView) -> Value {
    serde_json::to_value(to_relationship_json(view))
        .expect("RelationshipJson always serializes to JSON")
}

/// Maps a [`RelationshipView`] onto the Relationship JSON contract
/// (Requirements 5.1, 5.2, 5.4). See this module's doc comment ("No
/// local/remote branching, no injected config") for why this holds no
/// state.
#[derive(Debug, Clone, Copy, Default)]
pub struct RelationshipSerializer;

impl RelationshipSerializer {
    /// Builds a new serializer. Takes no configuration — see this module's
    /// doc comment.
    pub fn new() -> Self {
        RelationshipSerializer
    }

    /// Builds the Relationship JSON for `view` (Requirement 5.1, 5.2, 5.4).
    pub fn build_relationship(&self, view: &RelationshipView) -> Value {
        relationship_to_json(view)
    }
}
