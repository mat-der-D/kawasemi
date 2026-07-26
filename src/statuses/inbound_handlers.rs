//! `InboundHandlers` (design.md "Inbound / 受信層" -> `#### InboundHandlers`,
//! design.md lines ~590-616; Requirements 13.6, 14.1, 14.2, 14.3, 14.4, 14.5,
//! 15.2, 15.3; task 6.1, `Boundary: InboundHandlers`): implements
//! federation-core's [`InboundActivityHandler`] for the six post-related
//! inbound Activity kinds — `Create(Note)` / `Announce` / `Like` / `Delete` /
//! `Update` / `Undo(Announce|Like)` — and [`register_status_handlers`], which
//! registers all six against an [`InboundActivityDispatcher`] (Requirement
//! 14.1).
//!
//! ## Scope
//! Owns exactly [`CreateNoteHandler`], [`AnnounceHandler`], [`LikeHandler`],
//! [`DeleteHandler`], [`UpdateHandler`], [`UndoHandler`],
//! [`register_status_handlers`], [`StatusInboundDeps`], and the
//! [`RemoteActorResolver`] delegation port (see "Resolving the acting
//! remote actor" below). Does not touch federation-core's
//! `InboundActivityDispatcher`/`InboundActivityHandler` themselves (already
//! implemented and stable, `src/federation/inbound/dispatcher.rs`) and does
//! not wire `register_status_handlers` into `AppState`/bootstrap/the live
//! server (task 7.2's boundary — this is a standalone, independently
//! unit-testable module with no live caller yet, mirroring
//! `activity_builder.rs`'s/`status_service.rs`'s own identical "no live
//! caller yet" precedent).
//!
//! ## Task 6.2 additive widening: `ingest_note_object`/`object_reference_uri`
//! made `pub(crate)`
//! Task 6.2 (`Boundary: StatusIngestService`, `src/statuses/ingest_service.rs`)
//! needs the exact same Note-normalization/persistence logic
//! [`CreateNoteHandler::handle`] uses, for its own out-of-dispatch
//! "document/URL → Status" entry point (Requirement 14.5's shared-code-path
//! discipline extended one level up). Rather than duplicating that logic in
//! the new module, this task extracts it into [`ingest_note_object`] (a free
//! `pub(crate)` function both [`CreateNoteHandler`] and
//! `crate::statuses::ingest_service::StatusIngestService` call) and widens
//! [`object_reference_uri`] to `pub(crate)` (the new module reuses it
//! verbatim to read a `Note`'s `attributedTo` property). Neither change
//! alters this module's own observable behavior — `CreateNoteHandler` calls
//! [`ingest_note_object`] with the identical inputs/order of operations its
//! inlined code previously used.
//!
//! ## Every handler calls the exact same repository functions the
//! corresponding local-origin service already calls (Requirement 14.5)
//! No handler here reimplements a parallel state-transition:
//! - [`CreateNoteHandler`] calls [`status_repository::insert_status`] /
//!   [`status_repository::adjust_counts`] (Replies) /
//!   [`crate::statuses::tag_repository::upsert_tag`]/`associate_tag` — the
//!   same functions [`crate::statuses::status_service::StatusService::create_status`]
//!   calls, including reusing that module's own
//!   [`crate::statuses::status_service::extract_content_tokens`] hashtag
//!   scanner (widened to `pub(crate)` by this task — see that module's own
//!   doc comment, "`ExtractedTokens`") rather than a second, duplicated
//!   scanner.
//! - [`AnnounceHandler`]/[`LikeHandler`] call
//!   [`status_repository::insert_status`]/[`status_repository::adjust_counts`]
//!   (Reblogs) and [`interaction_repository::add_favourite`]/
//!   [`status_repository::adjust_counts`] (Favourites) respectively — the
//!   same functions [`crate::statuses::interaction_service::InteractionService::reblog`]/
//!   `favourite` call, minus the outbound `StatusActivityBuilder` dispatch
//!   (an inbound handler never re-delivers what it just received).
//! - [`DeleteHandler`]/[`UpdateHandler`] call
//!   [`status_repository::delete_status`]/[`status_repository::apply_edit`] —
//!   the same functions `StatusService::delete_status`/`edit_status` call.
//! - [`UndoHandler`] reverts an `Announce`/`Like` via
//!   [`status_repository::delete_status`]+[`status_repository::adjust_counts`]
//!   or [`interaction_repository::remove_favourite`]+[`status_repository::adjust_counts`]
//!   — the same functions `InteractionService::unreblog`/`unfavourite` call.
//! - The `Create{Note, name=...}` vote wire form branches into
//!   [`poll_repository::record_vote`] — the same function
//!   [`crate::statuses::poll_service::PollService::vote`] calls.
//!
//! No handler calls `StatusActivityBuilder`/`DeliverySink` at all: an inbound
//! handler only ever reflects state a remote peer already told us happened —
//! it never re-delivers the Activity it just received back out.
//!
//! ## Resolving the acting remote actor: `ctx.signer.actor_uri`, never the
//! JSON body's own `actor`/`attributedTo` property (security decision)
//! Every handler needs a stable [`Id`] to persist as `actor_id`/the
//! favourite-or-reblog-or-vote actor. [`InboundContext::signer`]
//! (federation-core's already-HTTP-Signature-verified
//! [`crate::federation::VerifiedSigner`]) is the *only* cryptographically
//! authenticated identity available to any inbound handler; the JSON body's
//! own `actor` (`Announce`/`Like`/`Delete`/`Update`/`Undo`) or `attributedTo`
//! (`Create`'s embedded `Note`) properties are merely *claimed*, unverified
//! data an attacker could set to any value without invalidating the HTTP
//! Signature (which only covers headers/digest, not deep-inspects every
//! embedded object property). Every handler in this module therefore
//! resolves the acting actor from `ctx.signer.actor_uri` exclusively via
//! [`resolve_actor_id`], never by reading `actor`/`attributedTo` off
//! `activity.raw` — this is what makes [`DeleteHandler`]/[`UpdateHandler`]'s
//! ownership check (below) meaningful rather than trivially spoofable.
//!
//! ## Resolving `actor_uri -> Id`: reuses `accounts-and-instance`'s already-
//! implemented `RemoteAccountFetcher` (documented cross-spec dependency, not
//! listed in task 6.1's own `_Depends:_` line)
//! `statuses.actor_id`/`favourites.actor_id`/`poll_votes.actor_id` (etc.) are
//! all documented as "logical-only reference to actor-model's
//! `local_actors.id`" (`migrations/0007_statuses.sql`'s own naming-note,
//! `model.rs`'s own doc comment) — written before this task's remote-actor
//! ingestion path existed, and narrower than what "ローカル/リモート共通モデ
//! ル" (`Status::local`) actually requires: a *remote*-authored row's
//! `actor_id` cannot reference `local_actors.id` at all. This task resolves
//! that gap the same way every other id-space gap in this crate is resolved
//! — reusing this crate's own global, single [`IdGenerator`] sequence, which
//! already makes `local_actors.id` and `remote_accounts.id` (accounts-and-
//! instance's own already-implemented remote-actor cache,
//! `src/accounts/remote_repository.rs`, task 2.2 of that spec) disjoint by
//! construction (both mint ids from the identical global sequence, never a
//! per-table counter) — so `actor_id` + `Status::local` together
//! unambiguously say *which* table (`local_actors` when `local`, effectively
//! `remote_accounts` when not) an `Id` addresses, exactly the same
//! discriminated-by-a-sibling-bool pattern `Status::local` already
//! establishes for every other actor-typed field in this table. Reusing
//! [`crate::accounts::RemoteAccountFetcher::fetch_and_normalize`] (rather
//! than inventing a second, narrower remote-actor cache inside this spec) is
//! the natural choice: it already does exactly "resolve an `actor_uri` to a
//! stable `Id`, fetching+caching the actor document on a cache miss/stale
//! entry" — reinventing it here would duplicate, not share, a state
//! transition (violating this same task's own Requirement 14.5 discipline
//! one level up, applied to actor identity instead of post state). This
//! module therefore defines a narrow [`RemoteActorResolver`] port (mirroring
//! `status_service.rs::MentionLookup`'s/`activity_builder.rs::ActorHandleLookup`'s
//! own "narrow trait wrapping a heavier real dependency" precedent) and
//! implements it for `RemoteAccountFetcher<H>` — every handler is generic
//! over `R: RemoteActorResolver`, so a unit test can supply a trivial
//! in-memory fake instead of a real HTTP-backed fetcher (this module's own
//! testing strategy, mirroring `poll_service/tests.rs`'s `MockActorLookup`
//! precedent).
//!
//! ## Locating a target by ActivityPub `uri`: `status_repository::find_by_uri`
//! (thin `pub` wrapper added by this task)
//! `Announce`/`Like`/`Delete`/`Update`/`Undo`'s inner `object` (and
//! `Create`'s `inReplyTo`) all reference their target by ActivityPub `uri`
//! string, never by this crate's internal [`Id`]. `status_repository.rs` had
//! no uri-keyed lookup before this task (every existing function is
//! `Id`-keyed); [`status_repository::find_by_uri`] is the thin, additive
//! `pub` wrapper this task adds — mirrors task 5.1's own identical precedent
//! of adding thin `pub` read wrappers to an earlier task's repository module
//! without touching any existing function's signature or behavior.
//!
//! ## `Requirement 14.3`'s "対象ローカル投稿" restricts `Announce`/`Like`/
//! `Undo` to a **local** target; `Delete`/`Update` require a **remote** one
//! `Announce`/`Like`/`Undo(Announce|Like)` only ever update the counters of a
//! post *we* host (`target.local == true`) — a remote actor boosting/liking
//! another remote post is not this instance's concern to track counters for
//! (Requirement 14.3's literal "対象ローカル投稿"). Conversely,
//! `Delete`/`Update` only ever act on a status *this instance itself
//! ingested from a remote origin* (`target.local == false`): a remote peer
//! claiming to `Delete`/`Update` one of *our own* local users' posts is
//! always rejected — such a request could otherwise let a malicious remote
//! server silently vanish/rewrite a local user's own post, which no local
//! owner ever authorized. Both directions return
//! [`HandleOutcome::Ignored`] (not an error): a target whose `local`-ness
//! disqualifies this handler is exactly "not owned by me", the same
//! semantics dispatcher.rs's own doc comment describes for the fan-out
//! contract.
//!
//! ## `DeleteHandler`/`UpdateHandler`'s ownership check (ctx.signer vs.
//! `target.actor_id`)
//! Beyond the `local`-ness gate above, both handlers additionally verify the
//! resolved acting actor ([`resolve_actor_id`]) equals `target.actor_id` —
//! rejecting a mismatch with a genuine `403 Forbidden` [`AppError`] (not
//! `Ignored`: the target *is* a remote post this instance tracks, so this
//! handler *does* own the activity type/inner-object-type combination; the
//! actor simply is not authorized to mutate *this particular* row). Without
//! this check, any remote actor could delete/edit any other remote actor's
//! already-ingested post merely by sending a `Delete`/`Update` naming that
//! post's `uri`.
//!
//! ## `Create{Note, name=...}` vote-wire-form detection (Requirement 13.6)
//! Mirrors [`crate::statuses::activity_builder::StatusActivityBuilder::deliver_vote`]'s
//! own emitted wire shape exactly (that function's own doc comment,
//! "`\"Vote\"` is never emitted as an Activity `type`"): a `Create` whose
//! inner `Note` carries a `name` property *and* an `inReplyTo` that resolves
//! (via [`status_repository::find_by_uri`]) to a **locally-owned**
//! (`target.local == true`) [`Status`] that itself carries a `poll_id`, where
//! `name` matches one of that poll's option titles exactly (case-sensitive —
//! mirrors `deliver_vote`'s own byte-exact title round-trip, no
//! normalization either side of the wire performs). When every one of these
//! conditions holds, [`CreateNoteHandler`] resolves `name` to that option's
//! `idx` and calls [`poll_repository::record_vote`] — the identical function
//! `PollService::vote` calls — instead of ingesting a `Status` row at all. A
//! `Create{Note, name=...}` that fails *any* one of these conditions (no
//! `inReplyTo`, `inReplyTo` target unknown/not local/has no `poll_id`, or
//! `name` matches no option title) falls through to ordinary `Note`
//! ingestion instead — `name` is simply not a field `Status`/`StatusEdit`
//! carries, so it is silently dropped on that path, matching Requirement
//! 15.2's "未知の方言プロパティ...解釈せず継続".
//!
//! ## Idempotent re-delivery: `Create(Note)` only; `record_vote`'s own
//! duplicate-vote rejection is left to propagate (documented judgment call)
//! [`CreateNoteHandler`]'s ordinary (non-vote) ingestion path checks
//! [`status_repository::find_by_uri`] first and returns
//! [`HandleOutcome::Handled`] with no further action if a `Status` under that
//! `uri` already exists (a safe no-op for a redelivered `Create`, avoiding a
//! `statuses_uri_key` unique-violation `409` on a harmless re-delivery).
//! `Announce`/`Like`'s own repository calls
//! ([`interaction_repository::find_reblog`]/[`interaction_repository::add_favourite`]'s
//! `ON CONFLICT DO NOTHING`) are already naturally idempotent the same way.
//! The vote branch is the one exception: a redelivered vote `Create` calls
//! [`poll_repository::record_vote`] a second time, which rejects it as a
//! duplicate vote (`422`) — that `AppError` is allowed to propagate
//! unchanged rather than being caught and silently converted to `Handled`,
//! mirroring `poll_service.rs`'s own documented philosophy ("Any rejection
//! ... propagates ... never silently swallowed") applied one layer up, to
//! the inbound path. Flagged here as a CONCERN per this task's own
//! instructions: a literal re-delivery of the identical vote Activity is
//! rare in practice (federation-core's own `ReceivedActivityStore` already
//! deduplicates by Activity `id` before this dispatcher is ever reached,
//! `dispatcher.rs`'s own doc comment, "Idempotency is the caller's
//! responsibility") but not structurally impossible (e.g. two *different*
//! vote Activities naming the same option, redelivered under different
//! Activity ids) — this handler does not add a second application-level
//! idempotency layer on top of `record_vote`'s own.
//!
//! ## What this handler does *not* persist (CONCERN — documented structural
//! gaps, same class already flagged elsewhere in this spec)
//! - **Attachments**: an ingested remote `Note`'s `attachment` property is
//!   never read at all. `status_media.media_id` is a logical reference to
//!   media-pipeline's own `media.id` (`status_repository.rs`'s own doc
//!   comment) — there is no reachable service in this task's dependency set
//!   ( `_Depends: 2.1, 2.2, 2.3, 3.1, 5.3_`) that fetches/normalizes/persists
//!   a *remote* media object into a local `media` row a `status_media` row
//!   could then reference; fabricating a `media_id` here would either
//!   violate that logical reference's own intended meaning or require this
//!   task to reach into media-pipeline's boundary, which it does not own.
//! - **Standalone (non-reply) mentions**: `Status`/`migrations/
//!   0007_statuses.sql` carry no mentions/addressee table at all
//!   (`status_service.rs`'s own "Mention resolution: local only" section
//!   already documents this exact schema limitation for the *outbound*
//!   direction; it is structurally identical, not a new gap, for inbound). A
//!   `tag`-array `Mention` entry on an ingested `Note` is therefore never
//!   read or persisted; the one form of "mention reflection" this schema
//!   *can* support — `in_reply_to_account_id`, set whenever `inReplyTo`
//!   resolves to a known [`Status`] — is implemented.
//!
//! Both gaps are read-omissions, not silent failures: nothing in this module
//! errors because of them, matching Requirement 15.2's "未知の方言プロパティ
//! ...を意味論として解釈せずコア処理を継続する" applied to a structural
//! (schema-level), not merely dialect-level, absence.
//!
//! ## Visibility derivation from `to`/`cc` (mirrors the outbound convention
//! in reverse)
//! [`derive_inbound_visibility`] mirrors `addressing.rs::derive_addressing`'s
//! own documented `to`/`cc` placement table in reverse: the ActivityStreams
//! public collection ([`crate::statuses::addressing::PUBLIC_COLLECTION_URI`])
//! in `to` means `Public`; in `cc` (not `to`) means `Unlisted`; a `to`/`cc`
//! entry whose URI ends in `/followers` with no public collection anywhere
//! means `Private`; anything else (only individually-named actor URIs, no
//! collection) means `Direct` — the same four-way split
//! `derive_addressing` encodes for the outbound direction, applied in
//! reverse for a document this instance did not itself generate.

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::{Map, Value};
use sqlx::postgres::PgPool;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::domain::{Id, Visibility};
use crate::error::AppError;
use crate::federation::inbound::dispatcher::{
    HandleOutcome, InboundActivityDispatcher, InboundActivityHandler, InboundContext,
};
use crate::federation::jsonld::ParsedActivity;
use crate::runtime::RuntimeContext;
use crate::statuses::addressing::PUBLIC_COLLECTION_URI;
use crate::statuses::interaction_repository;
use crate::statuses::model::{Status, StatusEdit};
use crate::statuses::poll_repository;
use crate::statuses::status_repository::{self, CountKind};
use crate::statuses::status_service::extract_content_tokens;
use crate::statuses::tag_repository;

/// The narrow `actor_uri -> Id` port every handler in this module depends on
/// to resolve the acting remote actor's stable, persistable [`Id`]. See this
/// module's doc comment ("Resolving `actor_uri -> Id`") for why this exists
/// and why it wraps [`crate::accounts::RemoteAccountFetcher`] rather than a new, narrower
/// remote-actor cache.
///
/// This method's return type is written explicitly as `impl Future<Output =
/// ..> + Send` rather than a plain `async fn` (unlike this crate's other
/// `#[allow(async_fn_in_trait)]` delegation-port traits, e.g.
/// `MentionLookup`/`ActorHandleLookup`): every handler in this module awaits
/// this port from *inside* the `Pin<Box<dyn Future<Output = ..> + Send +
/// 'a>>` that [`InboundActivityHandler::handle`] requires (see
/// `dispatcher.rs`'s own doc comment for why that trait is written in boxed-
/// future form at all), so this port's own future must carry an explicit
/// `Send` bound for that outer box to type-check; a plain `async fn` here
/// does not, by itself, guarantee one.
pub trait RemoteActorResolver: Send + Sync {
    /// Resolves `actor_uri` to a stable [`Id`], fetching and caching the
    /// remote actor document on a cache miss/stale entry as needed.
    fn resolve_remote_actor(
        &self,
        actor_uri: &str,
    ) -> impl std::future::Future<Output = Result<Id, AppError>> + Send;
}

// A blanket `impl<H: FederationHttpClient> RemoteActorResolver for
// RemoteAccountFetcher<H>` was deliberately *not* added here: `RemoteActorResolver`
// requires its future to be `Send` (this module's own doc comment above
// explains why), but `FederationHttpClient::fetch` — federation-core's own
// already-implemented, already-reviewed port (`src/federation/signatures/http_client.rs`,
// task 1.4, out of this task's boundary to modify) — declares a plain
// `async fn` with no `Send` bound, since none of its existing callers box it
// across a `dyn Future + Send` boundary the way this module's handlers must.
// `RemoteAccountFetcher::fetch_and_normalize` therefore does not, as written,
// satisfy this port's `Send` requirement. Providing the real production
// implementation (adapting `RemoteAccountFetcher` behind a `Send`-compatible
// shim, e.g. spawning the fetch onto a task) is left to task 7.2's own
// bootstrap-wiring boundary, alongside `register_status_handlers`'s actual
// call site — this task's own boundary is the handlers and the port
// contract, not federation-core's `FederationHttpClient` signature or task
// 7.2's production wiring.

/// Resolves `ctx.signer.actor_uri` (the HTTP-Signature-verified acting
/// remote actor — never the JSON body's own `actor`/`attributedTo`
/// property, see this module's doc comment) to a stable [`Id`] via `R`.
async fn resolve_actor_id<R: RemoteActorResolver>(
    remote_actors: &R,
    ctx: &InboundContext,
) -> Result<Id, AppError> {
    remote_actors
        .resolve_remote_actor(&ctx.signer.actor_uri)
        .await
}

/// Reads `activity.raw`'s top-level object map, if `activity.raw` is a JSON
/// object at all (it always is — [`crate::federation::jsonld::parse_activity`]
/// already guarantees this at the `ParsedActivity` construction boundary).
fn activity_map(activity: &ParsedActivity) -> Option<&Map<String, Value>> {
    activity.raw.as_object()
}

/// Reads a `uri`-shaped property that may appear either as a bare string
/// (`Announce`/`Like`/`Delete`'s own `object`, this crate's own outbound
/// wire shape) or as an embedded object carrying its own `id` (a `Tombstone`-
/// shaped `Delete` object, or any other embedded-object shape a foreign
/// dialect might send) — returns `None` (not an error) for any other shape,
/// letting the caller decide whether that is a safe [`HandleOutcome::Ignored`].
///
/// `pub(crate)` (widened from this module's original private visibility by
/// task 6.2, `Boundary: StatusIngestService`): reused as-is by
/// [`crate::statuses::ingest_service::StatusIngestService::ingest_document`]
/// to read a `Note`'s `attributedTo` property, which carries the identical
/// bare-string-or-embedded-object shape ambiguity as `Announce`/`Like`/
/// `Delete`'s `object` property.
pub(crate) fn object_reference_uri(value: Option<&Value>) -> Option<&str> {
    match value {
        Some(Value::String(uri)) => Some(uri.as_str()),
        Some(Value::Object(map)) => map.get("id").and_then(Value::as_str),
        _ => None,
    }
}

/// Reads `key` off `map` as a plain string, or `None` if absent/not a string
/// — mirrors `remote_fetcher.rs::optional_string`'s identical precedent for
/// the identical class of genuinely-optional wire property.
fn optional_string(map: &Map<String, Value>, key: &str) -> Option<String> {
    map.get(key).and_then(Value::as_str).map(str::to_string)
}

/// Reads `key` off `map` as a JSON array of strings, tolerating a bare single
/// string too (`to`/`cc` are conventionally arrays, but a JSON-LD document
/// compacted from a single-element array can legally collapse to a bare
/// string) — returns an empty `Vec` for anything else, never an error (`to`/
/// `cc` absence is a normal, tolerated shape here, not a validation failure).
fn string_array_prop(map: &Map<String, Value>, key: &str) -> Vec<String> {
    match map.get(key) {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

/// Parses an RFC 3339 timestamp string, or `None` if `s` is not one — never
/// an error (a missing/malformed `published`/`updated` property falls back
/// to the caller's own current-time read rather than rejecting an otherwise
/// well-formed inbound Activity over a single optional timestamp).
fn parse_rfc3339(s: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(s, &Rfc3339).ok()
}

/// Derives a [`Visibility`] from an inbound object's `to`/`cc` properties.
/// See this module's doc comment ("Visibility derivation from `to`/`cc`")
/// for the full rationale.
fn derive_inbound_visibility(object: &Map<String, Value>) -> Visibility {
    let to = string_array_prop(object, "to");
    let cc = string_array_prop(object, "cc");

    if to.iter().any(|uri| uri == PUBLIC_COLLECTION_URI) {
        Visibility::Public
    } else if cc.iter().any(|uri| uri == PUBLIC_COLLECTION_URI) {
        Visibility::Unlisted
    } else if to
        .iter()
        .chain(cc.iter())
        .any(|uri| uri.ends_with("/followers"))
    {
        Visibility::Private
    } else {
        Visibility::Direct
    }
}

fn malformed(message: impl Into<String>) -> AppError {
    AppError::client(StatusCode::UNPROCESSABLE_ENTITY, message.into())
}

fn forbidden(message: impl Into<String>) -> AppError {
    AppError::client(StatusCode::FORBIDDEN, message.into())
}

/// Persists every hashtag [`extract_content_tokens`] finds in `content`,
/// associated to `status_id` — the same two repository calls
/// (`tag_repository::upsert_tag`/`associate_tag`)
/// `status_service.rs::persist_tags` already makes for local-origin posts
/// (Requirement 14.5's "共通コードパス" extended to hashtag persistence).
async fn persist_tags(
    pool: &PgPool,
    runtime: &RuntimeContext,
    status_id: Id,
    content: &str,
    now: OffsetDateTime,
) -> Result<(), AppError> {
    let extracted = extract_content_tokens(content);
    for name in &extracted.hashtags {
        let tag = tag_repository::upsert_tag(
            pool,
            &crate::statuses::model::Tag {
                id: runtime.ids.next_id(),
                name: name.clone(),
                created_at: now,
            },
        )
        .await?;
        tag_repository::associate_tag(pool, status_id, tag.id).await?;
    }
    Ok(())
}

/// Shared dependencies every handler [`register_status_handlers`] builds
/// needs: the repository connection pool, the id/clock injection boundary,
/// and the [`RemoteActorResolver`] port. Bundled into one struct (design.md's
/// exact `register_status_handlers(dispatcher, deps: StatusInboundDeps)`
/// signature) so a future caller (task 7.2's bootstrap wiring) passes one
/// value rather than three positional parameters.
pub struct StatusInboundDeps<R: RemoteActorResolver> {
    pub pool: PgPool,
    pub runtime: RuntimeContext,
    pub remote_actors: Arc<R>,
}

impl<R: RemoteActorResolver> Clone for StatusInboundDeps<R> {
    fn clone(&self) -> Self {
        Self {
            pool: self.pool.clone(),
            runtime: self.runtime.clone(),
            remote_actors: Arc::clone(&self.remote_actors),
        }
    }
}

// ---------------------------------------------------------------------------
// CreateNoteHandler
// ---------------------------------------------------------------------------

/// Normalizes an already-identified ActivityPub `Note` `object` map into a
/// local [`Status`] row and persists it (Requirement 14.2), given the
/// already-resolved acting `actor_id`. This is the single "Note ingestion"
/// code path both [`CreateNoteHandler`] (the inbound `Create(Note)` dispatch
/// handler, below) and
/// [`crate::statuses::ingest_service::StatusIngestService`] (task 6.2, an
/// out-of-dispatch entry point reusing this exact function per its own
/// `_Depends: 6.1_`) call — neither reimplements a second, parallel
/// normalization, satisfying Requirement 14.5's "共通コードパス" discipline
/// across both entry points (task 6.2's own observable-completion criterion,
/// "受信ハンドラ経路と同一結果になる").
///
/// Idempotent: if `object`'s `id` already names an ingested [`Status`], that
/// existing row is returned unchanged (no re-insert, no re-`persist_tags`) —
/// see this module's doc comment ("Idempotent re-delivery"). Fails with a
/// `422 Unprocessable Entity` [`AppError`] if `object` carries no `id`
/// property at all.
pub(crate) async fn ingest_note_object(
    pool: &PgPool,
    runtime: &RuntimeContext,
    object: &Map<String, Value>,
    actor_id: Id,
) -> Result<Status, AppError> {
    let Some(object_uri) = object.get("id").and_then(Value::as_str) else {
        return Err(malformed("Note object is missing a required 'id' property"));
    };

    if let Some(existing) = status_repository::find_by_uri(pool, object_uri).await? {
        // Already ingested (a harmless re-delivery) — see this module's doc
        // comment, "Idempotent re-delivery".
        return Ok(existing);
    }

    let content = optional_string(object, "content").unwrap_or_default();
    let sensitive = object
        .get("sensitive")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let spoiler_text = optional_string(object, "summary").unwrap_or_default();
    let url = optional_string(object, "url").unwrap_or_else(|| object_uri.to_string());
    let published = optional_string(object, "published")
        .as_deref()
        .and_then(parse_rfc3339);

    let (in_reply_to_id, in_reply_to_account_id) = match optional_string(object, "inReplyTo") {
        Some(parent_uri) => match status_repository::find_by_uri(pool, &parent_uri).await? {
            Some(parent) => (Some(parent.id), Some(parent.actor_id)),
            None => (None, None),
        },
        None => (None, None),
    };

    let visibility = derive_inbound_visibility(object);

    let id = runtime.ids.next_id();
    let now = runtime.clock.now();
    let created_at = published.unwrap_or(now);

    let status = Status {
        id,
        actor_id,
        uri: object_uri.to_string(),
        url: Some(url),
        content,
        visibility,
        sensitive,
        spoiler_text,
        in_reply_to_id,
        in_reply_to_account_id,
        reblog_of_id: None,
        poll_id: None,
        language: None,
        reblogs_count: 0,
        favourites_count: 0,
        replies_count: 0,
        local: false,
        created_at,
        edited_at: None,
    };

    status_repository::insert_status(pool, &status).await?;

    if let Some(parent_id) = in_reply_to_id {
        status_repository::adjust_counts(pool, parent_id, CountKind::Replies, 1).await?;
    }

    persist_tags(pool, runtime, status.id, &status.content, now).await?;

    Ok(status)
}

/// Ingests an inbound `Create(Note)` as a remote [`Status`] (Requirements
/// 14.1, 14.2), or — when the wire shape matches a poll vote (Requirement
/// 13.6) — branches into [`poll_repository::record_vote`] instead. See this
/// module's doc comment for the full ingestion/vote-detection contract.
pub struct CreateNoteHandler<R: RemoteActorResolver> {
    pool: PgPool,
    runtime: RuntimeContext,
    remote_actors: Arc<R>,
}

impl<R: RemoteActorResolver> CreateNoteHandler<R> {
    pub fn new(pool: PgPool, runtime: RuntimeContext, remote_actors: Arc<R>) -> Self {
        Self {
            pool,
            runtime,
            remote_actors,
        }
    }

    /// Attempts the `Create{Note, name=...}` vote-wire-form branch (Requirement
    /// 13.6). Returns `Ok(Some(Handled))` when every vote-shape condition
    /// held and the vote was recorded; `Ok(None)` when any condition failed
    /// (the caller should fall through to ordinary `Note` ingestion); `Err`
    /// only if `record_vote` itself rejects an otherwise-detected vote
    /// (deadline/range/duplicate — see this module's doc comment,
    /// "Idempotent re-delivery").
    async fn try_record_vote(
        &self,
        object: &Map<String, Value>,
        actor_id: Id,
    ) -> Result<Option<HandleOutcome>, AppError> {
        let Some(name) = optional_string(object, "name") else {
            return Ok(None);
        };
        let Some(in_reply_to_uri) = optional_string(object, "inReplyTo") else {
            return Ok(None);
        };
        let Some(target) = status_repository::find_by_uri(&self.pool, &in_reply_to_uri).await?
        else {
            return Ok(None);
        };
        if !target.local {
            return Ok(None);
        }
        let Some(poll_id) = target.poll_id else {
            return Ok(None);
        };
        let tally = poll_repository::tally(&self.pool, poll_id, None).await?;
        let Some(option) = tally.options.iter().find(|option| option.title == name) else {
            return Ok(None);
        };

        let now = self.runtime.clock.now();
        poll_repository::record_vote(&self.pool, poll_id, actor_id, &[option.idx], now).await?;
        Ok(Some(HandleOutcome::Handled))
    }
}

impl<R: RemoteActorResolver> InboundActivityHandler for CreateNoteHandler<R> {
    fn activity_types(&self) -> &[&str] {
        &["Create"]
    }

    fn handle<'a>(
        &'a self,
        activity: &'a ParsedActivity,
        ctx: &'a InboundContext,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<HandleOutcome, AppError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let Some(top) = activity_map(activity) else {
                return Ok(HandleOutcome::Ignored);
            };
            let Some(Value::Object(object)) = top.get("object") else {
                return Ok(HandleOutcome::Ignored);
            };
            if object.get("type").and_then(Value::as_str) != Some("Note") {
                return Ok(HandleOutcome::Ignored);
            }

            let actor_id = resolve_actor_id(&*self.remote_actors, ctx).await?;

            if let Some(outcome) = self.try_record_vote(object, actor_id).await? {
                return Ok(outcome);
            }

            // Delegates to the shared Note-ingestion code path also called
            // by `StatusIngestService` — see [`ingest_note_object`]'s own
            // doc comment for why (Requirement 14.5's "共通コードパス",
            // task 6.2's "受信ハンドラ経路と同一結果になる").
            ingest_note_object(&self.pool, &self.runtime, object, actor_id).await?;

            Ok(HandleOutcome::Handled)
        })
    }
}

// ---------------------------------------------------------------------------
// AnnounceHandler
// ---------------------------------------------------------------------------

/// Records an inbound `Announce` (a remote boost of a **local** post) as a
/// reblog row and increments the target's `reblogs_count` (Requirements
/// 14.1, 14.3).
pub struct AnnounceHandler<R: RemoteActorResolver> {
    pool: PgPool,
    runtime: RuntimeContext,
    remote_actors: Arc<R>,
}

impl<R: RemoteActorResolver> AnnounceHandler<R> {
    pub fn new(pool: PgPool, runtime: RuntimeContext, remote_actors: Arc<R>) -> Self {
        Self {
            pool,
            runtime,
            remote_actors,
        }
    }
}

impl<R: RemoteActorResolver> InboundActivityHandler for AnnounceHandler<R> {
    fn activity_types(&self) -> &[&str] {
        &["Announce"]
    }

    fn handle<'a>(
        &'a self,
        activity: &'a ParsedActivity,
        ctx: &'a InboundContext,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<HandleOutcome, AppError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let Some(top) = activity_map(activity) else {
                return Ok(HandleOutcome::Ignored);
            };
            let Some(object_uri) = object_reference_uri(top.get("object")) else {
                return Ok(HandleOutcome::Ignored);
            };

            let Some(target) = status_repository::find_by_uri(&self.pool, object_uri).await? else {
                return Ok(HandleOutcome::Ignored);
            };
            if !target.local {
                return Ok(HandleOutcome::Ignored);
            }

            let actor_id = resolve_actor_id(&*self.remote_actors, ctx).await?;

            if interaction_repository::find_reblog(&self.pool, actor_id, target.id)
                .await?
                .is_some()
            {
                return Ok(HandleOutcome::Handled);
            }

            let id = self.runtime.ids.next_id();
            let now = self.runtime.clock.now();
            let uri = activity.id.clone();

            let reblog = Status {
                id,
                actor_id,
                uri,
                url: None,
                content: String::new(),
                visibility: target.visibility,
                sensitive: false,
                spoiler_text: String::new(),
                in_reply_to_id: None,
                in_reply_to_account_id: None,
                reblog_of_id: Some(target.id),
                poll_id: None,
                language: None,
                reblogs_count: 0,
                favourites_count: 0,
                replies_count: 0,
                local: false,
                created_at: now,
                edited_at: None,
            };

            status_repository::insert_status(&self.pool, &reblog).await?;
            status_repository::adjust_counts(&self.pool, target.id, CountKind::Reblogs, 1).await?;

            Ok(HandleOutcome::Handled)
        })
    }
}

// ---------------------------------------------------------------------------
// LikeHandler
// ---------------------------------------------------------------------------

/// Records an inbound `Like` (a remote favourite of a **local** post) and
/// increments the target's `favourites_count` (Requirements 14.1, 14.3).
pub struct LikeHandler<R: RemoteActorResolver> {
    pool: PgPool,
    runtime: RuntimeContext,
    remote_actors: Arc<R>,
}

impl<R: RemoteActorResolver> LikeHandler<R> {
    pub fn new(pool: PgPool, runtime: RuntimeContext, remote_actors: Arc<R>) -> Self {
        Self {
            pool,
            runtime,
            remote_actors,
        }
    }
}

impl<R: RemoteActorResolver> InboundActivityHandler for LikeHandler<R> {
    fn activity_types(&self) -> &[&str] {
        &["Like"]
    }

    fn handle<'a>(
        &'a self,
        activity: &'a ParsedActivity,
        ctx: &'a InboundContext,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<HandleOutcome, AppError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let Some(top) = activity_map(activity) else {
                return Ok(HandleOutcome::Ignored);
            };
            let Some(object_uri) = object_reference_uri(top.get("object")) else {
                return Ok(HandleOutcome::Ignored);
            };

            let Some(target) = status_repository::find_by_uri(&self.pool, object_uri).await? else {
                return Ok(HandleOutcome::Ignored);
            };
            if !target.local {
                return Ok(HandleOutcome::Ignored);
            }

            let actor_id = resolve_actor_id(&*self.remote_actors, ctx).await?;

            let now = self.runtime.clock.now();
            let is_new =
                interaction_repository::add_favourite(&self.pool, actor_id, target.id, now).await?;
            if is_new {
                status_repository::adjust_counts(&self.pool, target.id, CountKind::Favourites, 1)
                    .await?;
            }

            Ok(HandleOutcome::Handled)
        })
    }
}

// ---------------------------------------------------------------------------
// DeleteHandler
// ---------------------------------------------------------------------------

/// Applies an inbound `Delete` to a status this instance ingested from a
/// **remote** origin (Requirements 14.1, 14.4). See this module's doc
/// comment for the `local`-ness gate and actor-ownership check.
pub struct DeleteHandler<R: RemoteActorResolver> {
    pool: PgPool,
    remote_actors: Arc<R>,
}

impl<R: RemoteActorResolver> DeleteHandler<R> {
    pub fn new(pool: PgPool, remote_actors: Arc<R>) -> Self {
        Self {
            pool,
            remote_actors,
        }
    }
}

impl<R: RemoteActorResolver> InboundActivityHandler for DeleteHandler<R> {
    fn activity_types(&self) -> &[&str] {
        &["Delete"]
    }

    fn handle<'a>(
        &'a self,
        activity: &'a ParsedActivity,
        ctx: &'a InboundContext,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<HandleOutcome, AppError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let Some(top) = activity_map(activity) else {
                return Ok(HandleOutcome::Ignored);
            };
            let Some(object_uri) = object_reference_uri(top.get("object")) else {
                return Ok(HandleOutcome::Ignored);
            };

            let Some(target) = status_repository::find_by_uri(&self.pool, object_uri).await? else {
                return Ok(HandleOutcome::Ignored);
            };
            if target.local {
                return Ok(HandleOutcome::Ignored);
            }

            let actor_id = resolve_actor_id(&*self.remote_actors, ctx).await?;
            if actor_id != target.actor_id {
                return Err(forbidden(
                    "the signed actor does not own the status named by this Delete",
                ));
            }

            status_repository::delete_status(&self.pool, target.id).await?;

            Ok(HandleOutcome::Handled)
        })
    }
}

// ---------------------------------------------------------------------------
// UpdateHandler
// ---------------------------------------------------------------------------

/// Applies an inbound `Update` to a status this instance ingested from a
/// **remote** origin (Requirements 14.1, 14.4). See this module's doc
/// comment for the `local`-ness gate and actor-ownership check.
pub struct UpdateHandler<R: RemoteActorResolver> {
    pool: PgPool,
    runtime: RuntimeContext,
    remote_actors: Arc<R>,
}

impl<R: RemoteActorResolver> UpdateHandler<R> {
    pub fn new(pool: PgPool, runtime: RuntimeContext, remote_actors: Arc<R>) -> Self {
        Self {
            pool,
            runtime,
            remote_actors,
        }
    }
}

impl<R: RemoteActorResolver> InboundActivityHandler for UpdateHandler<R> {
    fn activity_types(&self) -> &[&str] {
        &["Update"]
    }

    fn handle<'a>(
        &'a self,
        activity: &'a ParsedActivity,
        ctx: &'a InboundContext,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<HandleOutcome, AppError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let Some(top) = activity_map(activity) else {
                return Ok(HandleOutcome::Ignored);
            };
            let Some(Value::Object(object)) = top.get("object") else {
                return Ok(HandleOutcome::Ignored);
            };
            if object.get("type").and_then(Value::as_str) != Some("Note") {
                return Ok(HandleOutcome::Ignored);
            }
            let Some(object_uri) = object.get("id").and_then(Value::as_str) else {
                return Err(malformed(
                    "Update(Note) object is missing a required 'id' property",
                ));
            };

            let Some(target) = status_repository::find_by_uri(&self.pool, object_uri).await? else {
                return Ok(HandleOutcome::Ignored);
            };
            if target.local {
                return Ok(HandleOutcome::Ignored);
            }

            let actor_id = resolve_actor_id(&*self.remote_actors, ctx).await?;
            if actor_id != target.actor_id {
                return Err(forbidden(
                    "the signed actor does not own the status named by this Update",
                ));
            }

            let content = optional_string(object, "content").unwrap_or_default();
            let sensitive = object
                .get("sensitive")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let spoiler_text = optional_string(object, "summary").unwrap_or_default();

            let now = self.runtime.clock.now();
            let edit = StatusEdit {
                id: self.runtime.ids.next_id(),
                status_id: target.id,
                content,
                spoiler_text,
                sensitive,
                created_at: now,
            };
            status_repository::apply_edit(&self.pool, target.id, &edit, now).await?;

            Ok(HandleOutcome::Handled)
        })
    }
}

// ---------------------------------------------------------------------------
// UndoHandler
// ---------------------------------------------------------------------------

/// Reverts an inbound `Undo(Announce)`/`Undo(Like)` against a **local**
/// target (Requirements 14.1, 14.3). Returns [`HandleOutcome::Ignored`] for
/// any other inner object type (`Follow`/`Block`/...), so social-graph's own
/// `Undo` handler — registered for the same outer `"Undo"` type — is
/// unaffected (see `dispatcher.rs`'s own doc comment, "Multimap, not
/// one-handler-per-type").
pub struct UndoHandler<R: RemoteActorResolver> {
    pool: PgPool,
    remote_actors: Arc<R>,
}

impl<R: RemoteActorResolver> UndoHandler<R> {
    pub fn new(pool: PgPool, remote_actors: Arc<R>) -> Self {
        Self {
            pool,
            remote_actors,
        }
    }
}

impl<R: RemoteActorResolver> InboundActivityHandler for UndoHandler<R> {
    fn activity_types(&self) -> &[&str] {
        &["Undo"]
    }

    fn handle<'a>(
        &'a self,
        activity: &'a ParsedActivity,
        ctx: &'a InboundContext,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<HandleOutcome, AppError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let Some(top) = activity_map(activity) else {
                return Ok(HandleOutcome::Ignored);
            };
            let Some(Value::Object(inner)) = top.get("object") else {
                return Ok(HandleOutcome::Ignored);
            };
            let inner_type = inner.get("type").and_then(Value::as_str);

            let is_announce = match inner_type {
                Some("Announce") => true,
                Some("Like") => false,
                _ => return Ok(HandleOutcome::Ignored),
            };

            let Some(object_uri) = object_reference_uri(inner.get("object")) else {
                return Err(malformed(
                    "Undo's inner Activity is missing a required 'object' reference",
                ));
            };

            let Some(target) = status_repository::find_by_uri(&self.pool, object_uri).await? else {
                // Unknown target: nothing to undo — a safe no-op success
                // (this handler does own Undo(Announce)/Undo(Like)
                // semantics; there is simply no matching state to revert).
                return Ok(HandleOutcome::Handled);
            };
            if !target.local {
                return Ok(HandleOutcome::Handled);
            }

            let actor_id = resolve_actor_id(&*self.remote_actors, ctx).await?;

            if is_announce {
                let Some(reblog) =
                    interaction_repository::find_reblog(&self.pool, actor_id, target.id).await?
                else {
                    return Ok(HandleOutcome::Handled);
                };
                status_repository::delete_status(&self.pool, reblog.id).await?;
                status_repository::adjust_counts(&self.pool, target.id, CountKind::Reblogs, -1)
                    .await?;
            } else {
                let removed =
                    interaction_repository::remove_favourite(&self.pool, actor_id, target.id)
                        .await?;
                if removed {
                    status_repository::adjust_counts(
                        &self.pool,
                        target.id,
                        CountKind::Favourites,
                        -1,
                    )
                    .await?;
                }
            }

            Ok(HandleOutcome::Handled)
        })
    }
}

// ---------------------------------------------------------------------------
// register_status_handlers
// ---------------------------------------------------------------------------

/// Registers all six post-related inbound handlers against `dispatcher`
/// (design.md's exact `register_status_handlers` Service Interface;
/// Requirement 14.1). `deps` is cloned once per handler (`PgPool`/
/// `RuntimeContext` are cheap-clone handles; `Arc<R>` is a pointer clone) —
/// this function itself never touches the database or network.
pub fn register_status_handlers<R: RemoteActorResolver + 'static>(
    dispatcher: &mut InboundActivityDispatcher,
    deps: StatusInboundDeps<R>,
) {
    dispatcher.register(Arc::new(CreateNoteHandler::new(
        deps.pool.clone(),
        deps.runtime.clone(),
        Arc::clone(&deps.remote_actors),
    )));
    dispatcher.register(Arc::new(AnnounceHandler::new(
        deps.pool.clone(),
        deps.runtime.clone(),
        Arc::clone(&deps.remote_actors),
    )));
    dispatcher.register(Arc::new(LikeHandler::new(
        deps.pool.clone(),
        deps.runtime.clone(),
        Arc::clone(&deps.remote_actors),
    )));
    dispatcher.register(Arc::new(DeleteHandler::new(
        deps.pool.clone(),
        Arc::clone(&deps.remote_actors),
    )));
    dispatcher.register(Arc::new(UpdateHandler::new(
        deps.pool.clone(),
        deps.runtime.clone(),
        Arc::clone(&deps.remote_actors),
    )));
    dispatcher.register(Arc::new(UndoHandler::new(
        deps.pool.clone(),
        Arc::clone(&deps.remote_actors),
    )));
}
