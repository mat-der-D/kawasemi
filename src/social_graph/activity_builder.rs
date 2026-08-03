//! `ActivityBuilder` (design.md "Federation" -> `#### ActivityBuilder`,
//! design.md lines ~443-464; Requirements 1.2, 1.3, 1.4, 2.3, 2.4, 5.3, 5.4;
//! task 2.3, `Boundary: ActivityBuilder`): generates the canonical
//! ActivityPub logical representations for Follow / Accept / Reject / Block
//! / Undo, embedding the id an eventual Undo (Follow/Block) or Accept/Reject
//! must reference. This module owns exactly Activity *generation* — no
//! delivery (`DeliveryService`, out of this task's boundary — no live caller
//! exists yet, the same situation tasks 2.1/2.2 documented for their own
//! siblings before being wired up by task 3.x) and no persistence beyond the
//! read-only actor-URI lookups Activity generation itself requires.
//!
//! ## Scope
//! Owns [`ActivityBuilder`] and its five `build_*` methods, plus the two
//! narrow resolution ports [`LocalActorLookup`]/[`RemoteActorLookup`] (see
//! below). Does not implement `FollowService` / `FollowRequestService` /
//! `BlockService` / `InboundHandler` (later tasks, the eventual callers that
//! hold a `Follow`/`Block` row's persisted `activity_id` and hand it to this
//! module), does not touch bootstrap/`AppState`/`DeliveryService` wiring, and
//! does not decide *which* recipients an Activity addresses (delivery
//! recipient derivation is federation-core's `DeliveryService`/`Recipient`
//! concern, never this module's).
//!
//! ## Task 4.1 addition (`InboundHandler`, `Boundary: InboundHandler,
//! Transitions, ActivityBuilder`): `LocalActorLookup`/`RemoteActorLookup`
//! widened to `-> impl Future<..> + Send`
//! [`LocalActorLookup::resolve_handle`]/[`RemoteActorLookup::
//! resolve_actor_uri`] originally declared plain `async fn` (this task's own
//! doc comment, below). Task 4.1's `SocialGraphInboundHandler::handle`
//! (`crate::social_graph::inbound`) implements federation-core's
//! `InboundActivityHandler::handle`, which must return `Pin<Box<dyn
//! Future<..> + Send + 'a>>` — and every generic caller nested inside that
//! box (including this module's own `build_*` methods, which the inbound
//! handler calls to build `Accept(Follow)`) needs `AL`/`AR`'s trait methods
//! to *declare* `Send`, not merely happen to produce a Send future for
//! whichever concrete type a given caller uses (Rust's `async fn`-in-trait
//! auto-trait leakage rule). Both trait methods therefore now read `fn
//! resolve_handle(&self, id: Id) -> impl Future<Output = ..> + Send` /
//! `fn resolve_actor_uri(&self, id: Id) -> impl Future<Output = ..> + Send`
//! — a minimal, additive signature widening; every existing `async fn`-bodied
//! implementation ([`ActorDirectory`]'s own [`LocalActorLookup`] impl,
//! [`PgRemoteActorLookup`]'s own [`RemoteActorLookup`] impl, and every test
//! double in `activity_builder/tests.rs`/`follow_service/tests.rs`/etc.)
//! satisfies the widened signature unchanged (an `async fn` impl body needs
//! no edit to satisfy an `-> impl Future<..> + Send`-declared trait method).
//! See `crate::social_graph::inbound`'s own doc comment ("`deliver:
//! BoxedDeliver`") for the fuller architecture discussion, including why the
//! *other* two trait boundaries `SocialGraphInboundHandler` depends on
//! (`crate::federation::LocalActorLookup`/`DeliverySink`) could not receive
//! the same treatment (out of this task's boundary — federation-core, not
//! `ActivityBuilder`).
//!
//! ## Deliberate deviation from design.md's literal Service Interface: `async fn ... -> Result<_, AppError>`, not sync/infallible
//! design.md's sketch (lines 458-464) writes every method as a plain
//! synchronous, infallible function:
//! ```text
//! pub fn build_follow(&self, follower: &AccountRef, followee: &AccountRef) -> (String, serde_json::Value);
//! pub fn build_accept(&self, target: &AccountRef, follow_activity_id: &str, source: &AccountRef) -> serde_json::Value;
//! pub fn build_reject(&self, target: &AccountRef, follow_activity_id: &str, source: &AccountRef) -> serde_json::Value;
//! pub fn build_block(&self, blocker: &AccountRef, blocked: &AccountRef) -> (String, serde_json::Value);
//! pub fn build_undo(&self, actor: &AccountRef, wrapped_activity_id: &str, wrapped: serde_json::Value) -> serde_json::Value;
//! ```
//! Every one of these methods needs to turn an [`AccountRef`] into the actual
//! URI string a canonical Activity's `actor`/`object` fields require. Unlike
//! `StatusActivityBuilder`'s identical-shaped gap (`src/statuses/
//! activity_builder.rs`'s own doc comment, "`ActorHandleLookup`: a narrow,
//! DB-free port for `Id -> Handle`"), which only ever resolves a *local*
//! actor (every status-related operation is triggered by an authenticated
//! local actor), this builder's `AccountRef` parameters may be **either**
//! [`AccountRef::Local`] **or** [`AccountRef::Remote`] — a `Follow`/`Block`'s
//! `follower`/`followee`/`blocker`/`blocked` can be any pairing of local and
//! remote accounts (Requirements 1.2, 1.3, 5.3, 5.5's whole point is that
//! local and remote targets produce the identical logical Activity). Both
//! resolution paths are genuinely async, DB-backed lookups with no sync,
//! infallible substitute:
//! - `AccountRef::Local(id)` resolves via [`crate::actor::ActorDirectory::resolve_actor_by_id`]
//!   (async, `Result<Option<ResolvedActor>, AppError>`) to a [`crate::actor::Handle`],
//!   then [`ActorUrls::actor_url`].
//! - `AccountRef::Remote(id)` resolves via
//!   [`crate::accounts::remote_repository::find_remote_by_id`] (async,
//!   `Result<Option<RemoteAccount>, AppError>`) to that row's `actor_uri`.
//!
//! So this module follows the exact same precedent
//! `StatusActivityBuilder`/`ActorHandleLookup` already established for the
//! narrower local-only case, applied to *both* variants: every `build_*`
//! method becomes `pub async fn ... -> Result<_, AppError>`, and actor-URI
//! resolution is pushed behind two narrow, mockable, DB-free ports —
//! [`LocalActorLookup`] and [`RemoteActorLookup`] — so this builder's own
//! unit tests need no real Postgres pool, exactly like
//! `StatusActivityBuilder`'s own testing strategy.
//!
//! ## `LocalActorLookup` / `RemoteActorLookup`: two narrow, DB-free ports
//! Mirroring `ActorHandleLookup`'s exact shape (`src/statuses/
//! activity_builder.rs`): [`LocalActorLookup::resolve_handle`] wraps
//! `ActorDirectory::resolve_actor_by_id` (`Id -> Handle`, blanket-implemented
//! for `ActorDirectory` itself, so a real caller just passes the directory
//! it already has); [`RemoteActorLookup::resolve_actor_uri`] wraps
//! `remote_repository::find_remote_by_id` (`Id -> actor_uri: String`). Since
//! `remote_repository` is this crate's established free-function-over-
//! `&PgPool` repository style (not a `&self` struct, per `RelationshipRepository`'s
//! and every other `*_repository.rs`'s own documented convention — tasks.md's
//! Implementation Notes, task 1.3), [`PgRemoteActorLookup`] is a minimal
//! newtype wrapping one `PgPool` to give `find_remote_by_id` an
//! implementation site for the trait, the same "adapt a free-function
//! repository behind a `&self` port" shape `AccountCountsContribution`
//! (`src/statuses/account_provider.rs`) already uses for the mirror-image
//! situation. Both `Ok(None)` results are mapped to a `404`-shaped
//! [`AppError`] (mirrors `ActorHandleLookup::resolve_handle`'s own "fail
//! loudly rather than silently drop" convention for the identical situation
//! — an `AccountRef` this builder is asked to resolve must already
//! correspond to a persisted account by the time any `build_*` method runs).
//!
//! ## JSON shape conventions (mirrors `StatusActivityBuilder`)
//! - Manual `serde_json::Map` construction, never `json!{}` — same
//!   precedent `StatusActivityBuilder`'s own doc comment documents
//!   (`src/federation/endpoints/document.rs`, `src/statuses/serializer.rs`).
//! - No `@context` stamping here — `DeliveryService::deliver` stamps it
//!   exactly once per delivery (`StatusActivityBuilder`'s own doc comment,
//!   "`@context` stamping"); this module only ever produces the raw,
//!   not-yet-canonicalized document a future `DeliveryRequest::activity`
//!   would expect.
//! - Activity `id` minting: every freshly-minted Activity `id` (the outer
//!   Follow/Block/Accept/Reject/Undo `id`, never the *referenced* id a
//!   caller passes in) is built from an injected `Arc<dyn IdGenerator>`
//!   rendered via `ActorUrls::object_url` under a locally-defined
//!   [`ACTIVITY_OBJECT_KIND`] (`"activities"`) — the same path segment
//!   `StatusActivityBuilder` already uses for the same logical concept (a
//!   generic Activity object, not a status); reusing it is deliberate, not
//!   an oversight, since `Id` values are unique crate-wide regardless of
//!   which `ObjectKind` they are rendered under.
//!
//! ## `Follow`'s `object` is the followee's actor URI, not a collection
//! Per plain ActivityPub semantics (and design.md's own "アクター/object URL
//! は `ActorUrls` から取得"), [`ActivityBuilder::build_follow`]'s `object`
//! field is the followee's plain actor URI string — never a followers
//! collection URL.
//!
//! ## `Accept`/`Reject`: object embeds the referenced Follow by id/type/actor/object
//! Requirement 2.3/2.4 and design.md's own "Accept/Reject は受信 Follow の id
//! を object に参照" call for the *received* Follow Activity's id to appear
//! in `Accept`/`Reject`'s `object`. This builder embeds a full `Follow`-
//! shaped object (`{"id": follow_activity_id, "type": "Follow", "actor":
//! <source actor URL>, "object": <target actor URL>}`) rather than a bare id
//! string: this is the real-world ActivityPub/Mastodon convention for
//! Accept/Reject(Follow) (a peer reconciling an Accept needs to know *whose*
//! Follow — actor/object — is being accepted, not just an opaque id it may
//! never have durably stored), and mirrors this same module's own
//! `build_undo` embedding convention (below) for consistency across every
//! "wraps a referenced Activity" method this builder exposes. `target` is
//! the account *sending* the Accept/Reject (the original Follow's
//! recipient, now the `actor` of Accept/Reject); `source` is the account
//! that originally sent the Follow (appears as the embedded Follow's own
//! `actor`).
//!
//! ## `Undo`: object embeds `wrapped` with its `id` forced to `wrapped_activity_id`
//! [`ActivityBuilder::build_undo`] takes both an already-built `wrapped`
//! Activity `Value` (typically the caller's own `build_follow`/`build_block`
//! output, rebuilt from the same `AccountRef` pair since this spec's
//! `Follow`/`Block` rows persist only the outbound `activity_id` string, not
//! the full original JSON — `model.rs`'s own doc comment) and the
//! `wrapped_activity_id` the relationship row actually persisted. Because a
//! freshly rebuilt `wrapped` Value would otherwise carry a *newly* minted
//! `id` (not the original one the row remembers), this builder unconditionally
//! overwrites `wrapped`'s `"id"` field with `wrapped_activity_id` before
//! embedding it as `Undo`'s `object` — making `wrapped_activity_id` the
//! single source of truth for what the emitted `Undo` actually references,
//! regardless of what id happened to be baked into `wrapped` itself. This is
//! exactly what Requirement 1.4/5.4 and this task's own completion condition
//! ("Undo が元 Activity を参照する") ask for and is what this module's test
//! suite asserts directly.

#[cfg(test)]
mod tests;

use std::future::Future;
use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::{Map, Value};
use sqlx::PgPool;

use crate::accounts::remote_repository::find_remote_by_id;
use crate::actor::{ActorDirectory, Handle};
use crate::domain::{AccountRef, Id};
use crate::error::AppError;
use crate::federation::urls::{ActorUrls, ObjectKind};
use crate::runtime::IdGenerator;

/// The URL path segment under which this builder mints Activity `id` URIs
/// (`ActorUrls::object_url`) — the same choice `StatusActivityBuilder`
/// already made for the identical logical concept. See this module's doc
/// comment ("JSON shape conventions") for why reusing it is deliberate.
const ACTIVITY_OBJECT_KIND: ObjectKind = ObjectKind::new("activities");

/// The narrow, DB-free `Id -> Handle` port for resolving a **local**
/// [`AccountRef::Local`] to its actor URL. See this module's doc comment
/// ("`LocalActorLookup` / `RemoteActorLookup`") for why this exists rather
/// than this builder holding `ActorDirectory` directly.
///
/// Declared as `-> impl Future<..> + Send` rather than a plain `async fn`
/// (task 4.1, `Boundary: InboundHandler` — a minimal, additive widening of
/// this already-implemented task 2.3 signature, the exact same situation
/// tasks.md's own Implementation Notes already document for
/// `Transitions::promote_pending`/`drop_pending`/`clear_block`): task 4.1's
/// `SocialGraphInboundHandler::handle` must return `Pin<Box<dyn Future<..> +
/// Send + 'a>>` (`InboundActivityHandler`'s own dyn-compatible contract,
/// federation-core), and every generic caller nested inside that boxed
/// future — including `ActivityBuilder<AL, AR>`'s own `build_*` methods,
/// which this trait backs — needs `AL::resolve_handle`'s future to be
/// unconditionally `Send`, not merely Send-for-the-concrete-types-a-given-
/// caller-happens-to-use (Rust's `async fn`-in-trait auto-trait leakage: a
/// generic caller may only assume what the trait signature itself
/// guarantees, regardless of what any particular impl's body would actually
/// support). `ActorDirectory`'s [`resolve_handle`](Self::resolve_handle) impl
/// body is unchanged — `async fn` impls satisfy an `-> impl Future<..> +
/// Send`-declared trait method without modification.
#[allow(async_fn_in_trait)]
pub trait LocalActorLookup: Send + Sync {
    /// Resolves `id` to its [`Handle`], failing with a `404`-shaped
    /// [`AppError`] if `id` no longer resolves to an existing local actor.
    fn resolve_handle(&self, id: Id) -> impl Future<Output = Result<Handle, AppError>> + Send;
}

impl LocalActorLookup for ActorDirectory {
    async fn resolve_handle(&self, id: Id) -> Result<Handle, AppError> {
        self.resolve_actor_by_id(id)
            .await?
            .map(|resolved| resolved.handle)
            .ok_or_else(|| {
                AppError::client(
                    StatusCode::NOT_FOUND,
                    format!("account id {id:?} does not resolve to an existing local actor"),
                )
            })
    }
}

/// The narrow, DB-free `Id -> actor_uri` port for resolving a **remote**
/// [`AccountRef::Remote`] to its actor URI. See this module's doc comment
/// ("`LocalActorLookup` / `RemoteActorLookup`") for why this exists rather
/// than this builder issuing raw SQL itself.
///
/// Declared as `-> impl Future<..> + Send` for the exact same reason (task
/// 4.1's `Send`-boxed `InboundActivityHandler::handle` contract) documented
/// on [`LocalActorLookup`]'s own doc comment, above.
#[allow(async_fn_in_trait)]
pub trait RemoteActorLookup: Send + Sync {
    /// Resolves `id` to its cached `actor_uri`, failing with a `404`-shaped
    /// [`AppError`] if `id` no longer resolves to a known remote account.
    fn resolve_actor_uri(&self, id: Id) -> impl Future<Output = Result<String, AppError>> + Send;
}

/// The real, Postgres-backed [`RemoteActorLookup`] — a minimal newtype
/// wrapping one `PgPool` so `remote_repository::find_remote_by_id`'s
/// free-function style has an implementation site for the trait (mirrors
/// `AccountCountsContribution`'s identical "adapt a free-function repository
/// behind a `&self` port" precedent, `src/statuses/account_provider.rs`).
#[derive(Debug, Clone)]
pub struct PgRemoteActorLookup {
    pool: PgPool,
}

impl PgRemoteActorLookup {
    /// Builds a [`PgRemoteActorLookup`] over `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl RemoteActorLookup for PgRemoteActorLookup {
    async fn resolve_actor_uri(&self, id: Id) -> Result<String, AppError> {
        find_remote_by_id(&self.pool, id)
            .await?
            .map(|remote| remote.actor_uri)
            .ok_or_else(|| {
                AppError::client(
                    StatusCode::NOT_FOUND,
                    format!("account id {id:?} does not resolve to a known remote account"),
                )
            })
    }
}

/// Generates the canonical Follow/Accept/Reject/Block/Undo Activities
/// (design.md's exact `ActivityBuilder` component; Requirements 1.2, 1.3,
/// 1.4, 2.3, 2.4, 5.3, 5.4). See this module's doc comment for the full
/// deviation rationale (async/fallible signatures) and JSON shape
/// conventions.
pub struct ActivityBuilder<L, R>
where
    L: LocalActorLookup,
    R: RemoteActorLookup,
{
    urls: ActorUrls,
    ids: Arc<dyn IdGenerator>,
    local: L,
    remote: R,
}

impl<L, R> ActivityBuilder<L, R>
where
    L: LocalActorLookup,
    R: RemoteActorLookup,
{
    /// Builds an `ActivityBuilder` over `urls` (actor/Activity URI
    /// construction), `ids` (fresh Activity `id` minting), `local` (local
    /// `AccountRef -> actor URL` resolution), and `remote` (remote
    /// `AccountRef -> actor URL` resolution).
    pub fn new(urls: ActorUrls, ids: Arc<dyn IdGenerator>, local: L, remote: R) -> Self {
        Self {
            urls,
            ids,
            local,
            remote,
        }
    }

    /// Mints a fresh Activity `id` URI (see this module's doc comment,
    /// "JSON shape conventions").
    fn mint_activity_id(&self) -> String {
        self.urls
            .object_url(ACTIVITY_OBJECT_KIND, self.ids.next_id())
    }

    /// Resolves `account`'s actor URL via whichever of [`LocalActorLookup`]/
    /// [`RemoteActorLookup`] matches its variant — the single point where
    /// this builder's local/remote symmetry (Requirements 1.3, 5.5, 10.3) is
    /// structurally enforced: every `build_*` method below calls this same
    /// helper regardless of which `AccountRef` variant it is given, so the
    /// emitted JSON shape never itself branches on locality.
    async fn resolve_url(&self, account: &AccountRef) -> Result<String, AppError> {
        match account {
            AccountRef::Local(id) => {
                let handle = self.local.resolve_handle(*id).await?;
                Ok(self.urls.actor_url(&handle))
            }
            AccountRef::Remote(id) => self.remote.resolve_actor_uri(*id).await,
        }
    }

    /// Generates a canonical `Follow` Activity: `follower` follows
    /// `followee` (Requirements 1.2, 1.3). `object` is `followee`'s plain
    /// actor URI (see this module's doc comment, "`Follow`'s `object`").
    /// Returns `(activity_id, json)` — the same freshly-minted id a caller
    /// must persist as the relationship row's `activity_id` for a later
    /// `Undo(Follow)` to reference (Requirement 1.4).
    pub async fn build_follow(
        &self,
        follower: &AccountRef,
        followee: &AccountRef,
    ) -> Result<(String, Value), AppError> {
        let follower_url = self.resolve_url(follower).await?;
        let followee_url = self.resolve_url(followee).await?;
        let id = self.mint_activity_id();

        let mut activity: Map<String, Value> = Map::new();
        activity.insert("id".to_string(), Value::String(id.clone()));
        activity.insert("type".to_string(), Value::String("Follow".to_string()));
        activity.insert("actor".to_string(), Value::String(follower_url));
        activity.insert("object".to_string(), Value::String(followee_url));

        Ok((id, Value::Object(activity)))
    }

    /// Generates a canonical `Accept` Activity, sent by `target` (the
    /// original Follow's recipient, now approving) referencing the received
    /// Follow (`follow_activity_id`) sent by `source` (Requirement 2.3). See
    /// this module's doc comment ("`Accept`/`Reject`: object embeds...") for
    /// the embedded object's exact shape.
    pub async fn build_accept(
        &self,
        target: &AccountRef,
        follow_activity_id: &str,
        source: &AccountRef,
    ) -> Result<Value, AppError> {
        self.build_accept_or_reject("Accept", target, follow_activity_id, source)
            .await
    }

    /// Generates a canonical `Reject` Activity, sent by `target` (the
    /// original Follow's recipient, now declining) referencing the received
    /// Follow (`follow_activity_id`) sent by `source` (Requirement 2.4). See
    /// this module's doc comment ("`Accept`/`Reject`: object embeds...") for
    /// the embedded object's exact shape.
    pub async fn build_reject(
        &self,
        target: &AccountRef,
        follow_activity_id: &str,
        source: &AccountRef,
    ) -> Result<Value, AppError> {
        self.build_accept_or_reject("Reject", target, follow_activity_id, source)
            .await
    }

    /// Shared body for [`Self::build_accept`]/[`Self::build_reject`] — the
    /// two differ only in their outer Activity `type`.
    async fn build_accept_or_reject(
        &self,
        activity_type: &str,
        target: &AccountRef,
        follow_activity_id: &str,
        source: &AccountRef,
    ) -> Result<Value, AppError> {
        let target_url = self.resolve_url(target).await?;
        let source_url = self.resolve_url(source).await?;

        let mut inner: Map<String, Value> = Map::new();
        inner.insert(
            "id".to_string(),
            Value::String(follow_activity_id.to_string()),
        );
        inner.insert("type".to_string(), Value::String("Follow".to_string()));
        inner.insert("actor".to_string(), Value::String(source_url));
        inner.insert("object".to_string(), Value::String(target_url.clone()));

        let mut activity: Map<String, Value> = Map::new();
        activity.insert("id".to_string(), Value::String(self.mint_activity_id()));
        activity.insert("type".to_string(), Value::String(activity_type.to_string()));
        activity.insert("actor".to_string(), Value::String(target_url));
        activity.insert("object".to_string(), Value::Object(inner));

        Ok(Value::Object(activity))
    }

    /// Generates a canonical `Block` Activity: `blocker` blocks `blocked`
    /// (Requirement 5.3). `object` is `blocked`'s plain actor URI (mirrors
    /// [`Self::build_follow`]'s identical shape). Returns
    /// `(activity_id, json)` — the id a caller must persist as the block
    /// row's `activity_id` for a later `Undo(Block)` to reference
    /// (Requirement 5.4).
    pub async fn build_block(
        &self,
        blocker: &AccountRef,
        blocked: &AccountRef,
    ) -> Result<(String, Value), AppError> {
        let blocker_url = self.resolve_url(blocker).await?;
        let blocked_url = self.resolve_url(blocked).await?;
        let id = self.mint_activity_id();

        let mut activity: Map<String, Value> = Map::new();
        activity.insert("id".to_string(), Value::String(id.clone()));
        activity.insert("type".to_string(), Value::String("Block".to_string()));
        activity.insert("actor".to_string(), Value::String(blocker_url));
        activity.insert("object".to_string(), Value::String(blocked_url));

        Ok((id, Value::Object(activity)))
    }

    /// Generates a canonical `Undo` Activity by `actor`, wrapping `wrapped`
    /// (a caller-rebuilt `Follow`/`Block` Activity) as `object`, with
    /// `wrapped`'s own `"id"` forcibly overwritten to `wrapped_activity_id`
    /// — the id the relationship row actually persisted (Requirements 1.4,
    /// 5.4). See this module's doc comment ("`Undo`: object embeds
    /// `wrapped`...") for why this overwrite, rather than trusting
    /// `wrapped`'s own id, is what makes the reference correct.
    pub async fn build_undo(
        &self,
        actor: &AccountRef,
        wrapped_activity_id: &str,
        wrapped: Value,
    ) -> Result<Value, AppError> {
        let actor_url = self.resolve_url(actor).await?;

        let mut object = match wrapped {
            Value::Object(map) => map,
            other => {
                // Defensive fallback: a non-object `wrapped` (not produced by
                // this builder's own `build_follow`/`build_block`) still gets
                // a well-formed object with at least the referenced id.
                let mut map = Map::new();
                map.insert("value".to_string(), other);
                map
            }
        };
        object.insert(
            "id".to_string(),
            Value::String(wrapped_activity_id.to_string()),
        );

        let mut activity: Map<String, Value> = Map::new();
        activity.insert("id".to_string(), Value::String(self.mint_activity_id()));
        activity.insert("type".to_string(), Value::String("Undo".to_string()));
        activity.insert("actor".to_string(), Value::String(actor_url));
        activity.insert("object".to_string(), Value::Object(object));

        Ok(Value::Object(activity))
    }
}
