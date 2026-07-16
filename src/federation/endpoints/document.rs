//! `ObjectDocumentProvider` / `OutboxSource` (design.md `#### ObjectDocumentProvider
//! / OutboxSource（下流供給の委譲境界）` -> Service Interface; Requirements
//! 6.2, 6.6, 8.1, 8.2, 8.3; task 3.5, `Boundary: ObjectDocumentProvider,
//! OutboxSource`): the downstream-supply delegation boundary for local
//! objects/collections and outbox contents.
//!
//! This spec does not own Activity/object bodies (Non-Goals) —
//! `ActivityPubDocumentBuilder` (task 3.6, added to this same file later per
//! design.md's File Structure Plan) builds only the actor representation and
//! the outbox "container" (`OrderedCollectionPage` structure/paging); the
//! *contents* (individual objects, individual outbox items) are supplied by
//! whatever downstream spec (statuses-core, etc.) registers here. Until a
//! downstream spec registers anything, both registries must answer safely:
//! `ObjectDocumentRegistry::resolve` always returns `Ok(None)` (treated as
//! 404 by the caller, Requirement 6.6), and `OutboxSourceRegistry::collect`
//! always returns `Ok(vec![])` (an empty outbox, consistent with Requirement
//! 8.3's "no out-of-scope items" — nothing registered means nothing in
//! scope either).
//!
//! ## `ObjectDocumentRegistry`: ordered list, first match wins
//! Unlike `InboundActivityDispatcher` (task 3.2), which is a type-keyed
//! multimap that fans out to *every* matching handler, this registry is a
//! plain `Vec<Arc<dyn ObjectDocumentProvider>>` tried **in registration
//! order**: each provider's `can_resolve` is a pure ownership predicate over
//! a URL, and the first provider that claims a URL is delegated to
//! exclusively — its own `resolve` result (including `Ok(None)`, "I own this
//! URL space but this particular object doesn't exist") is returned as-is,
//! without falling through to later providers. This matches design.md's own
//! wording: "`ApGet` ハンドラは登録順に最初に一致したプロバイダへ委譲する".
//! A URL nobody claims (empty registry, or no registered provider's
//! `can_resolve` returns `true`) falls through to the registry's own safe
//! default, `Ok(None)`.
//!
//! ## `OutboxSourceRegistry`: fan-out collection, not delegation
//! Multiple downstream specs can each contribute outbox-eligible Activities
//! for the same actor (design.md's example: statuses-core's Create/
//! Announce), so `OutboxSourceRegistry::collect` calls **every** registered
//! `OutboxSource`'s `outbox_page` for the requested actor/page and returns
//! the `Vec<OutboxItemsPage>` of all their individual results untouched.
//! Merging/ordering those into one page is `ActivityPubDocumentBuilder`'s
//! job (task 3.6, out of this task's `_Boundary: ObjectDocumentProvider,
//! OutboxSource_`), not this registry's — this registry's only
//! responsibility is to collect, never to merge.
//!
//! ## Why boxed futures instead of literal `async fn` in the traits
//! design.md prints both `ObjectDocumentProvider::resolve` and
//! `OutboxSource::outbox_page` as plain `async fn` in its Service Interface
//! sketch. Both registries here must hold a *heterogeneous* collection of
//! implementations behind `Arc<dyn ObjectDocumentProvider>` /
//! `Arc<dyn OutboxSource>` (design.md's own `register` signatures take
//! exactly these `Arc<dyn _>` types) for actual dynamic dispatch, and a
//! trait with a literal `async fn` method is not dyn-compatible in Rust. As
//! with `InboundActivityDispatcher::register`'s `Arc<dyn
//! InboundActivityHandler>` (task 3.2, `src/federation/inbound/dispatcher.rs`),
//! both methods are written here in the equivalent desugared
//! `Pin<Box<dyn Future<...> + Send + '_>>` form — the same transformation the
//! `async-trait` crate's macro performs automatically, without adding that
//! (or any) new dependency. Callers still simply `.await` these methods;
//! only implementors write `Box::pin(async move { .. })` in the body (see
//! this module's test stub providers/sources for the pattern).
//!
//! ## `PageCursor`: not defined anywhere else in this spec
//! design.md uses `PageCursor` as a parameter/field type
//! (`outbox_page(actor: &Handle, page: PageCursor)`,
//! `OutboxItemsPage { next: Option<PageCursor> }`) but never defines its
//! shape. It is **not** the same thing as api-foundation's `Cursor` trait /
//! `PageCursors` struct (`src/api/pagination.rs`) — that toolkit is
//! Mastodon REST API's own `Link`-header-based pagination convention for a
//! completely different endpoint family (`max_id`/`since_id`/`min_id`
//! semantics over a `Page<T>`), unrelated to this ActivityPub
//! `OrderedCollectionPage` boundary.
//!
//! This task's job is a pass-through registry, not the actual
//! outbox-page-building logic (task 3.6's `ActivityPubDocumentBuilder`), so
//! `PageCursor` only needs to be a minimal, opaque type that (a) a test can
//! construct, (b) is handed to registered `OutboxSource`s untouched — this
//! registry never inspects or parses it — and (c) can appear as
//! `Option<PageCursor>` in `OutboxItemsPage::next`.
//!
//! Chosen shape: `PageCursor(pub Option<String>)`, where `None` means
//! "start from the head of the collection" (the first page — design.md's
//! trait signature takes `page: PageCursor` unconditionally, not
//! `Option<PageCursor>`, so the "no cursor yet" case has to live *inside*
//! `PageCursor` itself rather than being expressed by omitting the
//! parameter) and `Some(token)` is an opaque continuation token whose
//! encoding is entirely up to whatever later builds real cursor tokens
//! (task 3.6 / concrete `OutboxSource` implementors) — this module never
//! looks inside it. [`PageCursor::start`] and [`PageCursor::token`] are
//! small constructors documenting each case; nothing here parses or
//! validates a token's contents.

#[cfg(test)]
mod tests;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::{Map, Value};

use crate::actor::{ActorDirectory, ActorType, Handle, ResolvedActor};
use crate::error::AppError;
use crate::federation::jsonld::ACTIVITYSTREAMS_CONTEXT;
use crate::federation::urls::ActorUrls;

/// An opaque outbox-page cursor, passed through
/// [`OutboxSourceRegistry::collect`] untouched. See this module's doc
/// comment ("`PageCursor`: not defined anywhere else in this spec") for why
/// this shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageCursor(pub Option<String>);

impl PageCursor {
    /// The first page: no continuation token yet.
    pub fn start() -> Self {
        PageCursor(None)
    }

    /// A continuation token previously returned via
    /// [`OutboxItemsPage::next`].
    pub fn token(token: impl Into<String>) -> Self {
        PageCursor(Some(token.into()))
    }
}

/// A downstream provider of local object/collection AP JSON representations
/// (design.md's exact `ObjectDocumentProvider` interface). See this module's
/// doc comment for the ownership-predicate / first-match-wins contract this
/// trait is used under.
pub trait ObjectDocumentProvider: Send + Sync {
    /// Whether this provider owns `url` (a pure ownership predicate, no side
    /// effects).
    fn can_resolve(&self, url: &str) -> bool;

    /// Returns the AP JSON representation for a `url` this provider owns.
    /// `Ok(None)` means the URL is in this provider's space but no such
    /// object currently exists (treated as 404 by the caller). See this
    /// trait's doc comment for why this returns a boxed future rather than
    /// a literal `async fn`.
    fn resolve<'a>(
        &'a self,
        url: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<serde_json::Value>, AppError>> + Send + 'a>>;
}

/// The multi-provider registry that tries each registered
/// [`ObjectDocumentProvider`]'s `can_resolve` in registration order and
/// delegates to the first match (design.md's exact
/// `ObjectDocumentRegistry` Service Interface). See this module's doc
/// comment for the full ordered/first-match/safe-default contract.
#[derive(Default)]
pub struct ObjectDocumentRegistry {
    providers: Vec<Arc<dyn ObjectDocumentProvider>>,
}

impl ObjectDocumentRegistry {
    /// Builds an empty registry (no providers registered yet).
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `provider`. Providers are tried in the order they were
    /// registered (see this module's doc comment).
    pub fn register(&mut self, provider: Arc<dyn ObjectDocumentProvider>) {
        self.providers.push(provider);
    }

    /// Resolves `url` by delegating to the first registered provider whose
    /// `can_resolve(url)` returns `true`, returning that provider's own
    /// `resolve` result unchanged. If no provider is registered, or none of
    /// them claim `url`, returns `Ok(None)` (Requirement 6.6's "not found",
    /// never an error just because nobody owns this URL).
    pub async fn resolve(&self, url: &str) -> Result<Option<serde_json::Value>, AppError> {
        for provider in &self.providers {
            if provider.can_resolve(url) {
                return provider.resolve(url).await;
            }
        }
        Ok(None)
    }
}

/// One page's worth of outbox Activities supplied by a single
/// [`OutboxSource`] (design.md's exact `OutboxItemsPage` interface).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxItemsPage {
    pub items: Vec<serde_json::Value>,
    pub next: Option<PageCursor>,
}

/// A downstream supplier of Activities to include in a local actor's outbox
/// page (design.md's exact `OutboxSource` interface). See this module's doc
/// comment for the fan-out-not-delegation contract [`OutboxSourceRegistry`]
/// uses this trait under.
pub trait OutboxSource: Send + Sync {
    /// Supplies this source's contribution to `actor`'s outbox page at
    /// `page`. A source with nothing applicable to `actor` (or unsupported
    /// entirely) returns an empty [`OutboxItemsPage`], never an error. See
    /// this trait's doc comment for why this returns a boxed future rather
    /// than a literal `async fn`.
    fn outbox_page<'a>(
        &'a self,
        actor: &'a Handle,
        page: PageCursor,
    ) -> Pin<Box<dyn Future<Output = Result<OutboxItemsPage, AppError>> + Send + 'a>>;
}

/// The registry that collects every registered [`OutboxSource`]'s
/// contribution for a given actor/page (design.md's exact
/// `OutboxSourceRegistry` Service Interface). See this module's doc comment
/// for the full collect-not-merge contract.
#[derive(Default)]
pub struct OutboxSourceRegistry {
    sources: Vec<Arc<dyn OutboxSource>>,
}

impl OutboxSourceRegistry {
    /// Builds an empty registry (no sources registered yet).
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `source`. All registered sources are queried on every
    /// [`OutboxSourceRegistry::collect`] call (see this module's doc
    /// comment).
    pub fn register(&mut self, source: Arc<dyn OutboxSource>) {
        self.sources.push(source);
    }

    /// Calls `outbox_page(actor, page)` on every registered
    /// [`OutboxSource`] and returns the `Vec` of all their individual
    /// results, in registration order. Merging/ordering into a single page
    /// is the caller's job (`ActivityPubDocumentBuilder`, task 3.6), not
    /// this registry's. An empty registry returns `Ok(vec![])`.
    pub async fn collect(
        &self,
        actor: &Handle,
        page: PageCursor,
    ) -> Result<Vec<OutboxItemsPage>, AppError> {
        let mut pages = Vec::with_capacity(self.sources.len());
        for source in &self.sources {
            pages.push(source.outbox_page(actor, page.clone()).await?);
        }
        Ok(pages)
    }
}

/// Builds ActivityPub document representations for local actors and their
/// outbox (design.md's exact `ActivityPubDocumentBuilder` Service Interface;
/// Requirements 6.1, 6.2, 6.5, 8.1, 8.2, 8.3; task 3.6, `Boundary:
/// ActivityPubDocumentBuilder`).
///
/// This is strictly a *container* builder, not a content owner (this spec's
/// own Non-Goals, restated in this module's own doc comment):
/// [`build_actor_document`](Self::build_actor_document) shapes the actor
/// representation from already-resolved actor-model data ([`ResolvedActor`],
/// [`crate::actor::ActorPublicKey`] via [`ActorDirectory`]) and
/// [`ActorUrls`]'s own URL construction, while
/// [`build_outbox_page`](Self::build_outbox_page) shapes only the
/// `OrderedCollectionPage` *container* (structure/paging) -- the Activity
/// bodies it bundles come entirely from whatever [`OutboxSource`]s are
/// registered on this builder's own [`OutboxSourceRegistry`] (task 3.5),
/// collected and merged here, never invented or extended by this builder
/// itself.
///
/// ## Actor document shape (design decision, Requirements 6.1, 6.5)
/// design.md only requires `id`/`inbox`/`outbox`/public key to be present and
/// owner information to be absent. This builder emits a conventional
/// ActivityPub actor document: `@context`, `id`, `type` (from
/// [`ActorType`]), `preferredUsername` (the handle), `name`
/// (`display_name`), `summary`, `inbox`, `outbox`, and -- only when
/// [`ActorDirectory::actor_public_key`] returns `Some` -- a `publicKey`
/// object (`id`/`owner`/`publicKeyPem`). An actor with no active signing key
/// gets no `publicKey` field at all (never an error, never a fabricated
/// key): [`ResolvedActor`]/[`crate::actor::ActorPublicKey`] already carry no
/// owner-identifying field (actor-model's own structural guarantee,
/// `src/actor/model.rs`'s own exhaustive-destructuring tests), so this
/// builder cannot leak owner information even by accident -- nothing it
/// reads from either type has an owner concept to leak. The `publicKey.owner`
/// field below is the ActivityPub-standard "which actor's key is this"
/// back-reference (a public actor URL) -- an entirely different concept from
/// this spec's "オーナー情報" (management-layer administrator identity,
/// Requirement 6.5), which never enters this method's inputs at all.
///
/// ## Outbox page merge (design decision, Requirements 8.1, 8.2, 8.3)
/// [`OutboxSourceRegistry::collect`] (task 3.5) returns one
/// [`OutboxItemsPage`] per registered source, uncombined. This builder's own
/// job (8.1's "順序付きコレクション", 8.2's per-page contract, 8.3's
/// "範囲外除外") is to:
/// - bundle every collected source's `items` into one `orderedItems` array
///   -- exactly their union, nothing invented or dropped (an empty registry,
///   or all-empty sources, yields an empty `orderedItems`, never a
///   fabricated one);
/// - order that union chronologically by each item's own top-level
///   `"published"` string field, compared as a plain lexicographic string --
///   sufficient for this spec's own ISO-8601/RFC-3339 convention without
///   pulling in a date-parsing crate feature this crate does not otherwise
///   enable. Items missing `"published"` (or where it is present but not a
///   JSON string) sort **after** every item that has one, preserving their
///   original (registration-then-page) relative order among themselves and
///   among any other missing-`published` items ([`Vec::sort_by`] is a stable
///   sort);
/// - propagate the **first** non-`None` `next` cursor reported by any
///   registered source, in registration order, as this page's own `next`.
///   design.md does not specify multi-source cursor composition; until more
///   than one real [`OutboxSource`] implementor exists, this is a defensible
///   MVP choice -- full composition (e.g. a compound cursor encoding every
///   source's own position) is out of this task's scope.
pub struct ActivityPubDocumentBuilder {
    urls: ActorUrls,
    actor_directory: Arc<ActorDirectory>,
    outbox_sources: OutboxSourceRegistry,
}

impl ActivityPubDocumentBuilder {
    /// Builds a document builder from its three dependencies (design.md's
    /// Components table: `ActorUrls, JsonLdCodec, ActorDirectory, OutboxSource
    /// (P0)` -- `JsonLdCodec` enters only as the [`ACTIVITYSTREAMS_CONTEXT`]
    /// constant this module stamps onto every document it builds, not as a
    /// stored field).
    pub fn new(
        urls: ActorUrls,
        actor_directory: Arc<ActorDirectory>,
        outbox_sources: OutboxSourceRegistry,
    ) -> Self {
        Self {
            urls,
            actor_directory,
            outbox_sources,
        }
    }

    /// Builds `actor`'s ActivityPub actor document (Requirements 6.1, 6.5).
    /// See this type's doc comment ("Actor document shape") for the exact
    /// JSON shape and the no-active-key/no-`publicKey`-field decision.
    pub async fn build_actor_document(
        &self,
        actor: &ResolvedActor,
    ) -> Result<serde_json::Value, AppError> {
        let actor_url = self.urls.actor_url(&actor.handle);

        let mut fields: Map<String, Value> = Map::new();
        fields.insert(
            "@context".to_string(),
            Value::String(ACTIVITYSTREAMS_CONTEXT.to_string()),
        );
        fields.insert("id".to_string(), Value::String(actor_url.clone()));
        fields.insert(
            "type".to_string(),
            Value::String(actor_type_label(actor.actor_type).to_string()),
        );
        fields.insert(
            "preferredUsername".to_string(),
            Value::String(actor.handle.as_str().to_string()),
        );
        fields.insert(
            "name".to_string(),
            Value::String(actor.display_name.clone()),
        );
        fields.insert("summary".to_string(), Value::String(actor.summary.clone()));
        fields.insert(
            "inbox".to_string(),
            Value::String(self.urls.inbox_url(&actor.handle)),
        );
        fields.insert(
            "outbox".to_string(),
            Value::String(self.urls.outbox_url(&actor.handle)),
        );

        // Requirement 6.1: include the public key when this actor has an
        // active one. Requirement 6.5: never include owner information --
        // `actor`/the fetched key are already structurally owner-free (see
        // this type's doc comment), so there is nothing owner-identifying
        // to omit beyond simply not inventing a new field for it.
        if let Some(key) = self.actor_directory.actor_public_key(actor.id).await? {
            let mut public_key: Map<String, Value> = Map::new();
            public_key.insert(
                "id".to_string(),
                Value::String(self.urls.key_id(&actor.handle)),
            );
            // The ActivityPub-standard back-reference to the actor this key
            // belongs to (a public actor URL) -- see this type's doc comment
            // ("Actor document shape") for why this is not the same concept
            // as this spec's "オーナー情報" (Requirement 6.5).
            public_key.insert("owner".to_string(), Value::String(actor_url));
            public_key.insert(
                "publicKeyPem".to_string(),
                Value::String(key.public_key_pem),
            );
            fields.insert("publicKey".to_string(), Value::Object(public_key));
        }

        Ok(Value::Object(fields))
    }

    /// Builds one page of `actor`'s outbox as an `OrderedCollectionPage`
    /// container (Requirements 8.1, 8.2, 8.3). See this type's doc comment
    /// ("Outbox page merge") for the exact merge/ordering/next-cursor rules.
    pub async fn build_outbox_page(
        &self,
        actor: &Handle,
        page: PageCursor,
    ) -> Result<serde_json::Value, AppError> {
        let collected = self.outbox_sources.collect(actor, page.clone()).await?;

        // Bundle every source's items into one union -- nothing invented,
        // nothing dropped (Requirement 8.3) -- then order the union
        // chronologically (Requirement 8.1). `sort_by` is stable, so this
        // also satisfies the documented "preserve relative order among
        // missing-`published` items" fallback.
        let mut items: Vec<Value> = Vec::new();
        for source_page in &collected {
            items.extend(source_page.items.iter().cloned());
        }
        items.sort_by(|a, b| outbox_item_sort_key(a).cmp(&outbox_item_sort_key(b)));

        // MVP next-cursor rule (see this type's doc comment, "Outbox page
        // merge"): the first registered source to report a `next` wins.
        let next = collected
            .into_iter()
            .find_map(|source_page| source_page.next);

        let outbox_url = self.urls.outbox_url(actor);
        let mut fields: Map<String, Value> = Map::new();
        fields.insert(
            "@context".to_string(),
            Value::String(ACTIVITYSTREAMS_CONTEXT.to_string()),
        );
        fields.insert(
            "id".to_string(),
            Value::String(page_url(&outbox_url, &page)),
        );
        fields.insert(
            "type".to_string(),
            Value::String("OrderedCollectionPage".to_string()),
        );
        fields.insert("partOf".to_string(), Value::String(outbox_url.clone()));
        fields.insert("orderedItems".to_string(), Value::Array(items));
        if let Some(next_cursor) = next {
            fields.insert(
                "next".to_string(),
                Value::String(page_url(&outbox_url, &next_cursor)),
            );
        }

        Ok(Value::Object(fields))
    }
}

/// Maps `actor_type` to the ActivityPub actor-object `type` string this spec
/// uses (Requirement 6.1): [`ActorType::Person`] -> `"Person"`,
/// [`ActorType::Service`] -> `"Service"` (ActivityStreams' own
/// automated-actor convention).
fn actor_type_label(actor_type: ActorType) -> &'static str {
    match actor_type {
        ActorType::Person => "Person",
        ActorType::Service => "Service",
    }
}

/// This outbox page merge's chronological sort key (see
/// [`ActivityPubDocumentBuilder`]'s doc comment, "Outbox page merge"): `(0,
/// published)` for an item with a top-level `"published"` string field
/// (compared lexicographically via the tuple's second element), `(1, "")`
/// for one without -- so every `published`-bearing item sorts before every
/// one that lacks it, and [`Vec::sort_by`]'s stability preserves each
/// bucket's original relative order.
fn outbox_item_sort_key(item: &Value) -> (u8, &str) {
    match item.get("published").and_then(Value::as_str) {
        Some(published) => (0, published),
        None => (1, ""),
    }
}

/// Builds the URL this builder uses to address one outbox page -- its own
/// `id`, and any `next` cursor's URL: `{outbox_url}?page=<token>` for a
/// continuation token, or `{outbox_url}?page=true` for the head page (no
/// token yet) -- mirrors Mastodon/ActivityPub's own `?page=` outbox-paging
/// convention. Not itself pinned by any requirement design.md states; kept
/// private (never parsed back by this builder itself, only by whatever later
/// task wires an incoming `?page=` query parameter to a [`PageCursor`]).
fn page_url(outbox_url: &str, page: &PageCursor) -> String {
    match &page.0 {
        Some(token) => format!("{outbox_url}?page={token}"),
        None => format!("{outbox_url}?page=true"),
    }
}
