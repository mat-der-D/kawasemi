//! Timeline domain types (`model` component, design.md "Timeline Domain /
//! ドメイン層" -> `model / TimelineKindRules`, Requirements 1.1, 2.1, 3.1,
//! 4.1, 7.1, 8.1; task 1.1, `Boundary: model`).
//!
//! Scope: this module owns exactly the domain value types this task's own
//! instruction enumerates — [`TimelineKind`], [`TagFilter`],
//! [`TimelineParams`], [`TimelineQuerySpec`], and [`FilterContext`] — built
//! on core-runtime's [`Id`]/`time::OffsetDateTime` and api-foundation's
//! [`PageParams`] (`crate::api::pagination`), per design.md's exact type
//! sketch (design.md lines 271-277):
//!
//! ```ignore
//! pub enum TimelineKind { Home, Public, Local, Tag }
//! pub struct TagFilter { pub primary: String, pub any: Vec<String>, pub all: Vec<String>, pub none: Vec<String> }
//! pub struct TimelineParams { pub local: bool, pub remote: bool, pub only_media: bool, pub tag: Option<TagFilter>, pub page: PageParams }
//! pub struct TimelineQuerySpec { /* kind + 条件 + cursor 範囲（candidate 取得用） */ }
//! pub struct FilterContext { pub viewer: Option<Id>, pub blocked: HashSet<Id>, pub blocked_by: HashSet<Id>, pub muted: HashSet<Id>, pub following: HashSet<Id>, pub reblogs_hidden: HashSet<Id>, pub now: OffsetDateTime }
//! ```
//!
//! `TimelineQuerySpec`'s exact fields are deliberately left unspecified by
//! design.md ("kind + 条件 + cursor 範囲（candidate 取得用）"). This module
//! resolves that ambiguity with the most conservative, literal reading: the
//! [`TimelineKind`] being queried, the [`TimelineParams`] that carry the
//! per-kind conditions (`local`/`remote`/`only_media`/`tag`), and an explicit
//! `max_id`/`since_id`/`min_id` cursor range typed as `Option<Id>` — the same
//! three cursor slots api-foundation's [`PageParams`] carries as raw
//! `Option<String>` wire params, but decoded to the candidate-retrieval-ready
//! id type `CandidateRepository` (task 1.3+, out of this task's boundary)
//! will actually bind into its `statuses` query. No repository/query
//! function lives here — only the shape a future `CandidateRepository` will
//! consume.
//!
//! No `TimelineKindRules` (the per-kind condition *logic*, task 1.2), no
//! `CandidateRepository`/`TimelineFilter`/`TimelineMatcher`/`StatusHydrator`/
//! `TimelineService`/`TimelineEndpoints` (later tasks), and no HTTP surface
//! or persistence live here — those consume the types defined in this module
//! but are out of scope for task 1.1 (`Boundary: model`).

use std::collections::HashSet;

use time::OffsetDateTime;

use crate::api::pagination::PageParams;
use crate::domain::Id;

/// The four timeline kinds this spec provides (Requirements 1.1, 2.1, 3.1,
/// 4.1): home (follows + self), public (federated), local (this server
/// only), and tag (hashtag-filtered). Exactly these four — no list timeline
/// (out of scope, `experience-expansion`) — per design.md's model doc
/// ("`TimelineKind` は `Home`/`Public`/`Local`/`Tag` の 4 種").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimelineKind {
    Home,
    Public,
    Local,
    Tag,
}

/// A tag timeline's hashtag conditions (Requirement 4.1, 4.4): `primary` is
/// the hashtag the timeline is anchored on (from the request path, e.g. `GET
/// /api/v1/timelines/tag/:hashtag`); `any`/`all`/`none` are the additional
/// tag-name conditions a request may combine with it — a candidate must
/// match at least one of `any` (when non-empty), all of `all`, and none of
/// `none` (Requirement 4.4's "any を含む（any）/ すべてを含む（all）/ いずれ
/// も含まない（none）"). Matching itself (case-insensitive, normalized) is
/// `TimelineKindRules`'/`CandidateRepository`'s responsibility (task 1.2+,
/// out of this task's boundary) — this type only carries the condition, not
/// the matching logic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagFilter {
    pub primary: String,
    pub any: Vec<String>,
    pub all: Vec<String>,
    pub none: Vec<String>,
}

/// A timeline request's parameters (design.md model doc: "`TimelineParams`
/// は `local`/`remote`/`only_media`/タグ（`hashtag` + 任意 `any`/`all`/
/// `none`）/`PageParams` を保持"): `local`/`remote` narrow public/local
/// timelines to local-only or remote-only authors (Requirements 2.3, 2.4),
/// `only_media` narrows to posts carrying a media attachment (Requirements
/// 2.5, 3.4, 4.5), `tag` carries the tag timeline's hashtag conditions (only
/// meaningful for [`TimelineKind::Tag`] requests — `None` for every other
/// kind), and `page` reuses api-foundation's [`PageParams`] verbatim rather
/// than redefining a parallel pagination shape (Requirement 7.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineParams {
    pub local: bool,
    pub remote: bool,
    pub only_media: bool,
    pub tag: Option<TagFilter>,
    pub page: PageParams,
}

/// The candidate-retrieval query shape a future `CandidateRepository` (task
/// 1.3+) consumes: which [`TimelineKind`] to aggregate, the [`TimelineParams`]
/// carrying the per-kind conditions, and the cursor range to bound the
/// candidate batch by (Requirement 7.1's `max_id`/`since_id`/`min_id`,
/// decoded to the id type candidate queries actually bind — see this
/// module's own doc comment on why `TimelineQuerySpec`'s exact fields are
/// this task's own conservative reading of design.md's "kind + 条件 + cursor
/// 範囲" sketch).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineQuerySpec {
    pub kind: TimelineKind,
    pub params: TimelineParams,
    pub max_id: Option<Id>,
    pub since_id: Option<Id>,
    pub min_id: Option<Id>,
}

/// The viewer-relative relationship context a candidate is filtered against
/// (design.md model doc: "`FilterContext` は viewer + blocked/blocked_by/
/// muted/following/reblogs_hidden 集合 + now を保持"), sourced from
/// social-graph's `FilterQuery` (Requirement 6.1) rather than reimplemented
/// here. `viewer` is `None` for an unauthenticated request (Requirement
/// 9.2's "未認証アクセスを一律拒否しない"); the five relationship sets are
/// each keyed by the *other* account's [`Id`] (e.g. `blocked` is the set of
/// accounts `viewer` has blocked); `reblogs_hidden` is the set of followed
/// accounts whose boosts are hidden from `viewer` (Requirement 1.4's
/// `show_reblogs` condition); `now` is the evaluation instant, carried
/// explicitly rather than read from the system clock so filtering (e.g.
/// mute-expiry consideration, Requirement 6.4) stays deterministic and
/// testable.
#[derive(Debug, Clone, PartialEq)]
pub struct FilterContext {
    pub viewer: Option<Id>,
    pub blocked: HashSet<Id>,
    pub blocked_by: HashSet<Id>,
    pub muted: HashSet<Id>,
    pub following: HashSet<Id>,
    pub reblogs_hidden: HashSet<Id>,
    pub now: OffsetDateTime,
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    // -- TimelineKind: exactly 4 variants, usable in match/equality --------

    /// Exhaustive match with no wildcard arm: this function fails to compile
    /// the moment a fifth `TimelineKind` variant is added (or one of the
    /// four is removed/renamed), structurally proving "exactly 4 values" at
    /// the type level rather than via a runtime enumeration alone (mirrors
    /// `src/statuses/model.rs`'s identical `Visibility` precedent).
    fn kind_ordinal(kind: TimelineKind) -> u8 {
        match kind {
            TimelineKind::Home => 0,
            TimelineKind::Public => 1,
            TimelineKind::Local => 2,
            TimelineKind::Tag => 3,
        }
    }

    #[test]
    fn timeline_kind_has_exactly_four_distinct_variants() {
        let all = [
            TimelineKind::Home,
            TimelineKind::Public,
            TimelineKind::Local,
            TimelineKind::Tag,
        ];
        assert_eq!(all.len(), 4);
        let ordinals: Vec<u8> = all.iter().copied().map(kind_ordinal).collect();
        let mut sorted = ordinals.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 4, "expected 4 distinct TimelineKind variants");
        assert_eq!(ordinals, vec![0, 1, 2, 3]);
    }

    #[test]
    fn timeline_kind_supports_equality_comparison() {
        assert_eq!(TimelineKind::Home, TimelineKind::Home);
        assert_ne!(TimelineKind::Home, TimelineKind::Public);
        assert_ne!(TimelineKind::Local, TimelineKind::Tag);
    }

    // -- TagFilter: any/all/none are independently constructible -----------

    #[test]
    fn tag_filter_expresses_any_semantics_independently_of_all_and_none() {
        let filter = TagFilter {
            primary: "rust".to_string(),
            any: vec!["rustlang".to_string(), "programming".to_string()],
            all: Vec::new(),
            none: Vec::new(),
        };
        assert_eq!(filter.primary, "rust");
        assert_eq!(filter.any.len(), 2);
        assert!(filter.all.is_empty());
        assert!(filter.none.is_empty());
    }

    #[test]
    fn tag_filter_expresses_all_semantics_independently_of_any_and_none() {
        let filter = TagFilter {
            primary: "rust".to_string(),
            any: Vec::new(),
            all: vec!["async".to_string(), "tokio".to_string()],
            none: Vec::new(),
        };
        assert!(filter.any.is_empty());
        assert_eq!(filter.all, vec!["async".to_string(), "tokio".to_string()]);
        assert!(filter.none.is_empty());
    }

    #[test]
    fn tag_filter_expresses_none_semantics_independently_of_any_and_all() {
        let filter = TagFilter {
            primary: "rust".to_string(),
            any: Vec::new(),
            all: Vec::new(),
            none: vec!["spam".to_string()],
        };
        assert!(filter.any.is_empty());
        assert!(filter.all.is_empty());
        assert_eq!(filter.none, vec!["spam".to_string()]);
    }

    #[test]
    fn tag_filter_can_combine_any_all_and_none_simultaneously() {
        // Requirement 4.4 permits any/all/none conditions in combination on
        // the same request — distinct field values must not collapse into
        // one another.
        let filter = TagFilter {
            primary: "rust".to_string(),
            any: vec!["rustlang".to_string()],
            all: vec!["async".to_string()],
            none: vec!["spam".to_string()],
        };
        assert_ne!(filter.any, filter.all);
        assert_ne!(filter.all, filter.none);
        assert_ne!(filter.any, filter.none);
    }

    // -- TimelineParams: holds local/remote/only_media/tag/page ------------

    #[test]
    fn timeline_params_holds_local_remote_only_media_tag_and_page() {
        let params = TimelineParams {
            local: true,
            remote: false,
            only_media: true,
            tag: Some(TagFilter {
                primary: "rust".to_string(),
                any: Vec::new(),
                all: Vec::new(),
                none: Vec::new(),
            }),
            page: PageParams {
                max_id: Some("100".to_string()),
                since_id: None,
                min_id: None,
                limit: Some(20),
            },
        };
        assert!(params.local);
        assert!(!params.remote);
        assert!(params.only_media);
        assert_eq!(params.tag.as_ref().unwrap().primary, "rust");
        assert_eq!(params.page.max_id, Some("100".to_string()));
    }

    #[test]
    fn timeline_params_tag_is_absent_for_non_tag_timelines() {
        let params = TimelineParams {
            local: false,
            remote: false,
            only_media: false,
            tag: None,
            page: PageParams::default(),
        };
        assert!(params.tag.is_none());
    }

    // -- TimelineQuerySpec: carries a TimelineKind + cursor range -----------

    #[test]
    fn timeline_query_spec_carries_kind_params_and_cursor_range() {
        let spec = TimelineQuerySpec {
            kind: TimelineKind::Home,
            params: TimelineParams {
                local: false,
                remote: false,
                only_media: false,
                tag: None,
                page: PageParams::default(),
            },
            max_id: Some(Id::from_i64(100)),
            since_id: None,
            min_id: Some(Id::from_i64(1)),
        };
        assert_eq!(spec.kind, TimelineKind::Home);
        assert_eq!(spec.max_id, Some(Id::from_i64(100)));
        assert_eq!(spec.min_id, Some(Id::from_i64(1)));
        assert_eq!(spec.since_id, None);
    }

    #[test]
    fn timeline_query_spec_distinguishes_each_timeline_kind() {
        let base_params = TimelineParams {
            local: false,
            remote: false,
            only_media: false,
            tag: None,
            page: PageParams::default(),
        };
        let home = TimelineQuerySpec {
            kind: TimelineKind::Home,
            params: base_params.clone(),
            max_id: None,
            since_id: None,
            min_id: None,
        };
        let public = TimelineQuerySpec {
            kind: TimelineKind::Public,
            params: base_params,
            max_id: None,
            since_id: None,
            min_id: None,
        };
        assert_ne!(home, public);
        assert_ne!(home.kind, public.kind);
    }

    // -- FilterContext: viewer + 5 relation sets + now ----------------------

    #[test]
    fn filter_context_holds_viewer_and_the_five_relationship_sets() {
        let viewer = Id::from_i64(1);
        let blocked_account = Id::from_i64(2);
        let blocked_by_account = Id::from_i64(3);
        let muted_account = Id::from_i64(4);
        let following_account = Id::from_i64(5);
        let reblogs_hidden_account = Id::from_i64(6);

        let ctx = FilterContext {
            viewer: Some(viewer),
            blocked: HashSet::from([blocked_account]),
            blocked_by: HashSet::from([blocked_by_account]),
            muted: HashSet::from([muted_account]),
            following: HashSet::from([following_account]),
            reblogs_hidden: HashSet::from([reblogs_hidden_account]),
            now: datetime!(2026-07-31 00:00:00 UTC),
        };

        assert_eq!(ctx.viewer, Some(viewer));
        assert!(ctx.blocked.contains(&blocked_account));
        assert!(!ctx.blocked.contains(&blocked_by_account));
        assert!(ctx.blocked_by.contains(&blocked_by_account));
        assert!(ctx.muted.contains(&muted_account));
        assert!(ctx.following.contains(&following_account));
        assert!(ctx.reblogs_hidden.contains(&reblogs_hidden_account));
        assert_eq!(ctx.now, datetime!(2026-07-31 00:00:00 UTC));
    }

    #[test]
    fn filter_context_viewer_is_none_for_an_unauthenticated_request() {
        // Requirement 9.2: unauthenticated access is not rejected outright
        // for public/local/tag timelines — `viewer: None` is a valid,
        // constructible state, not an invariant violation.
        let ctx = FilterContext {
            viewer: None,
            blocked: HashSet::new(),
            blocked_by: HashSet::new(),
            muted: HashSet::new(),
            following: HashSet::new(),
            reblogs_hidden: HashSet::new(),
            now: datetime!(2026-07-31 00:00:00 UTC),
        };
        assert!(ctx.viewer.is_none());
        assert!(ctx.blocked.is_empty());
    }

    #[test]
    fn filter_context_relationship_sets_are_independent_of_each_other() {
        // An account can be in `following` without being in any of the
        // exclusion sets, and vice versa — the five sets must not be
        // conflated into one another.
        let followed_only = Id::from_i64(42);
        let ctx = FilterContext {
            viewer: Some(Id::from_i64(1)),
            blocked: HashSet::new(),
            blocked_by: HashSet::new(),
            muted: HashSet::new(),
            following: HashSet::from([followed_only]),
            reblogs_hidden: HashSet::new(),
            now: datetime!(2026-07-31 00:00:00 UTC),
        };
        assert!(ctx.following.contains(&followed_only));
        assert!(!ctx.blocked.contains(&followed_only));
        assert!(!ctx.blocked_by.contains(&followed_only));
        assert!(!ctx.muted.contains(&followed_only));
        assert!(!ctx.reblogs_hidden.contains(&followed_only));
    }
}
