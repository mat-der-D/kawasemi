//! `TimelineMatcher` (design.md "Domain (single point) / 単一生成点" ->
//! `#### TimelineMatcher`, Requirements 8.1, 8.2, 8.4 — also 1.1, 2.1, 3.1,
//! 4.1 per this component's own Req Coverage row in design.md's Components
//! and Interfaces table ("TimelineMatcher | ... | 8,1,2,3,4"), since
//! `matches`/`candidate_spec` *compose* the per-kind conditions those
//! requirements describe via [`TimelineKindRules`], never re-deriving them;
//! task 3.2, `Boundary: TimelineMatcher`).
//!
//! Scope: this module owns exactly [`TimelineMatcher::candidate_spec`] and
//! [`TimelineMatcher::matches`] — the single generation point design.md's
//! "タイムライン種別条件と単一生成点" flow section names:
//! - [`TimelineMatcher::candidate_spec`] converts a [`TimelineKind`] +
//!   [`TimelineParams`] into the [`TimelineQuerySpec`]
//!   [`crate::timelines::candidate_repository::fetch_candidates`] consumes
//!   (the REST candidate-query side, 8.2).
//! - [`TimelineMatcher::matches`] decides whether a single already-known
//!   [`Status`] belongs to a given [`TimelineKind`] for a given viewer (the
//!   Streaming single-post-membership side, 8.1), by composing
//!   [`TimelineKindRules::matches`] (the kind condition) with
//!   [`TimelineFilter::keep`] (the visibility + relationship filter) —
//!   never re-deriving either judgment (design.md's "条件の二重定義禁止",
//!   8.2; this task's own instruction: "REST 取得とロジックを二重定義せず
//!   同一の `TimelineKindRules`/`TimelineFilter` の上に実装する").
//!
//! `candidate_spec`'s output does not itself re-encode `TimelineKindRules`'s
//! per-kind boolean condition — `TimelineQuerySpec` (task 1.1's model.rs,
//! frozen, out of this task's boundary) carries only `kind` + `params` +
//! cursor range, no arbitrary predicate slot. The actual per-kind SQL
//! translation of `TimelineKindRules`'s condition already lives in
//! `CandidateRepository::fetch_candidates` (task 2.1, already committed),
//! whose own doc comment states it "mirrors, never re-derives,
//! `TimelineKindRules`". `candidate_spec`'s job, given that division of
//! labor, is exactly to carry `kind`/`params` through unchanged (plus decode
//! the cursor strings — see "Cursor decoding" below) so that downstream
//! dispatch can happen — not to duplicate a second boolean-predicate
//! encoding of the same condition.
//!
//! Delivery/broadcast itself is out of scope (Requirement 8.4) — this module
//! produces only a query spec and a boolean judgment, nothing that sends
//! data anywhere.
//!
//! ## Signature deviation #1: `candidate_spec`'s `ctx` parameter is accepted but not read
//! design.md's sketch is `pub fn candidate_spec(&self, kind: TimelineKind,
//! params: &TimelineParams, ctx: &FilterContext) -> TimelineQuerySpec;` —
//! this module keeps that exact three-parameter shape (`&self` too,
//! mirroring [`TimelineFilter::keep`]'s literal-signature-preservation
//! precedent, `src/timelines/filter.rs`). [`TimelineQuerySpec`] (task 1.1's
//! `model.rs`, frozen — out of this task's boundary to modify) carries only
//! `kind`/`params`/three cursor `Option<Id>` fields; it has no field for a
//! relationship set. So even though `ctx.following` is reachable inside this
//! function's body ([`FilterContext::following`], task 1.1), there is
//! nowhere in `TimelineQuerySpec`'s shape to route it into the return value
//! — this is not a gap this task can close by adding logic, because the
//! target type itself has no slot. `CandidateRepository::fetch_candidates`
//! (task 2.1, already committed) resolved the analogous "home needs a
//! following set but `TimelineQuerySpec` has no field for it" problem by
//! taking `following_and_self: &[Id]` as its **own** fourth parameter,
//! entirely separate from `spec` — i.e. the home following-set is threaded
//! to `fetch_candidates` directly by *its* caller (`TimelineService`, task
//! 4.2), never through `TimelineQuerySpec`/`candidate_spec` at all. Given
//! that precedent, `candidate_spec`'s only usable inputs for constructing
//! its return value are `kind` and `params`; `ctx` is retained in the
//! signature (bound as `_ctx`) purely for parity with design.md's own sketch
//! (a future caller passing all four values it already has, uniformly with
//! `matches`), not because this function's body currently has a use for it.
//! Confirming this explicitly rather than silently dropping the parameter:
//! there is no unstated gap here — `following_and_self` remains
//! `TimelineService`'s (task 4.2's) responsibility to source from its own
//! loaded `FilterContext`/`FilterQuery` and pass straight to
//! `fetch_candidates`, bypassing `candidate_spec`/`TimelineQuerySpec`
//! entirely, exactly as task 2.1's own Implementation Notes entry already
//! anticipates.
//!
//! ## Signature deviation #2: `matches` gains `tags` and `reblogged_author`
//! design.md's sketch is `pub fn matches(&self, status: &Status, kind:
//! TimelineKind, params: &TimelineParams, ctx: &FilterContext) -> bool;` — a
//! single candidate `Status` plus kind/params/ctx. Composing this from
//! [`TimelineKindRules::matches`] + [`TimelineFilter::keep`] (as 8.2
//! requires) exposes two structural gaps a bare `&Status` cannot close:
//!
//! 1. **Tag condition.** [`TimelineKindRules::matches`] takes a
//!    [`TimelineCandidate`] (task 1.2's own candidate-fixture type), which
//!    carries `tags: HashSet<String>` — but [`Status`] (`src/statuses/
//!    model.rs`) has **no** tags field at all (confirmed by reading its full
//!    field list). For [`TimelineKind::Tag`], evaluating the kind condition
//!    against an arbitrary single `Status` (the Streaming reuse case 8.1
//!    exists for) is therefore impossible without the caller separately
//!    supplying that status's tag set. Unlike `CandidateRepository::
//!    fetch_candidates`'s Tag-kind SQL path (task 2.1), which encodes the
//!    tag condition directly as `WHERE EXISTS (...)` — so a REST-fetched
//!    row already satisfies it by construction and never needs its tags
//!    handed back — `matches` is evaluated against an arbitrary status
//!    (e.g. one just created via Streaming) that has not been pre-filtered
//!    by any such query, so the caller must resolve and pass the tag set
//!    itself.
//! 2. **Boosted-original-author exclusion (Requirement 6.3).** Identical to
//!    task 3.1's own already-documented `TimelineFilter::keep` deviation
//!    (`src/timelines/filter.rs`'s "Signature deviation" doc comment): a
//!    boost's original author is not reachable from the boost's own
//!    `Status` row (`reblog_of_id` points at the row's id, not its author).
//!    `matches` delegates the filter half of its judgment straight to
//!    `TimelineFilter::keep`, which already requires this value as its own
//!    parameter — `matches` cannot supply a hardcoded `None` here without
//!    reintroducing exactly the false-negative risk task 3.1's own doc
//!    comment already reasoned through (silently dropping the
//!    boosted-original-author half of 6.3's exclusion is the *conservative*
//!    fallback, not a correctness improvement, and definitely not something
//!    to fabricate silently rather than expose to the caller).
//!
//! Mirroring both prior tasks' identical precedent (add exactly the slot(s)
//! structurally needed, inserted before the trailing `ctx` parameter — task
//! 2.1's `following_and_self` before `batch_limit`, task 3.1's
//! `reblogged_author` before `ctx`), this module extends `matches` with two
//! parameters inserted before `ctx`: `tags: &HashSet<String>` (the
//! candidate's own normalized tag set — read only when `kind ==
//! TimelineKind::Tag`; pass `&HashSet::new()` for every other kind, which
//! resolves to [`TimelineKindRules::matches_tag`]'s own "no filter/no tags
//! -> matches nothing" conservative behavior if `kind` happens to be `Tag`
//! anyway) and `reblogged_author: Option<Id>` (forwarded verbatim to
//! [`TimelineFilter::keep`] — see that function's own doc comment for `None`
//! handling; irrelevant for a non-boost `status`).
//!
//! ## Cursor decoding (`candidate_spec`): best-effort, not the authoritative validation path
//! `TimelineParams::page` (task 1.1) carries raw wire-format `Option<String>`
//! cursor values (api-foundation's `PageParams`), while `TimelineQuerySpec`'s
//! `max_id`/`since_id`/`min_id` (task 1.1's own `model.rs` doc comment:
//! "decoded to the candidate-retrieval-ready id type `CandidateRepository`
//! ... will actually bind") are already-decoded `Option<Id>`. Something has
//! to perform that decode, and per task 1.1's own forward reference ("task
//! 1.3+, out of this task's boundary"), this task — the first one whose
//! signature both receives a raw `TimelineParams` and returns a
//! `TimelineQuerySpec` — is where it happens. design.md's `candidate_spec`
//! sketch is infallible (`-> TimelineQuerySpec`, not `-> Result<
//! TimelineQuerySpec, AppError>`), so a malformed cursor string cannot be
//! rejected here the way api-foundation's own `PageParams::parse::<C:
//! Cursor>()` (`src/api/pagination.rs`) rejects one (a 422-ish `AppError`).
//! This module does not reuse that fallible toolkit for that reason;
//! instead each cursor slot is decoded with a simple, infallible
//! `str::parse::<i64>().ok()` — a malformed string decodes as `None` (no
//! bound on that side), the same fail-toward-the-less-harmful-side posture
//! task 3.1's `reblogged_author: None` handling already established
//! (silently widening the candidate batch is safe; `CandidateRepository`/
//! `TimelineFilter`/`TimelineService` all still narrow further downstream),
//! rather than panicking or fabricating a wrong id. Authoritative cursor
//! validation (rejecting a malformed cursor with a 422 Mastodon-compatible
//! error body, per api-foundation's own `PageParams::parse`) is
//! `TimelineEndpoints`'s job (task 5.1, out of this task's boundary).
//!
//! No `StatusHydrator`/`TimelineService`/`TimelineEndpoints` (later tasks),
//! and no wiring into `crate::state`/`crate::bootstrap`/`crate::server`
//! (task 5.2) live here — [`TimelineMatcher`]'s methods are made `pub` and
//! self-contained (no dependency on endpoint-layer state) precisely so a
//! later task can expose them through `AppState` (task 5.2) without this
//! module changing shape, but performing that wiring is not this task's job.

use std::collections::HashSet;

use crate::domain::Id;
use crate::statuses::model::Status;

use super::filter::TimelineFilter;
use super::kind_rules::{TimelineCandidate, TimelineKindRules};
use super::model::{FilterContext, TimelineKind, TimelineParams, TimelineQuerySpec};

/// Decodes one of `TimelineParams::page`'s raw `Option<String>` cursor slots
/// into the `Option<Id>` shape [`TimelineQuerySpec`] carries — see this
/// module's own doc comment, "Cursor decoding", for why this is a lenient,
/// infallible best-effort parse rather than the authoritative
/// validate-and-reject path.
fn decode_cursor(raw: &Option<String>) -> Option<Id> {
    raw.as_deref()
        .and_then(|value| value.trim().parse::<i64>().ok())
        .map(Id::from_i64)
}

/// `TimelineMatcher` is the single generation point design.md's "単一生成
/// 点" flow names — see this module's doc comment for the exact composition
/// [`Self::candidate_spec`]/[`Self::matches`] each perform and why their
/// signatures extend design.md's literal sketch. Holds no state of its own
/// (every input is a per-call parameter, mirroring [`TimelineFilter`]'s own
/// convention), so a future caller can hold a `TimelineMatcher` value (e.g.
/// behind `TimelineService`, or `AppState` per task 5.2's Matcher-seam
/// exposure) without this module's own API needing to change shape later.
#[derive(Debug, Clone, Copy, Default)]
pub struct TimelineMatcher;

impl TimelineMatcher {
    /// Converts `kind` + `params` into the [`TimelineQuerySpec`]
    /// [`crate::timelines::candidate_repository::fetch_candidates`] consumes
    /// (design.md: "種別条件（`TimelineKindRules`）をカーソル付きクエリ仕様
    /// へ変換する", 8.2). `params`'s cursor strings
    /// ([`TimelineParams::page`]) are decoded via [`decode_cursor`] — see
    /// this module's own doc comment, "Cursor decoding", for the exact
    /// (lenient, infallible) decode rule. `ctx` is accepted for parity with
    /// design.md's own sketch but not read by this function's body — see
    /// this module's own doc comment, "Signature deviation #1", for why
    /// `TimelineQuerySpec` structurally has nowhere to route it.
    pub fn candidate_spec(
        &self,
        kind: TimelineKind,
        params: &TimelineParams,
        _ctx: &FilterContext,
    ) -> TimelineQuerySpec {
        TimelineQuerySpec {
            kind,
            params: params.clone(),
            max_id: decode_cursor(&params.page.max_id),
            since_id: decode_cursor(&params.page.since_id),
            min_id: decode_cursor(&params.page.min_id),
        }
    }

    /// Decides whether `status` belongs to `kind` for the viewer/relationship
    /// context `ctx`, combining the kind condition
    /// ([`TimelineKindRules::matches`]) with the visibility + relationship
    /// filter ([`TimelineFilter::keep`]) — the Streaming single-post-
    /// membership judgment (design.md: "単一投稿の所属を判定する `matches`
    /// （種別条件 + 可視性 + 関係フィルタ）", 8.1). `tags` and
    /// `reblogged_author` extend design.md's literal sketch — see this
    /// module's own doc comment, "Signature deviation #2", for exactly why
    /// each is needed and how a caller without a resolved value should treat
    /// it (empty tag set / `None`).
    ///
    /// Postcondition (Requirement 8.2): returns `true` exactly when `status`
    /// would (a) satisfy `kind`'s structural condition — the same condition
    /// `candidate_spec`'s resulting query, executed by
    /// `CandidateRepository::fetch_candidates`, structurally narrows
    /// candidates to — and (b) survive `TimelineFilter::keep` for `ctx` —
    /// this module's own unit tests demonstrate this equivalence directly by
    /// composing the same two calls independently and asserting agreement
    /// with this method's own result.
    pub fn matches(
        &self,
        status: &Status,
        kind: TimelineKind,
        params: &TimelineParams,
        tags: &HashSet<String>,
        reblogged_author: Option<Id>,
        ctx: &FilterContext,
    ) -> bool {
        let candidate = TimelineCandidate {
            author: status.actor_id,
            local: status.local,
            visibility: status.visibility,
            is_boost: status.reblog_of_id.is_some(),
            tags: tags.clone(),
        };

        if !TimelineKindRules::matches(kind, params, &candidate, ctx) {
            return false;
        }

        let filter = TimelineFilter;
        filter.keep(status, reblogged_author, ctx)
    }
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use crate::api::pagination::PageParams;
    use crate::domain::Visibility;

    use super::super::model::TagFilter;
    use super::*;

    fn status(
        id: i64,
        actor: Id,
        visibility: Visibility,
        reblog_of: Option<Id>,
        local: bool,
    ) -> Status {
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
            local,
            created_at: datetime!(2026-07-31 00:00:00 UTC),
            edited_at: None,
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

    fn ctx_with_following(viewer: Option<Id>, following: &[Id]) -> FilterContext {
        FilterContext {
            viewer,
            blocked: HashSet::new(),
            blocked_by: HashSet::new(),
            muted: HashSet::new(),
            following: following.iter().copied().collect(),
            reblogs_hidden: HashSet::new(),
            now: datetime!(2026-07-31 00:00:00 UTC),
        }
    }

    /// Independently reconstructs "would this status have come out of
    /// `candidate_spec`'s query, then survived `TimelineFilter::keep`" —
    /// i.e. `TimelineKindRules::matches` (the structural condition
    /// `CandidateRepository::fetch_candidates`'s SQL mirrors) AND
    /// `TimelineFilter::keep` (applied identically to the REST path) — the
    /// exact equivalence Requirement 8.2 demands `TimelineMatcher::matches`
    /// prove.
    fn expected_membership(
        kind: TimelineKind,
        params: &TimelineParams,
        status: &Status,
        tags: &HashSet<String>,
        reblogged_author: Option<Id>,
        ctx: &FilterContext,
    ) -> bool {
        let candidate = TimelineCandidate {
            author: status.actor_id,
            local: status.local,
            visibility: status.visibility,
            is_boost: status.reblog_of_id.is_some(),
            tags: tags.clone(),
        };
        let kind_ok = TimelineKindRules::matches(kind, params, &candidate, ctx);
        let filter = TimelineFilter;
        let filter_ok = filter.keep(status, reblogged_author, ctx);
        kind_ok && filter_ok
    }

    // -- candidate_spec: carries kind/params through, decodes cursors ------

    #[test]
    fn candidate_spec_carries_kind_and_params_through_unchanged() {
        let matcher = TimelineMatcher;
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
            page: PageParams::default(),
        };
        let ctx = ctx_with_following(Some(Id::from_i64(1)), &[]);

        let spec = matcher.candidate_spec(TimelineKind::Tag, &params, &ctx);

        assert_eq!(spec.kind, TimelineKind::Tag);
        assert_eq!(spec.params, params);
    }

    #[test]
    fn candidate_spec_decodes_valid_cursor_strings_to_ids() {
        let matcher = TimelineMatcher;
        let params = TimelineParams {
            local: false,
            remote: false,
            only_media: false,
            tag: None,
            page: PageParams {
                max_id: Some("100".to_string()),
                since_id: Some("5".to_string()),
                min_id: None,
                limit: None,
            },
        };
        let ctx = ctx_with_following(None, &[]);

        let spec = matcher.candidate_spec(TimelineKind::Public, &params, &ctx);

        assert_eq!(spec.max_id, Some(Id::from_i64(100)));
        assert_eq!(spec.since_id, Some(Id::from_i64(5)));
        assert_eq!(spec.min_id, None);
    }

    #[test]
    fn candidate_spec_treats_a_malformed_cursor_string_as_absent() {
        let matcher = TimelineMatcher;
        let params = TimelineParams {
            local: false,
            remote: false,
            only_media: false,
            tag: None,
            page: PageParams {
                max_id: Some("not-a-number".to_string()),
                since_id: None,
                min_id: None,
                limit: None,
            },
        };
        let ctx = ctx_with_following(None, &[]);

        let spec = matcher.candidate_spec(TimelineKind::Local, &params, &ctx);

        assert_eq!(spec.max_id, None);
    }

    // -- matches: composes TimelineKindRules + TimelineFilter --------------

    #[test]
    fn matches_includes_a_followed_authors_public_home_post() {
        let matcher = TimelineMatcher;
        let viewer = Id::from_i64(1);
        let author = Id::from_i64(2);
        let ctx = ctx_with_following(Some(viewer), &[author]);
        let post = status(10, author, Visibility::Public, None, true);
        let params = empty_params();
        let tags = HashSet::new();

        let result = matcher.matches(&post, TimelineKind::Home, &params, &tags, None, &ctx);

        assert!(result);
        assert_eq!(
            result,
            expected_membership(TimelineKind::Home, &params, &post, &tags, None, &ctx)
        );
    }

    #[test]
    fn matches_excludes_when_kind_condition_fails_even_if_filter_would_pass() {
        // A public boost is structurally excluded from the Public kind
        // (TimelineKindRules::matches_public), even though nothing about it
        // would fail TimelineFilter::keep on its own (no blocked/muted
        // relation, public visibility, not reblogs-hidden). This proves the
        // kind gate is applied, not bypassed.
        let matcher = TimelineMatcher;
        let booster = Id::from_i64(2);
        let ctx = ctx_with_following(None, &[]);
        let boost = status(
            10,
            booster,
            Visibility::Public,
            Some(Id::from_i64(100)),
            true,
        );
        let params = empty_params();
        let tags = HashSet::new();

        let result = matcher.matches(&boost, TimelineKind::Public, &params, &tags, None, &ctx);

        assert!(!result);
        assert_eq!(
            result,
            expected_membership(TimelineKind::Public, &params, &boost, &tags, None, &ctx)
        );
    }

    #[test]
    fn matches_excludes_when_filter_condition_fails_even_if_kind_condition_passes() {
        // A public original post from a blocked author structurally
        // satisfies the Public kind condition, but TimelineFilter::keep
        // excludes it via relationship exclusion. This proves the filter
        // gate is applied, not bypassed.
        let matcher = TimelineMatcher;
        let viewer = Id::from_i64(1);
        let author = Id::from_i64(2);
        let mut ctx = ctx_with_following(Some(viewer), &[]);
        ctx.blocked.insert(author);
        let post = status(10, author, Visibility::Public, None, true);
        let params = empty_params();
        let tags = HashSet::new();

        let result = matcher.matches(&post, TimelineKind::Public, &params, &tags, None, &ctx);

        assert!(!result);
        assert_eq!(
            result,
            expected_membership(TimelineKind::Public, &params, &post, &tags, None, &ctx)
        );
    }

    #[test]
    fn matches_includes_when_both_kind_and_filter_conditions_pass() {
        let matcher = TimelineMatcher;
        let ctx = ctx_with_following(None, &[]);
        let post = status(10, Id::from_i64(2), Visibility::Public, None, false);
        let params = empty_params();
        let tags = HashSet::new();

        let result = matcher.matches(&post, TimelineKind::Local, &params, &tags, None, &ctx);

        // Not local -> excluded from the Local kind, agreeing with
        // TimelineKindRules::matches_local's own local-author requirement.
        assert!(!result);
        assert_eq!(
            result,
            expected_membership(TimelineKind::Local, &params, &post, &tags, None, &ctx)
        );

        let local_post = status(11, Id::from_i64(2), Visibility::Public, None, true);
        let local_result =
            matcher.matches(&local_post, TimelineKind::Local, &params, &tags, None, &ctx);
        assert!(local_result);
        assert_eq!(
            local_result,
            expected_membership(TimelineKind::Local, &params, &local_post, &tags, None, &ctx)
        );
    }

    #[test]
    fn matches_home_excludes_a_reblogs_hidden_boost_via_the_filter_half() {
        // TimelineKindRules::matches_home structurally includes boosts (no
        // is_boost check); TimelineFilter::keep excludes a reblogs_hidden
        // booster's boost. matches() must reflect the filter's exclusion.
        let matcher = TimelineMatcher;
        let viewer = Id::from_i64(1);
        let booster = Id::from_i64(2);
        let mut ctx = ctx_with_following(Some(viewer), &[booster]);
        ctx.reblogs_hidden.insert(booster);
        let boost = status(
            10,
            booster,
            Visibility::Public,
            Some(Id::from_i64(100)),
            true,
        );
        let params = empty_params();
        let tags = HashSet::new();

        let result = matcher.matches(&boost, TimelineKind::Home, &params, &tags, None, &ctx);

        assert!(!result);
        assert_eq!(
            result,
            expected_membership(TimelineKind::Home, &params, &boost, &tags, None, &ctx)
        );
    }

    #[test]
    fn matches_home_excludes_a_boost_whose_reblogged_author_is_blocked() {
        // Requirement 6.3: booster OR boosted-original-author blocked
        // excludes the boost. `matches` must forward `reblogged_author`
        // through to `TimelineFilter::keep` for this to take effect.
        let matcher = TimelineMatcher;
        let viewer = Id::from_i64(1);
        let booster = Id::from_i64(2);
        let original_author = Id::from_i64(3);
        let mut ctx = ctx_with_following(Some(viewer), &[booster]);
        ctx.blocked.insert(original_author);
        let boost = status(
            10,
            booster,
            Visibility::Public,
            Some(Id::from_i64(100)),
            true,
        );
        let params = empty_params();
        let tags = HashSet::new();

        let result = matcher.matches(
            &boost,
            TimelineKind::Home,
            &params,
            &tags,
            Some(original_author),
            &ctx,
        );

        assert!(!result);
        assert_eq!(
            result,
            expected_membership(
                TimelineKind::Home,
                &params,
                &boost,
                &tags,
                Some(original_author),
                &ctx
            )
        );
    }

    #[test]
    fn matches_home_boost_with_unresolved_reblogged_author_only_applies_booster_side_checks() {
        // Mirrors TimelineFilter::keep's own `reblogged_author: None`
        // fail-safe test: an unresolved original author does not fail
        // closed — only the booster-relationship/reblogs_hidden checks
        // still apply.
        let matcher = TimelineMatcher;
        let viewer = Id::from_i64(1);
        let booster = Id::from_i64(2);
        let ctx = ctx_with_following(Some(viewer), &[booster]);
        let boost = status(
            10,
            booster,
            Visibility::Public,
            Some(Id::from_i64(100)),
            true,
        );
        let params = empty_params();
        let tags = HashSet::new();

        let result = matcher.matches(&boost, TimelineKind::Home, &params, &tags, None, &ctx);

        assert!(result);
        assert_eq!(
            result,
            expected_membership(TimelineKind::Home, &params, &boost, &tags, None, &ctx)
        );
    }

    #[test]
    fn matches_tag_kind_uses_the_supplied_tags_parameter() {
        // Status carries no tags field of its own — matches() can only
        // evaluate the Tag kind condition because the caller supplies the
        // candidate's tag set explicitly.
        let matcher = TimelineMatcher;
        let ctx = ctx_with_following(None, &[]);
        let mut params = empty_params();
        params.tag = Some(TagFilter {
            primary: "rust".to_string(),
            any: Vec::new(),
            all: Vec::new(),
            none: Vec::new(),
        });
        let post = status(10, Id::from_i64(2), Visibility::Public, None, true);
        let matching_tags: HashSet<String> = ["rust".to_string()].into_iter().collect();
        let non_matching_tags: HashSet<String> = ["ruby".to_string()].into_iter().collect();

        let matched = matcher.matches(
            &post,
            TimelineKind::Tag,
            &params,
            &matching_tags,
            None,
            &ctx,
        );
        let unmatched = matcher.matches(
            &post,
            TimelineKind::Tag,
            &params,
            &non_matching_tags,
            None,
            &ctx,
        );

        assert!(matched);
        assert!(!unmatched);
        assert_eq!(
            matched,
            expected_membership(
                TimelineKind::Tag,
                &params,
                &post,
                &matching_tags,
                None,
                &ctx
            )
        );
        assert_eq!(
            unmatched,
            expected_membership(
                TimelineKind::Tag,
                &params,
                &post,
                &non_matching_tags,
                None,
                &ctx
            )
        );
    }

    #[test]
    fn matches_unauthenticated_viewer_only_includes_public_visibility() {
        let matcher = TimelineMatcher;
        let ctx = ctx_with_following(None, &[]);
        let params = empty_params();
        let tags = HashSet::new();
        let public_post = status(10, Id::from_i64(2), Visibility::Public, None, true);
        let unlisted_post = status(11, Id::from_i64(2), Visibility::Unlisted, None, true);

        let public_result = matcher.matches(
            &public_post,
            TimelineKind::Public,
            &params,
            &tags,
            None,
            &ctx,
        );
        let unlisted_result = matcher.matches(
            &unlisted_post,
            TimelineKind::Public,
            &params,
            &tags,
            None,
            &ctx,
        );

        assert!(public_result);
        assert!(!unlisted_result);
        assert_eq!(
            public_result,
            expected_membership(
                TimelineKind::Public,
                &params,
                &public_post,
                &tags,
                None,
                &ctx
            )
        );
        assert_eq!(
            unlisted_result,
            expected_membership(
                TimelineKind::Public,
                &params,
                &unlisted_post,
                &tags,
                None,
                &ctx
            )
        );
    }

    // -- Comprehensive equivalence matrix (Requirement 8.2) -----------------

    #[test]
    fn matches_agrees_with_the_kind_rules_and_filter_composition_across_a_scenario_matrix() {
        let viewer = Id::from_i64(1);
        let followed = Id::from_i64(2);
        let blocked_author = Id::from_i64(3);
        let stranger = Id::from_i64(4);

        let mut ctx = ctx_with_following(Some(viewer), &[followed]);
        ctx.blocked.insert(blocked_author);

        let mut tag_params = empty_params();
        tag_params.tag = Some(TagFilter {
            primary: "rust".to_string(),
            any: Vec::new(),
            all: Vec::new(),
            none: Vec::new(),
        });
        let rust_tags: HashSet<String> = ["rust".to_string()].into_iter().collect();
        let no_tags: HashSet<String> = HashSet::new();

        // A local alias only for this test's own scenario table — keeps
        // clippy's `type_complexity` lint quiet without hiding the shape
        // from readers (kind, params, status, tags, reblogged_author).
        type Scenario<'a> = (
            TimelineKind,
            &'a TimelineParams,
            Status,
            &'a HashSet<String>,
            Option<Id>,
        );

        let scenarios: Vec<Scenario> = vec![
            (
                TimelineKind::Home,
                &tag_params, // params unused by non-tag kinds; reused for brevity
                status(1, followed, Visibility::Public, None, true),
                &no_tags,
                None,
            ),
            (
                TimelineKind::Home,
                &tag_params,
                status(2, viewer, Visibility::Direct, None, true),
                &no_tags,
                None,
            ),
            (
                TimelineKind::Home,
                &tag_params,
                status(3, blocked_author, Visibility::Public, None, true),
                &no_tags,
                None,
            ),
            (
                TimelineKind::Public,
                &tag_params,
                status(4, stranger, Visibility::Public, None, false),
                &no_tags,
                None,
            ),
            (
                TimelineKind::Public,
                &tag_params,
                status(
                    5,
                    stranger,
                    Visibility::Public,
                    Some(Id::from_i64(999)),
                    false,
                ),
                &no_tags,
                None,
            ),
            (
                TimelineKind::Local,
                &tag_params,
                status(6, stranger, Visibility::Public, None, true),
                &no_tags,
                None,
            ),
            (
                TimelineKind::Local,
                &tag_params,
                status(7, stranger, Visibility::Public, None, false),
                &no_tags,
                None,
            ),
            (
                TimelineKind::Tag,
                &tag_params,
                status(8, stranger, Visibility::Public, None, true),
                &rust_tags,
                None,
            ),
            (
                TimelineKind::Tag,
                &tag_params,
                status(9, stranger, Visibility::Public, None, true),
                &no_tags,
                None,
            ),
            (
                TimelineKind::Home,
                &tag_params,
                status(
                    10,
                    followed,
                    Visibility::Public,
                    Some(Id::from_i64(998)),
                    true,
                ),
                &no_tags,
                Some(blocked_author),
            ),
        ];

        let matcher = TimelineMatcher;
        for (kind, params, post, tags, reblogged_author) in scenarios {
            let actual = matcher.matches(&post, kind, params, tags, reblogged_author, &ctx);
            let expected = expected_membership(kind, params, &post, tags, reblogged_author, &ctx);
            assert_eq!(
                actual, expected,
                "matches() disagreed with candidate-query+filter composition for status id {:?}, kind {:?}",
                post.id, kind
            );
        }
    }
}
