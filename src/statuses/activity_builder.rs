//! `StatusActivityBuilder` (design.md "Federation Bridge / 連合橋渡し層" ->
//! `#### StatusActivityBuilder`, design.md lines ~485-509; Requirements 4.2,
//! 4.3, 4.4, 7.3, 8.4, 9.2, 9.4, 10.2, 10.3, 13.6, 13.7; task 4.1 (+ task
//! 10.1 for 13.7's `Question` wire form), `Boundary: StatusActivityBuilder`):
//! generates the six canonical post-related Activities (`Create(Note)` /
//! `Announce` / `Like` / `Delete` / `Update` / `Undo(Announce|Like)`), plus
//! the vote wire form `Create{Note, name=<selected option text>}`, and hands
//! each one — with the `Addressing`-derived recipients this task's own
//! callers resolve — to federation-core's `DeliveryService::deliver`
//! (Requirement 4.3's "論理的に同一の正規 Activity を生成・検証してから、配送
//! 手段のみを federation-core に分岐させる"). This module owns exactly
//! Activity generation; every physical local-vs-remote branch is
//! `DeliveryService`'s own job (`crate::federation::outbound::delivery`),
//! never this module's.
//!
//! ## Poll embedding in `Create` (Requirement 13.7, task 10.1)
//! [`StatusActivityBuilder::deliver_create`]'s `object` is still a `Create`'s
//! *single* object — a poll-bearing post is not a second Activity type, per
//! design.md's own "a poll-bearing Note is just a Note with poll data
//! embedded in its JSON-LD representation" framing (see
//! `status_service.rs`'s "Outbound wire format" doc comment, the documented
//! gap this task closes). When the caller passes `Some((poll, options))`,
//! that object's `type` becomes `"Question"` and gains `oneOf`/`anyOf` +
//! `endTime` — see [`StatusActivityBuilder::deliver_create`]'s own doc
//! comment for the exact shape. `deliver_update` is deliberately left
//! untouched by this task: Requirement 13.7 names only `Create`, this spec
//! has no poll-editing feature (a poll's options/deadline/single-vs-multiple
//! never change after creation — `CreateStatus`'s own poll field has no
//! `edit_status` counterpart), so an `Update` Activity never has a *changed*
//! poll shape to convey, and retrofitting the identical embedding there
//! would be speculative scope this task's own boundary/observable-completion
//! text does not ask for.
//!
//! ## Scope
//! Owns [`StatusActivityBuilder`] and its seven `deliver_*` methods, the
//! [`UndoKind`] gap-fill type (see below), and the narrow [`ActorHandleLookup`]
//! delegation port (see below). Does not implement `StatusService` /
//! `InteractionService` / `PollService` (later tasks, the eventual callers
//! that resolve a `Status`/`Poll`/`Addressing`/`Vec<Recipient>` and hand them
//! to this module), does not touch bootstrap/`AppState`/the live
//! `DeliveryService` wiring (task 7.2, out of this task's boundary — this is
//! a standalone, independently unit-testable module with no live caller
//! yet), and does not itself decide *which* recipients a post addresses
//! (`Addressing`/`derive_recipients`, task 3.2, imported here rather than
//! redefined, per that module's own Implementation Note).
//!
//! ## `UndoKind`: not defined anywhere in design.md, defined here
//! design.md's own Service Interface sketch (line 505) names a
//! `deliver_undo(&self, actor: Id, undone: UndoKind, target: &Status) ->
//! ...` parameter but never defines `UndoKind` anywhere in the document —
//! the same situation task 3.2's own Implementation Note documents for
//! `ActorRef` (`addressing.rs`'s doc comment: "not literally defined in
//! design.md... This module defines it here"). This module is the sole
//! consumer of the distinction ("which inner Activity type is being undone"
//! — `Announce` or `Like`, the only two Undo cases Requirements 9.4/10.3
//! name), so it is the natural owner: [`UndoKind`] is a two-variant enum
//! naming exactly those two cases, with no third variant and no attempt to
//! generalize to arbitrary undoable Activity types this spec has no
//! requirement for.
//!
//! ## `ActorHandleLookup`: a narrow, DB-free port for `Id -> Handle`
//! Every `deliver_*` method needs to resolve the *sending* actor's `Id`
//! (`Status::actor_id`, or an explicit `actor: Id` parameter for
//! Like/Undo/Vote) to the [`crate::actor::Handle`]
//! [`crate::federation::DeliveryRequest::sender`] requires. This is always a
//! **local** actor: every operation that reaches this builder is triggered
//! by an authenticated local actor (Requirements 3.1, 7.1, 8.1, 9.1, 10.1,
//! 13.2 each read "認証済みアクターが...を要求したとき") — remote-origin
//! state changes flow through the *inbound* handlers (task 6.1), a
//! completely separate boundary that never calls this builder. The
//! `Id -> Handle` gap itself is not new: federation-core's `DeliveryWorker`
//! (task 4.3) hit the identical gap for `DeliveryJob::sender_actor_id` and
//! closed it with `ActorDirectory::resolve_actor_by_id` (see that method's
//! own doc comment in `src/actor/directory.rs`). This module reuses that
//! same method, but — unlike `DeliveryWorker`, which holds a plain
//! `Arc<ActorDirectory>` because its own tests already require a real
//! Postgres-backed `DeliveryQueue` — wraps it behind a narrow local trait,
//! [`ActorHandleLookup`], mirroring `crate::federation::outbound::target`'s
//! own `LocalActorLookup` precedent ("a narrow mockable port over
//! `ActorDirectory`"). This keeps `StatusActivityBuilder` itself pure/DB-free
//! and unit-testable with a plain in-memory test double, per this task's own
//! testing strategy (no Postgres needed — the observable behavior under test
//! is Activity *shape* and recipient pass-through, not actor persistence).
//!
//! ## Deliberate deviations from design.md's literal Service Interface
//! design.md's sketch (lines 502-508):
//! ```text
//! pub async fn deliver_create(&self, status: &Status, addressing: &Addressing, recipients: Vec<Recipient>) -> Result<(), AppError>;
//! pub async fn deliver_announce(&self, reblog: &Status, target: &Status, recipients: Vec<Recipient>) -> Result<(), AppError>;
//! pub async fn deliver_like(&self, actor: Id, target: &Status) -> Result<(), AppError>;
//! pub async fn deliver_undo(&self, actor: Id, undone: UndoKind, target: &Status) -> Result<(), AppError>;
//! pub async fn deliver_delete(&self, status: &Status, recipients: Vec<Recipient>) -> Result<(), AppError>;
//! pub async fn deliver_update(&self, status: &Status, recipients: Vec<Recipient>) -> Result<(), AppError>;
//! pub async fn deliver_vote(&self, actor: Id, poll: &Poll, target: &Status, choices: &[i32]) -> Result<(), AppError>;
//! ```
//! Every deviation below is a documented, narrow gap-fill, not a silent
//! guess:
//! - **`deliver_announce`/`deliver_delete`/`deliver_update` gain an
//!   `addressing: &Addressing` parameter** design.md's sketch omitted for
//!   these three (only `deliver_create`'s sketch carries one). Every one of
//!   these Activities still needs a `to`/`cc` pair to be a valid,
//!   Requirement-4.2-compliant ActivityPub document ("投稿を作成・配送する
//!   とき...to/cc...を導出する単一の addressing ロジックを通し" is not
//!   scoped to `Create` alone), and `Addressing` (task 3.2) is the single
//!   already-established source for it — inventing a second, narrower
//!   addressing derivation for three of six operations would violate
//!   Requirement 4.2's "単一の addressing ロジック" more than adding the
//!   parameter design.md's own sketch merely omitted.
//! - **`deliver_like`/`deliver_undo`/`deliver_vote` take a
//!   `recipient: ActorRef`** (imported from `addressing.rs`, not redefined)
//!   instead of resolving `target.actor_id` internally. `target: &Status`'s
//!   author may be local *or* remote (a post can be liked/voted-on/reblogged
//!   regardless of its own origin), and this spec's remote-actor
//!   identity/inbox resolution is explicitly out of boundary
//!   (requirements.md's Boundary Context: "リモートアクターの完全なプロ
//!   フィール永続化...は accounts-and-instance") — resolving it here would
//!   both violate that boundary and reintroduce a DB dependency this task's
//!   own testing strategy says this module must not have. `ActorRef { uri,
//!   recipient }` already bundles exactly what a single addressee's `to`
//!   field content (`uri`) and delivery destination (`recipient`) both need,
//!   is already the established "caller pre-resolves a mention/addressee"
//!   shape `Addressing`/`derive_recipients` use throughout this spec, and is
//!   imported here per this task's own explicit instruction (task 3.2's
//!   Implementation Note: "task 4.1 はこれらを新規定義せず本モジュールから
//!   import して再利用すること").
//! - **`deliver_create`/`deliver_update` gain an
//!   `in_reply_to_uri: Option<&str>` parameter**, absent from design.md's
//!   sketch. `Status::in_reply_to_id` (`model.rs`) is a logical `Id`
//!   reference to another `statuses` row, not the parent's ActivityPub
//!   `uri` string an outbound `Note`'s `inReplyTo` property requires — and
//!   this pure, DB-free builder has no `StatusRepository` to resolve one
//!   `Id` to another row's `uri`. The caller (a later task,
//!   `StatusService::create_status`/`edit_status`, which already has the
//!   parent row in hand when it does) supplies the already-resolved URI,
//!   mirroring `StatusRenderInput`'s (`serializer.rs`) identical
//!   "pre-resolved caller input" convention for exactly this class of gap.
//!   `None` omits the `inReplyTo` property entirely (a top-level post).
//! - **`deliver_vote`'s `choices: &[i32]` becomes `choice_titles: &[String]`**.
//!   Resolving a selected option's numeric index to its display title is
//!   `PollRepository`'s data (`poll_options.title`, task 2.3), not something
//!   this DB-free builder can look up; the caller (a later task,
//!   `PollService::vote`) already has the chosen `PollOption`s in hand after
//!   validating the vote and passes their titles directly. One independent
//!   `Create{Note, name=<title>}` Activity is generated and delivered **per
//!   title** (a `for` loop over `choice_titles`, each its own
//!   `DeliveryService::deliver` call) rather than one combined Activity —
//!   this mirrors Mastodon's own real multi-choice wire convention (one
//!   `Create` per selected answer, each a reply to the poll's `Status`), and
//!   is the only shape that keeps every individual emitted Activity a
//!   genuine `Create{Note, name=...}` (Requirement 13.6) rather than an
//!   invented, non-standard "batch vote" Activity no requirement asks for.
//! - **`UndoKind`'s embedded inner Activity mints its own fresh `id`**,
//!   rather than reusing the original `Announce`/`Like` Activity's `id`.
//!   This spec never persists an outbound Activity's own `id` anywhere (a
//!   reblog/favourite is represented purely as row state —
//!   `statuses.reblog_of_id` / `favourites` — never as a stored Activity
//!   document, per `model.rs`'s and `migrations/0007_statuses.sql`'s own
//!   doc comments), so there is no original `id` this builder could recall
//!   even if design.md's sketch expected it to. A federated peer
//!   reconciling an `Undo` identifies *what* is being undone by the inner
//!   object's `actor`+`object` pair (which actor announced/liked which
//!   status), not by matching the inner Activity's `id` back to one it saw
//!   earlier — this is the same reasoning real ActivityPub implementations
//!   (including Mastodon's own) already rely on.
//!
//! ## Manual `serde_json::Map` construction, not `json!{}`
//! Follows `src/federation/endpoints/document.rs`'s (`ActivityPubDocumentBuilder`)
//! and `src/statuses/serializer.rs`'s established precedent: every Activity/
//! object body here is built by `Map::new()` + `.insert()`, never a `json!{
//! ... }` literal a field could silently go missing from without a compiler
//! error.
//!
//! ## `@context` stamping: `DeliveryService`'s job, not this module's
//! `crate::federation::outbound::delivery::DeliveryService::deliver` already
//! calls `crate::federation::jsonld::serialize` (which stamps
//! `@context`) exactly once per `deliver()` call, before any sink runs — see
//! that module's own doc comment ("The 'one canonical Activity, one
//! resolution' invariant") and its test `sample_activity` helper, which
//! likewise omits `@context` from the raw activity a caller hands in. This
//! module therefore never stamps `@context` itself; every `Value` built here
//! is the *raw*, not-yet-canonicalized document `DeliveryRequest::activity`
//! expects.
//!
//! ## Activity `id` minting
//! Activities have no persisted row of their own anywhere in this spec's
//! schema (see the `UndoKind` section above), so each Activity's own `id`
//! URI is minted fresh, per call, from an injected `Arc<dyn IdGenerator>`
//! (this crate's standard "id/time is caller/`RuntimeContext`-injected,
//! never invented ad hoc inside a component" determinism convention) and
//! rendered via `ActorUrls::object_url` under a locally-defined
//! [`ACTIVITY_OBJECT_KIND`] (`"activities"`) — `ActorUrls`'s own doc comment
//! documents `ObjectKind` as exactly this kind of caller-minted, extensible
//! path-segment newtype, not a fixed enumerated set. No `Clock` dependency
//! is introduced: every timestamp this module emits (`published`/`updated`)
//! is read directly off the already-given `Status`'s own `created_at`/
//! `edited_at` fields, never a fresh wall-clock read (the vote wire form
//! carries no timestamp at all, since no time value is threaded through
//! design.md's `deliver_vote` sketch or this module's adapted signature —
//! deliberately not backfilled with a wall-clock read, per this crate's "no
//! unnecessary non-determinism dependency" precedent, `src/actor/directory.rs`).
//!
//! ## `"Vote"` is never emitted as an Activity `type`
//! [`StatusActivityBuilder::deliver_vote`] only ever inserts the literal
//! strings `"Create"` (outer Activity `type`) and `"Note"` (inner object
//! `type`) — grep this file for `"Vote"` as a `type`/`activity_type` value
//! and there is none, matching Requirement 13.6's "独自の `Vote` Activity
//! type を持たないため生成しない" and this task's own completion criterion.

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::{Map, Value};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::actor::{ActorDirectory, Handle};
use crate::domain::Id;
use crate::error::AppError;
use crate::federation::urls::{ActorUrls, ObjectKind};
use crate::federation::{
    DeliveryRequest, DeliveryService, DeliverySink, LocalActorLookup, Recipient,
};
use crate::runtime::IdGenerator;
use crate::statuses::addressing::{ActorRef, Addressing};
use crate::statuses::model::{Poll, PollOption, Status};

/// The URL path segment under which this builder mints Activity `id` URIs
/// (`ActorUrls::object_url`). See this module's doc comment ("Activity `id`
/// minting") for why a fresh id is minted per Activity rather than derived
/// from any persisted row.
const ACTIVITY_OBJECT_KIND: ObjectKind = ObjectKind::new("activities");

/// Which inner Activity type an `Undo` wraps (design.md's `UndoKind`
/// parameter, never itself defined in design.md — see this module's doc
/// comment, "`UndoKind`: not defined anywhere in design.md"). Exactly the
/// two cases this spec undoes (Requirements 9.4, 10.3): unboosting
/// (`Undo(Announce)`) and unfavouriting (`Undo(Like)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UndoKind {
    Announce,
    Like,
}

impl UndoKind {
    /// The inner (undone) Activity's own `type` string.
    fn activity_type(self) -> &'static str {
        match self {
            UndoKind::Announce => "Announce",
            UndoKind::Like => "Like",
        }
    }
}

/// The narrow, DB-free `Id -> Handle` port [`StatusActivityBuilder`] depends
/// on to resolve a delivering *local* actor's identity. See this module's
/// doc comment ("`ActorHandleLookup`: a narrow, DB-free port") for why this
/// exists rather than holding `Arc<ActorDirectory>` directly.
#[allow(async_fn_in_trait)]
pub trait ActorHandleLookup: Send + Sync {
    /// Resolves `actor_id` to its [`Handle`], failing with a `404`-shaped
    /// [`AppError`] if `actor_id` no longer resolves to an existing local
    /// actor (mirrors `crate::federation::outbound::target`'s
    /// "fail loudly rather than silently drop" convention for the
    /// structurally analogous situation).
    async fn resolve_handle(&self, actor_id: Id) -> Result<Handle, AppError>;
}

impl ActorHandleLookup for ActorDirectory {
    async fn resolve_handle(&self, actor_id: Id) -> Result<Handle, AppError> {
        self.resolve_actor_by_id(actor_id)
            .await?
            .map(|resolved| resolved.handle)
            .ok_or_else(|| {
                AppError::client(
                    StatusCode::NOT_FOUND,
                    format!("actor id {actor_id:?} does not resolve to an existing local actor"),
                )
            })
    }
}

/// Generates the canonical post-related Activities (design.md's exact
/// `StatusActivityBuilder` component; Requirements 4.2, 4.3, 4.4, 7.3, 8.4,
/// 9.2, 9.4, 10.2, 10.3, 13.6) and hands each one to `DeliveryService::deliver`
/// unmodified. See this module's doc comment for the full deviation
/// rationale and the "one canonical Activity per operation" discipline this
/// type enforces structurally (every `deliver_*` method builds exactly one
/// `serde_json::Value` and passes it to exactly one `deliver_one` call,
/// itself exactly one `DeliveryService::deliver` call — never rebuilt or
/// re-derived per recipient).
pub struct StatusActivityBuilder<A, D, L, H>
where
    A: ActorHandleLookup,
    D: LocalActorLookup,
    L: DeliverySink,
    H: DeliverySink,
{
    urls: ActorUrls,
    ids: Arc<dyn IdGenerator>,
    actor_lookup: A,
    delivery: Arc<DeliveryService<D, L, H>>,
}

impl<A, D, L, H> StatusActivityBuilder<A, D, L, H>
where
    A: ActorHandleLookup,
    D: LocalActorLookup,
    L: DeliverySink,
    H: DeliverySink,
{
    /// Builds a `StatusActivityBuilder` over `urls` (Activity/actor URI
    /// construction), `ids` (fresh Activity `id` minting), `actor_lookup`
    /// (sending-actor `Id -> Handle` resolution), and `delivery` (the
    /// federation-core common delivery path this builder's only job is to
    /// feed).
    ///
    /// `delivery` is `Arc<DeliveryService<D, L, H>>`, not an owned
    /// `DeliveryService<D, L, H>` (task 7.2, `Boundary: StatusesModule,
    /// server, bootstrap, config`, plus this file's own explicitly-justified
    /// exception — see that task's own status report): the real
    /// `FederationModule` (`src/federation/module.rs`) only ever exposes its
    /// one live `ConcreteDeliveryService` behind `&Arc<ConcreteDeliveryService>`
    /// (`FederationModule::delivery_service`), never by value, and
    /// `StatusesModule` must build three independent `StatusActivityBuilder`
    /// instances (one embedded in each of `StatusService`/`InteractionService`/
    /// `PollService`) that all feed the identical shared `DeliveryService`
    /// rather than each cloning/reconstructing their own. Widening this field
    /// from an owned value to an `Arc` is a pure signature change: every
    /// existing call site in this module (`deliver_one`, the sole consumer of
    /// `self.delivery`) is unaffected, since `Arc<T>` transparently derefs to
    /// `&T` and `DeliveryService::deliver` only ever needs `&self`.
    pub fn new(
        urls: ActorUrls,
        ids: Arc<dyn IdGenerator>,
        actor_lookup: A,
        delivery: Arc<DeliveryService<D, L, H>>,
    ) -> Self {
        Self {
            urls,
            ids,
            actor_lookup,
            delivery,
        }
    }

    /// Mints a fresh Activity `id` URI (see this module's doc comment,
    /// "Activity `id` minting").
    fn mint_activity_id(&self) -> String {
        self.urls
            .object_url(ACTIVITY_OBJECT_KIND, self.ids.next_id())
    }

    /// Resolves `actor_id`'s local actor URL (`ActorUrls::actor_url`) via
    /// this builder's own `ActorHandleLookup`. Added by task 5.1
    /// (`StatusService`, this builder's first real caller): a narrow escape
    /// hatch so a caller that needs to build its own `Addressing`
    /// (`derive_addressing`'s `followers_uri` parameter, e.g. `{actor_url}/
    /// followers`) does not have to duplicate this builder's own
    /// `ActorHandleLookup`/`ActorUrls` wiring just to resolve one actor's
    /// URL. Every other method on this type stays unmodified.
    pub async fn resolve_actor_url(&self, actor_id: Id) -> Result<String, AppError> {
        let handle = self.actor_lookup.resolve_handle(actor_id).await?;
        Ok(self.urls.actor_url(&handle))
    }

    /// Hands `activity` to `DeliveryService::deliver` as a single delivery
    /// request — the one call site every `deliver_*` method above funnels
    /// through, so "one canonical Activity, one `deliver()` call" is
    /// structural, not merely a convention each method separately follows.
    async fn deliver_one(
        &self,
        activity: Value,
        sender: Handle,
        recipients: Vec<Recipient>,
    ) -> Result<(), AppError> {
        self.delivery
            .deliver(DeliveryRequest {
                activity,
                sender,
                recipients,
            })
            .await
    }

    /// Generates and delivers a canonical `Create` Activity for `status`
    /// (Requirements 4.2, 4.3, 4.4). `addressing`/`recipients` are
    /// `Addressing`/`derive_recipients`'s already-derived output (task 3.2);
    /// `in_reply_to_uri` is the parent post's already-resolved ActivityPub
    /// `uri` when `status.in_reply_to_id` is `Some(..)` — see this module's
    /// doc comment ("Deliberate deviations") for why this arrives
    /// pre-resolved rather than looked up here.
    ///
    /// `poll` is `Some((poll, options))` when `status.poll_id` is `Some(_)`
    /// (Requirement 13.7): the caller (`StatusService::create_status`, which
    /// already has the just-persisted [`Poll`]/`Vec<PollOption>` in hand
    /// immediately after its own `poll_repository::insert_poll` call — see
    /// that module's "Poll handling" doc comment) passes them straight
    /// through, mirroring `in_reply_to_uri`'s identical "pre-resolved caller
    /// input" convention (this DB-free builder has no `PollRepository` to
    /// fetch them itself). When present, the emitted object's `type` becomes
    /// `"Question"` instead of `"Note"` and gains `oneOf` (single-choice,
    /// `poll.multiple == false`) or `anyOf` (multi-choice) holding one
    /// `Note`-shaped option per `options` entry, plus `endTime` when
    /// `poll.expires_at` is `Some(_)` (omitted when `None`, matching this
    /// object's own `summary`/`inReplyTo` "omit when absent" convention).
    /// `closed` is never emitted here: `deliver_create` fires at creation
    /// time, when a freshly-created poll cannot yet be expired, and neither
    /// Requirement 13.7 nor this task's observable-completion text calls for
    /// a runtime "is it expired now" check inside Activity generation.
    pub async fn deliver_create(
        &self,
        status: &Status,
        addressing: &Addressing,
        recipients: Vec<Recipient>,
        in_reply_to_uri: Option<&str>,
        poll: Option<(&Poll, &[PollOption])>,
    ) -> Result<(), AppError> {
        let sender = self.actor_lookup.resolve_handle(status.actor_id).await?;
        let actor_url = self.urls.actor_url(&sender);
        let published = rfc3339(status.created_at);

        let mut object: Map<String, Value> = Map::new();
        object.insert("id".to_string(), Value::String(status.uri.clone()));
        object.insert("type".to_string(), Value::String("Note".to_string()));
        object.insert("attributedTo".to_string(), Value::String(actor_url.clone()));
        object.insert("content".to_string(), Value::String(status.content.clone()));
        object.insert("published".to_string(), Value::String(published.clone()));
        object.insert("sensitive".to_string(), Value::Bool(status.sensitive));
        if !status.spoiler_text.is_empty() {
            object.insert(
                "summary".to_string(),
                Value::String(status.spoiler_text.clone()),
            );
        }
        if let Some(uri) = in_reply_to_uri {
            object.insert("inReplyTo".to_string(), Value::String(uri.to_string()));
        }
        if let Some((poll, options)) = poll {
            object.insert("type".to_string(), Value::String("Question".to_string()));
            let choices_key = if poll.multiple { "anyOf" } else { "oneOf" };
            object.insert(choices_key.to_string(), poll_options_value(options));
            if let Some(expires_at) = poll.expires_at {
                object.insert("endTime".to_string(), Value::String(rfc3339(expires_at)));
            }
        }
        object.insert("to".to_string(), string_array(&addressing.to));
        object.insert("cc".to_string(), string_array(&addressing.cc));

        let mut activity: Map<String, Value> = Map::new();
        activity.insert("id".to_string(), Value::String(self.mint_activity_id()));
        activity.insert("type".to_string(), Value::String("Create".to_string()));
        activity.insert("actor".to_string(), Value::String(actor_url));
        activity.insert("published".to_string(), Value::String(published));
        activity.insert("to".to_string(), string_array(&addressing.to));
        activity.insert("cc".to_string(), string_array(&addressing.cc));
        activity.insert("object".to_string(), Value::Object(object));

        self.deliver_one(Value::Object(activity), sender, recipients)
            .await
    }

    /// Generates and delivers a canonical `Announce` Activity representing
    /// `reblog` (a boost `Status` row, `reblog.reblog_of_id == Some(target.id)`)
    /// of `target` (Requirements 4.2, 4.3, 4.4, 9.2, 9.4). `addressing`/
    /// `recipients` are `reblog`'s own already-derived addressing/recipients.
    pub async fn deliver_announce(
        &self,
        reblog: &Status,
        target: &Status,
        addressing: &Addressing,
        recipients: Vec<Recipient>,
    ) -> Result<(), AppError> {
        let sender = self.actor_lookup.resolve_handle(reblog.actor_id).await?;
        let actor_url = self.urls.actor_url(&sender);

        let mut activity: Map<String, Value> = Map::new();
        activity.insert("id".to_string(), Value::String(self.mint_activity_id()));
        activity.insert("type".to_string(), Value::String("Announce".to_string()));
        activity.insert("actor".to_string(), Value::String(actor_url));
        activity.insert(
            "published".to_string(),
            Value::String(rfc3339(reblog.created_at)),
        );
        activity.insert("object".to_string(), Value::String(target.uri.clone()));
        activity.insert("to".to_string(), string_array(&addressing.to));
        activity.insert("cc".to_string(), string_array(&addressing.cc));

        self.deliver_one(Value::Object(activity), sender, recipients)
            .await
    }

    /// Generates and delivers a canonical `Like` Activity by `actor` for
    /// `target` (Requirements 4.2, 4.3, 10.2). `recipient` is `target`'s
    /// author, already resolved by the caller to an [`ActorRef`] — see this
    /// module's doc comment ("Deliberate deviations") for why.
    pub async fn deliver_like(
        &self,
        actor: Id,
        target: &Status,
        recipient: ActorRef,
    ) -> Result<(), AppError> {
        let sender = self.actor_lookup.resolve_handle(actor).await?;
        let actor_url = self.urls.actor_url(&sender);

        let mut activity: Map<String, Value> = Map::new();
        activity.insert("id".to_string(), Value::String(self.mint_activity_id()));
        activity.insert("type".to_string(), Value::String("Like".to_string()));
        activity.insert("actor".to_string(), Value::String(actor_url));
        activity.insert("object".to_string(), Value::String(target.uri.clone()));
        activity.insert(
            "to".to_string(),
            string_array(std::slice::from_ref(&recipient.uri)),
        );

        self.deliver_one(Value::Object(activity), sender, vec![recipient.recipient])
            .await
    }

    /// Generates and delivers a canonical `Undo` Activity by `actor`,
    /// wrapping an inner `Announce`/`Like` Activity (per `undone`) whose
    /// `object` is `target` (Requirements 4.2, 4.3, 9.4, 10.3). `recipient`
    /// is `target`'s author (the same single-addressee shape
    /// [`Self::deliver_like`] takes) — see this module's doc comment
    /// ("`UndoKind`'s embedded inner Activity mints its own fresh `id`") for
    /// why the inner Activity's `id` is freshly minted rather than recalled.
    pub async fn deliver_undo(
        &self,
        actor: Id,
        undone: UndoKind,
        target: &Status,
        recipient: ActorRef,
    ) -> Result<(), AppError> {
        let sender = self.actor_lookup.resolve_handle(actor).await?;
        let actor_url = self.urls.actor_url(&sender);

        let mut inner: Map<String, Value> = Map::new();
        inner.insert("id".to_string(), Value::String(self.mint_activity_id()));
        inner.insert(
            "type".to_string(),
            Value::String(undone.activity_type().to_string()),
        );
        inner.insert("actor".to_string(), Value::String(actor_url.clone()));
        inner.insert("object".to_string(), Value::String(target.uri.clone()));

        let mut activity: Map<String, Value> = Map::new();
        activity.insert("id".to_string(), Value::String(self.mint_activity_id()));
        activity.insert("type".to_string(), Value::String("Undo".to_string()));
        activity.insert("actor".to_string(), Value::String(actor_url));
        activity.insert("object".to_string(), Value::Object(inner));
        activity.insert(
            "to".to_string(),
            string_array(std::slice::from_ref(&recipient.uri)),
        );

        self.deliver_one(Value::Object(activity), sender, vec![recipient.recipient])
            .await
    }

    /// Generates and delivers a canonical `Delete` Activity for `status`
    /// (Requirements 4.2, 4.3, 7.3). `addressing`/`recipients` mirror
    /// `status`'s own original addressing/recipients (the caller's
    /// responsibility to re-derive or recall).
    pub async fn deliver_delete(
        &self,
        status: &Status,
        addressing: &Addressing,
        recipients: Vec<Recipient>,
    ) -> Result<(), AppError> {
        let sender = self.actor_lookup.resolve_handle(status.actor_id).await?;
        let actor_url = self.urls.actor_url(&sender);

        let mut activity: Map<String, Value> = Map::new();
        activity.insert("id".to_string(), Value::String(self.mint_activity_id()));
        activity.insert("type".to_string(), Value::String("Delete".to_string()));
        activity.insert("actor".to_string(), Value::String(actor_url));
        activity.insert("object".to_string(), Value::String(status.uri.clone()));
        activity.insert("to".to_string(), string_array(&addressing.to));
        activity.insert("cc".to_string(), string_array(&addressing.cc));

        self.deliver_one(Value::Object(activity), sender, recipients)
            .await
    }

    /// Generates and delivers a canonical `Update` Activity for `status`'s
    /// post-edit state (Requirements 4.2, 4.3, 8.4). `addressing`/
    /// `recipients`/`in_reply_to_uri` mirror [`Self::deliver_create`]'s own
    /// parameters and rationale.
    pub async fn deliver_update(
        &self,
        status: &Status,
        addressing: &Addressing,
        recipients: Vec<Recipient>,
        in_reply_to_uri: Option<&str>,
    ) -> Result<(), AppError> {
        let sender = self.actor_lookup.resolve_handle(status.actor_id).await?;
        let actor_url = self.urls.actor_url(&sender);
        let published = rfc3339(status.created_at);
        let updated = rfc3339(status.edited_at.unwrap_or(status.created_at));

        let mut object: Map<String, Value> = Map::new();
        object.insert("id".to_string(), Value::String(status.uri.clone()));
        object.insert("type".to_string(), Value::String("Note".to_string()));
        object.insert("attributedTo".to_string(), Value::String(actor_url.clone()));
        object.insert("content".to_string(), Value::String(status.content.clone()));
        object.insert("published".to_string(), Value::String(published));
        object.insert("updated".to_string(), Value::String(updated.clone()));
        object.insert("sensitive".to_string(), Value::Bool(status.sensitive));
        if !status.spoiler_text.is_empty() {
            object.insert(
                "summary".to_string(),
                Value::String(status.spoiler_text.clone()),
            );
        }
        if let Some(uri) = in_reply_to_uri {
            object.insert("inReplyTo".to_string(), Value::String(uri.to_string()));
        }
        object.insert("to".to_string(), string_array(&addressing.to));
        object.insert("cc".to_string(), string_array(&addressing.cc));

        let mut activity: Map<String, Value> = Map::new();
        activity.insert("id".to_string(), Value::String(self.mint_activity_id()));
        activity.insert("type".to_string(), Value::String("Update".to_string()));
        activity.insert("actor".to_string(), Value::String(actor_url));
        activity.insert("published".to_string(), Value::String(updated));
        activity.insert("to".to_string(), string_array(&addressing.to));
        activity.insert("cc".to_string(), string_array(&addressing.cc));
        activity.insert("object".to_string(), Value::Object(object));

        self.deliver_one(Value::Object(activity), sender, recipients)
            .await
    }

    /// Generates and delivers `choice_titles.len()` independent
    /// `Create{Note, name=<title>}` Activities — the Mastodon-compatible
    /// de-facto vote wire form (Requirement 13.6) — one per selected option
    /// title, each a reply (`inReplyTo`) to `target` (the `Status` owning
    /// `poll`), addressed solely to `recipient` (`target`'s author). No
    /// `Vote` Activity type is ever generated — see this module's doc
    /// comment, "`\"Vote\"` is never emitted as an Activity `type`". See
    /// "Deliberate deviations" for why this takes already-resolved
    /// `choice_titles: &[String]` rather than design.md's `choices: &[i32]`.
    pub async fn deliver_vote(
        &self,
        actor: Id,
        poll: &Poll,
        target: &Status,
        choice_titles: &[String],
        recipient: ActorRef,
    ) -> Result<(), AppError> {
        debug_assert_eq!(
            poll.status_id, target.id,
            "deliver_vote's poll must belong to target (poll.status_id == target.id)"
        );

        let sender = self.actor_lookup.resolve_handle(actor).await?;
        let actor_url = self.urls.actor_url(&sender);

        for title in choice_titles {
            let mut object: Map<String, Value> = Map::new();
            object.insert("id".to_string(), Value::String(self.mint_activity_id()));
            object.insert("type".to_string(), Value::String("Note".to_string()));
            object.insert("name".to_string(), Value::String(title.clone()));
            object.insert("attributedTo".to_string(), Value::String(actor_url.clone()));
            object.insert("inReplyTo".to_string(), Value::String(target.uri.clone()));
            object.insert(
                "to".to_string(),
                string_array(std::slice::from_ref(&recipient.uri)),
            );

            let mut activity: Map<String, Value> = Map::new();
            activity.insert("id".to_string(), Value::String(self.mint_activity_id()));
            activity.insert("type".to_string(), Value::String("Create".to_string()));
            activity.insert("actor".to_string(), Value::String(actor_url.clone()));
            activity.insert(
                "to".to_string(),
                string_array(std::slice::from_ref(&recipient.uri)),
            );
            activity.insert("object".to_string(), Value::Object(object));

            self.deliver_one(
                Value::Object(activity),
                sender.clone(),
                vec![recipient.recipient.clone()],
            )
            .await?;
        }

        Ok(())
    }
}

/// Renders `items` as a JSON array of strings (`to`/`cc` wire shape).
fn string_array(items: &[String]) -> Value {
    Value::Array(items.iter().cloned().map(Value::String).collect())
}

/// Renders a [`Poll`]'s `options` as the `oneOf`/`anyOf` array a `Question`
/// object's wire form expects (Requirement 13.7): one `Note`-shaped object
/// per option, `name` holding the option text (the same field the vote wire
/// form's own `Create{Note, name=...}` object uses, Requirement 13.6) and a
/// nested `replies: { type: "Collection", totalItems: <votes_count> }`, the
/// de-facto Mastodon convention for embedding a live vote count in the
/// create/update payload. Options are sorted by [`PollOption::idx`] (the
/// caller's own ordering, `poll_repository::insert_poll`'s insertion order)
/// rather than trusted to already arrive in that order.
fn poll_options_value(options: &[PollOption]) -> Value {
    let mut sorted: Vec<&PollOption> = options.iter().collect();
    sorted.sort_by_key(|option| option.idx);

    Value::Array(
        sorted
            .into_iter()
            .map(|option| {
                let mut replies: Map<String, Value> = Map::new();
                replies.insert("type".to_string(), Value::String("Collection".to_string()));
                replies.insert(
                    "totalItems".to_string(),
                    Value::Number(option.votes_count.into()),
                );

                let mut note: Map<String, Value> = Map::new();
                note.insert("type".to_string(), Value::String("Note".to_string()));
                note.insert("name".to_string(), Value::String(option.title.clone()));
                note.insert("replies".to_string(), Value::Object(replies));
                Value::Object(note)
            })
            .collect(),
    )
}

/// Renders `when` as an RFC 3339 timestamp string, matching
/// `serializer.rs::format_time`'s identical convention.
fn rfc3339(when: OffsetDateTime) -> String {
    when.format(&Rfc3339)
        .expect("a valid OffsetDateTime always formats as RFC 3339")
}
