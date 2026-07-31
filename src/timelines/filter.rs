//! `TimelineFilter` (design.md "Filter / フィルタ層" -> `#### TimelineFilter`,
//! Requirements 1.2, 1.4, 1.5, 2.6, 3.3, 4.6, 5.1, 5.2, 5.3, 5.4, 6.1, 6.2,
//! 6.3, 6.4; task 3.1, `Boundary: TimelineFilter`).
//!
//! Scope: this module owns exactly [`TimelineFilter::keep`] — applying, to a
//! single already-kind-matched candidate (`TimelineKindRules`'s job, task
//! 1.2, not re-run here), statuses-core's [`is_visible`] visibility judgment
//! and social-graph's relationship exclusion (blocked/blocked_by/muted,
//! mute-expiry already considered upstream) plus the boost-specific
//! `reblogs_hidden`/boosted-original-author rules — never reimplementing
//! either upstream judgment (this task's own instruction: "可視性・関係判定
//! は再実装せず上流へ委譲する").
//!
//! - Visibility (5.1, 5.2, 5.3, 5.4): delegates to
//!   [`crate::statuses::visibility::is_visible`] with a [`ViewerRelation`]
//!   built from [`FilterContext::following`] (already loaded once per
//!   viewer, per design.md's "関係集合の取得は閲覧者単位で1回行い候補ごと
//!   のN+1を避ける（`FilterContext` に前ロード）" — no additional query is
//!   issued here). An unauthenticated viewer (`ctx.viewer.is_none()`) is
//!   handled entirely by `is_visible` itself (`Public` visible regardless of
//!   viewer, every other visibility requires `viewer.is_some()`) — this
//!   module adds no separate "unauthenticated -> public only" branch, since
//!   duplicating that check here would itself be a small reimplementation of
//!   what `is_visible` already guarantees (5.2).
//! - Relationship exclusion (6.1, 6.2, 6.4): a candidate's author is
//!   excluded when present in [`FilterContext::blocked`],
//!   [`FilterContext::blocked_by`], or [`FilterContext::muted`] — all three
//!   sourced from social-graph's `FilterQuery` and consumed as given (6.4:
//!   `muted`'s mute-expiry consideration is social-graph's job, already
//!   applied before this module ever sees the set — this module never reads
//!   [`FilterContext::now`] itself, since re-deriving expiry here would
//!   duplicate what upstream already resolved).
//! - Boost rules (1.4, 1.5, 6.3): a boost (`status.reblog_of_id.is_some()`)
//!   is additionally excluded when its booster (`status.actor_id` — a boost
//!   row's own author field *is* the booster, per
//!   `crate::statuses::interaction_service::InteractionService::reblog`'s
//!   row construction, `actor_id: actor_id` the booster /
//!   `reblog_of_id: Some(target.id)`) is in [`FilterContext::reblogs_hidden`]
//!   (1.4's `show_reblogs`-disabled-follow exclusion), or when the boosted
//!   *original* post's author is in the same blocked/blocked_by/muted union
//!   (6.3, 1.5).
//!
//! ## Signature deviation from design.md's literal sketch: `reblogged_author`
//! design.md's Service Interface sketch is `pub fn keep(&self, status:
//! &Status, ctx: &FilterContext) -> bool` — a single candidate `Status`.
//! That sketch has no slot for "the boosted-original post's author",
//! because [`Status`] itself has none: a boost is persisted as its own
//! `statuses` row whose `actor_id` is the *booster*, not the original
//! author (see this module's doc comment above, citing
//! `InteractionService::reblog`'s row construction) — the boosted-original's
//! author simply is not reachable from a single `&Status` value. Requirement
//! 6.3 ("ブースト実行者**または**被ブースト元投稿の投稿者" — booster OR
//! boosted-original's author) is nonetheless explicit that both identities
//! must be checked. Resolving this the same way
//! `crate::timelines::candidate_repository::fetch_candidates` resolved its
//! own analogous design.md-sketch gap (its own doc comment, "adding exactly
//! the one slot [kind] structurally needs" for `following_and_self`), this
//! module adds exactly the one slot the boost case needs:
//! `reblogged_author: Option<Id>`, the boosted-original post's author id,
//! inserted before `ctx`. Every non-boost call passes `None` (ignored, since
//! [`Self::keep`] only reads it when `status.reblog_of_id.is_some()`); a
//! boost call should pass the resolved original author whenever the caller
//! has it (a future `TimelineMatcher`/`TimelineService`, tasks 3.2/4.2, out
//! of this task's boundary, which already needs the boosted-original row for
//! `StatusHydrator`'s nested-`reblog` hydration, Requirement 10.3, and can
//! reuse that same lookup here). Passing `None` for an actual boost
//! candidate simply skips the boosted-original-author half of 6.3's check
//! (the booster half, and the `reblogs_hidden` check, still apply
//! unconditionally) — this module does not fail-closed/exclude on a missing
//! `reblogged_author`, since silently excluding every boost whose original
//! author a caller has not resolved would be a correctness regression of
//! its own (Requirement 1.1's "フォロー中アカウントによるブーストを...集約
//! して返す" default-inclusion), not a safety improvement; wiring the real
//! lookup through is later tasks' responsibility, not fabricable here
//! without touching `CandidateRepository`/`StatusHydrator`/`TimelineMatcher`
//! (explicitly out of this task's boundary).
//!
//! No `CandidateRepository`/`TimelineMatcher`/`StatusHydrator`/
//! `TimelineService`/`TimelineEndpoints` (later tasks), and no wiring into
//! `crate::state`/`crate::bootstrap`/`crate::server` (task 5.2), live here.

use crate::domain::Id;
use crate::statuses::model::Status;
use crate::statuses::visibility::{ViewerRelation, is_visible};

use super::model::FilterContext;

/// `TimelineFilter` applies statuses-core's visibility judgment and
/// social-graph's relationship/boost-display rules to a single candidate —
/// see this module's doc comment for the exact rules and why
/// [`Self::keep`]'s signature adds one slot (`reblogged_author`) beyond
/// design.md's literal sketch. Holds no state of its own (every input is a
/// per-call parameter); `&self` mirrors design.md's own exact method
/// signature (`pub fn keep(&self, ...)`) rather than a bare associated
/// function, so a future caller can hold a `TimelineFilter` value (e.g.
/// behind `TimelineService`) without this module's own API needing to
/// change shape later.
#[derive(Debug, Clone, Copy, Default)]
pub struct TimelineFilter;

impl TimelineFilter {
    /// Decides whether `status` survives visibility + relationship + boost
    /// filtering for `ctx`'s viewer (design.md: "可視 && 関係除外に該当し
    /// ない && ブースト規律を満たす").
    ///
    /// `reblogged_author` is the boosted-original post's author id when
    /// `status` is a boost and the caller has resolved it (`None` for a
    /// non-boost candidate, or for a boost whose original author is not yet
    /// resolved by the caller — see this module's doc comment, "Signature
    /// deviation").
    pub fn keep(&self, status: &Status, reblogged_author: Option<Id>, ctx: &FilterContext) -> bool {
        // Visibility (5.1, 5.2, 5.3, 5.4) — delegated to statuses-core's
        // `is_visible`, never reimplemented here. `rel.is_follower` is
        // resolved from `ctx.following` (already loaded once per viewer),
        // not a fresh relationship query.
        let rel = ViewerRelation {
            is_follower: ctx.viewer.is_some() && ctx.following.contains(&status.actor_id),
        };
        if !is_visible(status, ctx.viewer, &rel) {
            return false;
        }

        // Relationship exclusion for the candidate's own author (which, for
        // a boost row, is the booster) — Requirements 6.1, 6.2, 6.4.
        if Self::is_relationship_excluded(status.actor_id, ctx) {
            return false;
        }

        // Boost-specific rules (1.4, 1.5, 6.3) — only evaluated when
        // `status` is itself a boost (`reblog_of_id.is_some()`); public/
        // local/tag candidates never reach here as boosts at all
        // (`TimelineKindRules` already excludes them structurally, task
        // 1.2), so this branch is effectively home-only in practice without
        // needing to know the timeline kind here.
        if status.reblog_of_id.is_some() {
            // 1.4: the booster's `show_reblogs`-disabled follow hides their
            // boosts.
            if ctx.reblogs_hidden.contains(&status.actor_id) {
                return false;
            }
            // 6.3, 1.5: the boosted-original post's author is also a
            // relationship-exclusion target, when known (see this module's
            // doc comment, "Signature deviation").
            if let Some(original_author) = reblogged_author
                && Self::is_relationship_excluded(original_author, ctx)
            {
                return false;
            }
        }

        true
    }

    /// Whether `account` is one of `ctx`'s three relationship-exclusion
    /// targets (blocked, blocked_by, muted — Requirements 6.1, 6.2, 6.4).
    /// `ctx.muted` is consumed as given, already mute-expiry-considered by
    /// social-graph's `FilterQuery` (6.4) — this function performs no
    /// timestamp comparison of its own.
    fn is_relationship_excluded(account: Id, ctx: &FilterContext) -> bool {
        ctx.blocked.contains(&account)
            || ctx.blocked_by.contains(&account)
            || ctx.muted.contains(&account)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use time::OffsetDateTime;
    use time::macros::datetime;

    use crate::domain::Visibility;

    use super::*;

    fn status(id: i64, actor: Id, visibility: Visibility, reblog_of: Option<Id>) -> Status {
        Status {
            id: Id::from_i64(id),
            actor_id: actor,
            uri: format!("https://example.test/statuses/{id}"),
            url: None,
            content: "hello".to_string(),
            visibility,
            sensitive: false,
            spoiler_text: String::new(),
            in_reply_to_id: None,
            in_reply_to_account_id: None,
            reblog_of_id: reblog_of,
            poll_id: None,
            language: None,
            reblogs_count: 0,
            favourites_count: 0,
            replies_count: 0,
            local: true,
            created_at: datetime!(2026-07-31 00:00:00 UTC),
            edited_at: None,
        }
    }

    fn empty_ctx(viewer: Option<Id>) -> FilterContext {
        FilterContext {
            viewer,
            blocked: HashSet::new(),
            blocked_by: HashSet::new(),
            muted: HashSet::new(),
            following: HashSet::new(),
            reblogs_hidden: HashSet::new(),
            now: OffsetDateTime::now_utc(),
        }
    }

    // -- Visibility (5.1-5.4) ------------------------------------------------

    #[test]
    fn unauthenticated_viewer_keeps_a_public_post() {
        let filter = TimelineFilter;
        let author = Id::from_i64(1);
        let post = status(10, author, Visibility::Public, None);
        let ctx = empty_ctx(None);
        assert!(filter.keep(&post, None, &ctx));
    }

    #[test]
    fn unauthenticated_viewer_excludes_an_unlisted_post() {
        let filter = TimelineFilter;
        let author = Id::from_i64(1);
        let post = status(10, author, Visibility::Unlisted, None);
        let ctx = empty_ctx(None);
        assert!(!filter.keep(&post, None, &ctx));
    }

    #[test]
    fn unauthenticated_viewer_excludes_a_private_post() {
        let filter = TimelineFilter;
        let author = Id::from_i64(1);
        let post = status(10, author, Visibility::Private, None);
        let ctx = empty_ctx(None);
        assert!(!filter.keep(&post, None, &ctx));
    }

    #[test]
    fn unauthenticated_viewer_excludes_a_direct_post_even_from_a_stranger() {
        let filter = TimelineFilter;
        let author = Id::from_i64(1);
        let post = status(10, author, Visibility::Direct, None);
        let ctx = empty_ctx(None);
        assert!(!filter.keep(&post, None, &ctx));
    }

    #[test]
    fn authenticated_non_follower_viewer_excludes_a_private_post() {
        let filter = TimelineFilter;
        let viewer = Id::from_i64(1);
        let author = Id::from_i64(2);
        let post = status(10, author, Visibility::Private, None);
        let ctx = empty_ctx(Some(viewer));
        assert!(!filter.keep(&post, None, &ctx));
    }

    #[test]
    fn authenticated_follower_viewer_keeps_a_private_post() {
        let filter = TimelineFilter;
        let viewer = Id::from_i64(1);
        let author = Id::from_i64(2);
        let post = status(10, author, Visibility::Private, None);
        let mut ctx = empty_ctx(Some(viewer));
        ctx.following.insert(author);
        assert!(filter.keep(&post, None, &ctx));
    }

    #[test]
    fn local_and_remote_authors_receive_identical_visibility_treatment() {
        // Requirement 5.3: no local-only shortcut. `TimelineFilter` never
        // reads `Status::local` at all for the visibility decision.
        let filter = TimelineFilter;
        let local_author = Id::from_i64(1);
        let remote_author = Id::from_i64(2);
        let local_post = status(10, local_author, Visibility::Public, None);
        let mut remote_post = status(11, remote_author, Visibility::Public, None);
        remote_post.local = false;
        let ctx = empty_ctx(None);
        assert!(filter.keep(&local_post, None, &ctx));
        assert!(filter.keep(&remote_post, None, &ctx));
    }

    // -- Relationship exclusion (6.1, 6.2) -----------------------------------

    #[test]
    fn excludes_a_post_from_a_blocked_author() {
        let filter = TimelineFilter;
        let viewer = Id::from_i64(1);
        let author = Id::from_i64(2);
        let post = status(10, author, Visibility::Public, None);
        let mut ctx = empty_ctx(Some(viewer));
        ctx.blocked.insert(author);
        assert!(!filter.keep(&post, None, &ctx));
    }

    #[test]
    fn excludes_a_post_from_an_author_who_blocked_the_viewer() {
        let filter = TimelineFilter;
        let viewer = Id::from_i64(1);
        let author = Id::from_i64(2);
        let post = status(10, author, Visibility::Public, None);
        let mut ctx = empty_ctx(Some(viewer));
        ctx.blocked_by.insert(author);
        assert!(!filter.keep(&post, None, &ctx));
    }

    #[test]
    fn excludes_a_post_from_a_muted_author() {
        let filter = TimelineFilter;
        let viewer = Id::from_i64(1);
        let author = Id::from_i64(2);
        let post = status(10, author, Visibility::Public, None);
        let mut ctx = empty_ctx(Some(viewer));
        ctx.muted.insert(author);
        assert!(!filter.keep(&post, None, &ctx));
    }

    #[test]
    fn keeps_a_post_from_an_unrelated_author() {
        let filter = TimelineFilter;
        let viewer = Id::from_i64(1);
        let author = Id::from_i64(2);
        let post = status(10, author, Visibility::Public, None);
        let ctx = empty_ctx(Some(viewer));
        assert!(filter.keep(&post, None, &ctx));
    }

    // -- Mute expiry delegation (6.4) ----------------------------------------

    #[test]
    fn muted_membership_is_trusted_as_already_expiry_considered() {
        // Requirement 6.4: `TimelineFilter` never re-derives mute expiry
        // from `ctx.now` — an account present in `ctx.muted` is excluded
        // regardless of `ctx.now`'s value, trusting social-graph's
        // `FilterQuery` to have already dropped expired mutes before this
        // set was built.
        let filter = TimelineFilter;
        let viewer = Id::from_i64(1);
        let author = Id::from_i64(2);
        let post = status(10, author, Visibility::Public, None);
        let mut ctx = empty_ctx(Some(viewer));
        ctx.muted.insert(author);
        ctx.now = datetime!(2099-01-01 00:00:00 UTC);
        assert!(!filter.keep(&post, None, &ctx));
    }

    // -- Boost rules: reblogs_hidden (1.4) -----------------------------------

    #[test]
    fn excludes_a_boost_from_a_reblogs_hidden_booster() {
        let filter = TimelineFilter;
        let viewer = Id::from_i64(1);
        let booster = Id::from_i64(2);
        let original_id = Id::from_i64(100);
        let boost = status(10, booster, Visibility::Public, Some(original_id));
        let mut ctx = empty_ctx(Some(viewer));
        ctx.following.insert(booster);
        ctx.reblogs_hidden.insert(booster);
        assert!(!filter.keep(&boost, None, &ctx));
    }

    #[test]
    fn keeps_a_boost_from_a_booster_not_in_reblogs_hidden() {
        let filter = TimelineFilter;
        let viewer = Id::from_i64(1);
        let booster = Id::from_i64(2);
        let original_id = Id::from_i64(100);
        let boost = status(10, booster, Visibility::Public, Some(original_id));
        let mut ctx = empty_ctx(Some(viewer));
        ctx.following.insert(booster);
        assert!(filter.keep(&boost, None, &ctx));
    }

    #[test]
    fn a_non_boost_post_from_an_author_present_in_reblogs_hidden_is_unaffected() {
        // `reblogs_hidden` only applies to boosts (1.4) — an original post
        // from the same account is not excluded by it.
        let filter = TimelineFilter;
        let viewer = Id::from_i64(1);
        let author = Id::from_i64(2);
        let post = status(10, author, Visibility::Public, None);
        let mut ctx = empty_ctx(Some(viewer));
        ctx.reblogs_hidden.insert(author);
        assert!(filter.keep(&post, None, &ctx));
    }

    // -- Boost rules: booster/original-author relationship exclusion (6.3, 1.5)

    #[test]
    fn excludes_a_boost_whose_booster_is_blocked() {
        let filter = TimelineFilter;
        let viewer = Id::from_i64(1);
        let booster = Id::from_i64(2);
        let original_id = Id::from_i64(100);
        let boost = status(10, booster, Visibility::Public, Some(original_id));
        let mut ctx = empty_ctx(Some(viewer));
        ctx.blocked.insert(booster);
        assert!(!filter.keep(&boost, Some(Id::from_i64(3)), &ctx));
    }

    #[test]
    fn excludes_a_boost_whose_original_author_is_blocked() {
        let filter = TimelineFilter;
        let viewer = Id::from_i64(1);
        let booster = Id::from_i64(2);
        let original_author = Id::from_i64(3);
        let original_id = Id::from_i64(100);
        let boost = status(10, booster, Visibility::Public, Some(original_id));
        let mut ctx = empty_ctx(Some(viewer));
        ctx.following.insert(booster);
        ctx.blocked.insert(original_author);
        assert!(!filter.keep(&boost, Some(original_author), &ctx));
    }

    #[test]
    fn excludes_a_boost_whose_original_author_blocked_the_viewer() {
        let filter = TimelineFilter;
        let viewer = Id::from_i64(1);
        let booster = Id::from_i64(2);
        let original_author = Id::from_i64(3);
        let original_id = Id::from_i64(100);
        let boost = status(10, booster, Visibility::Public, Some(original_id));
        let mut ctx = empty_ctx(Some(viewer));
        ctx.following.insert(booster);
        ctx.blocked_by.insert(original_author);
        assert!(!filter.keep(&boost, Some(original_author), &ctx));
    }

    #[test]
    fn excludes_a_boost_whose_original_author_is_muted() {
        let filter = TimelineFilter;
        let viewer = Id::from_i64(1);
        let booster = Id::from_i64(2);
        let original_author = Id::from_i64(3);
        let original_id = Id::from_i64(100);
        let boost = status(10, booster, Visibility::Public, Some(original_id));
        let mut ctx = empty_ctx(Some(viewer));
        ctx.following.insert(booster);
        ctx.muted.insert(original_author);
        assert!(!filter.keep(&boost, Some(original_author), &ctx));
    }

    #[test]
    fn keeps_a_boost_whose_booster_and_original_author_are_both_unrelated() {
        let filter = TimelineFilter;
        let viewer = Id::from_i64(1);
        let booster = Id::from_i64(2);
        let original_author = Id::from_i64(3);
        let original_id = Id::from_i64(100);
        let boost = status(10, booster, Visibility::Public, Some(original_id));
        let mut ctx = empty_ctx(Some(viewer));
        ctx.following.insert(booster);
        assert!(filter.keep(&boost, Some(original_author), &ctx));
    }

    #[test]
    fn a_boost_with_unresolved_original_author_only_applies_the_booster_and_reblogs_hidden_checks()
    {
        // `reblogged_author: None` (caller has not resolved the boosted
        // post's author yet) does not fail-closed — only the booster
        // relationship and `reblogs_hidden` checks still apply (see this
        // module's doc comment, "Signature deviation").
        let filter = TimelineFilter;
        let viewer = Id::from_i64(1);
        let booster = Id::from_i64(2);
        let original_id = Id::from_i64(100);
        let boost = status(10, booster, Visibility::Public, Some(original_id));
        let mut ctx = empty_ctx(Some(viewer));
        ctx.following.insert(booster);
        assert!(filter.keep(&boost, None, &ctx));
    }
}
