//! `SearchBackend` port and its query request types (design.md "Search
//! Port / 照合境界層" -> `SearchBackend(ports) / PgSearchBackend`,
//! Requirements 7.1, 7.2, 7.3, 7.4, 7.5; task 1.4, `Boundary:
//! SearchBackend`).
//!
//! Scope: this module owns exactly what design.md's own Service Interface
//! excerpt (lines ~344-356) names — the [`SearchBackend`] trait
//! (`search_accounts` / `search_statuses` / `search_hashtags`, each
//! returning upstream entity *identifiers* only, Requirement 7.2) plus its
//! three per-method request types ([`AccountQuery`], [`StatusQuery`],
//! [`HashtagQuery`]) and a genuinely working test double
//! ([`StubSearchBackend`]) that satisfies the trait without touching any
//! concrete engine. No SQL, no `PgPool`, no `HashtagIndexRepository` call
//! lives here — the standard-PostgreSQL default implementation
//! (`PgSearchBackend`) is task 3.1's job, strictly downstream of and
//! outside this port definition (Requirement 7.3). This task also does not
//! touch `src/search/model.rs` (task 1.2, already committed) or
//! `src/search/query_parser.rs` (task 1.3, already committed) — it only
//! reuses [`crate::domain::AccountRef`]/[`crate::domain::Id`] and
//! [`crate::search::model::TagMatch`] as the trait's return element types,
//! exactly as design.md's own excerpt writes them.
//!
//! ## `async fn` in trait, not boxed futures (mirrors `MediaStore`, not
//! `AccountPortsRegistry`)
//! design.md's own Service Interface excerpt writes each `SearchBackend`
//! method as a plain `async fn` and this module keeps that literally,
//! unlike `crate::accounts::ports`/`crate::notifications::ports`, which
//! deviate to `Pin<Box<dyn Future<..> + Send + 'a>>` because *those* ports
//! back a registry that must hold `Arc<dyn Trait>` and be swapped at
//! runtime, *after* the registry is already live inside `AppState`
//! (downstream specs register post-boot). `SearchBackend` has no such
//! requirement in this task's boundary: design.md's own wording for this
//! port is "将来エンジンは新実装で差し替え" (a future engine is substituted
//! by a *new implementation*) and "配線点で実装を差し替え可能に"
//! (substitutable *at the wiring point*, i.e. bootstrap-time selection,
//! not a live post-boot registry swap like `AccountPortsRegistry`'s
//! `set_*` methods) — task 5.3's own text is "既定 `PgSearchBackend` を
//! `SearchBackend` として配線... 差し替え点を1箇所に集約する", which reads
//! as a single compile-/boot-time wiring point, not a runtime-mutable
//! registry slot. This module therefore follows this crate's *other*
//! precedent for a swappable-but-not-runtime-mutable async port,
//! `crate::media::store::MediaStore` (design.md excerpt likewise a literal
//! `async fn`, kept as-is, "stays generic ... never boxed as `dyn
//! MediaStore`") rather than the registry precedent. `SearchBackend` is
//! consequently not `dyn`-compatible (Rust's native `async fn`-in-trait
//! desugars to an opaque per-impl associated type that cannot be named in
//! a trait object) — callers that need to be engine-agnostic take `impl
//! SearchBackend` / are generic over `B: SearchBackend`, exactly like
//! `MediaService<S: MediaStore>`. If a later task (5.3, `SearchModule`
//! wiring) turns out to need genuine post-boot runtime substitution after
//! all, that task can still box `SearchBackend` behind its own
//! `Arc<dyn ..>`-friendly wrapper without changing this trait's method
//! signatures — flagged as a CONCERN in this task's status report for
//! reviewer confirmation, mirroring the precedent `accounts::ports`'s own
//! doc comment sets for the identical design.md-vs-object-safety
//! trade-off, decided the other way here because this port's own
//! design.md wording does not ask for post-boot mutability.
//!
//! ## `StubSearchBackend`: a genuinely working in-memory double, not a stub
//! that always returns empty
//! [`StubSearchBackend`] is not an `unimplemented!()`- or
//! always-empty-return stub: it holds real in-memory collections (accounts,
//! statuses, hashtags, and optional following edges) registered via its
//! `with_*` builder methods, and its three `SearchBackend` methods actually
//! filter by substring term match (case-insensitive, mirroring
//! `PgSearchBackend`'s own planned `ILIKE` substring semantics, design.md:
//! "部分一致（標準 SQL、`ILIKE`）"), optionally by `account_id`
//! (`StatusQuery`) or by a registered following edge (`AccountQuery`'s
//! `following_of`), and apply `limit`/`offset` pagination — so it is
//! useful as a real, swappable `SearchBackend` for later tasks' tests (task
//! 6.4's planned `search_backend_swap_it`, and any earlier integration test
//! that wants a deterministic backend without a database), not merely a
//! type that happens to compile.
//!
//! ## `AccountQuery::following_of` is carried but not required to filter in
//! every implementation
//! design.md's Responsibilities note for `PgSearchBackend::search_accounts`
//! is explicit that its own default implementation does *not* filter by
//! `following` at the matching stage: "`following` は照合段では候補抽出に
//! 留め、フォロー限定は Hydrator/上流関係に委譲" (following-scoping is
//! deferred to `SearchHydrator`/upstream relationship data, not the
//! backend). The query type nonetheless carries `following_of: Option<Id>`
//! exactly as design.md's own struct sketch does (Requirement 7's port
//! contract does not get to silently drop a field design.md names), so a
//! future backend *may* honor it directly if that is cheaper for a given
//! engine. [`StubSearchBackend`] chooses to honor it (see previous
//! section) precisely because doing so is what makes it "genuinely
//! useful" as a test double for following-scoped searches, without that
//! choice implying `PgSearchBackend` (task 3.1, out of this task's
//! boundary) must do the same.

use std::collections::{HashMap, HashSet};

use crate::domain::{AccountRef, Id};
use crate::error::AppError;
use crate::search::model::TagMatch;

/// Request type for [`SearchBackend::search_accounts`] (design.md's exact
/// field set, "型定義（抜粋）" / Service Interface, line 346): a free-text
/// `term` to match against display name/username/acct (matching semantics
/// are the implementing backend's job, not this type's), an optional
/// `following_of` viewer id carried through for a backend that chooses to
/// honor it (see this module's doc comment), and `limit`/`offset`
/// pagination (Requirement 3.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountQuery {
    pub term: String,
    pub following_of: Option<Id>,
    pub limit: u32,
    pub offset: u32,
}

/// Request type for [`SearchBackend::search_statuses`] (design.md's exact
/// field set, line 347): a free-text `term`, the authenticated `viewer`
/// (every `SearchParams`/`StatusQuery` has one — `read:search` requires
/// authentication, mirroring `SearchParams::viewer`'s identical
/// non-`Option<Id>` shape in `crate::search::model`), an optional
/// `account_id` scope (Requirement 4.3), and `limit`/`offset` pagination
/// (Requirement 4.6). `viewer` is carried through so a backend can
/// prefilter to visibility *candidates* (design.md: "閲覧者可視候補の投稿
/// `Id` 群") — final visibility is still re-applied by `SearchHydrator`
/// downstream of this port (design.md: "最終可視性は Hydrator が
/// `VisibilityPolicy` で再適用"), so a backend is free to treat `viewer` as
/// an optimization hint rather than a hard filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusQuery {
    pub term: String,
    pub viewer: Id,
    pub account_id: Option<Id>,
    pub limit: u32,
    pub offset: u32,
}

/// Request type for [`SearchBackend::search_hashtags`] (design.md's exact
/// field set, line 348): a free-text `term` matched against hashtag names
/// (Requirement 5.1) and `limit`/`offset` pagination (Requirement 5.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HashtagQuery {
    pub term: String,
    pub limit: u32,
    pub offset: u32,
}

/// The search backend abstraction boundary (Requirement 7.1): the sole
/// seam through which account/status/hashtag matching happens, so calling
/// code (the eventual `SearchService`/`SearchEndpoint`, tasks 5.1/5.2) can
/// be written against this trait alone and never needs to know whether the
/// concrete implementation is `PgSearchBackend` (task 3.1, standard
/// PostgreSQL) or any future engine (Requirement 7.4). Every method returns
/// upstream entity *identifiers* only — [`AccountRef`], [`Id`], or
/// [`TagMatch`] (itself just a hashtag name, `crate::search::model`'s own
/// "identifier only" type) — never `Account`/`Status`/`Tag` JSON; building
/// those is `SearchHydrator`'s job, strictly outside this port (Requirement
/// 7.2). See this module's doc comment ("`async fn` in trait") for why
/// this is a native `async fn` trait rather than boxed futures, and
/// [`StubSearchBackend`] for the swap-in test double (Requirement 7.5)
/// that proves engine-agnostic substitution is possible.
///
/// `#[allow(async_fn_in_trait)]` mirrors this crate's established pattern
/// for a narrow async port consumed generically (`impl SearchBackend`/
/// `B: SearchBackend`), not through `dyn` — see
/// `crate::media::store::MediaStore`'s identical `#[allow(..)]` and its own
/// documented rationale, which this module's doc comment ("`async fn` in
/// trait") applies to `SearchBackend` for the same reason.
#[allow(async_fn_in_trait)]
pub trait SearchBackend: Send + Sync {
    /// Matches accounts (local actors and known remote accounts) whose
    /// display name/username/acct contains `q.term` (Requirement 3.1),
    /// returning bare [`AccountRef`]s (never `Account` JSON, Requirement
    /// 7.2).
    async fn search_accounts(&self, q: &AccountQuery) -> Result<Vec<AccountRef>, AppError>;

    /// Matches visibility-candidate posts whose body contains `q.term`
    /// (Requirement 4.1), optionally scoped to `q.account_id`
    /// (Requirement 4.3), returning bare post [`Id`]s (never `Status`
    /// JSON, Requirement 7.2). Final visibility is re-applied downstream
    /// by `SearchHydrator` — see [`StatusQuery`]'s own doc comment.
    async fn search_statuses(&self, q: &StatusQuery) -> Result<Vec<Id>, AppError>;

    /// Matches hashtags whose name contains `q.term` (Requirement 5.1),
    /// returning bare [`TagMatch`]es (never full `Tag` JSON with
    /// `url`/`history`, Requirement 7.2 — that is `TagSerializer`'s job,
    /// task 4.1, from the richer [`crate::search::model::TagView`]).
    async fn search_hashtags(&self, q: &HashtagQuery) -> Result<Vec<TagMatch>, AppError>;
}

/// Applies `offset` then `limit` to `items`, the same pagination discipline
/// every [`SearchBackend`] method owes its query type's `limit`/`offset`
/// fields (Requirements 3.4, 4.6, 5.5). An `offset` at or beyond the end of
/// `items` yields an empty result rather than panicking.
fn paginate<T>(mut items: Vec<T>, limit: u32, offset: u32) -> Vec<T> {
    let offset = offset as usize;
    if offset >= items.len() {
        return Vec::new();
    }
    items.drain(0..offset);
    items.truncate(limit as usize);
    items
}

/// One registered account entry inside a [`StubSearchBackend`]: the
/// identifier callers get back, plus the free-text haystack `search_accounts`
/// matches `term` against (a stand-in for `PgSearchBackend`'s planned
/// display_name/username/acct `ILIKE` match, design.md).
#[derive(Debug, Clone)]
struct StubAccountEntry {
    account: AccountRef,
    haystack: String,
}

/// One registered status entry inside a [`StubSearchBackend`].
#[derive(Debug, Clone)]
struct StubStatusEntry {
    id: Id,
    account_id: Id,
    haystack: String,
}

/// A genuinely working, in-memory [`SearchBackend`] test double (design.md:
/// "既定: PgSearchBackend（標準 PostgreSQL） / テスト: StubSearchBackend";
/// Requirement 7.5). See this module's doc comment ("`StubSearchBackend`: a
/// genuinely working in-memory double") for why this is not an
/// `unimplemented!()`/always-empty stub. Built via its `with_*` builder
/// methods (consuming `self`, mirroring this crate's other builder-style
/// test-fixture constructors), then exercised purely through the
/// [`SearchBackend`] trait.
#[derive(Debug, Default)]
pub struct StubSearchBackend {
    accounts: Vec<StubAccountEntry>,
    statuses: Vec<StubStatusEntry>,
    hashtags: Vec<TagMatch>,
    following: HashMap<Id, HashSet<AccountRef>>,
}

impl StubSearchBackend {
    /// Builds an empty backend (every `search_*` call returns `Ok(vec![])`
    /// until entries are registered).
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers an account [`search_accounts`](SearchBackend::search_accounts)
    /// may match, keyed by a free-text `haystack` (e.g. `"Alice Example
    /// alice@example.social"`, mirroring the display_name/username/acct
    /// fields `PgSearchBackend` will eventually search across).
    pub fn with_account(mut self, account: AccountRef, haystack: impl Into<String>) -> Self {
        self.accounts.push(StubAccountEntry {
            account,
            haystack: haystack.into(),
        });
        self
    }

    /// Registers a following edge: `follower` follows the account
    /// `followee` refers to. Only consulted when a query's
    /// `following_of` is `Some(follower)` (see this module's doc comment,
    /// "`AccountQuery::following_of`").
    pub fn with_following_edge(mut self, follower: Id, followee: AccountRef) -> Self {
        self.following.entry(follower).or_default().insert(followee);
        self
    }

    /// Registers a post [`search_statuses`](SearchBackend::search_statuses)
    /// may match, keyed by a free-text `haystack` (its body) and the
    /// authoring `account_id` (consulted when a query's `account_id` is
    /// `Some(..)`, Requirement 4.3).
    pub fn with_status(mut self, id: Id, account_id: Id, haystack: impl Into<String>) -> Self {
        self.statuses.push(StubStatusEntry {
            id,
            account_id,
            haystack: haystack.into(),
        });
        self
    }

    /// Registers a hashtag [`search_hashtags`](SearchBackend::search_hashtags)
    /// may match, by name.
    pub fn with_hashtag(mut self, name: impl Into<String>) -> Self {
        self.hashtags.push(TagMatch { name: name.into() });
        self
    }
}

impl SearchBackend for StubSearchBackend {
    async fn search_accounts(&self, q: &AccountQuery) -> Result<Vec<AccountRef>, AppError> {
        let term = q.term.to_lowercase();
        let mut matched: Vec<AccountRef> = self
            .accounts
            .iter()
            .filter(|entry| entry.haystack.to_lowercase().contains(&term))
            .map(|entry| entry.account)
            .collect();

        if let Some(follower) = q.following_of {
            let followed = self.following.get(&follower);
            matched.retain(|account| followed.is_some_and(|set| set.contains(account)));
        }

        Ok(paginate(matched, q.limit, q.offset))
    }

    async fn search_statuses(&self, q: &StatusQuery) -> Result<Vec<Id>, AppError> {
        let term = q.term.to_lowercase();
        let matched: Vec<Id> = self
            .statuses
            .iter()
            .filter(|entry| entry.haystack.to_lowercase().contains(&term))
            .filter(|entry| {
                q.account_id
                    .is_none_or(|account_id| account_id == entry.account_id)
            })
            .map(|entry| entry.id)
            .collect();

        Ok(paginate(matched, q.limit, q.offset))
    }

    async fn search_hashtags(&self, q: &HashtagQuery) -> Result<Vec<TagMatch>, AppError> {
        let term = q.term.to_lowercase();
        let matched: Vec<TagMatch> = self
            .hashtags
            .iter()
            .filter(|tag| tag.name.to_lowercase().contains(&term))
            .cloned()
            .collect();

        Ok(paginate(matched, q.limit, q.offset))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Requirement 7.1 / 7.5 / this task's own completion definition
    /// ("呼び出し側がエンジン非依存に書ける"): a caller written purely
    /// against `B: SearchBackend` (no mention of `StubSearchBackend` or any
    /// other concrete type inside its own body) compiles and runs against
    /// whichever backend is substituted in. Before this task landed
    /// `SearchBackend`/`AccountQuery` did not exist, so this function
    /// itself failed to compile (this task's RED phase) -- see this task's
    /// status report RED_PHASE_OUTPUT.
    async fn engine_agnostic_account_search<B: SearchBackend>(
        backend: &B,
        term: &str,
    ) -> Vec<AccountRef> {
        let query = AccountQuery {
            term: term.to_string(),
            following_of: None,
            limit: 50,
            offset: 0,
        };
        backend.search_accounts(&query).await.unwrap()
    }

    fn sample_backend() -> StubSearchBackend {
        StubSearchBackend::new()
            .with_account(
                AccountRef::Local(Id::from_i64(1)),
                "Alice Example alice@example.social",
            )
            .with_account(
                AccountRef::Remote(Id::from_i64(2)),
                "Bob Remote bob@remote.example",
            )
            .with_account(
                AccountRef::Local(Id::from_i64(3)),
                "Carol Local carol@example.social",
            )
    }

    /// Proves identifier-only substitution is possible: calling code
    /// (`engine_agnostic_account_search`) is written once against the
    /// trait and produces the expected match set when backed by
    /// [`StubSearchBackend`], without that caller ever naming
    /// `StubSearchBackend` (Requirements 7.1, 7.2, 7.5).
    #[tokio::test]
    async fn stub_backend_satisfies_engine_agnostic_caller() {
        let backend = sample_backend();
        let matches = engine_agnostic_account_search(&backend, "alice").await;
        assert_eq!(matches, vec![AccountRef::Local(Id::from_i64(1))]);
    }

    /// [`StubSearchBackend::search_accounts`] matches case-insensitively
    /// across the registered haystack (display name/username/acct
    /// combined) and returns bare [`AccountRef`]s, never embedded entity
    /// JSON (Requirement 7.2).
    #[tokio::test]
    async fn search_accounts_matches_term_case_insensitively() {
        let backend = sample_backend();
        let query = AccountQuery {
            term: "EXAMPLE.SOCIAL".to_string(),
            following_of: None,
            limit: 50,
            offset: 0,
        };
        let matches = backend.search_accounts(&query).await.unwrap();
        assert_eq!(
            matches,
            vec![
                AccountRef::Local(Id::from_i64(1)),
                AccountRef::Local(Id::from_i64(3)),
            ]
        );
    }

    /// `AccountQuery::following_of` scopes results to accounts the given
    /// follower is registered as following (see this module's doc
    /// comment, "`AccountQuery::following_of`").
    #[tokio::test]
    async fn search_accounts_following_of_scopes_to_registered_edges() {
        let backend = sample_backend()
            .with_following_edge(Id::from_i64(99), AccountRef::Local(Id::from_i64(3)));
        let query = AccountQuery {
            term: "example".to_string(),
            following_of: Some(Id::from_i64(99)),
            limit: 50,
            offset: 0,
        };
        let matches = backend.search_accounts(&query).await.unwrap();
        assert_eq!(matches, vec![AccountRef::Local(Id::from_i64(3))]);
    }

    /// `limit`/`offset` pagination is applied after term matching
    /// (Requirement 3.4).
    #[tokio::test]
    async fn search_accounts_applies_limit_and_offset() {
        let backend = sample_backend();
        // "example.social" matches only Alice (id 1) and Carol (id 3), in
        // that registration order -- Bob's "remote.example" haystack does
        // not contain the ".social" suffix, so it stays out of this
        // narrower two-match set (unlike the broader "example" term used
        // by the case-insensitivity test above, which also matches Bob).
        let query = AccountQuery {
            term: "example.social".to_string(),
            following_of: None,
            limit: 1,
            offset: 1,
        };
        let matches = backend.search_accounts(&query).await.unwrap();
        assert_eq!(matches, vec![AccountRef::Local(Id::from_i64(3))]);
    }

    /// An `offset` at or beyond the match count yields an empty page
    /// rather than panicking.
    #[tokio::test]
    async fn search_accounts_offset_beyond_matches_returns_empty() {
        let backend = sample_backend();
        let query = AccountQuery {
            term: "example".to_string(),
            following_of: None,
            limit: 50,
            offset: 100,
        };
        let matches = backend.search_accounts(&query).await.unwrap();
        assert!(matches.is_empty());
    }

    /// [`StubSearchBackend::search_statuses`] matches by body term and
    /// scopes to `account_id` when requested (Requirement 4.3), returning
    /// bare [`Id`]s (Requirement 7.2).
    #[tokio::test]
    async fn search_statuses_matches_term_and_scopes_to_account_id() {
        let backend = StubSearchBackend::new()
            .with_status(Id::from_i64(10), Id::from_i64(1), "hello rustlang world")
            .with_status(Id::from_i64(11), Id::from_i64(2), "hello mastodon world")
            .with_status(Id::from_i64(12), Id::from_i64(1), "unrelated post");

        let unscoped = StatusQuery {
            term: "hello".to_string(),
            viewer: Id::from_i64(1),
            account_id: None,
            limit: 50,
            offset: 0,
        };
        let matches = backend.search_statuses(&unscoped).await.unwrap();
        assert_eq!(matches, vec![Id::from_i64(10), Id::from_i64(11)]);

        let scoped = StatusQuery {
            term: "hello".to_string(),
            viewer: Id::from_i64(1),
            account_id: Some(Id::from_i64(1)),
            limit: 50,
            offset: 0,
        };
        let matches = backend.search_statuses(&scoped).await.unwrap();
        assert_eq!(matches, vec![Id::from_i64(10)]);
    }

    /// [`StubSearchBackend::search_hashtags`] matches by name and returns
    /// bare [`TagMatch`]es, never full Tag JSON (Requirement 7.2).
    #[tokio::test]
    async fn search_hashtags_matches_name() {
        let backend = StubSearchBackend::new()
            .with_hashtag("rustlang")
            .with_hashtag("mastodon")
            .with_hashtag("rustacean");

        let query = HashtagQuery {
            term: "rust".to_string(),
            limit: 50,
            offset: 0,
        };
        let matches = backend.search_hashtags(&query).await.unwrap();
        assert_eq!(
            matches,
            vec![
                TagMatch {
                    name: "rustlang".to_string()
                },
                TagMatch {
                    name: "rustacean".to_string()
                },
            ]
        );
    }

    /// An empty [`StubSearchBackend`] (no entries registered) returns
    /// empty results for every method, rather than erroring.
    #[tokio::test]
    async fn empty_backend_returns_empty_results_for_every_method() {
        let backend = StubSearchBackend::new();

        let accounts = backend
            .search_accounts(&AccountQuery {
                term: "anything".to_string(),
                following_of: None,
                limit: 50,
                offset: 0,
            })
            .await
            .unwrap();
        assert!(accounts.is_empty());

        let statuses = backend
            .search_statuses(&StatusQuery {
                term: "anything".to_string(),
                viewer: Id::from_i64(1),
                account_id: None,
                limit: 50,
                offset: 0,
            })
            .await
            .unwrap();
        assert!(statuses.is_empty());

        let hashtags = backend
            .search_hashtags(&HashtagQuery {
                term: "anything".to_string(),
                limit: 50,
                offset: 0,
            })
            .await
            .unwrap();
        assert!(hashtags.is_empty());
    }
}
