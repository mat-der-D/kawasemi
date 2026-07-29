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
//! ## Idempotent re-delivery, `Create(Note)` and vote branch alike (revised
//! by task 8.1's integration tests — see below)
//! [`CreateNoteHandler`]'s ordinary (non-vote) ingestion path checks
//! [`status_repository::find_by_uri`] first and returns
//! [`HandleOutcome::Handled`] with no further action if a `Status` under that
//! `uri` already exists (a safe no-op for a redelivered `Create`, avoiding a
//! `statuses_uri_key` unique-violation `409` on a harmless re-delivery).
//! `Announce`/`Like`'s own repository calls
//! ([`interaction_repository::find_reblog`]/[`interaction_repository::add_favourite`]'s
//! `ON CONFLICT DO NOTHING`) are already naturally idempotent the same way.
//!
//! The vote branch ([`CreateNoteHandler::try_record_vote`]) was originally
//! left as a documented exception here — a redelivered vote `Create` would
//! call [`poll_repository::record_vote`] a second time, rejecting it as a
//! duplicate vote (`422`), and that `AppError` was allowed to propagate
//! unchanged, on the theory that a literal re-delivery of the identical
//! vote Activity is rare. Task 8.1's own integration tests
//! (`tests/polls_it.rs`) found this was not merely a rare-redelivery risk
//! but a **deterministic, always-reproducible failure** for the single most
//! common case: an actor voting on a poll whose owning `Status` is authored
//! by a **local** actor. `PollService::vote` records the vote directly
//! (`poll_repository::record_vote`), then calls
//! `StatusActivityBuilder::deliver_vote` to notify the poll's author; when
//! that author is local, `DeliveryService::deliver`'s local-recipient path
//! (federation-core, task 4.1/5.3's synchronous, in-process delivery
//! architecture — out of this spec's boundary to change) dispatches the
//! notification `Create{Note,name=...}` back to this exact dispatcher
//! *within the same request*, landing here a moment after the direct call
//! already wrote the identical `poll_votes` row. `record_vote`'s duplicate
//! check correctly reports "already voted" — but that `AppError`, left to
//! propagate, then bubbles all the way back up through
//! `local_sink.dispatch(...).await?` inside `deliver()`, through
//! `deliver_vote`, into `PollService::vote`'s own `self.activity_builder
//! .deliver_vote(...).await?`, turning an already fully-recorded,
//! successful vote into a spurious `422` returned to the voter who just
//! made it — a direct violation of Requirement 13.2 ("有効な選択肢で投票し
//! たとき...投票を記録し、更新後の集計を反映した Poll を返す") for the
//! common local-poll case, not an edge case.
//!
//! [`CreateNoteHandler::try_record_vote`] therefore now catches
//! specifically `record_vote`'s "actor has already voted in this poll"
//! rejection (matched on `poll_repository.rs`'s own literal
//! `AppError::client` message/status, the only signal currently available
//! without widening that already-reviewed function's return type) and
//! reports [`HandleOutcome::Handled`] instead of propagating it — the same
//! outcome a genuine wire-level re-delivery of the identical vote Activity
//! gets. This does not weaken Requirement 13.5: the transactional
//! uniqueness `record_vote` itself enforces (task 2.3's `FOR UPDATE`-locked
//! check) is completely unchanged — a second, truly independent vote
//! attempt (a different actor, or a genuine duplicate *client* request via
//! `PollService::vote`, Requirement 13.5's own literal target) is still
//! rejected exactly as before. Only this *inbound-dispatch* branch's
//! handling of an already-true "this actor already voted" fact changes,
//! from "propagate as an error" to "report as already-applied" — which is
//! also the textbook-correct idempotent-inbox-handler behavior for every
//! *other* genuinely-rare re-delivery this section already covers (two
//! *different* vote Activities naming the same option, redelivered under
//! different Activity ids, remain covered by this same fix). Every *other*
//! `record_vote` rejection (deadline passed, out-of-range choice,
//! single/multiple violation) is a distinct wire condition this fix does
//! not touch, and continues to propagate unchanged.
//!
//! ## Notification emit (task 10.2, Requirements 9.1, 9.2, 10.1) — closes the
//! known gap `interaction_service.rs`'/`status_service.rs`' own task-9.2 doc
//! comments flagged ("リモートアクター起点の reblog/favourite/mention...は
//! emit されず...ローカル/リモート対称配信を満たしていない")
//! [`AnnounceHandler`]/[`LikeHandler`]/[`CreateNoteHandler`] each emit to the
//! *same* [`crate::statuses::notification_sink::NotificationSinkRegistry`]
//! instance `InteractionService`/`StatusService` already hold (task 9.2) —
//! not a second, independent registry — via `StatusInboundDeps::notifications`,
//! threaded through `register_downstream_handlers`
//! (`crate::statuses::register_downstream_handlers`) from the *same*
//! `NotificationSinkRegistry` `crate::statuses::build_statuses_module` builds
//! (`src/bootstrap.rs`/`src/test_harness.rs` construct it once, before
//! `federation::build_federation_module` runs, and pass a clone to each), so
//! a single future `set_sink` call reaches every emit site — local- and
//! remote-origin alike — at once, exactly as task 9.2's own doc comment
//! promises for `StatusesModule::notification_sink_registry`.
//!
//! Each handler emits only on the branch that records a genuinely *new*
//! state transition — mirroring task 9.2's own placement discipline exactly:
//! - [`AnnounceHandler`]: only past its own `find_reblog`-miss branch (the
//!   same duplicate-Announce guard that already exists for idempotent
//!   re-delivery — see this module's doc comment, "Idempotent re-delivery").
//! - [`LikeHandler`]: only when `interaction_repository::add_favourite`
//!   reports `is_new` (its own `ON CONFLICT DO NOTHING` already makes a
//!   duplicate `Like` naturally idempotent).
//! - [`CreateNoteHandler`]: only when [`ingest_note_object`]'s own
//!   `find_by_uri` pre-check (this task's own addition, mirroring
//!   [`AnnounceHandler`]'s explicit pre-check pattern rather than widening
//!   [`ingest_note_object`]'s return type — that function is shared with
//!   `StatusIngestService`, task 6.2's boundary, which this task does not
//!   touch) finds no existing `Status` under that `uri` — i.e. a genuinely
//!   new ingestion, not a redelivery. Mentions are resolved from the
//!   ingested `Note`'s `content` via [`extract_content_tokens`]/
//!   [`crate::statuses::status_service::ExtractedTokens::mentions`] (widened
//!   from private to `pub(crate)` by this task — see that module's own doc
//!   comment) through [`LocalMentionResolver`] (a `Send`-bound-future wrapper
//!   around [`crate::statuses::status_service::MentionLookup`] — see that
//!   trait's own doc comment for why it exists instead of a direct
//!   `MentionLookup` bound), filtered to this instance's own configured
//!   `domain` (a bare `@handle` or `@handle@{own domain}`) exactly like
//!   `status_service.rs::build_addressing`'s own local-origin mention loop —
//!   see that module's doc comment, "Mention resolution: local only" —
//!   applied symmetrically here for the inbound
//!   direction.
//!
//! **Recipient/origin tagging**: `AnnounceHandler`/`LikeHandler` only ever
//! reach their emit branch when `target.local` (this handler's own existing
//! gate), so `recipient: AccountRef::Local(target.actor_id)` needs no extra
//! resolution call (unlike `InteractionService::reblog`'s own
//! `account_ref_for_notification` fallback, which exists only because
//! *that* call site does not already know the target is local).
//! `origin: AccountRef::Remote(actor_id)` in all three handlers: `actor_id`
//! comes from [`resolve_actor_id`]/[`RemoteActorResolver`], and every branch
//! that reaches an emit call is, by the "genuinely new state transition"
//! discipline just above, an interaction *not already recorded* by
//! `InteractionService`/`StatusService` — the only two call sites that ever
//! write these rows for a local actor's own action — so despite
//! `ProdRemoteActorResolver`'s documented local-actor loopback shortcut (its
//! own doc comment, "Local-actor shortcut": a local-to-local in-process
//! delivery can resolve `actor_id` to a genuine `local_actors.id`), an
//! emit-reachable branch here can only be a genuinely first-recorded, remote-
//! origin interaction: a local actor's own reblog/favourite/mentioning-post
//! is always inserted by `InteractionService`/`StatusService` *before* that
//! same call delivers the Activity (see each's own code — insert then
//! deliver), so any in-process loopback of that exact interaction always
//! lands on this module's already-recorded branch (duplicate `find_reblog`
//! hit, `is_new == false`, or `already_ingested == true`) and never reaches
//! the emit call.
//!
//! **Self-interaction skip (defensive, currently unreachable — kept for
//! symmetry)**: each handler additionally skips emit when the resolved
//! recipient/mentioned actor equals `actor_id` — mirroring
//! `InteractionService::reblog`/`favourite`'s and `StatusService::create_status`'s
//! own self-interaction skip verbatim. Per the previous paragraph's own
//! reasoning this condition cannot currently be reached (an emit-reachable
//! branch is never a local actor's own already-recorded interaction), but
//! it costs one cheap `Id` comparison and guards against a future change to
//! that invariant (e.g. a new entry point that writes these rows without
//! going through `InteractionService`/`StatusService` first) — the same
//! "cheap, defensive, documented" judgment call this task's own brief asks
//! for.
//!
//! ## Attachment/mention reflection (task 10.3, closes the gap task 6.1's own
//! doc comment flagged here — "要件14.2の未充足解消")
//! Task 6.1 originally left an ingested remote `Note`'s `attachment`/`tag`
//! (`Mention`) properties entirely unread, for two structural reasons this
//! section previously documented in full: (1) `status_media.media_id`
//! (`migrations/0007_statuses.sql`) is a logical FK to media-pipeline's own
//! `media.id`, and no capability anywhere in `src/media/*` can create a
//! `Media` row from a remote URL (`MediaService::accept_upload` takes raw
//! uploaded bytes, not a URL to fetch — building that would be new
//! media-pipeline capability, out of this spec's boundary per design.md's Out
//! of Boundary note); (2) `Status`/`migrations/0007_statuses.sql` carried no
//! mentions/addressee table at all.
//!
//! Task 10.3 closes both gaps **within this task's own boundary**
//! (`InboundHandlers`, `StatusIngestService`), deliberately *not* by building
//! new media-pipeline capability:
//! - **Attachments**: [`extract_attachments`] reads an ingested `Note`'s
//!   `attachment` property and [`ingest_note_object`] persists each entry as
//!   lightweight, statuses-core-owned metadata (`url`/`media_type`/
//!   `description`) via [`status_repository::insert_remote_attachments`], a
//!   brand-new table (`status_remote_attachments`,
//!   `migrations/0011_status_mentions_and_remote_attachments.sql`, added by
//!   this task) — **not** a real media-pipeline `Media` row, and **not** a
//!   `status_media` row (that table's `media_id` stays a logical FK to a real
//!   `media.id` this task never fabricates). This deliberately mirrors this
//!   crate's own already-implemented precedent for the identical class of
//!   problem: [`crate::accounts::remote_fetcher::RemoteAccountFetcher::
//!   fetch_and_normalize`] reflects a remote actor's avatar/header image as a
//!   plain URL string (`account_profiles.avatar_url`/`header_url`), never as
//!   a full local `Media` row, for the same reason (no capability to
//!   fetch-and-own a remote media object exists outside media-pipeline's own
//!   upload boundary). A future task that gives media-pipeline a genuine
//!   "create a `Media` row from a remote URL" capability could migrate
//!   `status_remote_attachments` rows into real `status_media` rows: that
//!   migration is out of this task's scope, not attempted here.
//! - **Mentions**: [`extract_tag_mentions`] reads an ingested `Note`'s
//!   `tag`-array `Mention` entries (the authoritative ActivityPub mention
//!   source — distinct from the *content*-text `@handle` scanning
//!   [`resolve_local_mentions`]/[`extract_content_tokens`] already used for
//!   task 10.2's notification emit, see that function's own doc comment for
//!   why this task does not unify the two: they serve genuinely different
//!   purposes from genuinely different wire sources, changing task 10.2's
//!   already-reviewed notification behavior is out of this task's scope).
//!   Both extraction paths funnel into the *same* resolution primitive,
//!   [`resolve_mentions`] (the domain-filter + [`Handle`]-parse +
//!   [`LocalMentionResolver::resolve_local_mention`] call task 10.2's
//!   [`resolve_local_mentions`] already established) — this task does not
//!   fork a second resolution algorithm, only a second *extraction* source.
//!   Only mentions that resolve to a **local** actor are persisted (this
//!   crate's single, already-established "mention resolution: local only"
//!   convention, `status_service.rs`'s own doc comment of that exact title,
//!   applied symmetrically here), via the new `status_mentions` table (same
//!   migration as above) and
//!   [`status_repository::insert_mentions`]/[`status_repository::mentioned_actor_ids`].
//!   A remote-domain mention (naming neither no domain nor this instance's
//!   own `domain`) is left unresolved and therefore unpersisted, matching
//!   Requirement 15.2's "未知の方言プロパティ...を意味論として解釈せずコア
//!   処理を継続する" applied to a mention this instance has no local identity
//!   for.
//!
//! Both new tables share `ingest_note_object`'s existing idempotency guard
//! (its own `find_by_uri` pre-check, "Idempotent re-delivery" above): a
//! redelivered `Create(Note)` returns the already-persisted [`Status`]
//! without a second `insert_mentions`/`insert_remote_attachments` call, the
//! same discipline `persist_tags` already follows.
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

use crate::actor::Handle;
use crate::domain::{AccountRef, Id, Visibility};
use crate::error::AppError;
use crate::federation::inbound::dispatcher::{
    HandleOutcome, InboundActivityDispatcher, InboundActivityHandler, InboundContext,
};
use crate::federation::jsonld::ParsedActivity;
use crate::runtime::RuntimeContext;
use crate::statuses::addressing::PUBLIC_COLLECTION_URI;
use crate::statuses::interaction_repository;
use crate::statuses::model::{Status, StatusEdit};
use crate::statuses::notification_sink::{
    NotificationEvent, NotificationSinkRegistry, NotificationType,
};
use crate::statuses::poll_repository;
use crate::statuses::status_repository::{self, CountKind, RemoteAttachment};
use crate::statuses::status_service::{Mention, MentionLookup, extract_content_tokens};
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

/// The narrow `Handle -> local actor Id` port [`CreateNoteHandler`] depends
/// on to resolve an extracted mention (task 10.2, Requirements 9.1, 9.2,
/// 10.1). A separate, `Send`-bound-future trait rather than a direct
/// [`CreateNoteHandler`] generic bound on
/// [`crate::statuses::status_service::MentionLookup`] itself — mirrors
/// [`RemoteActorResolver`]'s own identical treatment (see that trait's own
/// doc comment) for the identical reason: `MentionLookup::resolve_local_handle`
/// (task 5.1, `#[allow(async_fn_in_trait)]`) is a plain `async fn` with no
/// `Send` bound, adequate for `StatusService`'s own never-boxed call site but
/// not for this module's `Pin<Box<dyn Future<Output = ..> + Send + 'a>>`
/// handler boundary.
pub trait LocalMentionResolver: Send + Sync {
    /// Resolves `handle` to a registered local actor's [`Id`], if any.
    /// `Ok(None)` (not an error) when no local actor is registered under
    /// `handle` — mirrors `MentionLookup::resolve_local_handle`'s own
    /// identical "no error for absence" contract.
    fn resolve_local_mention(
        &self,
        handle: &Handle,
    ) -> impl std::future::Future<Output = Result<Option<Id>, AppError>> + Send;
}

/// Unlike [`RemoteActorResolver`] (no blanket impl possible — see that
/// trait's own doc comment), [`crate::actor::ActorDirectory`]'s own
/// [`MentionLookup`] implementation (`status_service.rs`) *can* satisfy this
/// port directly: `ActorDirectory` is a concrete, non-generic type (a thin
/// `PgPool` wrapper, `crate::actor::directory`'s own doc comment), so its
/// `resolve_local_handle` body's desugared future is a fully concrete,
/// monomorphized type the compiler can — and does — verify is `Send`
/// (`sqlx::PgPool` queries are `Send`), the same reason
/// `ProdRemoteActorResolver::resolve_remote_actor` can call
/// `ActorDirectory::resolve_actor_by_handle` directly in place without a
/// `tokio::spawn` shim, unlike its own genuinely-generic
/// `RemoteAccountFetcher<H>` fallback immediately below it in this same
/// file. Production wiring (`crate::statuses::register_downstream_handlers`)
/// passes a fresh `ActorDirectory` here.
impl LocalMentionResolver for crate::actor::ActorDirectory {
    fn resolve_local_mention(
        &self,
        handle: &Handle,
    ) -> impl std::future::Future<Output = Result<Option<Id>, AppError>> + Send {
        MentionLookup::resolve_local_handle(self, handle)
    }
}

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

/// Resolves a list of already-extracted [`Mention`]s to registered **local**
/// actor [`Id`]s (task 10.2, Requirement 9.1's local/remote-symmetric emit
/// invariant applied to mentions; widened by task 10.3, Requirement 14.2, to
/// also serve mention *persistence*). Mirrors
/// `status_service.rs::StatusService::build_addressing`'s own mention-
/// resolution loop exactly: a mention is only resolved when it names no
/// domain at all (a bare `@handle`, assumed local by this crate's own
/// established convention) or names `domain` itself (case-insensitively);
/// any other domain is left unresolved (see that module's own doc comment,
/// "Mention resolution: local only"). `Id`s are returned in `extracted`'s own
/// order.
///
/// This is the single shared resolution primitive both
/// [`resolve_local_mentions`] (content-text-derived, task 10.2's notification
/// source) and [`extract_tag_mentions`] (`tag`-array-derived, task 10.3's
/// persistence source) funnel into — task 10.3 does not fork a second
/// resolution algorithm, only a second *extraction* source feeding this same
/// function (see this module's doc comment, "Attachment/mention reflection").
async fn resolve_mentions<M: LocalMentionResolver>(
    mentions: &M,
    domain: &str,
    extracted: &[Mention],
) -> Result<Vec<Id>, AppError> {
    let mut ids = Vec::with_capacity(extracted.len());
    for mention in extracted {
        if let Some(mention_domain) = &mention.domain
            && !mention_domain.eq_ignore_ascii_case(domain)
        {
            continue;
        }
        let Ok(handle) = Handle::new(mention.local.clone()) else {
            continue;
        };
        if let Some(id) = mentions.resolve_local_mention(&handle).await? {
            ids.push(id);
        }
    }
    Ok(ids)
}

/// Resolves `content`'s extracted mentions (via [`extract_content_tokens`])
/// to registered **local** actor [`Id`]s (task 10.2, Requirement 9.1's
/// local/remote-symmetric emit invariant applied to mentions). `Id`s are
/// returned in first-appearance order, already deduplicated by
/// [`extract_content_tokens`]'s own exact-token-text dedup. See
/// [`resolve_mentions`] (this function's shared resolution core) for the
/// domain-filtering/`Handle`-parsing contract.
async fn resolve_local_mentions<M: LocalMentionResolver>(
    mentions: &M,
    domain: &str,
    content: &str,
) -> Result<Vec<Id>, AppError> {
    let extracted = extract_content_tokens(content);
    resolve_mentions(mentions, domain, &extracted.mentions).await
}

/// Extracts `tag`-array `Mention` entries from an inbound `Note` object
/// (task 10.3, Requirement 14.2's "メンションを反映する"): the ActivityStreams
/// convention this crate's own outbound side already emits
/// (`activity_builder.rs`'s `Mention` tag shape, `{"type":"Mention",
/// "href":"...","name":"@user@domain"}`). Reads only `name` (the
/// conventional `@user`/`@user@domain` acct form) — `href` is intentionally
/// not read: resolving a mention to a local actor goes through the identical
/// [`Handle`]-based [`LocalMentionResolver`] port [`resolve_local_mentions`]
/// already uses for the content-derived case (via the shared
/// [`resolve_mentions`]), not a second, URI-keyed resolution path. Tolerant
/// of a missing/malformed `tag` entry (skipped, not an error — `tag` is a
/// foreign wire property this module never trusted to be well-formed,
/// Requirement 15.2). Deduplicated by exact `(local, domain)` pair, in
/// first-appearance order, mirroring [`extract_content_tokens`]'s own
/// dedup discipline for the sibling content-derived extraction.
fn extract_tag_mentions(object: &Map<String, Value>) -> Vec<Mention> {
    let Some(Value::Array(tags)) = object.get("tag") else {
        return Vec::new();
    };
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();
    for tag in tags {
        let Some(tag_obj) = tag.as_object() else {
            continue;
        };
        if tag_obj.get("type").and_then(Value::as_str) != Some("Mention") {
            continue;
        }
        let Some(name) = tag_obj.get("name").and_then(Value::as_str) else {
            continue;
        };
        let trimmed = name.trim_start_matches('@');
        if trimmed.is_empty() {
            continue;
        }
        let (local, domain) = match trimmed.split_once('@') {
            Some((local, domain)) if !local.is_empty() && !domain.is_empty() => {
                (local.to_string(), Some(domain.to_string()))
            }
            _ => (trimmed.to_string(), None),
        };
        let key = format!("{local}@{}", domain.as_deref().unwrap_or(""));
        if seen.insert(key) {
            result.push(Mention { local, domain });
        }
    }
    result
}

/// Reads an attachment entry's own `url`/`href` property, tolerating the
/// same string-or-object-or-array shapes
/// `remote_fetcher.rs::extract_image_url` already tolerates for `icon`/
/// `image` — the identical ActivityStreams property-value ambiguity, applied
/// here to a `Document`/`Image`-typed `attachment` entry's own `url` member
/// (an embedded object's `href` is checked first, then its `url`, since a
/// conventional AS2 `Link`-shaped attachment entry names its target via
/// `href` while a `Document`/`Image`-shaped one names it via `url`).
fn attachment_url(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(url) => Some(url.clone()),
        Value::Object(map) => map
            .get("href")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| attachment_url(map.get("url"))),
        Value::Array(items) => items.iter().find_map(|item| attachment_url(Some(item))),
        _ => None,
    }
}

/// Extracts a remote `Note`'s `attachment` property as
/// [`RemoteAttachment`]s (task 10.3, Requirement 14.2's "添付を反映する") —
/// see this module's doc comment ("Attachment/mention reflection") for why
/// this reads plain metadata rather than fetching/normalizing the referenced
/// media into a real media-pipeline `Media`/`status_media` row. An entry
/// missing a usable `url` is skipped, not an error (Requirement 15.2 applied
/// to a malformed/foreign attachment shape); `mediaType` maps to
/// `RemoteAttachment::media_type`, and `name` (falling back to `summary`) —
/// the conventional ActivityStreams attachment-description property, mirrors
/// `activity_builder.rs`'s own outbound attachment `name`/description
/// convention — maps to `RemoteAttachment::description`. Order-preserving:
/// [`status_repository::insert_remote_attachments`] persists this `Vec`'s own
/// order as `position`.
fn extract_attachments(object: &Map<String, Value>) -> Vec<RemoteAttachment> {
    let Some(Value::Array(items)) = object.get("attachment") else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let entry = item.as_object()?;
            let url = attachment_url(entry.get("url")).or_else(|| {
                entry
                    .get("href")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })?;
            let media_type = optional_string(entry, "mediaType");
            let description =
                optional_string(entry, "name").or_else(|| optional_string(entry, "summary"));
            Some(RemoteAttachment {
                url,
                media_type,
                description,
            })
        })
        .collect()
}

/// Shared dependencies every handler [`register_status_handlers`] builds
/// needs: the repository connection pool, the id/clock injection boundary,
/// the [`RemoteActorResolver`] port, this instance's own configured
/// `domain` (task 10.2 — mention-domain filtering,
/// [`resolve_local_mentions`]), the [`LocalMentionResolver`] port (task 10.2
/// — [`CreateNoteHandler`]'s mention-notification resolution), and the
/// shared [`NotificationSinkRegistry`] (task 10.2 — see this module's own
/// doc comment, "Notification emit"). Bundled into one struct (design.md's
/// exact `register_status_handlers(dispatcher, deps: StatusInboundDeps)`
/// signature) so a future caller (task 7.2's bootstrap wiring) passes one
/// value rather than several positional parameters.
pub struct StatusInboundDeps<R: RemoteActorResolver, M: LocalMentionResolver> {
    pub pool: PgPool,
    pub runtime: RuntimeContext,
    pub remote_actors: Arc<R>,
    pub domain: String,
    pub mentions: M,
    pub notifications: NotificationSinkRegistry,
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
/// existing row is returned unchanged (no re-insert, no re-`persist_tags`, no
/// re-`insert_mentions`/`insert_remote_attachments`) — see this module's doc
/// comment ("Idempotent re-delivery"). Fails with a `422 Unprocessable
/// Entity` [`AppError`] if `object` carries no `id` property at all.
///
/// `domain`/`mentions` (task 10.3, Requirement 14.2): threaded through by
/// both callers — [`CreateNoteHandler`] passes its own `domain`/`mentions`
/// fields, [`crate::statuses::ingest_service::StatusIngestService`] passes
/// its own identically-named fields (added by this same task) — so mention
/// persistence goes through the exact same [`resolve_mentions`] primitive
/// task 10.2's notification emit already established, from both entry
/// points, rather than only one of them. See this module's doc comment
/// ("Attachment/mention reflection") for the full reasoning.
pub(crate) async fn ingest_note_object<M: LocalMentionResolver>(
    pool: &PgPool,
    runtime: &RuntimeContext,
    object: &Map<String, Value>,
    actor_id: Id,
    domain: &str,
    mentions: &M,
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

    // Task 10.3 (Requirement 14.2): reflect the remote Note's tag-array
    // Mention entries and attachment entries — see this module's doc
    // comment ("Attachment/mention reflection") for the full boundary
    // reasoning (why attachments become plain metadata, not a real
    // media-pipeline Media row; why only locally-resolved mentions are
    // persisted).
    let tag_mentions = extract_tag_mentions(object);
    if !tag_mentions.is_empty() {
        let mentioned_ids = resolve_mentions(mentions, domain, &tag_mentions).await?;
        if !mentioned_ids.is_empty() {
            status_repository::insert_mentions(pool, status.id, &mentioned_ids).await?;
        }
    }

    let attachments = extract_attachments(object);
    if !attachments.is_empty() {
        status_repository::insert_remote_attachments(pool, status.id, &attachments).await?;
    }

    Ok(status)
}

/// Ingests an inbound `Create(Note)` as a remote [`Status`] (Requirements
/// 14.1, 14.2), or — when the wire shape matches a poll vote (Requirement
/// 13.6) — branches into [`poll_repository::record_vote`] instead. Also
/// emits a `Mention` [`NotificationEvent`] per resolved local mention on a
/// genuinely new ingestion (task 10.2 — see this module's own doc comment,
/// "Notification emit"). See this module's doc comment for the full
/// ingestion/vote-detection contract.
pub struct CreateNoteHandler<R: RemoteActorResolver, M: LocalMentionResolver> {
    pool: PgPool,
    runtime: RuntimeContext,
    remote_actors: Arc<R>,
    domain: String,
    mentions: M,
    notifications: NotificationSinkRegistry,
}

impl<R: RemoteActorResolver, M: LocalMentionResolver> CreateNoteHandler<R, M> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pool: PgPool,
        runtime: RuntimeContext,
        remote_actors: Arc<R>,
        domain: impl Into<String>,
        mentions: M,
        notifications: NotificationSinkRegistry,
    ) -> Self {
        Self {
            pool,
            runtime,
            remote_actors,
            domain: domain.into(),
            mentions,
            notifications,
        }
    }

    /// Attempts the `Create{Note, name=...}` vote-wire-form branch (Requirement
    /// 13.6). Returns `Ok(Some(Handled))` when every vote-shape condition
    /// held and the vote was recorded *or* was already recorded (the
    /// duplicate-vote case — see this module's doc comment, "Idempotent
    /// re-delivery, `Create(Note)` and vote branch alike"); `Ok(None)` when
    /// any vote-shape condition failed (the caller should fall through to
    /// ordinary `Note` ingestion); `Err` only if `record_vote` rejects an
    /// otherwise-detected vote for a reason *other* than "already voted"
    /// (deadline passed, out-of-range choice, single/multiple violation).
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
        match poll_repository::record_vote(&self.pool, poll_id, actor_id, &[option.idx], now).await
        {
            Ok(_) => Ok(Some(HandleOutcome::Handled)),
            // See this module's doc comment ("Idempotent re-delivery... /
            // Self-notification loopback of a locally-already-recorded
            // vote") — `record_vote`'s specific duplicate-vote rejection
            // (`poll_repository.rs`'s own literal message) reaching this
            // *inbound* branch does not mean a client made a genuinely new,
            // rejected request: when the poll's author is local,
            // `StatusActivityBuilder::deliver_vote`'s own notification
            // Activity for a vote `PollService::vote` already recorded
            // moments earlier (via the direct, synchronous local call)
            // loops back in-process to this exact handler. The fact this
            // duplicate check reports ("`actor_id` has already voted for
            // this option") is already true and already correctly
            // reflected in `poll_votes`/`poll_options.votes_count` — so
            // this is reported as `Handled` (idempotent, already-applied),
            // the same outcome a genuine wire-level re-delivery of the
            // identical vote Activity gets, rather than an `AppError` that
            // would otherwise propagate all the way back through
            // `DeliveryService::deliver`'s local-recipient dispatch (task
            // 4.1/5.3's synchronous, in-process delivery path) into
            // `PollService::vote`'s own `Result`, turning an already
            // fully-succeeded vote into a spurious 422 for the voter who
            // just made it. Every *other* `record_vote` rejection (deadline
            // passed, out-of-range choice, single/multiple violation) is a
            // distinct wire condition, not the "I already knew that" case
            // this arm narrowly targets, and continues to propagate
            // unchanged.
            Err(err)
                if err.status == StatusCode::UNPROCESSABLE_ENTITY
                    && err.public_message == "actor has already voted in this poll" =>
            {
                Ok(Some(HandleOutcome::Handled))
            }
            Err(err) => Err(err),
        }
    }
}

impl<R: RemoteActorResolver, M: LocalMentionResolver> InboundActivityHandler
    for CreateNoteHandler<R, M>
{
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

            // Task 10.2: determined *before* ingestion (mirrors
            // `AnnounceHandler`'s own explicit pre-check pattern — see this
            // module's doc comment, "Notification emit") so a redelivered
            // `Create(Note)` (already ingested by `ingest_note_object`'s own
            // `find_by_uri` idempotency guard, "Idempotent re-delivery")
            // never re-emits a `Mention` notification. `ingest_note_object`
            // itself repeats this exact `find_by_uri` lookup internally —
            // deliberately not widened to return a New/Existing distinction,
            // since that function is shared with `StatusIngestService` (task
            // 6.2, out of this task's boundary to touch).
            let already_ingested = match object.get("id").and_then(Value::as_str) {
                Some(uri) => status_repository::find_by_uri(&self.pool, uri)
                    .await?
                    .is_some(),
                None => false,
            };

            // Delegates to the shared Note-ingestion code path also called
            // by `StatusIngestService` — see [`ingest_note_object`]'s own
            // doc comment for why (Requirement 14.5's "共通コードパス",
            // task 6.2's "受信ハンドラ経路と同一結果になる").
            let status = ingest_note_object(
                &self.pool,
                &self.runtime,
                object,
                actor_id,
                &self.domain,
                &self.mentions,
            )
            .await?;

            // Task 10.2: emit a `Mention` NotificationEvent per resolved
            // local mention, only for a genuinely new ingestion — see this
            // module's doc comment ("Notification emit") for the full
            // reasoning (resolution scope, recipient/origin tagging, the
            // defensive self-mention skip).
            if !already_ingested {
                let mentioned_ids =
                    resolve_local_mentions(&self.mentions, &self.domain, &status.content).await?;
                let occurred_at = self.runtime.clock.now();
                for mentioned_id in mentioned_ids {
                    if mentioned_id == actor_id {
                        continue;
                    }
                    self.notifications
                        .emit(NotificationEvent {
                            recipient: AccountRef::Local(mentioned_id),
                            origin: AccountRef::Remote(actor_id),
                            kind: NotificationType::Mention,
                            target_status_id: Some(status.id),
                            occurred_at,
                        })
                        .await?;
                }
            }

            Ok(HandleOutcome::Handled)
        })
    }
}

// ---------------------------------------------------------------------------
// AnnounceHandler
// ---------------------------------------------------------------------------

/// Records an inbound `Announce` (a remote boost of a **local** post) as a
/// reblog row and increments the target's `reblogs_count` (Requirements
/// 14.1, 14.3). Also emits a `Reblog` [`NotificationEvent`] on a genuinely
/// new boost (task 10.2 — see this module's own doc comment, "Notification
/// emit").
pub struct AnnounceHandler<R: RemoteActorResolver> {
    pool: PgPool,
    runtime: RuntimeContext,
    remote_actors: Arc<R>,
    notifications: NotificationSinkRegistry,
}

impl<R: RemoteActorResolver> AnnounceHandler<R> {
    pub fn new(
        pool: PgPool,
        runtime: RuntimeContext,
        remote_actors: Arc<R>,
        notifications: NotificationSinkRegistry,
    ) -> Self {
        Self {
            pool,
            runtime,
            remote_actors,
            notifications,
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

            // Task 10.2: emit exactly once per new remote-origin boost — only
            // reached past the `find_reblog` duplicate-check above (see this
            // module's doc comment, "Notification emit"). Defensive
            // self-interaction skip (currently unreachable — see that same
            // doc comment) mirrors `InteractionService::reblog`'s identical
            // guard.
            if actor_id != target.actor_id {
                self.notifications
                    .emit(NotificationEvent {
                        recipient: AccountRef::Local(target.actor_id),
                        origin: AccountRef::Remote(actor_id),
                        kind: NotificationType::Reblog,
                        target_status_id: Some(target.id),
                        occurred_at: now,
                    })
                    .await?;
            }

            Ok(HandleOutcome::Handled)
        })
    }
}

// ---------------------------------------------------------------------------
// LikeHandler
// ---------------------------------------------------------------------------

/// Records an inbound `Like` (a remote favourite of a **local** post) and
/// increments the target's `favourites_count` (Requirements 14.1, 14.3).
/// Also emits a `Favourite` [`NotificationEvent`] on a genuinely new
/// favourite (task 10.2 — see this module's own doc comment, "Notification
/// emit").
pub struct LikeHandler<R: RemoteActorResolver> {
    pool: PgPool,
    runtime: RuntimeContext,
    remote_actors: Arc<R>,
    notifications: NotificationSinkRegistry,
}

impl<R: RemoteActorResolver> LikeHandler<R> {
    pub fn new(
        pool: PgPool,
        runtime: RuntimeContext,
        remote_actors: Arc<R>,
        notifications: NotificationSinkRegistry,
    ) -> Self {
        Self {
            pool,
            runtime,
            remote_actors,
            notifications,
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

                // Task 10.2: emit exactly once per new remote-origin
                // favourite — only reached when `add_favourite` reports
                // `is_new` (see this module's doc comment, "Notification
                // emit"). Defensive self-interaction skip (currently
                // unreachable — see that same doc comment) mirrors
                // `InteractionService::favourite`'s identical guard.
                if actor_id != target.actor_id {
                    self.notifications
                        .emit(NotificationEvent {
                            recipient: AccountRef::Local(target.actor_id),
                            origin: AccountRef::Remote(actor_id),
                            kind: NotificationType::Favourite,
                            target_status_id: Some(target.id),
                            occurred_at: now,
                        })
                        .await?;
                }
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
/// `RuntimeContext`/`NotificationSinkRegistry` are cheap-clone handles;
/// `Arc<R>` is a pointer clone) — this function itself never touches the
/// database or network. `deps.mentions` (`M`, task 10.2) is moved, not
/// cloned: only [`CreateNoteHandler`] consumes it, so no `M: Clone` bound is
/// needed.
pub fn register_status_handlers<
    R: RemoteActorResolver + 'static,
    M: LocalMentionResolver + 'static,
>(
    dispatcher: &mut InboundActivityDispatcher,
    deps: StatusInboundDeps<R, M>,
) {
    dispatcher.register(Arc::new(CreateNoteHandler::new(
        deps.pool.clone(),
        deps.runtime.clone(),
        Arc::clone(&deps.remote_actors),
        deps.domain.clone(),
        deps.mentions,
        deps.notifications.clone(),
    )));
    dispatcher.register(Arc::new(AnnounceHandler::new(
        deps.pool.clone(),
        deps.runtime.clone(),
        Arc::clone(&deps.remote_actors),
        deps.notifications.clone(),
    )));
    dispatcher.register(Arc::new(LikeHandler::new(
        deps.pool.clone(),
        deps.runtime.clone(),
        Arc::clone(&deps.remote_actors),
        deps.notifications.clone(),
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
