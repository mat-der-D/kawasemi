//! Delegation ports (design.md "Delegation Port / 委譲境界層" ->
//! `ports（AccountStatusesProvider / RelationshipStateProvider /
//! AccountCountsProvider）`, Requirements 4.2, 4.3, 4.4, 4.5, 5.3, 5.4, 1.1;
//! task 1.3, `Boundary: ports`).
//!
//! Scope: this module owns exactly the three downstream-owned-information
//! delegation boundaries design.md's ports component names —
//! [`AccountStatusesProvider`] (Status pages, owned by statuses-core),
//! [`RelationshipStateProvider`] (follow/mute/block flags, owned by
//! social-graph), and [`AccountCountsProvider`] (follower/following/status
//! counts, split between social-graph and statuses-core) — plus each
//! trait's built-in default implementation ([`EmptyStatusesProvider`] /
//! [`NoRelationshipProvider`] / [`ZeroCountsProvider`], design.md's exact
//! names) and the swap-in delegation registry ([`AccountPortsRegistry`]).
//! No real (DB/network-backed) implementation of any of the three traits
//! lives here — those are statuses-core's/social-graph's own later tasks
//! (design.md: "本 spec は port の **定義と既定実装** のみ所有。実装供給は
//! statuses-core / social-graph"); each default implementation is a
//! zero-field unit struct that cannot reach a `PgPool` or network client
//! even by accident, which is what lets this module's tests prove the
//! "no DB/network touch" postcondition structurally rather than just
//! behaviorally.
//!
//! ## Why boxed futures, not design.md's literal `async fn` sketch
//! design.md's Service Interface excerpt writes each trait method as a
//! plain `async fn` (e.g. `async fn list_statuses(&self, q: &StatusesQuery)
//! -> Result<Page<serde_json::Value>, AppError>;`). Taken literally, that
//! makes the trait *not* `dyn`-compatible (Rust's native
//! `async fn`-in-trait desugars to an opaque per-impl associated type,
//! which cannot be named in a trait object) — the same non-object-safety
//! this crate's [`crate::media::store::MediaStore`] port already documents
//! for its own `async fn` methods. `MediaStore` sidesteps that by staying
//! generic (`MediaService<S: MediaStore>`), never boxed as `dyn
//! MediaStore`. That escape hatch is not available here: design.md is
//! explicit that "レジストリは `AppState` に保持し bootstrap が既定で初期化、
//! 下流が差し替え" — a downstream spec (statuses-core/social-graph) must be
//! able to swap in its own implementation *after* `AccountPortsRegistry` is
//! already constructed and possibly already live inside `AppState`, which
//! requires runtime (`dyn`) dispatch, not a compile-time generic parameter.
//! This module therefore follows this crate's other precedent for a
//! runtime-swappable async port —
//! `crate::federation::endpoints::document::ObjectDocumentProvider` — and
//! writes each trait method to return a boxed future
//! (`Pin<Box<dyn Future<Output = ...> + Send + 'a>>`) instead of a literal
//! `async fn`, which keeps `Arc<dyn AccountStatusesProvider>` (etc.)
//! constructible. This is a deliberate, documented deviation from
//! design.md's excerpted signature shape (method names, parameter names/
//! types, and return `Result` types are otherwise unchanged) — flagged in
//! this task's status report CONCERNS for reviewer confirmation, mirroring
//! the precedent `media/store.rs`'s own doc comment sets for a similar
//! design.md-vs-object-safety deviation.
//!
//! ## Registry shape: one replaceable slot per port, not an ordered/fan-out list
//! `ObjectDocumentRegistry` (this crate's other delegation-registry
//! precedent) holds an ordered `Vec` of providers and delegates to the
//! first one whose ownership predicate matches — appropriate there because
//! multiple independent providers can each own a different slice of URL
//! space. That shape does not fit here: design.md's own wording is "下流が
//! **差し替える**" (downstream *replaces* the value), not "下流が追加する"
//! (downstream *adds* a value), and each of the three ports has exactly one
//! eventual real owner (statuses-core for `AccountStatusesProvider`;
//! social-graph for `RelationshipStateProvider`; both, but never
//! concurrently, for `AccountCountsProvider`) rather than many competing
//! claimants. [`AccountPortsRegistry`] therefore holds exactly one
//! replaceable slot per port — `Arc<RwLock<Arc<dyn Trait>>>`, defaulting to
//! the built-in default implementation, swapped wholesale by a `set_*`
//! call — rather than a list with first-match resolution. It still reuses
//! `ObjectDocumentRegistry`'s interior-mutability idiom for the same
//! structural reason that registry adopted it: `AppState` is
//! immutable-after-construction (this crate's steering), yet a downstream
//! spec's registration must be able to happen after this registry is
//! already inside a live `AppState` (task 1.4's wiring will hand out
//! cloned handles the same way `FederationModule` does), so `set_*` takes
//! `&self` rather than `&mut self`, and [`AccountPortsRegistry`] derives
//! `Clone` (cheap: it clones three `Arc`s, not their contents). Each
//! read-then-await accessor (`list_statuses`/`relationships`/`counts`)
//! clones the currently-registered `Arc<dyn Trait>` out of its `RwLock`
//! read guard and lets the guard drop *before* awaiting the provider's own
//! future, for the identical `!Send`-across-`.await` reason
//! `ObjectDocumentRegistry::resolve`'s doc comment explains.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

use crate::accounts::model::{AccountCounts, RelationshipView};
use crate::api::pagination::{Page, PageParams};
use crate::domain::{AccountRef, Id};
use crate::error::AppError;

/// The query context passed to [`AccountStatusesProvider::list_statuses`]
/// (design.md ports Service Interface): which account's statuses, the
/// viewer (if any, for visibility filtering), raw pagination params, and
/// the `pinned`/`only_media`/`exclude_replies`/`exclude_reblogs` filters
/// Requirement 4.4 requires be carried through to the provider unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusesQuery {
    pub target: AccountRef,
    pub viewer: Option<Id>,
    pub page: PageParams,
    pub pinned: bool,
    pub only_media: bool,
    pub exclude_replies: bool,
    pub exclude_reblogs: bool,
}

/// Supplies the Status page for `query.target` (design.md: "対象アカウント・
/// ページネーション・絞り込み/可視性コンテキストを受け、Status 表現（不透明
/// JSON 値）のページを返す"). Status bodies are deliberately opaque
/// (`serde_json::Value`) — this spec does not own or interpret Status
/// shape, only routes to whichever downstream spec (statuses-core) supplies
/// it. Default: [`EmptyStatusesProvider`] (Requirement 4.3).
///
/// See this module's doc comment ("Why boxed futures") for why this method
/// returns a boxed future instead of design.md's literal `async fn` sketch.
pub trait AccountStatusesProvider: Send + Sync {
    fn list_statuses<'a>(
        &'a self,
        query: &'a StatusesQuery,
    ) -> Pin<Box<dyn Future<Output = Result<Page<serde_json::Value>, AppError>> + Send + 'a>>;
}

/// Supplies relationship flags between `viewer` and each of `targets`
/// (design.md: "閲覧者アクターと対象 id 群から関係フラグを返す"). Default:
/// [`NoRelationshipProvider`] (Requirement 5.4).
pub trait RelationshipStateProvider: Send + Sync {
    fn relationships<'a>(
        &'a self,
        viewer: Id,
        targets: &'a [AccountRef],
    ) -> Pin<Box<dyn Future<Output = Result<Vec<RelationshipView>, AppError>> + Send + 'a>>;
}

/// Supplies `target`'s counts (design.md: "対象アカウントのカウントを返す").
/// Real truth sources are split across downstream specs (followers/
/// following: social-graph; statuses/last_status_at: statuses-core) but
/// this port does not distinguish which sub-count came from where — a
/// caller gets one [`AccountCounts`] value. Default: [`ZeroCountsProvider`]
/// (Requirement 1.1's counts default).
pub trait AccountCountsProvider: Send + Sync {
    fn counts<'a>(
        &'a self,
        target: &'a AccountRef,
    ) -> Pin<Box<dyn Future<Output = Result<AccountCounts, AppError>> + Send + 'a>>;
}

/// design.md's exact default-implementation name for
/// [`AccountStatusesProvider`]. A zero-field unit struct: it cannot hold a
/// `PgPool`, HTTP client, or any other DB/network handle, so "touches no
/// DB/network" (Requirement 4.3's postcondition) holds structurally, not
/// just behaviorally. Always returns an empty [`Page`] (no items, no
/// prev/next cursor) regardless of `query`.
#[derive(Debug, Clone, Copy, Default)]
pub struct EmptyStatusesProvider;

impl AccountStatusesProvider for EmptyStatusesProvider {
    fn list_statuses<'a>(
        &'a self,
        _query: &'a StatusesQuery,
    ) -> Pin<Box<dyn Future<Output = Result<Page<serde_json::Value>, AppError>> + Send + 'a>> {
        Box::pin(async move {
            Ok(Page {
                items: Vec::new(),
                prev_cursor: None,
                next_cursor: None,
            })
        })
    }
}

/// Recovers the [`Id`] a `RelationshipView` needs from an [`AccountRef`],
/// regardless of local/remote-ness (a `RelationshipView`'s `id` is just
/// "which target account this flag set is about", not itself a local/
/// remote discriminant).
fn account_ref_id(account_ref: &AccountRef) -> Id {
    match *account_ref {
        AccountRef::Local(id) => id,
        AccountRef::Remote(id) => id,
    }
}

/// design.md's exact default-implementation name for
/// [`RelationshipStateProvider`]. A zero-field unit struct (see
/// [`EmptyStatusesProvider`]'s doc comment for why that structurally proves
/// "no DB/network touch"). Always returns "no relationship" (Requirement
/// 5.4: every boolean flag `false`, every count 0, `languages` empty,
/// `note` empty) for every requested target, ignoring `viewer` entirely.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoRelationshipProvider;

impl NoRelationshipProvider {
    /// Builds the Requirement-5.4 "no relationship" value for one target
    /// account id.
    fn no_relationship(id: Id) -> RelationshipView {
        RelationshipView {
            id,
            following: false,
            showing_reblogs: false,
            notifying: false,
            languages: Vec::new(),
            followed_by: false,
            blocking: false,
            blocked_by: false,
            muting: false,
            muting_notifications: false,
            requested: false,
            requested_by: false,
            domain_blocking: false,
            endorsed: false,
            note: String::new(),
        }
    }
}

impl RelationshipStateProvider for NoRelationshipProvider {
    fn relationships<'a>(
        &'a self,
        _viewer: Id,
        targets: &'a [AccountRef],
    ) -> Pin<Box<dyn Future<Output = Result<Vec<RelationshipView>, AppError>> + Send + 'a>> {
        Box::pin(async move {
            Ok(targets
                .iter()
                .map(|target| Self::no_relationship(account_ref_id(target)))
                .collect())
        })
    }
}

/// design.md's exact default-implementation name for
/// [`AccountCountsProvider`]. A zero-field unit struct (see
/// [`EmptyStatusesProvider`]'s doc comment for why that structurally proves
/// "no DB/network touch"). Always returns all-zero counts with
/// `last_status_at: None`, regardless of `target`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ZeroCountsProvider;

impl AccountCountsProvider for ZeroCountsProvider {
    fn counts<'a>(
        &'a self,
        _target: &'a AccountRef,
    ) -> Pin<Box<dyn Future<Output = Result<AccountCounts, AppError>> + Send + 'a>> {
        Box::pin(async move {
            Ok(AccountCounts {
                followers: 0,
                following: 0,
                statuses: 0,
                last_status_at: None,
            })
        })
    }
}

/// The delegation registry (design.md ports component: "既定実装 + レジスト
/// リ"). Held inside `AppState` (task 1.4's wiring), bootstrap-constructed
/// via [`AccountPortsRegistry::new`] (every slot defaulting to its built-in
/// default implementation), and later replaced wholesale per-port by a
/// downstream spec's own bootstrap/wiring code via `set_statuses_provider`/
/// `set_relationship_provider`/`set_counts_provider`. See this module's doc
/// comment ("Registry shape") for why this is three independent
/// single-slot fields rather than `ObjectDocumentRegistry`'s ordered/
/// fan-out list shape.
#[derive(Clone)]
pub struct AccountPortsRegistry {
    statuses: Arc<RwLock<Arc<dyn AccountStatusesProvider>>>,
    relationships: Arc<RwLock<Arc<dyn RelationshipStateProvider>>>,
    counts: Arc<RwLock<Arc<dyn AccountCountsProvider>>>,
}

impl Default for AccountPortsRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl AccountPortsRegistry {
    /// Builds a registry with every slot defaulting to its built-in default
    /// implementation ([`EmptyStatusesProvider`] / [`NoRelationshipProvider`]
    /// / [`ZeroCountsProvider`]) — no DB/network handle is reachable from a
    /// freshly built registry until a downstream spec calls one of the
    /// `set_*` methods.
    pub fn new() -> Self {
        AccountPortsRegistry {
            statuses: Arc::new(RwLock::new(Arc::new(EmptyStatusesProvider))),
            relationships: Arc::new(RwLock::new(Arc::new(NoRelationshipProvider))),
            counts: Arc::new(RwLock::new(Arc::new(ZeroCountsProvider))),
        }
    }

    /// Replaces the registered [`AccountStatusesProvider`] (statuses-core's
    /// own registration entry point). `&self`, not `&mut self` — see this
    /// module's doc comment ("Registry shape") for why.
    pub fn set_statuses_provider(&self, provider: Arc<dyn AccountStatusesProvider>) {
        *self
            .statuses
            .write()
            .expect("AccountPortsRegistry statuses lock must not be poisoned") = provider;
    }

    /// Replaces the registered [`RelationshipStateProvider`] (social-graph's
    /// own registration entry point).
    pub fn set_relationship_provider(&self, provider: Arc<dyn RelationshipStateProvider>) {
        *self
            .relationships
            .write()
            .expect("AccountPortsRegistry relationships lock must not be poisoned") = provider;
    }

    /// Replaces the registered [`AccountCountsProvider`].
    pub fn set_counts_provider(&self, provider: Arc<dyn AccountCountsProvider>) {
        *self
            .counts
            .write()
            .expect("AccountPortsRegistry counts lock must not be poisoned") = provider;
    }

    /// Delegates to the currently registered [`AccountStatusesProvider`]
    /// (the built-in default until a downstream spec replaces it).
    pub async fn list_statuses(
        &self,
        query: &StatusesQuery,
    ) -> Result<Page<serde_json::Value>, AppError> {
        let provider = self
            .statuses
            .read()
            .expect("AccountPortsRegistry statuses lock must not be poisoned")
            .clone();
        provider.list_statuses(query).await
    }

    /// Delegates to the currently registered [`RelationshipStateProvider`].
    pub async fn relationships(
        &self,
        viewer: Id,
        targets: &[AccountRef],
    ) -> Result<Vec<RelationshipView>, AppError> {
        let provider = self
            .relationships
            .read()
            .expect("AccountPortsRegistry relationships lock must not be poisoned")
            .clone();
        provider.relationships(viewer, targets).await
    }

    /// Delegates to the currently registered [`AccountCountsProvider`].
    pub async fn counts(&self, target: &AccountRef) -> Result<AccountCounts, AppError> {
        let provider = self
            .counts
            .read()
            .expect("AccountPortsRegistry counts lock must not be poisoned")
            .clone();
        provider.counts(target).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::accounts::model::{AccountCounts, RelationshipView};
    use crate::accounts::ports::{
        AccountCountsProvider, AccountPortsRegistry, AccountStatusesProvider,
        EmptyStatusesProvider, NoRelationshipProvider, RelationshipStateProvider, StatusesQuery,
        ZeroCountsProvider,
    };
    use crate::api::pagination::PageParams;
    use crate::domain::{AccountRef, Id};

    fn sample_query(target: AccountRef) -> StatusesQuery {
        StatusesQuery {
            target,
            viewer: None,
            page: PageParams::default(),
            pinned: false,
            only_media: false,
            exclude_replies: false,
            exclude_reblogs: false,
        }
    }

    #[tokio::test]
    async fn empty_statuses_provider_returns_an_empty_page() {
        let provider = EmptyStatusesProvider;
        let query = sample_query(AccountRef::Local(Id::from_i64(1)));
        let page = provider.list_statuses(&query).await.unwrap();
        assert!(page.items.is_empty());
        assert!(page.prev_cursor.is_none());
        assert!(page.next_cursor.is_none());
    }

    #[tokio::test]
    async fn no_relationship_provider_returns_all_false_for_every_target() {
        let provider = NoRelationshipProvider;
        let targets = [
            AccountRef::Local(Id::from_i64(1)),
            AccountRef::Remote(Id::from_i64(2)),
        ];
        let relationships = provider
            .relationships(Id::from_i64(99), &targets)
            .await
            .unwrap();
        assert_eq!(relationships.len(), 2);
        for view in &relationships {
            assert!(!view.following);
            assert!(!view.followed_by);
            assert!(!view.blocking);
            assert!(!view.blocked_by);
            assert!(!view.muting);
            assert!(!view.muting_notifications);
            assert!(!view.requested);
            assert!(!view.requested_by);
            assert!(!view.domain_blocking);
            assert!(!view.endorsed);
            assert!(!view.showing_reblogs);
            assert!(!view.notifying);
            assert!(view.languages.is_empty());
            assert!(view.note.is_empty());
        }
        assert_eq!(relationships[0].id, Id::from_i64(1));
        assert_eq!(relationships[1].id, Id::from_i64(2));
    }

    #[tokio::test]
    async fn zero_counts_provider_returns_zero_counts_and_no_last_status() {
        let provider = ZeroCountsProvider;
        let target = AccountRef::Local(Id::from_i64(1));
        let counts = provider.counts(&target).await.unwrap();
        assert_eq!(
            counts,
            AccountCounts {
                followers: 0,
                following: 0,
                statuses: 0,
                last_status_at: None,
            }
        );
    }

    #[tokio::test]
    async fn registry_defaults_to_empty_statuses_page_when_nothing_registered() {
        let registry = AccountPortsRegistry::new();
        let query = sample_query(AccountRef::Local(Id::from_i64(1)));
        let page = registry.list_statuses(&query).await.unwrap();
        assert!(page.items.is_empty());
    }

    #[tokio::test]
    async fn registry_defaults_to_no_relationship_when_nothing_registered() {
        let registry = AccountPortsRegistry::new();
        let targets = [AccountRef::Local(Id::from_i64(5))];
        let relationships = registry
            .relationships(Id::from_i64(1), &targets)
            .await
            .unwrap();
        assert_eq!(relationships.len(), 1);
        assert!(!relationships[0].following);
        assert_eq!(relationships[0].id, Id::from_i64(5));
    }

    #[tokio::test]
    async fn registry_defaults_to_zero_counts_when_nothing_registered() {
        let registry = AccountPortsRegistry::new();
        let target = AccountRef::Local(Id::from_i64(1));
        let counts = registry.counts(&target).await.unwrap();
        assert_eq!(counts.followers, 0);
        assert_eq!(counts.following, 0);
        assert_eq!(counts.statuses, 0);
        assert!(counts.last_status_at.is_none());
    }

    struct FixedCountsProvider(AccountCounts);

    impl AccountCountsProvider for FixedCountsProvider {
        fn counts<'a>(
            &'a self,
            _target: &'a AccountRef,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<AccountCounts, crate::error::AppError>>
                    + Send
                    + 'a,
            >,
        > {
            let counts = self.0;
            Box::pin(async move { Ok(counts) })
        }
    }

    #[tokio::test]
    async fn registry_uses_a_registered_counts_provider_instead_of_the_default() {
        let registry = AccountPortsRegistry::new();
        registry.set_counts_provider(Arc::new(FixedCountsProvider(AccountCounts {
            followers: 10,
            following: 20,
            statuses: 30,
            last_status_at: None,
        })));
        let target = AccountRef::Local(Id::from_i64(1));
        let counts = registry.counts(&target).await.unwrap();
        assert_eq!(counts.followers, 10);
        assert_eq!(counts.following, 20);
        assert_eq!(counts.statuses, 30);
    }

    struct FixedRelationshipProvider;

    impl RelationshipStateProvider for FixedRelationshipProvider {
        fn relationships<'a>(
            &'a self,
            _viewer: Id,
            targets: &'a [AccountRef],
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<Vec<RelationshipView>, crate::error::AppError>,
                    > + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                Ok(targets
                    .iter()
                    .map(|target| {
                        let id = match *target {
                            AccountRef::Local(id) => id,
                            AccountRef::Remote(id) => id,
                        };
                        RelationshipView {
                            id,
                            following: true,
                            showing_reblogs: true,
                            notifying: false,
                            languages: Vec::new(),
                            followed_by: true,
                            blocking: false,
                            blocked_by: false,
                            muting: false,
                            muting_notifications: false,
                            requested: false,
                            requested_by: false,
                            domain_blocking: false,
                            endorsed: false,
                            note: String::new(),
                        }
                    })
                    .collect())
            })
        }
    }

    #[tokio::test]
    async fn registry_uses_a_registered_relationship_provider_instead_of_the_default() {
        let registry = AccountPortsRegistry::new();
        registry.set_relationship_provider(Arc::new(FixedRelationshipProvider));
        let targets = [AccountRef::Local(Id::from_i64(7))];
        let relationships = registry
            .relationships(Id::from_i64(1), &targets)
            .await
            .unwrap();
        assert!(relationships[0].following);
        assert!(relationships[0].followed_by);
    }

    struct FixedStatusesProvider;

    impl AccountStatusesProvider for FixedStatusesProvider {
        fn list_statuses<'a>(
            &'a self,
            _query: &'a StatusesQuery,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<
                            crate::api::pagination::Page<serde_json::Value>,
                            crate::error::AppError,
                        >,
                    > + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                Ok(crate::api::pagination::Page {
                    items: vec![serde_json::json!({"id": "1"})],
                    prev_cursor: None,
                    next_cursor: Some("1".to_string()),
                })
            })
        }
    }

    #[tokio::test]
    async fn registry_uses_a_registered_statuses_provider_instead_of_the_default() {
        let registry = AccountPortsRegistry::new();
        registry.set_statuses_provider(Arc::new(FixedStatusesProvider));
        let query = sample_query(AccountRef::Local(Id::from_i64(1)));
        let page = registry.list_statuses(&query).await.unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.next_cursor.as_deref(), Some("1"));
    }
}
