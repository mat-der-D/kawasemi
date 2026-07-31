//! Timeline kind conditions (`TimelineKindRules` component, design.md
//! "Timeline Domain / ドメイン層" -> `model / TimelineKindRules`,
//! Requirements 1.1, 1.3, 2.1, 2.2, 3.1, 3.2, 4.1, 4.2, 4.3; task 1.2,
//! `Boundary: TimelineKindRules`).
//!
//! Scope: this module defines, in one place, the *structural* per-kind
//! aggregation condition design.md calls out ("種別ごとの集約条件を単一点
//! で定義する") — i.e. given a [`TimelineKind`](super::model::TimelineKind)
//! and a candidate post, whether that candidate structurally belongs to that
//! timeline kind:
//!
//! - **Home** (1.1, 1.3): author is in the viewer's `following` set ∪ the
//!   viewer themself; excludes `direct` visibility; includes boosts
//!   (reblogs) — the `show_reblogs`-disabled exclusion (1.4) and the
//!   general `VisibilityPolicy` visibility check are `TimelineFilter`'s job
//!   (task 3.1, out of this task's boundary) and are *not* applied here.
//! - **Public** (2.1, 2.2): `public` visibility only; excludes boosts —
//!   original posts only.
//! - **Local** (3.1, 3.2): same as Public, plus the author must be a local
//!   account.
//! - **Tag** (4.1, 4.2, 4.3): same as Public, plus the candidate's
//!   (already-normalized, per statuses-core) tag set must satisfy the
//!   request's [`TagFilter`](super::model::TagFilter) `primary`/`any`/`all`/
//!   `none` condition, compared case-insensitively.
//!
//! Per design.md: "種別条件はここにのみ存在し、`TimelineMatcher` と
//! `CandidateRepository` がこれを参照する（条件の二重定義禁止）" — later
//! tasks (`CandidateRepository`, task 2.1; `TimelineMatcher`, task 3.2) are
//! expected to call into [`TimelineKindRules`] rather than re-derive these
//! conditions.
//!
//! Out of scope (belongs to later tasks, not re-implemented here):
//! blocked/blocked_by/muted exclusion, `show_reblogs`/mute-expiry
//! application, and the general `VisibilityPolicy` visible-to-follower
//! check for non-public posts (`TimelineFilter`, task 3.1) — this module
//! only expresses the structural, kind-specific condition, referencing
//! [`FilterContext::following`](super::model::FilterContext) only for the
//! home-membership ("follows ∪ self") check design.md explicitly assigns to
//! this component.
//!
//! No DB/SQL access lives here (that's `CandidateRepository`, task 2.1) —
//! this module is pure domain logic, testable without a database. Since no
//! candidate-shaped type exists yet in the `timelines` module, this file
//! defines a minimal local fixture, [`TimelineCandidate`], carrying exactly
//! the fields a kind condition needs (author, local-ness, visibility,
//! boost-ness, normalized tags) — not a full `Status`/database-shape type,
//! which is `CandidateRepository`/`StatusHydrator`'s concern (task 2.1+).

use std::collections::HashSet;

use crate::domain::{Id, Visibility};

use super::model::{FilterContext, TagFilter, TimelineKind, TimelineParams};

/// A minimal, DB-free shape of "a candidate post" sufficient to evaluate a
/// [`TimelineKind`]'s structural condition against: who authored it, whether
/// the author is a local account, its [`Visibility`], whether it is itself a
/// boost (reblog) rather than an original post, and its normalized hashtag
/// names. This is intentionally not the eventual `Status`/database-shape
/// candidate type `CandidateRepository` (task 2.1) will produce — only the
/// slice of information a kind condition needs to decide membership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineCandidate {
    /// The post's author (or, for a boost, the account performing the
    /// boost — the account a `home`-kind "follows ∪ self" check is about).
    pub author: Id,
    /// Whether `author` is a local account (Requirement 3.1's local-only
    /// condition).
    pub local: bool,
    /// The candidate's own visibility (Requirement 1.3's `direct` exclusion,
    /// Requirements 2.1/3.1/4.1's `public`-only condition).
    pub visibility: Visibility,
    /// Whether this candidate is a boost (reblog) of another post, rather
    /// than an original post (Requirement 1.1's boost-inclusion for home;
    /// Requirements 2.2/3.2/4.3's boost-exclusion for public/local/tag).
    pub is_boost: bool,
    /// The candidate's hashtag names, already normalized (case-folded) by
    /// statuses-core at post-creation time per this spec's boundary (tag
    /// *extraction*/normalization *infrastructure* is explicitly out of
    /// scope here — this field simply carries the result). Matching in
    /// [`tag_matches`] additionally case-folds both sides defensively so the
    /// predicate shape holds even if a caller passes un-normalized names.
    pub tags: HashSet<String>,
}

/// Case-folds a tag name for case-insensitive comparison (Requirement 4.2).
/// This is deliberately minimal (ASCII/Unicode lowercasing + trim) — full
/// hashtag normalization (e.g. NFKC folding) is statuses-core's
/// infrastructure, not this component's concern.
fn fold_tag(tag: &str) -> String {
    tag.trim().to_lowercase()
}

/// Decides whether a candidate's normalized tag set satisfies a tag
/// timeline's [`TagFilter`] condition (Requirement 4.1's primary hashtag,
/// Requirement 4.4's `any`/`all`/`none` combination): the primary tag must
/// be present; if `any` is non-empty at least one of its tags must be
/// present; every tag in `all` must be present; no tag in `none` may be
/// present. Comparison is case-insensitive on both sides.
pub fn tag_matches(filter: &TagFilter, tags: &HashSet<String>) -> bool {
    let normalized: HashSet<String> = tags.iter().map(|t| fold_tag(t)).collect();

    let primary = fold_tag(&filter.primary);
    if !normalized.contains(&primary) {
        return false;
    }

    if !filter.any.is_empty() && !filter.any.iter().any(|t| normalized.contains(&fold_tag(t))) {
        return false;
    }

    if !filter.all.iter().all(|t| normalized.contains(&fold_tag(t))) {
        return false;
    }

    if filter
        .none
        .iter()
        .any(|t| normalized.contains(&fold_tag(t)))
    {
        return false;
    }

    true
}

/// `TimelineKindRules` defines, in one place, the structural per-kind
/// aggregation condition (design.md's "TimelineKindRules は種別ごとの条件
/// を一箇所で定義する") — see this module's doc comment for the exact
/// per-kind condition and what is deliberately excluded (relationship-based
/// filtering belongs to `TimelineFilter`, task 3.1).
pub struct TimelineKindRules;

impl TimelineKindRules {
    /// The single evaluation point: does `candidate` structurally belong to
    /// `kind` given `params` (for the tag condition) and `ctx` (for the home
    /// follows-or-self condition)? Dispatches to the per-kind condition
    /// below — no condition is duplicated at call sites (design.md's "条件
    /// の二重定義禁止").
    pub fn matches(
        kind: TimelineKind,
        params: &TimelineParams,
        candidate: &TimelineCandidate,
        ctx: &FilterContext,
    ) -> bool {
        match kind {
            TimelineKind::Home => Self::matches_home(candidate, ctx),
            TimelineKind::Public => Self::matches_public(candidate),
            TimelineKind::Local => Self::matches_local(candidate),
            TimelineKind::Tag => Self::matches_tag(params, candidate),
        }
    }

    /// Home (Requirements 1.1, 1.3): author is in `ctx.following` ∪ the
    /// viewer themself; `direct` visibility is excluded; boosts are
    /// structurally included (no `is_boost` check) — the `show_reblogs`
    /// exclusion (1.4) is `TimelineFilter`'s job, not this condition's.
    pub fn matches_home(candidate: &TimelineCandidate, ctx: &FilterContext) -> bool {
        let is_self = ctx.viewer == Some(candidate.author);
        let is_followed_author = ctx.following.contains(&candidate.author);
        (is_self || is_followed_author) && candidate.visibility != Visibility::Direct
    }

    /// Public (Requirements 2.1, 2.2): `public` visibility only; boosts are
    /// excluded — original posts only.
    pub fn matches_public(candidate: &TimelineCandidate) -> bool {
        candidate.visibility == Visibility::Public && !candidate.is_boost
    }

    /// Local (Requirements 3.1, 3.2): same as [`Self::matches_public`], plus
    /// the author must be a local account.
    pub fn matches_local(candidate: &TimelineCandidate) -> bool {
        Self::matches_public(candidate) && candidate.local
    }

    /// Tag (Requirements 4.1, 4.2, 4.3): same as [`Self::matches_public`],
    /// plus the candidate's tags must satisfy `params.tag` via
    /// [`tag_matches`]. A tag-kind request with no [`TagFilter`] attached is
    /// treated conservatively as matching nothing (there is no hashtag to
    /// anchor the timeline on).
    pub fn matches_tag(params: &TimelineParams, candidate: &TimelineCandidate) -> bool {
        let Some(filter) = params.tag.as_ref() else {
            return false;
        };
        Self::matches_public(candidate) && tag_matches(filter, &candidate.tags)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::pagination::PageParams;

    fn candidate(
        author: Id,
        local: bool,
        visibility: Visibility,
        is_boost: bool,
        tags: &[&str],
    ) -> TimelineCandidate {
        TimelineCandidate {
            author,
            local,
            visibility,
            is_boost,
            tags: tags.iter().map(|t| t.to_string()).collect(),
        }
    }

    fn empty_params() -> TimelineParams {
        TimelineParams {
            local: false,
            remote: false,
            only_media: false,
            tag: None,
            page: PageParams::default(),
        }
    }

    fn ctx_with_following(viewer: Id, following: &[Id]) -> FilterContext {
        FilterContext {
            viewer: Some(viewer),
            blocked: HashSet::new(),
            blocked_by: HashSet::new(),
            muted: HashSet::new(),
            following: following.iter().copied().collect(),
            reblogs_hidden: HashSet::new(),
            now: time::macros::datetime!(2026-07-31 00:00:00 UTC),
        }
    }

    // -- Home: follows ∪ self, direct excluded, boosts included ------------

    #[test]
    fn home_includes_post_from_a_followed_author() {
        let viewer = Id::from_i64(1);
        let author = Id::from_i64(2);
        let ctx = ctx_with_following(viewer, &[author]);
        let post = candidate(author, true, Visibility::Public, false, &[]);
        assert!(TimelineKindRules::matches_home(&post, &ctx));
    }

    #[test]
    fn home_includes_post_from_the_viewer_themself() {
        let viewer = Id::from_i64(1);
        let ctx = ctx_with_following(viewer, &[]);
        let own_post = candidate(viewer, true, Visibility::Private, false, &[]);
        assert!(TimelineKindRules::matches_home(&own_post, &ctx));
    }

    #[test]
    fn home_excludes_post_from_an_unfollowed_non_self_author() {
        let viewer = Id::from_i64(1);
        let stranger = Id::from_i64(99);
        let ctx = ctx_with_following(viewer, &[]);
        let post = candidate(stranger, true, Visibility::Public, false, &[]);
        assert!(!TimelineKindRules::matches_home(&post, &ctx));
    }

    #[test]
    fn home_excludes_direct_visibility_even_from_a_followed_author() {
        let viewer = Id::from_i64(1);
        let author = Id::from_i64(2);
        let ctx = ctx_with_following(viewer, &[author]);
        let direct_post = candidate(author, true, Visibility::Direct, false, &[]);
        assert!(!TimelineKindRules::matches_home(&direct_post, &ctx));
    }

    #[test]
    fn home_excludes_direct_visibility_from_the_viewer_themself() {
        let viewer = Id::from_i64(1);
        let ctx = ctx_with_following(viewer, &[]);
        let own_direct_post = candidate(viewer, true, Visibility::Direct, false, &[]);
        assert!(!TimelineKindRules::matches_home(&own_direct_post, &ctx));
    }

    #[test]
    fn home_includes_boosts_from_a_followed_author() {
        let viewer = Id::from_i64(1);
        let booster = Id::from_i64(2);
        let ctx = ctx_with_following(viewer, &[booster]);
        let boost = candidate(booster, true, Visibility::Public, true, &[]);
        assert!(TimelineKindRules::matches_home(&boost, &ctx));
    }

    // -- Public/Local/Tag: always public-only + non-reblog ------------------

    #[test]
    fn public_includes_a_public_original_post() {
        let post = candidate(Id::from_i64(1), true, Visibility::Public, false, &[]);
        assert!(TimelineKindRules::matches_public(&post));
    }

    #[test]
    fn public_excludes_a_boost_even_if_public() {
        let boost = candidate(Id::from_i64(1), true, Visibility::Public, true, &[]);
        assert!(!TimelineKindRules::matches_public(&boost));
    }

    #[test]
    fn public_excludes_non_public_visibility() {
        for v in [
            Visibility::Unlisted,
            Visibility::Private,
            Visibility::Direct,
        ] {
            let post = candidate(Id::from_i64(1), true, v, false, &[]);
            assert!(
                !TimelineKindRules::matches_public(&post),
                "expected {v:?} to be excluded from public"
            );
        }
    }

    #[test]
    fn local_includes_a_public_original_post_from_a_local_author() {
        let post = candidate(Id::from_i64(1), true, Visibility::Public, false, &[]);
        assert!(TimelineKindRules::matches_local(&post));
    }

    #[test]
    fn local_excludes_a_public_original_post_from_a_remote_author() {
        let post = candidate(Id::from_i64(1), false, Visibility::Public, false, &[]);
        assert!(!TimelineKindRules::matches_local(&post));
    }

    #[test]
    fn local_excludes_a_boost_from_a_local_author() {
        let boost = candidate(Id::from_i64(1), true, Visibility::Public, true, &[]);
        assert!(!TimelineKindRules::matches_local(&boost));
    }

    #[test]
    fn local_excludes_non_public_visibility_from_a_local_author() {
        let post = candidate(Id::from_i64(1), true, Visibility::Private, false, &[]);
        assert!(!TimelineKindRules::matches_local(&post));
    }

    // -- Tag: public-only + non-reblog + any/all/none matching --------------

    fn tag_filter(primary: &str, any: &[&str], all: &[&str], none: &[&str]) -> TagFilter {
        TagFilter {
            primary: primary.to_string(),
            any: any.iter().map(|t| t.to_string()).collect(),
            all: all.iter().map(|t| t.to_string()).collect(),
            none: none.iter().map(|t| t.to_string()).collect(),
        }
    }

    #[test]
    fn tag_includes_a_public_original_post_carrying_the_primary_tag() {
        let mut params = empty_params();
        params.tag = Some(tag_filter("rust", &[], &[], &[]));
        let post = candidate(Id::from_i64(1), true, Visibility::Public, false, &["rust"]);
        assert!(TimelineKindRules::matches_tag(&params, &post));
    }

    #[test]
    fn tag_matching_is_case_insensitive_and_normalized() {
        let filter = tag_filter("RuST", &[], &[], &[]);
        let tags: HashSet<String> = ["  rust  "].iter().map(|t| t.to_string()).collect();
        assert!(tag_matches(&filter, &tags));
    }

    #[test]
    fn tag_excludes_a_post_missing_the_primary_tag() {
        let mut params = empty_params();
        params.tag = Some(tag_filter("rust", &[], &[], &[]));
        let post = candidate(Id::from_i64(1), true, Visibility::Public, false, &["ruby"]);
        assert!(!TimelineKindRules::matches_tag(&params, &post));
    }

    #[test]
    fn tag_excludes_a_boost_even_if_it_carries_the_primary_tag() {
        let mut params = empty_params();
        params.tag = Some(tag_filter("rust", &[], &[], &[]));
        let boost = candidate(Id::from_i64(1), true, Visibility::Public, true, &["rust"]);
        assert!(!TimelineKindRules::matches_tag(&params, &boost));
    }

    #[test]
    fn tag_excludes_non_public_visibility_even_if_it_carries_the_primary_tag() {
        let mut params = empty_params();
        params.tag = Some(tag_filter("rust", &[], &[], &[]));
        let post = candidate(Id::from_i64(1), true, Visibility::Private, false, &["rust"]);
        assert!(!TimelineKindRules::matches_tag(&params, &post));
    }

    #[test]
    fn tag_any_matches_when_at_least_one_additional_tag_is_present() {
        let filter = tag_filter("rust", &["rustlang", "programming"], &[], &[]);
        let tags: HashSet<String> = ["rust", "programming"]
            .iter()
            .map(|t| t.to_string())
            .collect();
        assert!(tag_matches(&filter, &tags));
    }

    #[test]
    fn tag_any_excludes_when_none_of_the_additional_tags_are_present() {
        let filter = tag_filter("rust", &["rustlang", "programming"], &[], &[]);
        let tags: HashSet<String> = ["rust"].iter().map(|t| t.to_string()).collect();
        assert!(!tag_matches(&filter, &tags));
    }

    #[test]
    fn tag_all_requires_every_additional_tag_to_be_present() {
        let filter = tag_filter("rust", &[], &["async", "tokio"], &[]);
        let complete: HashSet<String> = ["rust", "async", "tokio"]
            .iter()
            .map(|t| t.to_string())
            .collect();
        assert!(tag_matches(&filter, &complete));

        let partial: HashSet<String> = ["rust", "async"].iter().map(|t| t.to_string()).collect();
        assert!(!tag_matches(&filter, &partial));
    }

    #[test]
    fn tag_none_excludes_when_any_excluded_tag_is_present() {
        let filter = tag_filter("rust", &[], &[], &["spam"]);
        let tags: HashSet<String> = ["rust", "spam"].iter().map(|t| t.to_string()).collect();
        assert!(!tag_matches(&filter, &tags));
    }

    #[test]
    fn tag_none_allows_when_no_excluded_tag_is_present() {
        let filter = tag_filter("rust", &[], &[], &["spam"]);
        let tags: HashSet<String> = ["rust"].iter().map(|t| t.to_string()).collect();
        assert!(tag_matches(&filter, &tags));
    }

    #[test]
    fn tag_combines_any_all_and_none_conditions_together() {
        let filter = tag_filter("rust", &["rustlang"], &["async"], &["spam"]);
        let matching: HashSet<String> = ["rust", "rustlang", "async"]
            .iter()
            .map(|t| t.to_string())
            .collect();
        assert!(tag_matches(&filter, &matching));

        let missing_all: HashSet<String> =
            ["rust", "rustlang"].iter().map(|t| t.to_string()).collect();
        assert!(!tag_matches(&filter, &missing_all));

        let has_none: HashSet<String> = ["rust", "rustlang", "async", "spam"]
            .iter()
            .map(|t| t.to_string())
            .collect();
        assert!(!tag_matches(&filter, &has_none));
    }

    #[test]
    fn tag_kind_with_no_tag_filter_matches_nothing() {
        let params = empty_params();
        let post = candidate(Id::from_i64(1), true, Visibility::Public, false, &["rust"]);
        assert!(!TimelineKindRules::matches_tag(&params, &post));
    }

    // -- matches(): single dispatch point, all four kinds --------------------

    #[test]
    fn matches_dispatches_to_the_correct_condition_for_each_kind() {
        let viewer = Id::from_i64(1);
        let followed = Id::from_i64(2);
        let ctx = ctx_with_following(viewer, &[followed]);

        let mut tag_params = empty_params();
        tag_params.tag = Some(tag_filter("rust", &[], &[], &[]));

        // Home: a private post from a followed author is included (no
        // public-only restriction at the kind-rule level for home).
        let home_post = candidate(followed, true, Visibility::Private, false, &[]);
        assert!(TimelineKindRules::matches(
            TimelineKind::Home,
            &empty_params(),
            &home_post,
            &ctx
        ));

        // Public: the same private post is excluded (public-only).
        assert!(!TimelineKindRules::matches(
            TimelineKind::Public,
            &empty_params(),
            &home_post,
            &ctx
        ));

        // Local: a public post from a remote author is excluded.
        let remote_public = candidate(followed, false, Visibility::Public, false, &[]);
        assert!(!TimelineKindRules::matches(
            TimelineKind::Local,
            &empty_params(),
            &remote_public,
            &ctx
        ));

        // Tag: a public post carrying the requested tag is included.
        let tagged = candidate(followed, true, Visibility::Public, false, &["rust"]);
        assert!(TimelineKindRules::matches(
            TimelineKind::Tag,
            &tag_params,
            &tagged,
            &ctx
        ));
    }

    // -- Invariant: public/local/tag are always public-only + non-reblog,
    //    home is the only kind that is neither. -----------------------------

    #[test]
    fn only_home_admits_a_non_public_visibility_post() {
        let viewer = Id::from_i64(1);
        let ctx = ctx_with_following(viewer, &[]);
        let own_private_post = candidate(viewer, true, Visibility::Private, false, &[]);

        assert!(TimelineKindRules::matches_home(&own_private_post, &ctx));
        assert!(!TimelineKindRules::matches_public(&own_private_post));
        assert!(!TimelineKindRules::matches_local(&own_private_post));

        let mut tag_params = empty_params();
        tag_params.tag = Some(tag_filter("rust", &[], &[], &[]));
        assert!(!TimelineKindRules::matches_tag(
            &tag_params,
            &own_private_post
        ));
    }

    #[test]
    fn only_home_admits_a_boost() {
        let viewer = Id::from_i64(1);
        let ctx = ctx_with_following(viewer, &[]);
        let own_boost = candidate(viewer, true, Visibility::Public, true, &[]);

        assert!(TimelineKindRules::matches_home(&own_boost, &ctx));
        assert!(!TimelineKindRules::matches_public(&own_boost));
        assert!(!TimelineKindRules::matches_local(&own_boost));

        let mut tag_params = empty_params();
        tag_params.tag = Some(tag_filter("rust", &[], &[], &[]));
        assert!(!TimelineKindRules::matches_tag(&tag_params, &own_boost));
    }
}
