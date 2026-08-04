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

#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::future::Future;
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
use crate::media::serializer::to_media_attachment;
use crate::statuses::interaction_repository;
use crate::statuses::model::{Poll, Status};
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

        let mut out = Vec::with_capacity(statuses.len());
        for (status, reblog_target) in statuses.iter().zip(reblog_targets) {
            let reblog = match reblog_target {
                Some(target) => Some(Box::new(self.render_input(target, ctx).await?)),
                None => None,
            };
            let mut input = self.render_input(status, ctx).await?;
            input.reblog = reblog;
            out.push(status_to_json(&input));
        }
        Ok(out)
    }

    /// Renders a single status — the same path [`Self::assemble_many`]
    /// takes, with a one-element batch, so the two can never drift.
    pub(crate) async fn assemble_one(
        &self,
        status: &Status,
        reblog_target: Option<&Status>,
        ctx: &RenderContext<'_>,
    ) -> Result<Value, AppError> {
        let reblog = match reblog_target {
            Some(target) => Some(Box::new(self.render_input(target, ctx).await?)),
            None => None,
        };
        let mut input = self.render_input(status, ctx).await?;
        input.reblog = reblog;
        Ok(status_to_json(&input))
    }

    /// Resolves everything [`status_to_json`] needs beyond the row itself,
    /// leaving `reblog` unset for the caller to fill.
    async fn render_input<'a>(
        &self,
        status: &'a Status,
        ctx: &RenderContext<'_>,
    ) -> Result<StatusRenderInput<'a>, AppError> {
        let account = self
            .accounts
            .show_account(&status.actor_id.as_i64().to_string(), None, ctx.origin)
            .await?;
        let media_attachments = self.media_json(status.id, ctx.origin).await?;
        let tags = self.tags_json(status.id, ctx.origin).await?;
        let emojis = self.resolve_emojis(&status.content).await?;
        let interactions = self.interaction_state(status, ctx).await?;
        let poll = self.poll_json(status.poll_id, ctx).await?;

        Ok(StatusRenderInput {
            status,
            account,
            media_attachments,
            mentions: Vec::new(),
            tags,
            emojis,
            poll,
            interactions,
            reblog: None,
        })
    }

    /// Renders the attachments, delegating the MediaAttachment shape itself
    /// to media-pipeline.
    ///
    /// A `media_id` with no resolvable row is omitted rather than raising:
    /// an attachment that has been reaped should cost the reader the
    /// attachment, not the whole status.
    async fn media_json(
        &self,
        status_id: Id,
        origin: &ForwardedOrigin,
    ) -> Result<Vec<Value>, AppError> {
        let media_ids = status_repository::media_ids_for_status(&self.pool, status_id).await?;
        let mut out = Vec::with_capacity(media_ids.len());
        for media_id in media_ids {
            if let Some(media) = media_repository::find_by_id(&self.pool, media_id).await? {
                out.push(
                    serde_json::to_value(to_media_attachment(&media, &self.media_store, origin))
                        .expect("MediaAttachmentJson always serializes to JSON"),
                );
            }
        }
        Ok(out)
    }

    async fn tags_json(
        &self,
        status_id: Id,
        origin: &ForwardedOrigin,
    ) -> Result<Vec<TagJson>, AppError> {
        let tags = tag_repository::tags_for_status(&self.pool, status_id).await?;
        Ok(tags
            .into_iter()
            .map(|tag| TagJson {
                url: format!("{}://{}/tags/{}", origin.scheme, origin.host, tag.name),
                name: tag.name,
            })
            .collect())
    }

    /// Resolves `content`'s `:shortcode:` tokens against the custom-emoji
    /// directory. An unregistered shortcode is simply absent from the
    /// result, never an error.
    async fn resolve_emojis(&self, content: &str) -> Result<Vec<CustomEmojiView>, AppError> {
        let shortcodes = extract_content_tokens(content).emoji_shortcodes;
        if shortcodes.is_empty() {
            return Ok(Vec::new());
        }
        emoji_repository::resolve_emojis(&self.pool, &shortcodes).await
    }

    /// Resolves the viewer-scoped operation state. All four interaction
    /// flags are `false` for an unauthenticated read; `muted` comes from
    /// the context and is independent of whether a viewer is present.
    async fn interaction_state(
        &self,
        status: &Status,
        ctx: &RenderContext<'_>,
    ) -> Result<StatusInteractionState, AppError> {
        let muted = ctx.is_muted(status.actor_id);
        let Some(viewer) = ctx.viewer else {
            return Ok(StatusInteractionState {
                muted,
                ..StatusInteractionState::default()
            });
        };

        Ok(StatusInteractionState {
            favourited: interaction_repository::exists_favourite(&self.pool, viewer, status.id)
                .await?,
            reblogged: interaction_repository::find_reblog(&self.pool, viewer, status.id)
                .await?
                .is_some(),
            bookmarked: interaction_repository::exists_bookmark(&self.pool, viewer, status.id)
                .await?,
            pinned: interaction_repository::exists_pin(&self.pool, viewer, status.id).await?,
            muted,
        })
    }

    /// Renders the poll, if the status has one and the caller's
    /// [`PollResolver`] produced it.
    async fn poll_json(
        &self,
        poll_id: Option<Id>,
        ctx: &RenderContext<'_>,
    ) -> Result<Option<Value>, AppError> {
        let Some(poll_id) = poll_id else {
            return Ok(None);
        };
        let resolved = ctx.polls.resolve_many(&[poll_id], ctx.viewer).await?;
        let Some((_, poll, tally)) = resolved.into_iter().next() else {
            return Ok(None);
        };
        Ok(Some(
            self.render_poll(&poll, &tally, ctx.viewer, ctx.now).await?,
        ))
    }

    /// Renders a poll on its own, for the poll endpoints, which return one
    /// without a surrounding status.
    ///
    /// Shared with [`Self::poll_json`] rather than duplicated there: the
    /// emoji resolution below is the same shortcode scan, and having it in
    /// two places is how the status-render glue started multiplying in the
    /// first place.
    pub(crate) async fn render_poll(
        &self,
        poll: &Poll,
        tally: &PollTally,
        viewer: Option<Id>,
        now: OffsetDateTime,
    ) -> Result<Value, AppError> {
        // Each option's title carries its own shortcodes, distinct from the
        // owning status's content. Joined with a space so a scan across the
        // boundary between two titles never merges them into one token.
        let combined_titles = tally
            .options
            .iter()
            .map(|option| option.title.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let emojis = self.resolve_emojis(&combined_titles).await?;
        let serialize_ctx = SerializeContext { viewer, now };
        Ok(poll_to_json(poll, tally, &emojis, &serialize_ctx))
    }
}
