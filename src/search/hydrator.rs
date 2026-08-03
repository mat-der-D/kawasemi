//! `SearchHydrator` (design.md "Service / サービス層" -> "#### SearchHydrator";
//! Requirements 1.2, 3.2, 3.3, 3.5, 4.2, 5.2; task 4.2, `Boundary:
//! SearchHydrator`): concretizes the bare identifiers a [`SearchBackend`]
//! match returns ([`AccountRef`] / [`Id`] / [`TagMatch`]) into the actual
//! Account/Status/Tag JSON a `SearchResults` response embeds, by consuming —
//! never redefining — accounts-and-instance's Account serialization,
//! statuses-core's visible-status resolution + Status serialization +
//! `VisibilityPolicy`, and this spec's own [`TagSerializer`].
//!
//! ## Scope
//! This module owns exactly [`SearchHydrator`] and its three public methods
//! ([`SearchHydrator::hydrate_accounts`] / [`SearchHydrator::hydrate_statuses`]
//! / [`SearchHydrator::hydrate_hashtags`]), design.md's own literal Service
//! Interface (lines ~427-429). It does not implement `RemoteResolver` (task
//! 4.3), `SearchService`/`SearchEndpoint` (task 5.x), or any bootstrap/
//! `AppState` wiring beyond registering this module in `src/search.rs`
//! (`pub mod` + `pub use`, mirroring task 4.1's own registration of
//! `tag_serializer`/`result_serializer`). It does not touch
//! `src/search/pg_backend.rs` (task 3.1/3.2, already committed and
//! unmodified here) or `src/search/hashtag_repository.rs` (task 2.1,
//! likewise unmodified — this module only *calls*
//! [`crate::search::hashtag_repository::match_hashtags`], the same function
//! [`crate::search::pg_backend::PgSearchBackend::search_hashtags`] already
//! calls for a different purpose).
//!
//! ## No `StatusService`/generic port-parameter stack (mirrors
//! `AccountStatusesProviderImpl`/`NotificationService`/`StatusHydrator`)
//! `crate::statuses::status_service::StatusService<A, D, L, H, R, M>` is
//! generic over six port type parameters purely for its *write* paths
//! (activity building/delivery/mention lookup) this module never exercises —
//! pulling it in just to reuse its `show` method would drag that entire
//! generic stack into this struct's own constructor for a redundant
//! visibility check, exactly the reasoning
//! `crate::statuses::account_provider`'s own doc comment ("Why `poll`/
//! reblog-target resolution bypasses `PollService`/`StatusService`") already
//! documents and this crate's other cross-spec hydrators
//! (`crate::notifications::service::NotificationService`,
//! `crate::timelines::hydrator::StatusHydrator`) already follow. This module
//! therefore reads `crate::statuses::status_repository`/
//! `interaction_repository`/`poll_repository`/`tag_repository` directly
//! (read-only) and applies `crate::statuses::visibility::is_visible` itself,
//! via a constructor-supplied [`RelationshipQueryRegistry`] handle — the
//! concrete, runtime-replaceable slot `crate::statuses::StatusesModule::
//! relationship_query_registry()` already exposes for exactly this kind of
//! downstream-spec consumer, not a `R: RelationshipQuery` generic parameter
//! (mirrors `AccountStatusesProviderImpl`'s identical "concrete, not
//! generic" choice, that module's own doc comment, "`RelationshipQuery`:
//! `RelationshipQueryRegistry`, not a generic parameter").
//!
//! This is "statuses-core 可視投稿の解決 + `VisibilityPolicy` で不可視除外"
//! (design.md's own [`SearchHydrator`] Responsibilities text) applied
//! directly rather than through `StatusService::show`: `StatusService::show`
//! itself does exactly this same `find_by_id` + `is_visible` sequence
//! internally (`src/statuses/status_service.rs`'s own `show`/`visible_to`),
//! so calling the pure building blocks it is itself built from is not a
//! weaker or divergent visibility check — it is the identical policy,
//! reached without paying for six unused generic parameters.
//!
//! ## Status JSON assembly: duplicated glue, not a new shared helper
//! (documented, already-reviewed convention — see
//! `crate::statuses::account_provider`'s own doc comment, "Rendering glue",
//! and `crate::notifications::service::NotificationService`'s own doc
//! comment, "Rendering `account`/`status`") [`SearchHydrator::render_status`]/
//! [`SearchHydrator::leaf_render_input`]/[`SearchHydrator::resolve_common`]
//! duplicate `AccountStatusesProviderImpl::render`'s exact assembly glue
//! (same repository calls, same [`StatusRenderInput`] construction, same
//! one-level-only reblog nesting with an independent visibility re-check on
//! the boosted target) — this is now the *fourth* module in this crate doing
//! so (after `endpoints.rs`, `account_provider.rs`, `notifications/
//! service.rs`, `timelines/hydrator.rs`), which several of those modules'
//! own doc comments already flag as the point at which a shared,
//! `pub(crate)` helper might finally be worth extracting. Flagged again here
//! as a CONCERN for reviewer confirmation rather than silently starting that
//! refactor inside this task's own boundary (`SearchHydrator` only).
//!
//! One deliberate divergence from `AccountStatusesProviderImpl::render`:
//! this module resolves `emojis` via `crate::statuses::status_service::
//! extract_content_tokens` + `crate::accounts::emoji_repository::
//! resolve_emojis` (mirrors `NotificationService`'s/`StatusHydrator`'s own
//! choice) rather than leaving `emojis: Vec::new()`
//! (`AccountStatusesProviderImpl`'s own narrower choice) — a search result
//! embedding a post's custom-emoji shortcodes unresolved would render
//! `:shortcode:` literally in every Mastodon client, which no requirement
//! asks for but is straightforward to avoid by reusing the exact same two
//! calls two sibling modules already make for the identical reason.
//!
//! ## `hydrate_accounts`: dedup first, `following` filter last (Requirements
//! 3.5, 3.3)
//! [`SearchHydrator::hydrate_accounts`] deduplicates `refs` first (an
//! order-preserving `HashSet`-backed scan — [`AccountRef`] is `Copy + Eq +
//! Hash`, `crate::domain::primitives`), matching Requirement 3.5's "同一ア
//! カウントが重複して現れないよう...一意化". The `following_only` filter
//! (Requirement 3.3) is applied next, *before* serialization — cheaper than
//! filtering post-hoc, and "最終フィルタ" (design.md's own wording) describes
//! this filter's place in the *filtering* pipeline (last filtering step: no
//! further narrowing happens after it), not a requirement that JSON
//! rendering itself must happen first. `following_only` is resolved via
//! `crate::accounts::ports::AccountPortsRegistry::relationships` — the same
//! delegation boundary `AccountService::relationships` already consumes for
//! accounts-and-instance's own `GET /accounts/relationships` — reading
//! `RelationshipView::following` per candidate rather than this spec
//! defining any follow-relationship storage of its own (this spec's own
//! Boundary Commitments: "フォロー関係状態の実体...本 spec は消費のみ").
//! Every remaining `AccountRef` is then rendered into Account JSON via
//! `crate::accounts::account_service::AccountService::show_account` — one
//! call per account (Requirement 1.2, 3.2's "複数 ID 一括" is read as "many
//! ids resolved in one `hydrate_accounts` invocation", not "one batched SQL
//! round trip": `AccountService`'s only account-resolution entry point,
//! `show_account`, is single-id — the same single-id call
//! `NotificationService::account_json`/`AccountStatusesProviderImpl::
//! account_json`/`StatusHydrator::account_json` already make once per
//! account for the identical reason; no batched variant exists anywhere in
//! this crate to call instead). Flagged as a CONCERN for reviewer
//! confirmation against design.md's literal "複数 ID 一括" wording.
//!
//! ## `hydrate_statuses`: best-effort truncation to `limit` (Requirement 4.6)
//! [`SearchHydrator::hydrate_statuses`] walks `ids` in order, resolving and
//! visibility-checking each one, and stops as soon as `limit` *visible*
//! results have been collected — never rendering (or even fetching) more
//! candidates than necessary once `limit` is reached. This is the
//! post-visibility-filter truncation design.md assigns to this method
//! (`PgSearchBackend::search_statuses`'s own overfetch convention hands this
//! method more candidate ids than `limit` precisely so this truncation has
//! room to absorb invisible-post exclusions) — if fewer than `limit`
//! candidates turn out visible, this method simply returns however many it
//! found, exactly design.md's own documented "ベストエフォートの緩和策...
//! `limit` に満たないことがある...ハードな保証ではない".
//!
//! ## `hydrate_hashtags`: re-resolves `TagView` by exact name (Requirement
//! 5.2)
//! [`TagMatch`] (`crate::search::model`) carries only a bare `name` — the
//! `url`/`history` `crate::search::hashtag_repository::match_hashtags`
//! itself already computed were deliberately dropped by
//! `PgSearchBackend::search_hashtags` (that module's own doc comment: "url/
//! history are dropped here, not this task's concern"). This method
//! re-fetches the full [`TagView`] via the same `match_hashtags` (a name
//! *prefix* match, `LIKE $1%`) with `limit = 1`, relying on `search_tags.name`
//! being `UNIQUE` (`migrations/0013_search.sql`) and `match_hashtags`'
//! `ORDER BY name ASC`: an exact-name row is always the lexicographically
//! shortest match among every row sharing that prefix (a string is always
//! `<=` any longer string it is a prefix of), so it is always the first row
//! returned when it exists — which it must, since `tag.name` was itself read
//! out of `search_tags` moments earlier by `PgSearchBackend::search_hashtags`
//! in the same request. As a defensive safety net against that ordering
//! assumption (rather than trusting it blindly), this method still filters
//! the (at most one) returned candidate down to an exact `view.name ==
//! tag.name` match before rendering, and silently skips (never errors) a tag
//! that — in the unlikely event of a concurrent delete between the backend's
//! match and this hydration call — no longer resolves at all, mirroring this
//! crate's established "dangling reference is not a hydration failure"
//! convention (`NotificationService`'s own doc comment, "Dangling references
//! are not errors").

#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::sync::Arc;

use serde_json::Value;
use sqlx::PgPool;

use crate::accounts::account_service::AccountService;
use crate::accounts::emoji_repository;
use crate::accounts::model::CustomEmojiView;
use crate::accounts::ports::AccountPortsRegistry;
use crate::api::pagination::ForwardedOrigin;
use crate::domain::{AccountRef, Id};
use crate::error::AppError;
use crate::federation::signatures::ReqwestFederationHttpClient;
use crate::media::LocalFsStore;
use crate::media::media_repository;
use crate::media::serializer::to_media_attachment;
use crate::runtime::RuntimeContext;
use crate::search::hashtag_repository::match_hashtags;
use crate::search::model::TagMatch;
use crate::search::tag_serializer::TagSerializer;
use crate::statuses::interaction_repository;
use crate::statuses::model::Status;
use crate::statuses::poll_repository;
use crate::statuses::serializer::{
    SerializeContext, StatusInteractionState, StatusRenderInput, TagJson, poll_to_json,
    status_to_json,
};
use crate::statuses::status_repository;
use crate::statuses::status_service::extract_content_tokens;
use crate::statuses::tag_repository;
use crate::statuses::visibility::{RelationshipQuery, RelationshipQueryRegistry, is_visible};

/// Recovers the bare [`Id`] an [`AccountRef`] carries, regardless of
/// local/remote-ness. Mirrors `crate::accounts::ports::account_ref_id`'s/
/// `crate::statuses::account_provider::account_ref_id`'s identical
/// private-to-their-own-module helper — this module keeps its own copy
/// rather than widening either (this crate's established small-duplication
/// convention for a one-line, cross-module helper, per this module's own
/// doc comment).
fn account_ref_id(account_ref: &AccountRef) -> Id {
    match *account_ref {
        AccountRef::Local(id) => id,
        AccountRef::Remote(id) => id,
    }
}

/// Every pre-resolved field [`status_to_json`] needs beyond the bare
/// [`Status`] row, minus `reblog` (assembled separately, since it recurses)
/// — mirrors `AccountStatusesProviderImpl`'s/`StatusHydrator`'s identical
/// tuple shape (see this module's own doc comment).
type ResolvedCommon = (
    Value,
    Vec<Value>,
    Vec<TagJson>,
    Vec<CustomEmojiView>,
    StatusInteractionState,
    Option<Value>,
);

/// Concretizes [`SearchBackend`](crate::search::ports::SearchBackend) match
/// identifiers into Account/Status/Tag JSON (Requirements 1.2, 3.2, 3.3,
/// 3.5, 4.2, 5.2). See this module's doc comment for the full reasoning
/// behind every constructor dependency and design.md deviation.
pub struct SearchHydrator {
    pool: PgPool,
    accounts: Arc<AccountService<LocalFsStore, ReqwestFederationHttpClient>>,
    account_ports: AccountPortsRegistry,
    media_store: LocalFsStore,
    relationship_query: RelationshipQueryRegistry,
    runtime: RuntimeContext,
    domain: String,
    tag_serializer: TagSerializer,
}

impl SearchHydrator {
    /// Builds a `SearchHydrator` from already-constructed collaborators —
    /// mirrors this crate's established "bundle, don't build" business-layer
    /// constructor convention (e.g. `NotificationService::new`,
    /// `AccountStatusesProviderImpl::new`). `pool`/`accounts`/`media_store`
    /// are the exact same shared handles `NotificationService`/
    /// `AccountStatusesProviderImpl` already take (`crate::accounts::
    /// AccountsModule::service()`/`crate::media::MediaModule::store()`, not
    /// a second independent instance); `account_ports` is
    /// `AccountsModule::ports()` (the `following`-filter delegation
    /// boundary); `relationship_query` is `crate::statuses::StatusesModule::
    /// relationship_query_registry()` (the status-visibility delegation
    /// boundary); `runtime` supplies poll rendering's `now`; `domain` is
    /// this instance's own configured server domain, used both for this
    /// hydrator's own synthesized `ForwardedOrigin` (see this module's doc
    /// comment on why no live per-request origin is available this many
    /// layers away from HTTP, mirroring `NotificationService::origin`/
    /// `AccountStatusesProviderImpl::origin`) and to construct this
    /// hydrator's own [`TagSerializer`].
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pool: PgPool,
        accounts: Arc<AccountService<LocalFsStore, ReqwestFederationHttpClient>>,
        account_ports: AccountPortsRegistry,
        media_store: LocalFsStore,
        relationship_query: RelationshipQueryRegistry,
        runtime: RuntimeContext,
        domain: impl Into<String>,
    ) -> Self {
        let domain = domain.into();
        let tag_serializer = TagSerializer::new(domain.clone());
        Self {
            pool,
            accounts,
            account_ports,
            media_store,
            relationship_query,
            runtime,
            domain,
            tag_serializer,
        }
    }

    /// See this module's doc comment ("Status JSON assembly") and
    /// `NotificationService::origin`'s identical rationale.
    fn origin(&self) -> ForwardedOrigin {
        ForwardedOrigin::resolve("https", &self.domain, None, None)
    }

    // ---- hydrate_accounts (Requirements 1.2, 3.2, 3.3, 3.5) ----------------

    /// Deduplicates `refs`, optionally narrows to accounts `viewer` follows,
    /// and renders every remaining [`AccountRef`] into Account JSON via
    /// accounts-and-instance's own serialization. See this module's doc
    /// comment ("`hydrate_accounts`: dedup first, `following` filter last")
    /// for the full ordering rationale.
    pub async fn hydrate_accounts(
        &self,
        refs: &[AccountRef],
        viewer: Id,
        following_only: bool,
    ) -> Result<Vec<Value>, AppError> {
        let mut seen = HashSet::with_capacity(refs.len());
        let mut unique = Vec::with_capacity(refs.len());
        for account_ref in refs {
            if seen.insert(*account_ref) {
                unique.push(*account_ref);
            }
        }

        if following_only {
            let relationships = self.account_ports.relationships(viewer, &unique).await?;
            let following: HashSet<Id> = unique
                .iter()
                .zip(relationships.iter())
                .filter(|(_, rel)| rel.following)
                .map(|(account_ref, _)| account_ref_id(account_ref))
                .collect();
            unique.retain(|account_ref| following.contains(&account_ref_id(account_ref)));
        }

        let origin = self.origin();
        let mut out = Vec::with_capacity(unique.len());
        for account_ref in unique {
            let id = account_ref_id(&account_ref);
            out.push(
                self.accounts
                    .show_account(&id.as_i64().to_string(), None, &origin)
                    .await?,
            );
        }
        Ok(out)
    }

    // ---- hydrate_statuses (Requirements 1.2, 4.2, 4.6) ---------------------

    /// Resolves `ids` in order, keeping only posts visible to `viewer`
    /// (Requirement 4.2, applying `crate::statuses::visibility::is_visible`
    /// directly — see this module's doc comment, "No `StatusService`/generic
    /// port-parameter stack"), and stops once `limit` visible results have
    /// been rendered (Requirement 4.6 — see this module's doc comment,
    /// "`hydrate_statuses`: best-effort truncation").
    pub async fn hydrate_statuses(
        &self,
        ids: &[Id],
        viewer: Id,
        limit: u32,
    ) -> Result<Vec<Value>, AppError> {
        let limit = limit as usize;
        let origin = self.origin();
        let mut out = Vec::new();
        for &id in ids {
            if out.len() >= limit {
                break;
            }
            let Some(status) = status_repository::find_by_id(&self.pool, id).await? else {
                continue;
            };
            if !self.status_visible(&status, Some(viewer)).await? {
                continue;
            }
            out.push(self.render_status(status, Some(viewer), &origin).await?);
        }
        Ok(out)
    }

    /// The single visibility judgment (`crate::statuses::visibility::
    /// is_visible`), resolved through this hydrator's own
    /// [`RelationshipQueryRegistry`] handle — the exact same pure function +
    /// registry pairing `AccountStatusesProviderImpl::visible_to` uses.
    async fn status_visible(&self, status: &Status, viewer: Option<Id>) -> Result<bool, AppError> {
        let rel = self
            .relationship_query
            .viewer_relation(status.actor_id, viewer)
            .await?;
        Ok(is_visible(status, viewer, &rel))
    }

    /// Resolves `status` (already known visible to `viewer`) into its full
    /// Status JSON, nesting a visible reblog target under `reblog` (at most
    /// one level, never recursive) — see this module's doc comment ("Status
    /// JSON assembly").
    async fn render_status(
        &self,
        status: Status,
        viewer: Option<Id>,
        origin: &ForwardedOrigin,
    ) -> Result<Value, AppError> {
        let reblog_target = match status.reblog_of_id {
            Some(target_id) => match status_repository::find_by_id(&self.pool, target_id).await? {
                Some(target) if self.status_visible(&target, viewer).await? => Some(target),
                _ => None,
            },
            None => None,
        };
        let reblog_box = match &reblog_target {
            Some(target) => Some(Box::new(
                self.leaf_render_input(target, viewer, origin).await?,
            )),
            None => None,
        };

        let (account, media_attachments, tags, emojis, interactions, poll) =
            self.resolve_common(&status, viewer, origin).await?;

        let input = StatusRenderInput {
            status: &status,
            account,
            media_attachments,
            mentions: Vec::new(),
            tags,
            emojis,
            poll,
            interactions,
            reblog: reblog_box,
        };
        Ok(status_to_json(&input))
    }

    /// Builds a non-recursive [`StatusRenderInput`] (its own `reblog`
    /// always `None`) for a boost's own boosted-status target — mirrors
    /// `AccountStatusesProviderImpl::leaf_render_input`'s identical
    /// one-level-only nesting discipline.
    async fn leaf_render_input<'a>(
        &self,
        status: &'a Status,
        viewer: Option<Id>,
        origin: &ForwardedOrigin,
    ) -> Result<StatusRenderInput<'a>, AppError> {
        let (account, media_attachments, tags, emojis, interactions, poll) =
            self.resolve_common(status, viewer, origin).await?;
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

    /// The pieces every rendered [`Status`] needs beyond the bare row
    /// itself, shared by both a top-level render and a nested reblog
    /// target's own (non-recursive) render — mirrors
    /// `AccountStatusesProviderImpl::resolve_common`'s (via its own inlined
    /// `render`) identical structure.
    async fn resolve_common(
        &self,
        status: &Status,
        viewer: Option<Id>,
        origin: &ForwardedOrigin,
    ) -> Result<ResolvedCommon, AppError> {
        let account = self
            .accounts
            .show_account(&status.actor_id.as_i64().to_string(), None, origin)
            .await?;
        let media_attachments = self.media_json(status.id, origin).await?;
        let tags = self.tags_json(status.id, origin).await?;
        let emojis = self.resolve_emojis(&status.content).await?;
        let interactions = self.interaction_state(status.id, viewer).await?;
        let poll = match status.poll_id {
            Some(poll_id) => self.poll_json(poll_id, viewer).await?,
            None => None,
        };
        Ok((account, media_attachments, tags, emojis, interactions, poll))
    }

    /// Delegates MediaAttachment JSON to media-pipeline wholesale. A media
    /// id with no resolvable row is simply omitted, mirroring
    /// `AccountStatusesProviderImpl::media_json`'s identical convention.
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

    /// Resolves `status_id`'s tags into `TagJson`, mirroring
    /// `AccountStatusesProviderImpl::tags_json`'s identical URL
    /// construction. Distinct from this spec's own hashtag *search* index
    /// (`crate::search::hashtag_repository`) — this reads statuses-core's
    /// own `tags`/`status_tags` tables (`crate::statuses::tag_repository`),
    /// the per-status embedded-tags list every Status JSON carries,
    /// unrelated to this hydrator's own [`Self::hydrate_hashtags`].
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

    /// Resolves `content`'s `:shortcode:` tokens against accounts-and-
    /// instance's custom-emoji directory — mirrors `NotificationService::
    /// resolve_emojis`'s/`StatusHydrator::resolve_emojis`'s identical reuse
    /// of `status_service::extract_content_tokens` +
    /// `emoji_repository::resolve_emojis`. An unregistered shortcode is
    /// simply absent from the result, never an error.
    async fn resolve_emojis(&self, content: &str) -> Result<Vec<CustomEmojiView>, AppError> {
        let shortcodes = extract_content_tokens(content).emoji_shortcodes;
        if shortcodes.is_empty() {
            return Ok(Vec::new());
        }
        emoji_repository::resolve_emojis(&self.pool, &shortcodes).await
    }

    /// Resolves `status_id`'s viewer-scoped operation state. `muted` is
    /// always `false` — this hydrator has no mute-signal source of its own
    /// (unlike `crate::timelines::hydrator::StatusHydrator`, which reads one
    /// from `FilterContext`), mirroring `AccountStatusesProviderImpl::
    /// interaction_state`'s identical `muted: false` default.
    async fn interaction_state(
        &self,
        status_id: Id,
        viewer: Option<Id>,
    ) -> Result<StatusInteractionState, AppError> {
        let Some(actor_id) = viewer else {
            return Ok(StatusInteractionState::default());
        };
        let favourited =
            interaction_repository::exists_favourite(&self.pool, actor_id, status_id).await?;
        let bookmarked =
            interaction_repository::exists_bookmark(&self.pool, actor_id, status_id).await?;
        let pinned = interaction_repository::exists_pin(&self.pool, actor_id, status_id).await?;
        let reblogged = interaction_repository::find_reblog(&self.pool, actor_id, status_id)
            .await?
            .is_some();
        Ok(StatusInteractionState {
            favourited,
            reblogged,
            bookmarked,
            pinned,
            muted: false,
        })
    }

    /// Resolves `poll_id`'s Poll JSON, mirroring
    /// `AccountStatusesProviderImpl::poll_json`'s identical
    /// `poll_repository`/`poll_to_json` reuse (empty emoji candidate slice —
    /// this method matches that module's own narrower choice for poll
    /// rendering specifically, unlike this module's own Status-level
    /// `resolve_emojis`, since a poll option's own title text is a separate,
    /// smaller surface no requirement in this task's boundary asks to
    /// resolve emoji in). Degrades to `None` — never an error — for a
    /// `poll_id` that no longer resolves to a row (a dangling reference, not
    /// a hydration failure; see this module's doc comment, "`hydrate_hashtags`:
    /// re-resolves `TagView`" and `NotificationService`'s identical
    /// precedent, "Dangling references are not errors").
    async fn poll_json(&self, poll_id: Id, viewer: Option<Id>) -> Result<Option<Value>, AppError> {
        let Some(poll) = poll_repository::find_poll_by_id(&self.pool, poll_id).await? else {
            return Ok(None);
        };
        let tally = poll_repository::tally(&self.pool, poll_id, viewer).await?;
        let ctx = SerializeContext {
            viewer,
            now: self.runtime.clock.now(),
        };
        Ok(Some(poll_to_json(&poll, &tally, &[], &ctx)))
    }

    // ---- hydrate_hashtags (Requirement 5.2) --------------------------------

    /// Renders every `tags` entry into Tag JSON via `TagView` re-resolution
    /// followed by `TagSerializer::build_tag` — see this module's doc
    /// comment ("`hydrate_hashtags`: re-resolves `TagView` by exact name")
    /// for why this re-queries `HashtagIndexRepository::match_hashtags`
    /// rather than reusing any `url`/`history` from the original match
    /// (there is none: [`TagMatch`] is identifier-only, Requirement 7.2). A
    /// tag that no longer resolves (a concurrent delete between match and
    /// hydration) is silently skipped, never an error.
    pub async fn hydrate_hashtags(&self, tags: &[TagMatch]) -> Result<Vec<Value>, AppError> {
        let mut out = Vec::with_capacity(tags.len());
        for tag in tags {
            let candidates = match_hashtags(&self.pool, &tag.name, 1, 0).await?;
            if let Some(view) = candidates.into_iter().find(|view| view.name == tag.name) {
                out.push(self.tag_serializer.build_tag(&view));
            }
        }
        Ok(out)
    }
}
