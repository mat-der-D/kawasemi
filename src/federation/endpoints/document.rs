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

use crate::actor::Handle;
use crate::error::AppError;

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
