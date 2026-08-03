//! `TagSerializer` (design.md "Serialization / シリアライズ層" ->
//! "#### TagSerializer / SearchResultSerializer"; Requirements 1.3, 1.5;
//! task 4.1, `Boundary: TagSerializer`): renders a [`TagView`] (task 1.2,
//! `crate::search::model`) into the Mastodon-compatible Tag JSON contract
//! (`name`/`url`/`history`).
//!
//! Scope: this module owns exactly [`TagSerializer`] and its
//! `build_tag`/`new` methods — design.md's own literal Service Interface
//! excerpt (`pub fn build_tag(&self, tag: &TagView) -> serde_json::Value;`).
//! It does not construct [`TagView`] values itself (that is
//! `HashtagIndexRepository::match_hashtags`'s job, task 2.1,
//! `crate::search::hashtag_repository`) and does not assemble the outer
//! SearchResults envelope (`SearchResultSerializer`, this same task,
//! `crate::search::result_serializer`).
//!
//! ## `TagView::url` is domain-relative; this module makes it absolute
//! `hashtag_repository.rs`'s own doc comment ("`TagView::url`: a
//! domain-relative path, not an absolute URL") explains why
//! `TagView::url` already carries a `/tags/{name}` path segment but no
//! scheme/host: `HashtagIndexRepository::match_hashtags` has no
//! server-domain/origin parameter to build one from, and explicitly defers
//! "making that a full absolute URL" to "whichever downstream layer
//! actually has origin context (task 4.1/4.2)". This module is that layer:
//! [`TagSerializer::build_tag`] prefixes `tag.url` with `{scheme}://{host}`
//! resolved from this serializer's own configured `domain`, mirroring the
//! `format!("{}://{}/tags/{}", origin.scheme, origin.host, tag.name)`
//! pattern already established at `src/statuses/endpoints.rs`,
//! `src/notifications/service.rs`, and `src/timelines/hydrator.rs` for the
//! identical `/tags/{name}` URL shape — the only difference is that those
//! three sites build the relative path themselves from a bare tag `name`,
//! while this module's input ([`TagView::url`]) already carries that
//! relative path pre-built, so it is appended verbatim rather than
//! reconstructed.
//!
//! ## No per-request `ForwardedOrigin` available (mirrors
//! `NotificationService`)
//! design.md's literal `build_tag(&self, tag: &TagView)` signature takes no
//! request/origin parameter, so — like
//! `crate::notifications::service::NotificationService` (that module's own
//! doc comment, "Rendering `account`/`status`": "no per-request
//! `ForwardedOrigin` available...this many layers away from a live HTTP
//! request") — this serializer cannot honor `X-Forwarded-Proto`/
//! `X-Forwarded-Host` on a per-request basis. It instead resolves a fixed
//! `https://{domain}` origin from its own constructor-supplied `domain`
//! (`ForwardedOrigin::resolve("https", &self.domain, None, None)`, the same
//! call `NotificationService::origin` makes), consistent with this crate's
//! established precedent for serializers that sit downstream of a live
//! request context.
//!
//! ## `history` embedding: no reshaping, `[]` when empty
//! [`TagView::history`] is already the minimal `day`/`uses`/`accounts`
//! aggregate `crate::search::model::TagHistoryEntry` defines (design.md:
//! "`history` は最小集計（または空配列）") — this module renders each entry's
//! three fields verbatim into a JSON object and collects them into a JSON
//! array. An empty `Vec` serializes as `[]`, never `null`, satisfying
//! Requirement 1.4's array-not-null discipline without any special-casing.

#[cfg(test)]
mod tests;

use serde_json::{Value, json};

use crate::api::pagination::ForwardedOrigin;
use crate::search::model::TagView;

/// Renders [`TagView`] values into the Tag JSON contract (`name`/`url`/
/// `history`; Requirement 1.3). See this module's doc comment for the full
/// rationale behind its `domain`-only (no per-request origin) construction.
#[derive(Debug, Clone)]
pub struct TagSerializer {
    domain: String,
}

impl TagSerializer {
    /// Builds a `TagSerializer` bound to `domain` (this instance's own bare
    /// server domain, e.g. `"example.social"` — the same shape
    /// `crate::accounts::serializer::AccountSerializer::new`/
    /// `crate::federation::urls::ActorUrls::new` take).
    pub fn new(domain: impl Into<String>) -> Self {
        TagSerializer {
            domain: domain.into(),
        }
    }

    /// See this module's doc comment ("No per-request `ForwardedOrigin`
    /// available").
    fn origin(&self) -> ForwardedOrigin {
        ForwardedOrigin::resolve("https", &self.domain, None, None)
    }

    /// Builds the Tag JSON contract for `tag` (Requirement 1.3): `name`
    /// verbatim, `url` made absolute from `tag.url`'s already-built
    /// domain-relative path (see this module's doc comment, "`TagView::url`
    /// is domain-relative"), and `history` rendered from `tag.history`
    /// (empty `Vec` -> `[]`, never `null` — Requirement 1.4).
    pub fn build_tag(&self, tag: &TagView) -> Value {
        let origin = self.origin();
        let history: Vec<Value> = tag
            .history
            .iter()
            .map(|entry| {
                json!({
                    "day": entry.day,
                    "uses": entry.uses,
                    "accounts": entry.accounts,
                })
            })
            .collect();
        json!({
            "name": tag.name,
            "url": format!("{}://{}{}", origin.scheme, origin.host, tag.url),
            "history": history,
        })
    }
}
