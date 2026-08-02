//! Search domain types (`model` component, design.md "Search Domain /
//! ドメイン層" -> `model` / `QueryParser`, Requirements 1.3, 2.1, 2.2, 7.2;
//! task 1.2, `Boundary: model`).
//!
//! Scope: this module owns exactly the seven types task 1.2's own
//! instruction enumerates — [`SearchType`], [`SearchParams`],
//! [`ParsedQuery`], [`SearchMatches`], [`TagMatch`], [`TagView`], and
//! [`TagHistoryEntry`] — built on core-runtime's [`Id`] (`crate::domain`,
//! not redefined here) and accounts-and-instance's [`AccountRef`]
//! (likewise re-exported from `crate::domain`, its actual canonical
//! definition site per `.kiro/steering/roadmap.md`'s note that
//! `AccountRef`/`Visibility` are shared core-runtime primitives — not a
//! type this spec owns or redefines), mirroring
//! `src/notifications/model.rs`'s and `src/statuses/model.rs`'s identical
//! `use crate::domain::{..}` precedent for consuming rather than
//! redefining the shared primitives.
//!
//! No query parsing logic (`parse_query`/`QueryParser`, task 1.3 —
//! `Boundary: QueryParser`, `src/search/query_parser.rs`), no backend
//! abstraction (`SearchBackend` trait, task 1.4), no hashtag persistence
//! (task 2.x), no remote resolution (task 4.3), no hydration/serialization
//! (task 4.1/4.2), no service/endpoint (task 5.x), and no `SearchModule`
//! composition/wiring (task 5.3) live here — those consume the types
//! defined in this module but are out of scope for task 1.2 (`Boundary:
//! model`). design.md's own model excerpt sketches a `parse_query` free
//! function inline with these types, but the File Structure Plan assigns
//! it to `src/search/query_parser.rs`'s `QueryParser` component instead
//! (task 1.3's own `Boundary: QueryParser`) — this module therefore
//! defines only the seven *types* task 1.2 names, not that function.
//!
//! ## `SearchMatches` holds identifiers only (Requirement 7.2)
//! [`SearchMatches`] is the `SearchBackend` port's own return shape
//! (design.md: "照合結果として上流のエンティティ識別子...を返す形に定義
//! し、エンティティの JSON シリアライズ...をポートの外...に保つ"): its three
//! fields are [`AccountRef`] (a local/remote actor reference, not an
//! `Account`), [`Id`] (a bare post identifier, not a `Status`), and
//! [`TagMatch`] (a bare hashtag name, not a `Tag` — see "`TagMatch` vs.
//! `TagView`" below). None of the three is, or embeds, `serde_json::Value`
//! or any other entity-JSON-shaped structure; concretizing those
//! identifiers into Account/Status/Tag JSON is `SearchHydrator`'s job
//! (task 4.2), strictly downstream of and outside this type. This module's
//! own unit tests prove the field set and field *types* exhaustively (no
//! `..` rest pattern), so a future change that widened any field to carry
//! embedded entity JSON would fail to compile against these tests until
//! updated here.
//!
//! ## `TagMatch` vs. `TagView`
//! [`TagMatch`] is the minimal identifier `SearchBackend`/`SearchMatches`
//! carries — just `name`, mirroring `AccountRef`/`Id`'s "identifier only"
//! shape for the hashtag case (there is no dedicated hashtag entity id in
//! this spec's own schema; the tag name is itself the natural key
//! `search_tags` is keyed on, `migrations/0013_search.sql`). [`TagView`] is
//! a different, downstream type: the *logical* representation of the Tag
//! JSON contract (`name`/`url`/`history`) `HashtagIndexRepository::
//! match_hashtags` (task 2.1) returns and `TagSerializer` (task 4.1)
//! renders into the final Tag JSON — it is not itself JSON either (no
//! `serde_json::Value`), but it does carry the extra `url`/`history` fields
//! a bare identifier does not need. `SearchMatches` deliberately carries
//! `TagMatch`, not `TagView` — the identifier-only discipline from
//! Requirement 7.2.
//!
//! ## Derives: no `Serialize`/`Deserialize` here (mirrors `Notification`'s split)
//! `crate::domain::Visibility` documents the precedent this module follows:
//! it owns only the enum and, because its serde representation *is* its
//! final wire representation, derives `Serialize`/`Deserialize` directly.
//! `SearchType`/`SearchParams`/`ParsedQuery`/`SearchMatches`/`TagMatch`/
//! `TagView`/`TagHistoryEntry` are different: none of them is itself a wire
//! shape (the Tag/SearchResults JSON contracts are `TagSerializer`'s/
//! `SearchResultSerializer`'s job, tasks 4.1, and embed upstream-delegated
//! Account/Status JSON `SearchHydrator` produces, task 4.2) — deriving
//! `Serialize`/`Deserialize` here would speculatively commit to a JSON
//! shape this task's own boundary does not need and those later tasks do
//! not reuse, so only `Debug`/`Clone`/`PartialEq`/`Eq` are derived (what
//! this task's own unit tests require), plus `Copy` on the fieldless
//! [`SearchType`] (mirrors `crate::domain::Visibility` and
//! `notifications::NotificationType`'s identical fieldless-enum
//! precedent).

use crate::domain::{AccountRef, Id};

/// The three result kinds a search request may be scoped to (design.md's
/// "型定義（抜粋）"; Requirements 2.1, 2.2 — `type` request parameter
/// filtering). Fieldless, so `Copy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchType {
    Accounts,
    Statuses,
    Hashtags,
}

/// The fully-parsed, validated search request (design.md's "型定義（抜
/// 粋）"; Requirements 2.1, 2.2, 2.5). `kind: None` means "search every
/// type" (Requirement 2.1); `Some(kind)` restricts to that one type and the
/// others are returned as empty arrays (Requirement 2.2, enforced by
/// `SearchService`/`SearchResultSerializer`, task 5.1/4.1 — not by this
/// type itself). `limit`/`offset` are already rounded to api-foundation's
/// pagination convention by the time a `SearchParams` value exists
/// (Requirement 2.5; this module's own Invariants note in design.md:
/// "`limit`/`offset` は api-foundation 規約で丸め済み"). `viewer` is a
/// plain [`Id`], not `Option<Id>`, because `read:search` requires
/// authentication — every `SearchParams` value has a viewer by
/// construction (design.md: "read:search は認証必須のため viewer は常に
/// 存在").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchParams {
    pub q: String,
    pub kind: Option<SearchType>,
    pub resolve: bool,
    pub following: bool,
    pub account_id: Option<Id>,
    pub limit: u32,
    pub offset: u32,
    pub exclude_unreviewed: bool,
    pub viewer: Id,
}

/// The classified/normalized form of the raw `q` query string (design.md's
/// "型定義（抜粋）"; Requirements 2.3, 6.1, 6.2). Classification and
/// normalization themselves (`acct:user@domain` / `@user@domain` / URL /
/// plain-word discrimination, empty/whitespace-only rejection) are
/// `QueryParser::parse_query`'s job (task 1.3, `src/search/
/// query_parser.rs`) — this variant set only defines the three shapes that
/// function's `Ok` case may produce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedQuery {
    Plain(String),
    Acct { user: String, domain: String },
    Url(String),
}

/// A bare hashtag identifier — the `SearchBackend`/`SearchMatches`-level
/// hashtag match (Requirement 7.2, "識別子のみ"). See this module's own doc
/// comment, "`TagMatch` vs. `TagView`", for why this is deliberately
/// narrower than [`TagView`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagMatch {
    pub name: String,
}

/// One day's usage aggregate for a hashtag's Tag JSON `history` field
/// (design.md's "型定義（抜粋）"). All three fields are `String`, matching
/// Mastodon's own Tag `history` wire contract, which represents the day
/// (Unix timestamp), use count, and account count as decimal strings
/// rather than JSON numbers (the same string-not-number convention
/// `crate::domain::Id` documents for identifiers, applied here to counters
/// instead).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagHistoryEntry {
    pub day: String,
    pub uses: String,
    pub accounts: String,
}

/// The logical representation of the Tag JSON contract (design.md's "型定
/// 義（抜粋）"; Requirement 1.3: Tag entities include at least `name` /
/// `url` / `history`). Produced by `HashtagIndexRepository::match_hashtags`
/// (task 2.1) and rendered into final Tag JSON by `TagSerializer` (task
/// 4.1) — this type itself carries no `serde_json::Value` and is not the
/// wire shape (see this module's own doc comment, "Derives").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagView {
    pub name: String,
    pub url: String,
    pub history: Vec<TagHistoryEntry>,
}

/// The `SearchBackend` port's own return shape: per-type match results as
/// upstream entity *identifiers* only, never entity JSON (Requirement
/// 7.2). See this module's own doc comment, "`SearchMatches` holds
/// identifiers only", for the full rationale and this type's relationship
/// to `SearchHydrator` (task 4.2), which is the only place these
/// identifiers are ever concretized into Account/Status/Tag JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchMatches {
    pub accounts: Vec<AccountRef>,
    pub statuses: Vec<Id>,
    pub hashtags: Vec<TagMatch>,
}

// Unit tests live inline below, not in a sibling `model/tests.rs` file.
// `.kiro/steering/structure.md`'s general statement is a same-directory
// `tests.rs` submodule, but the actual established precedent for exactly
// this kind of file — a pure value-type `model.rs` with no I/O — is
// inline: `src/notifications/model.rs`, `src/statuses/model.rs`, and
// `src/social_graph/model.rs` all declare `#[cfg(test)] mod tests { .. }`
// in-file (there is no `src/notifications/model/tests.rs` etc. on disk).
// This module matches that specific, closer-matching precedent rather than
// the steering doc's general statement — see CONCERNS in this task's
// status report.
#[cfg(test)]
mod tests {
    use super::*;

    fn sample_params(kind: Option<SearchType>) -> SearchParams {
        SearchParams {
            q: "hello".to_string(),
            kind,
            resolve: false,
            following: false,
            account_id: None,
            limit: 20,
            offset: 0,
            exclude_unreviewed: false,
            viewer: Id::from_i64(1),
        }
    }

    fn sample_matches() -> SearchMatches {
        SearchMatches {
            accounts: vec![
                AccountRef::Local(Id::from_i64(10)),
                AccountRef::Remote(Id::from_i64(11)),
            ],
            statuses: vec![Id::from_i64(20), Id::from_i64(21)],
            hashtags: vec![
                TagMatch {
                    name: "rustlang".to_string(),
                },
                TagMatch {
                    name: "mastodon".to_string(),
                },
            ],
        }
    }

    /// All seven of this task's types compile and can be constructed
    /// (completion definition, "各型がコンパイルされ"): exercised via each
    /// type's own dedicated test below plus this smoke test that touches
    /// every one of them in a single value graph.
    #[test]
    fn all_seven_search_domain_types_construct() {
        let params = sample_params(Some(SearchType::Hashtags));
        let parsed = ParsedQuery::Acct {
            user: "alice".to_string(),
            domain: "example.social".to_string(),
        };
        let history = TagHistoryEntry {
            day: "1735689600".to_string(),
            uses: "3".to_string(),
            accounts: "2".to_string(),
        };
        let view = TagView {
            name: "rustlang".to_string(),
            url: "https://example.test/tags/rustlang".to_string(),
            history: vec![history],
        };
        let matches = sample_matches();

        assert_eq!(params.kind, Some(SearchType::Hashtags));
        assert!(matches!(parsed, ParsedQuery::Acct { .. }));
        assert_eq!(view.history.len(), 1);
        assert_eq!(matches.hashtags.len(), 2);
    }

    /// Requirement 7.2 / completion definition ("`SearchMatches` がエンティ
    /// ティ JSON を持たず識別子のみで構成される"): exhaustively destructures
    /// a [`SearchMatches`] value (no `..` rest pattern) and proves each
    /// field is populated with bare identifiers — [`AccountRef`], [`Id`],
    /// and [`TagMatch`] (itself just a `name: String`) — never a
    /// `serde_json::Value` or any other entity-JSON-shaped structure. If a
    /// future change widened any field's element type to carry embedded
    /// entity JSON, this test would fail to compile against the identifier
    /// values constructed here until updated, mirroring
    /// `notifications::model`'s identical exhaustive-destructure technique
    /// for proving a field set (and, here, field *shape*) at compile time
    /// rather than via a runtime check.
    #[test]
    fn search_matches_holds_identifiers_only_no_entity_json() {
        let matches = sample_matches();
        let SearchMatches {
            accounts,
            statuses,
            hashtags,
        } = matches;

        // `accounts: Vec<AccountRef>` — a local/remote actor reference, not
        // an `Account` JSON body.
        assert_eq!(
            accounts,
            vec![
                AccountRef::Local(Id::from_i64(10)),
                AccountRef::Remote(Id::from_i64(11))
            ]
        );

        // `statuses: Vec<Id>` — a bare post identifier, not a `Status` JSON
        // body.
        assert_eq!(statuses, vec![Id::from_i64(20), Id::from_i64(21)]);

        // `hashtags: Vec<TagMatch>` — and `TagMatch` itself is exhaustively
        // destructured too, proving it carries only `name: String`, not a
        // full Tag JSON shape (`url`/`history`, which live on the separate
        // `TagView` type instead — see this module's doc comment, `TagMatch`
        // vs. `TagView`).
        assert_eq!(hashtags.len(), 2);
        for (tag, expected_name) in hashtags.into_iter().zip(["rustlang", "mastodon"]) {
            let TagMatch { name } = tag;
            assert_eq!(name, expected_name);
        }
    }

    /// Requirement 2.1/2.2 ("`type` による種別絞り込み"): `kind: None`
    /// (search every type) and `kind: Some(..)` (restrict to one type) are
    /// both representable.
    #[test]
    fn search_params_kind_is_optional_for_unified_vs_scoped_search() {
        let unified = sample_params(None);
        let scoped = sample_params(Some(SearchType::Accounts));
        assert_eq!(unified.kind, None);
        assert_eq!(scoped.kind, Some(SearchType::Accounts));
    }

    /// Exhaustively destructures a [`SearchParams`] value (no `..` rest
    /// pattern) — mirrors `notifications::model::Notification`'s identical
    /// technique for proving a required-or-absent field set at the type
    /// level: this test fails to compile the moment a field is added or
    /// removed without updating it here.
    #[test]
    fn search_params_field_set_is_exhaustive() {
        let params = SearchParams {
            q: "acct:alice@example.social".to_string(),
            kind: Some(SearchType::Accounts),
            resolve: true,
            following: true,
            account_id: Some(Id::from_i64(99)),
            limit: 40,
            offset: 5,
            exclude_unreviewed: true,
            viewer: Id::from_i64(7),
        };
        let SearchParams {
            q,
            kind,
            resolve,
            following,
            account_id,
            limit,
            offset,
            exclude_unreviewed,
            viewer,
        } = params;
        assert_eq!(q, "acct:alice@example.social");
        assert_eq!(kind, Some(SearchType::Accounts));
        assert!(resolve);
        assert!(following);
        assert_eq!(account_id, Some(Id::from_i64(99)));
        assert_eq!(limit, 40);
        assert_eq!(offset, 5);
        assert!(exclude_unreviewed);
        assert_eq!(viewer, Id::from_i64(7));
    }

    /// Requirement 6.1/6.2 (`acct:`/URL classification): an exhaustive
    /// `match` over all three [`ParsedQuery`] variants, with **no wildcard
    /// arm**, compiles — mirrors `notifications::model::NotificationType`'s
    /// identical closed-set proof technique. `QueryParser::parse_query`
    /// itself (the function that actually produces these values) is task
    /// 1.3's own boundary, not this module's.
    #[test]
    fn parsed_query_is_exhaustively_matched_by_exactly_three_variants() {
        fn label(parsed: &ParsedQuery) -> &'static str {
            match parsed {
                ParsedQuery::Plain(_) => "plain",
                ParsedQuery::Acct { .. } => "acct",
                ParsedQuery::Url(_) => "url",
            }
        }

        let plain = ParsedQuery::Plain("hello world".to_string());
        let acct = ParsedQuery::Acct {
            user: "bob".to_string(),
            domain: "instance.example".to_string(),
        };
        let url = ParsedQuery::Url("https://instance.example/@bob/1".to_string());

        assert_eq!(label(&plain), "plain");
        assert_eq!(label(&acct), "acct");
        assert_eq!(label(&url), "url");

        match acct {
            ParsedQuery::Acct { user, domain } => {
                assert_eq!(user, "bob");
                assert_eq!(domain, "instance.example");
            }
            _ => panic!("expected Acct"),
        }
    }

    /// Requirement 2.1/2.2: an exhaustive `match` over all three
    /// [`SearchType`] variants, with **no wildcard arm**, compiles.
    #[test]
    fn search_type_is_exhaustively_matched_by_exactly_three_variants() {
        fn label(kind: SearchType) -> &'static str {
            match kind {
                SearchType::Accounts => "accounts",
                SearchType::Statuses => "statuses",
                SearchType::Hashtags => "hashtags",
            }
        }

        let all = [
            SearchType::Accounts,
            SearchType::Statuses,
            SearchType::Hashtags,
        ];
        let labels: Vec<&'static str> = all.into_iter().map(label).collect();
        assert_eq!(labels, vec!["accounts", "statuses", "hashtags"]);
    }

    /// Requirement 1.3 (Tag entity includes at least `name`/`url`/
    /// `history`): [`TagView`] is exhaustively destructured (no `..` rest
    /// pattern), proving all three fields are present, and `history` can
    /// hold multiple [`TagHistoryEntry`] aggregates.
    #[test]
    fn tag_view_carries_name_url_and_history() {
        let view = TagView {
            name: "rustlang".to_string(),
            url: "https://example.test/tags/rustlang".to_string(),
            history: vec![
                TagHistoryEntry {
                    day: "1735689600".to_string(),
                    uses: "3".to_string(),
                    accounts: "2".to_string(),
                },
                TagHistoryEntry {
                    day: "1735603200".to_string(),
                    uses: "1".to_string(),
                    accounts: "1".to_string(),
                },
            ],
        };
        let TagView { name, url, history } = view;
        assert_eq!(name, "rustlang");
        assert_eq!(url, "https://example.test/tags/rustlang");
        assert_eq!(history.len(), 2);
        let TagHistoryEntry {
            day,
            uses,
            accounts,
        } = history[0].clone();
        assert_eq!(day, "1735689600");
        assert_eq!(uses, "3");
        assert_eq!(accounts, "2");
    }

    /// [`TagView`] with an empty `history` (design.md: "`history` は最小集
    /// 計（または空）") is representable — the field is `Vec`, not
    /// `Option<Vec<..>>` or a fixed-size array, so "no aggregate yet" is
    /// just an empty vector, not a distinct enum state.
    #[test]
    fn tag_view_history_may_be_empty() {
        let view = TagView {
            name: "newtag".to_string(),
            url: "https://example.test/tags/newtag".to_string(),
            history: vec![],
        };
        assert!(view.history.is_empty());
    }
}
