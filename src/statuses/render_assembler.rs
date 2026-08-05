//! The one place a [`Status`] row becomes Status JSON.
//!
//! Five modules render statuses — the status endpoints themselves, an
//! account's own post list, notifications' embedded posts, timelines, and
//! search results — and each had grown its own private copy of the same
//! assembly glue: fetch the author's Account JSON, fetch and render the
//! attachments, build the tag URLs, resolve the custom emoji, look up the
//! viewer's interaction state, render the poll, then hand the pieces to
//! [`status_to_json`]. Roughly seven hundred lines of it, five times over,
//! with each copy's doc comment noting that it mirrored the others; the
//! fifth even carried a standing note that the time had come to extract a
//! shared helper.
//!
//! The cost was never the line count. It was that the copies had drifted,
//! and nothing in the code said which drifts were decisions and which were
//! oversights. Two turned out to be oversights and were fixed on the way in
//! (custom emoji went unresolved on three of the five paths, so the same
//! post rendered as an image here and as raw `:shortcode:` text there); one
//! is a real difference — only timelines has mute state in hand — and is
//! now an explicit input rather than a hard-coded `false` in four places.
//! With one implementation, a difference has to be passed in, which means
//! it has to be named.
//!
//! ## What is deliberately *not* here
//! **Resolving a boost's target, and deciding whether the viewer may see
//! it.** Each of the five does this differently and for good reason: the
//! status endpoints go through `StatusService::show`, timelines re-check
//! against the [`FilterContext`](crate::timelines::model::FilterContext)
//! they already hold, search asks its own visibility helper. Pulling that
//! in would trade five explicit copies for one internal switch on "which
//! caller am I serving" — the same duplication, hidden. This module takes
//! boost targets already resolved.
//!
//! **Choosing how a poll is fetched.** Same reasoning, but the divergence
//! is sharper: the status endpoints resolve polls through `PollService`
//! (which applies its own visibility check), two callers require the row to
//! exist and raise their own not-found error when it does not, and two
//! degrade to a poll-less status instead. Those are three genuinely
//! different behaviors, so the choice is a [`PollResolver`] the caller
//! supplies.
//!
//! ## One round of lookups per call, not per status
//! Every material a status needs beyond its own row is resolved once for
//! the whole call, before any of them is rendered: the ids of every status
//! in the batch — **the boost targets included, since each is a row of its
//! own with its own author, attachments, tags, emoji, poll and interaction
//! state** — go into one set, and that set is what the repositories are
//! asked about. The query count is therefore a function of what kinds of
//! material exist, not of how many statuses were passed in (Requirement
//! 5.1), and [`StatusRenderAssembler::assemble_one`] is literally
//! [`StatusRenderAssembler::assemble_many`] with one element rather than a
//! second implementation that could drift from it.
//!
//! Two consequences worth stating, because both are easy to undo by
//! accident:
//!
//! - **Emoji resolution is one query, sliced back per status.**
//!   `emoji_repository::resolve_emojis` orders by `shortcode`, so each
//!   status's own emoji list is sorted — *not* in the order its content
//!   mentions them. Selecting a status's subset out of the batched result
//!   while keeping that result's relative order therefore reproduces the
//!   per-status query exactly, because a filtered subsequence of a sorted
//!   sequence is still sorted. See [`ResolvedEmojis::select`].
//! - **Author Account JSON is memoized for the duration of one call and no
//!   longer.** A list of twenty posts by one author resolves that author
//!   once (Requirements 5.2, 5.3); a *second* call resolves them again.
//!   Caching across calls would trade query count for a class of bug this
//!   module does not currently have — serving an account whose display
//!   name, avatar or counters changed since some earlier request.

#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::hash::Hash;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;
use sqlx::PgPool;
use time::OffsetDateTime;

use crate::accounts::account_service::AccountService;
use crate::accounts::emoji_repository;
use crate::accounts::model::CustomEmojiView;
use crate::api::pagination::ForwardedOrigin;
use crate::domain::Id;
use crate::error::AppError;
use crate::federation::signatures::ReqwestFederationHttpClient;
use crate::media::local_fs::LocalFsStore;
use crate::media::media_repository;
use crate::media::model::Media;
use crate::media::serializer::to_media_attachment;
use crate::statuses::interaction_repository;
use crate::statuses::model::{Poll, Status, Tag};
use crate::statuses::poll_repository::PollTally;
use crate::statuses::serializer::{
    SerializeContext, StatusInteractionState, StatusRenderInput, TagJson, poll_to_json,
    status_to_json,
};
use crate::statuses::status_repository;
use crate::statuses::status_service::extract_content_tokens;
use crate::statuses::tag_repository;

/// The future [`PollResolver::resolve_many`] returns.
///
/// Spelled out as an alias because the trait is deliberately `dyn`-safe —
/// see [`PollResolver`] for why a boxed future beats a generic parameter
/// here — and the boxed form is otherwise unreadable at five impl sites.
pub(crate) type PollResolution<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<(Id, Poll, PollTally)>, AppError>> + Send + 'a>>;

/// How a caller's polls are fetched, and what it means when the row is not
/// there.
///
/// The three implementations in this crate differ in exactly that second
/// half: one resolves through `PollService` and lets its visibility check
/// reject, two treat a dangling `poll_id` as a not-found error of their own
/// making, and two treat it as a status that simply renders without a poll.
/// Encoding that as a supplied port rather than a flag keeps each caller's
/// own error type — and its own decision — where the caller can see it.
pub(crate) trait PollResolver: Send + Sync {
    /// Resolves `poll_ids`, which contains no duplicates.
    ///
    /// A key absent from the returned map means "this poll does not exist
    /// and that is acceptable"; an implementation for which it is *not*
    /// acceptable returns `Err` instead of omitting the entry.
    fn resolve_many<'a>(&'a self, poll_ids: &'a [Id], viewer: Option<Id>) -> PollResolution<'a>;
}

/// Everything about one assembly call that is not the statuses themselves.
pub(crate) struct RenderContext<'a> {
    /// The actor whose interaction state (`favourited`/`bookmarked`/
    /// `pinned`/`reblogged`) is being rendered, or `None` for an
    /// unauthenticated read — in which case all four are `false`.
    pub viewer: Option<Id>,
    /// Injected rather than read from a clock here, so a caller that
    /// already fixed a timestamp for the surrounding request renders every
    /// status in that request against the same instant.
    pub now: OffsetDateTime,
    pub origin: &'a ForwardedOrigin,
    /// Accounts the viewer has muted, keyed by the *other* account's id.
    ///
    /// `None` means the caller has no mute context to offer and every
    /// status renders `muted: false` — which is what four of the five
    /// callers do. Making it an explicit `None` rather than a hard-coded
    /// `false` inside this module is the point: the difference between the
    /// callers is now stated at the call site instead of being
    /// reconstructable only by diffing five copies.
    pub muted: Option<&'a HashSet<Id>>,
    pub polls: &'a dyn PollResolver,
}

impl RenderContext<'_> {
    /// Whether `author` is muted for this render — `false` whenever the
    /// caller supplied no mute context.
    ///
    /// Applied per status, so a boost and the post it boosts are each
    /// judged by their own author rather than the outer booster's.
    fn is_muted(&self, author: Id) -> bool {
        self.muted.is_some_and(|muted| muted.contains(&author))
    }
}

/// The four viewer-scoped id sets one assembly call resolves, one query
/// each.
///
/// Only ever built when [`RenderContext::viewer`] is `Some`: an
/// unauthenticated read renders all four flags `false` by definition, so
/// issuing the queries at all would be four round trips spent confirming a
/// constant.
struct ViewerInteractions {
    favourited: HashSet<Id>,
    reblogged: HashSet<Id>,
    bookmarked: HashSet<Id>,
    pinned: HashSet<Id>,
}

/// Every custom emoji any status in the batch could need, resolved in one
/// query and handed back per status by [`Self::select`].
struct ResolvedEmojis(Vec<CustomEmojiView>);

impl ResolvedEmojis {
    /// The rows matching `shortcodes`, in the order the batched resolution
    /// returned them.
    ///
    /// This is what a per-status `resolve_emojis(&shortcodes)` would have
    /// returned, and for the same two reasons the batched call is legitimate
    /// at all: the row *set* is identical (both are "every row whose
    /// shortcode is in this list", and the batched list is a superset that
    /// this filter narrows back down), and the row *order* is identical
    /// because `ORDER BY shortcode` makes the batched result sorted and a
    /// filtered subsequence of a sorted sequence is still sorted.
    ///
    /// The one ordering the query does not pin down is between two rows
    /// carrying the *same* shortcode under different `domain`s, which
    /// `resolve_emojis` deliberately does not filter on — `ORDER BY
    /// shortcode` leaves their relative order to the planner in the
    /// unbatched form just as much as in this one. Keeping the batched
    /// result's own order here at least makes that order consistent for
    /// every status in one response, which resolving per status did not
    /// guarantee either.
    fn select(&self, shortcodes: &[String]) -> Vec<CustomEmojiView> {
        if shortcodes.is_empty() {
            return Vec::new();
        }
        let wanted: HashSet<&str> = shortcodes.iter().map(String::as_str).collect();
        self.0
            .iter()
            .filter(|emoji| wanted.contains(emoji.shortcode.as_str()))
            .cloned()
            .collect()
    }
}

/// Everything one [`StatusRenderAssembler::assemble_many`] call resolved up
/// front, keyed for the render pass that follows.
///
/// A miss carries the meaning the corresponding batch function documents,
/// which is the same degradation the per-status lookups had: a status absent
/// from `media_ids`/`tags` simply has none, a media id absent from `media`
/// has been reaped and is silently dropped from the attachment list, and a
/// poll id absent from `polls` is one the caller's [`PollResolver`] chose to
/// omit rather than raise on.
struct BatchedMaterials {
    media_ids: HashMap<Id, Vec<Id>>,
    media: HashMap<Id, Media>,
    tags: HashMap<Id, Vec<Tag>>,
    emojis: ResolvedEmojis,
    /// Already-rendered Poll JSON, keyed by poll id — polls are rendered
    /// during resolution rather than in the render pass because that is
    /// where their emoji are, and re-selecting them per status would mean
    /// scanning the same option titles twice.
    polls: HashMap<Id, Value>,
    accounts: HashMap<Id, Value>,
    interactions: Option<ViewerInteractions>,
}

/// `values` in first-appearance order, with repeats dropped — the shape
/// every batch function wants (they all take a slice and none of them
/// benefits from being asked about the same id twice).
fn unique<T: Eq + Hash + Clone>(values: impl IntoIterator<Item = T>) -> Vec<T> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .filter(|v| seen.insert(v.clone()))
        .collect()
}

/// The shortcodes a poll's own emoji resolution scans for.
///
/// Each option's title carries its own shortcodes, distinct from the owning
/// status's content. Joined with a space so a scan across the boundary
/// between two titles never merges them into one token.
fn poll_emoji_shortcodes(tally: &PollTally) -> Vec<String> {
    let combined_titles = tally
        .options
        .iter()
        .map(|option| option.title.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    extract_content_tokens(&combined_titles).emoji_shortcodes
}

/// Renders a poll whose emoji have already been resolved — the half
/// [`StatusRenderAssembler::render_poll`] and the batched path share, split
/// out so the batch can resolve every poll's emoji in the same query as the
/// statuses' own.
fn poll_json(
    poll: &Poll,
    tally: &PollTally,
    emojis: &[CustomEmojiView],
    viewer: Option<Id>,
    now: OffsetDateTime,
) -> Value {
    poll_to_json(poll, tally, emojis, &SerializeContext { viewer, now })
}

/// Assembles Status JSON for every module that renders statuses.
pub(crate) struct StatusRenderAssembler {
    pool: PgPool,
    accounts: Arc<AccountService<LocalFsStore, ReqwestFederationHttpClient>>,
    media_store: LocalFsStore,
}

impl StatusRenderAssembler {
    /// Takes already-constructed collaborators, following this crate's
    /// "bundle, don't build" convention for business-layer constructors.
    pub(crate) fn new(
        pool: PgPool,
        accounts: Arc<AccountService<LocalFsStore, ReqwestFederationHttpClient>>,
        media_store: LocalFsStore,
    ) -> Self {
        Self {
            pool,
            accounts,
            media_store,
        }
    }

    /// Renders `statuses` in order.
    ///
    /// `reblog_targets` runs parallel to `statuses`: entry `i` is the post
    /// that `statuses[i]` boosts, already fetched and already checked for
    /// visibility by the caller, or `None` when `statuses[i]` is not a
    /// boost or its target is gone or hidden. Nesting stops there — a boost
    /// target renders with `reblog: None` regardless of its own
    /// `reblog_of_id`.
    ///
    /// Every material both lists need is resolved before any status is
    /// rendered, in a number of queries that does not grow with either
    /// list's length — see this module's doc comment ("One round of lookups
    /// per call, not per status").
    ///
    /// # Panics
    /// If `reblog_targets` is not the same length as `statuses`. The two
    /// are positionally paired, so a length mismatch is a caller bug that
    /// would otherwise silently attach one status's boost to another.
    pub(crate) async fn assemble_many(
        &self,
        statuses: &[Status],
        reblog_targets: &[Option<Status>],
        ctx: &RenderContext<'_>,
    ) -> Result<Vec<Value>, AppError> {
        assert_eq!(
            statuses.len(),
            reblog_targets.len(),
            "reblog_targets is positionally paired with statuses"
        );

        // Boost targets are rows in their own right, so they belong in the
        // same id set as the statuses that carry them (Requirement 5.6).
        // Interleaved target-then-status rather than appended, matching the
        // order the render loop below visits them in: nothing observable
        // depends on it, but it keeps the order a `PollResolver` sees its
        // ids in — and therefore which dangling poll a strict implementation
        // reports on — the one the unbatched path produced.
        let all: Vec<&Status> = statuses
            .iter()
            .zip(reblog_targets)
            .flat_map(|(status, target)| target.iter().chain(std::iter::once(status)))
            .collect();
        let materials = self.resolve_materials(&all, ctx).await?;

        let mut out = Vec::with_capacity(statuses.len());
        for (status, reblog_target) in statuses.iter().zip(reblog_targets) {
            let mut input = self.render_input(status, ctx, &materials);
            input.reblog = reblog_target
                .as_ref()
                .map(|target| Box::new(self.render_input(target, ctx, &materials)));
            out.push(status_to_json(&input));
        }
        Ok(out)
    }

    /// Renders a single status — the same path [`Self::assemble_many`]
    /// takes, with a one-element batch, so the two can never drift.
    ///
    /// The boost target is cloned to build that batch. `assemble_many` pairs
    /// its two lists positionally and so needs an owned `Option<Status>`
    /// slot; widening it to borrow instead would push the difference onto
    /// all five call sites to save one clone per rendered status, which is
    /// the wrong trade for a module whose entire purpose is that the callers
    /// look the same.
    pub(crate) async fn assemble_one(
        &self,
        status: &Status,
        reblog_target: Option<&Status>,
        ctx: &RenderContext<'_>,
    ) -> Result<Value, AppError> {
        let rendered = self
            .assemble_many(std::slice::from_ref(status), &[reblog_target.cloned()], ctx)
            .await?;
        Ok(rendered
            .into_iter()
            .next()
            .expect("a one-element batch renders exactly one status"))
    }

    /// Resolves every material `statuses` collectively needs, one round of
    /// queries for the whole set.
    ///
    /// Ordering within this function is not free: a poll's option titles
    /// carry `:shortcode:` tokens of their own, and they are only readable
    /// once the polls have resolved, so the single emoji query has to come
    /// after the poll resolution even though every other lookup could be
    /// issued at any point.
    async fn resolve_materials(
        &self,
        statuses: &[&Status],
        ctx: &RenderContext<'_>,
    ) -> Result<BatchedMaterials, AppError> {
        let status_ids = unique(statuses.iter().map(|status| status.id));

        let media_ids = status_repository::media_ids_for_statuses(&self.pool, &status_ids).await?;
        let attached = unique(media_ids.values().flatten().copied());
        let media = media_repository::find_by_ids(&self.pool, &attached).await?;
        let tags = tag_repository::tags_for_statuses(&self.pool, &status_ids).await?;

        let interactions = match ctx.viewer {
            Some(viewer) => Some(self.viewer_interactions(viewer, &status_ids).await?),
            None => None,
        };

        let poll_ids = unique(statuses.iter().filter_map(|status| status.poll_id));
        let resolved_polls = self.resolve_polls(&poll_ids, ctx).await?;

        let shortcodes = unique(
            statuses
                .iter()
                .flat_map(|status| extract_content_tokens(&status.content).emoji_shortcodes)
                .chain(
                    resolved_polls
                        .iter()
                        .flat_map(|(_, _, tally)| poll_emoji_shortcodes(tally)),
                ),
        );
        let emojis = ResolvedEmojis(self.resolve_shortcodes(&shortcodes).await?);

        let mut polls: HashMap<Id, Value> = HashMap::new();
        for (poll_id, poll, tally) in &resolved_polls {
            // First entry wins, mirroring the unbatched path's "take the
            // first resolution and ignore the rest" for a resolver that
            // returns more than one entry for an id.
            polls.entry(*poll_id).or_insert_with(|| {
                let emojis = emojis.select(&poll_emoji_shortcodes(tally));
                poll_json(poll, tally, &emojis, ctx.viewer, ctx.now)
            });
        }

        // One `show_account` per distinct author, and none of it kept past
        // this call — see this module's doc comment for why the cache stops
        // at the call boundary (Requirements 5.2, 5.3).
        let mut accounts: HashMap<Id, Value> = HashMap::new();
        for author in unique(statuses.iter().map(|status| status.actor_id)) {
            let account = self
                .accounts
                .show_account(&author.as_i64().to_string(), None, ctx.origin)
                .await?;
            accounts.insert(author, account);
        }

        Ok(BatchedMaterials {
            media_ids,
            media,
            tags,
            emojis,
            polls,
            accounts,
            interactions,
        })
    }

    /// The four viewer-scoped lookups, one query each, issued only for an
    /// authenticated read.
    async fn viewer_interactions(
        &self,
        viewer: Id,
        status_ids: &[Id],
    ) -> Result<ViewerInteractions, AppError> {
        Ok(ViewerInteractions {
            favourited: interaction_repository::favourited_status_ids(
                &self.pool, viewer, status_ids,
            )
            .await?,
            reblogged: interaction_repository::reblogged_status_ids(&self.pool, viewer, status_ids)
                .await?,
            bookmarked: interaction_repository::bookmarked_status_ids(
                &self.pool, viewer, status_ids,
            )
            .await?,
            pinned: interaction_repository::pinned_status_ids(&self.pool, viewer, status_ids)
                .await?,
        })
    }

    /// Hands the whole batch's poll ids to the caller's [`PollResolver`] in
    /// one call, skipping it entirely when there are none.
    ///
    /// The skip is what keeps a poll-less list from consulting the port at
    /// all, exactly as the unbatched path returned early on a `None`
    /// `poll_id`. The port's own contract — no duplicate ids, a missing key
    /// meaning "absent and that is acceptable", an `Err` from an
    /// implementation for which it is not — is unchanged; what a strict
    /// implementation *does* change is when its error surfaces, from
    /// midway through rendering to before any status renders. Both produce
    /// the same `Err` to the same caller with nothing written, so no caller
    /// can observe the difference.
    async fn resolve_polls(
        &self,
        poll_ids: &[Id],
        ctx: &RenderContext<'_>,
    ) -> Result<Vec<(Id, Poll, PollTally)>, AppError> {
        if poll_ids.is_empty() {
            return Ok(Vec::new());
        }
        ctx.polls.resolve_many(poll_ids, ctx.viewer).await
    }

    /// Resolves `shortcodes` against the custom-emoji directory. An
    /// unregistered shortcode is simply absent from the result, never an
    /// error; an empty list resolves without a query.
    async fn resolve_shortcodes(
        &self,
        shortcodes: &[String],
    ) -> Result<Vec<CustomEmojiView>, AppError> {
        if shortcodes.is_empty() {
            return Ok(Vec::new());
        }
        emoji_repository::resolve_emojis(&self.pool, shortcodes).await
    }

    /// Assembles everything [`status_to_json`] needs beyond the row itself
    /// out of the already-resolved `materials`, leaving `reblog` unset for
    /// the caller to fill.
    ///
    /// Synchronous by construction: if this function could await a
    /// repository, the call it made would be per status and Requirement
    /// 5.1's "a count that does not depend on N" would be quietly lost.
    fn render_input<'a>(
        &self,
        status: &'a Status,
        ctx: &RenderContext<'_>,
        materials: &BatchedMaterials,
    ) -> StatusRenderInput<'a> {
        StatusRenderInput {
            status,
            // Unconditional: `resolve_materials` was handed exactly the
            // statuses this pass renders, and it resolves every one of their
            // authors. A miss would mean the two disagree about the batch,
            // which is a bug in this module rather than a degradation any
            // caller should have to render around.
            account: materials
                .accounts
                .get(&status.actor_id)
                .cloned()
                .expect("every rendered status's author was resolved for this batch"),
            media_attachments: self.media_json(status.id, materials, ctx.origin),
            mentions: Vec::new(),
            tags: tags_json(status.id, materials, ctx.origin),
            emojis: materials
                .emojis
                .select(&extract_content_tokens(&status.content).emoji_shortcodes),
            poll: status
                .poll_id
                .and_then(|poll_id| materials.polls.get(&poll_id).cloned()),
            interactions: interaction_state(status, ctx, materials),
            reblog: None,
        }
    }

    /// Renders the attachments, delegating the MediaAttachment shape itself
    /// to media-pipeline.
    ///
    /// A `media_id` with no resolved row is omitted rather than raising: an
    /// attachment that has been reaped should cost the reader the
    /// attachment, not the whole status. Batching preserves that exactly,
    /// because `media_ids_for_statuses` keeps each status's attachment order
    /// and `find_by_ids` simply has no entry for a reaped id.
    fn media_json(
        &self,
        status_id: Id,
        materials: &BatchedMaterials,
        origin: &ForwardedOrigin,
    ) -> Vec<Value> {
        let Some(media_ids) = materials.media_ids.get(&status_id) else {
            return Vec::new();
        };
        media_ids
            .iter()
            .filter_map(|media_id| materials.media.get(media_id))
            .map(|media| {
                serde_json::to_value(to_media_attachment(media, &self.media_store, origin))
                    .expect("MediaAttachmentJson always serializes to JSON")
            })
            .collect()
    }

    /// Renders a poll on its own, for the poll endpoints, which return one
    /// without a surrounding status.
    ///
    /// Shared with the batched path rather than duplicated there: the emoji
    /// resolution below is the same shortcode scan, and having it in two
    /// places is how the status-render glue started multiplying in the first
    /// place. This entry point resolves that one poll's emoji itself, since
    /// its callers have no batch to fold them into.
    pub(crate) async fn render_poll(
        &self,
        poll: &Poll,
        tally: &PollTally,
        viewer: Option<Id>,
        now: OffsetDateTime,
    ) -> Result<Value, AppError> {
        let emojis = self
            .resolve_shortcodes(&poll_emoji_shortcodes(tally))
            .await?;
        Ok(poll_json(poll, tally, &emojis, viewer, now))
    }
}

fn tags_json(
    status_id: Id,
    materials: &BatchedMaterials,
    origin: &ForwardedOrigin,
) -> Vec<TagJson> {
    let Some(tags) = materials.tags.get(&status_id) else {
        return Vec::new();
    };
    tags.iter()
        .map(|tag| TagJson {
            url: format!("{}://{}/tags/{}", origin.scheme, origin.host, tag.name),
            name: tag.name.clone(),
        })
        .collect()
}

/// Reads the viewer-scoped operation state out of the resolved id sets. All
/// four interaction flags are `false` for an unauthenticated read — for
/// which no set was resolved at all; `muted` comes from the context and is
/// independent of whether a viewer is present.
fn interaction_state(
    status: &Status,
    ctx: &RenderContext<'_>,
    materials: &BatchedMaterials,
) -> StatusInteractionState {
    let muted = ctx.is_muted(status.actor_id);
    let Some(interactions) = &materials.interactions else {
        return StatusInteractionState {
            muted,
            ..StatusInteractionState::default()
        };
    };

    StatusInteractionState {
        favourited: interactions.favourited.contains(&status.id),
        reblogged: interactions.reblogged.contains(&status.id),
        bookmarked: interactions.bookmarked.contains(&status.id),
        pinned: interactions.pinned.contains(&status.id),
        muted,
    }
}
