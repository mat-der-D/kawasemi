//! `SearchService` (design.md "Service / サービス層" -> "#### SearchService";
//! Requirements 2.1, 2.2, 2.3, 2.5, 5.4, 6.3, 6.5, 7.1, 9.5; task 5.1,
//! `Boundary: SearchService`): aggregates the unified-search business —
//! parse (empty `q` -> 422) -> `type` dispatch (unrequested types return
//! `[]`) -> optional remote resolution (`resolve=true` + authenticated
//! only) -> `SearchBackend` matching (`limit`/`offset`/`account_id`/
//! `following`/`exclude_unreviewed` accepted) -> `SearchHydrator`
//! concretization -> `SearchResultSerializer` assembly.
//!
//! ## Scope
//! This module owns exactly [`SearchService`] and its one public method
//! ([`SearchService::search`]), design.md's own literal Service Interface
//! (`pub async fn search(&self, params: SearchParams) -> Result<serde_json::
//! Value, AppError>;`, lines ~443-445). It does not implement the HTTP
//! surface (`SearchEndpoint`, task 5.2 — no Bearer/`read:search` scope
//! verification, no query-string extraction, no response-code mapping
//! beyond returning whatever [`AppError`] its own collaborators already
//! produce) and no `SearchModule`/`AppState`/bootstrap/router wiring (task
//! 5.3). It composes — never reimplements — every collaborator task 5.1's
//! own dispatch brief names: [`crate::search::query_parser::parse_query`]
//! (task 1.3), [`crate::search::ports::SearchBackend`] (task 1.4, matched
//! generically — see "Engine-agnostic by construction" below),
//! [`crate::search::remote_resolver::RemoteResolver`] (task 4.3),
//! [`crate::search::hydrator::SearchHydrator`] (task 4.2), and
//! [`crate::search::result_serializer::SearchResultSerializer`] (task 4.1).
//!
//! ## Engine-agnostic by construction (Requirement 7.1)
//! [`SearchService`] is generic over `B: SearchBackend` (mirroring
//! `crate::media::service::MediaService<S: MediaStore>`'s identical
//! rationale — `SearchBackend` is a plain `async fn` trait, not `dyn`-safe,
//! see `crate::search::ports`'s own doc comment, "`async fn` in trait, not
//! boxed futures") plus the same `H: FederationHttpClient, R:
//! RemoteActorResolver, M: LocalMentionResolver` triple
//! [`crate::search::remote_resolver::RemoteResolver`] is already generic
//! over — this struct wraps that resolver directly rather than re-narrowing
//! its type parameters, the same choice `RemoteResolver<H, R, M>`'s own doc
//! comment makes for its own upstream collaborators. `search()`'s own body
//! never matches on, or otherwise depends on, which concrete `B` it was
//! built with — every match call goes through the `SearchBackend` trait
//! alone — which is what makes this task's completion condition
//! ("呼び出し側がエンジン非依存（`SearchBackend` 経由）") true by
//! construction, not merely by convention; this module's own tests prove it
//! by exercising `SearchService<StubSearchBackend, ..>` end to end without
//! this module's own code ever naming `StubSearchBackend`.
//!
//! ## Constructor: "bundle, don't build" (mirrors `SearchHydrator::new`/
//! `RemoteResolver::new`)
//! [`SearchService::new`] takes every collaborator already constructed —
//! this module never builds a `PgPool`, a `PgSearchBackend`, a
//! `RemoteResolver`, or a `SearchHydrator` itself. Wiring the *default*
//! `PgSearchBackend` (Requirement 7.3) and assembling every collaborator
//! from `AppState`'s shared handles is `SearchModule`'s job (task 5.3),
//! strictly downstream of and outside this module's boundary.
//!
//! ## Resolve gating: `resolve=true` AND `Acct`/`Url` only (Requirements
//! 6.1, 6.3, 6.5)
//! [`SearchService::search`] calls
//! [`RemoteResolver::resolve_remote`](crate::search::remote_resolver::RemoteResolver::resolve_remote)
//! only when `params.resolve` is `true` **and** the parsed query is
//! [`ParsedQuery::Acct`] or [`ParsedQuery::Url`] — never for
//! [`ParsedQuery::Plain`] (design.md's own flowchart: "Kind -->|acct or url
//! and resolve true| Remote", "Kind -->|plain or resolve false| LocalOnly").
//! `resolve=false` (or omitted) skips this call entirely, searching only
//! locally-known data via `SearchBackend` (Requirement 6.3).
//!
//! **On authentication**: [`SearchParams::viewer`] is documented (design.md,
//! `crate::search::model`'s own doc comment) as unconditionally populated —
//! "read:search は認証必須のため viewer は常に存在" — because `read:search`
//! authentication is enforced *upstream* of this service, by the not-yet-
//! built `SearchEndpoint` (task 5.2), before a `SearchParams` value can ever
//! exist. This module therefore has no separate "unauthenticated" branch to
//! implement for Requirement 6.5 ("リモート解決を認証済みリクエストに限定
//! し、未認証リクエストではリモート取得を行わない"): by the time `search()`
//! runs, the request is *already* known-authenticated by construction, so
//! the only gate this layer can meaningfully apply is `resolve == true`
//! itself. Flagged as a CONCERN in this task's status report for reviewer
//! confirmation against design.md, since it is subtle and this module
//! cannot itself prove the upstream enforcement exists (task 5.2 does not
//! exist yet).
//!
//! ## `resolve_remote`'s own `Err` arm is handled defensively, even though
//! it "never returns `Err`"
//! `RemoteResolver::resolve_remote`'s own doc comment states plainly that it
//! always returns `Ok` (every internal failure mode already normalizes to
//! `Resolved::None`, Requirement 6.4). This module still matches its `Err`
//! arm rather than `.expect()`/unwrapping it away: if a future change to
//! `RemoteResolver` ever violated that invariant, treating the failure as
//! equivalent to `Resolved::None` (logged, not propagated) keeps this
//! layer's own promise that a remote-resolution problem never fails the
//! whole search (Requirement 6.4's "エンドポイント自体は正常応答する"
//! spirit, applied defensively one layer up).
//!
//! ## `term`: the raw normalized query, not the original `q` verbatim
//! [`SearchBackend`]'s three query types (`AccountQuery`/`StatusQuery`/
//! `HashtagQuery`) each take a free-text `term`. This module derives it from
//! the already-parsed [`ParsedQuery`] rather than re-reading `params.q`:
//! `Plain(text)` supplies `text` verbatim; `Acct { user, domain }` supplies
//! `"{user}@{domain}"` (the normalized acct form — the natural text an
//! `ILIKE`-style match against a known remote account's synthesized acct
//! should compare against, `PgSearchBackend`'s own doc comment); `Url(url)`
//! supplies the parser's own normalized URL string. Backend matching is
//! **not** skipped for `Acct`/`Url` queries even when `resolve=true`
//! already ran (design.md's flowchart: both the `Remote` and `LocalOnly`
//! branches converge into the same `Match` step) — a remote resolution only
//! ever adds at most one extra candidate; it does not replace the backend's
//! own locally-known-data match.
//!
//! ## Resolved candidates are merged in, `type`-gated (Requirement 2.2)
//! A successful remote resolution's `AccountRef`/`Id` is only added to this
//! service's own working match list when the corresponding
//! [`SearchType`] is actually requested (`kind.is_none()` or
//! `kind == Some(that type)`) — even though design.md's flowchart calls
//! `RemoteResolver` unconditionally (gated only on "acct or url and resolve
//! true", not on the requested `type`), its `Match` step is explicitly
//! "search backend match **per requested type**", and Requirement 2.2 is
//! unconditional ("指定された種別のみを検索し、他の種別の結果を空配列で返
//! す"). A request with `type=hashtags` and `resolve=true` against an
//! `acct:` query therefore still triggers a (wasted) `RemoteResolver` call
//! per the literal flowchart gate, but its result is discarded rather than
//! leaking an account into the `hashtags`-only response — flagged as a
//! CONCERN for reviewer confirmation, since design.md does not explicitly
//! address this combination and skipping the resolver call entirely in
//! that case would also be defensible (and cheaper).
//!
//! Resolved statuses are additionally deduplicated against the backend's
//! own match set before hydration (`Vec::contains`, small `N`) — not itself
//! a stated requirement (only Requirement 3.5's account dedup is explicit),
//! but avoids an avoidable exact-duplicate entry in the `statuses` array
//! when a query both resolves *and* already matches locally by content.
//! Resolved accounts need no equivalent guard: [`SearchHydrator::
//! hydrate_accounts`] already deduplicates every `AccountRef` it receives
//! (Requirement 3.5).
//!
//! ## Structured failure diagnostics (Requirement 9.5)
//! Every stage this module orchestrates (parse, remote resolution, each
//! `SearchBackend` match, each `SearchHydrator` concretization) logs a
//! `tracing::warn!` event on failure carrying `query_kind` (the parsed
//! query's shape: `"plain"`/`"acct"`/`"url"`, absent for a parse failure
//! itself, which has no parsed shape yet), `target_kind` (which of
//! `"parse"`/`"remote_resolve"`/`"accounts"`/`"statuses"`/`"hashtags"` was
//! being produced), `failure_point` (the specific step name, e.g.
//! `"search_accounts"`/`"hydrate_statuses"`), the failing [`AppError`]'s own
//! `status`, and `viewer` (an [`Id`], not a secret). No query text, token,
//! or other caller-supplied value is ever included — only these structural
//! labels plus the already-public-safe `AppError::status` (never
//! `AppError::source`, which `AppError::log_if_server` already logs
//! separately, at the point a `Server`-kind error is converted to an HTTP
//! response — duplicating it here would risk logging the same internal
//! detail twice for no additional diagnostic value at this layer).
//! `RemoteResolver`/`PgSearchBackend`/`SearchHydrator` all nest inside the
//! request-scoped `tracing` span `crate::telemetry::request_span` opens once
//! a later task wires it into the request pipeline (`crate::error`'s own
//! doc comment on `AppError::log_if_server`) — no explicit correlation-id
//! handling belongs in this module either, mirroring every other
//! `tracing::warn!` call site in this crate (`RemoteResolver`'s own,
//! `TimelineService`'s own fill-loop-cap event).
//!
//! A genuine (non-`resolve`) collaborator failure — a `SearchBackend` match
//! error or a `SearchHydrator` concretization error — is logged then
//! propagated as `Err` (never silently downgraded to an empty result): only
//! remote-resolution failure is required to degrade gracefully (Requirement
//! 6.4 names *that* case specifically; design.md's Error Strategy
//! separately states "全失敗を core-runtime `AppError` に集約する", with no
//! equivalent "keep responding 200" carve-out for a `SearchBackend`/
//! `SearchHydrator` failure).
//!
//! ## Where this module's tests live
//! There is no `service/tests.rs`. [`SearchService::new`] itself requires a
//! fully-constructed [`RemoteResolver`]/[`SearchHydrator`] pair, both of
//! which need a real `PgPool` to build, so every one of this module's tests
//! requires a running instance (`spawn_test_app`) and lives in
//! `tests/search_service_it.rs` — placed there by
//! `.kiro/specs/test-placement-migration` task 3.1 so that steering
//! `structure.md`'s test layout rule ("DB込みの実起動インスタンスを要する検証
//! は `tests/` 直下の `*_it.rs` に置く") holds in fact and not only on paper.
//! A module with no `tests.rs` therefore means "no pure unit test applies
//! here", not "untested".

use crate::domain::{AccountRef, Id};
use crate::error::AppError;
use crate::federation::signatures::FederationHttpClient;
use crate::search::hydrator::SearchHydrator;
use crate::search::model::{ParsedQuery, SearchParams, SearchType};
use crate::search::ports::{AccountQuery, HashtagQuery, SearchBackend, StatusQuery};
use crate::search::query_parser::parse_query;
use crate::search::remote_resolver::{RemoteResolver, Resolved};
use crate::search::result_serializer::SearchResultSerializer;
use crate::statuses::inbound_handlers::{LocalMentionResolver, RemoteActorResolver};

/// A `'static` label for the parsed query's own shape, used only for this
/// module's Requirement 9.5 structured diagnostics — never for control flow
/// beyond what `search()`'s own `match`/`matches!` already decides.
fn query_kind_label(parsed: &ParsedQuery) -> &'static str {
    match parsed {
        ParsedQuery::Plain(_) => "plain",
        ParsedQuery::Acct { .. } => "acct",
        ParsedQuery::Url(_) => "url",
    }
}

/// The free-text term passed to every [`SearchBackend`] query type — see
/// this module's doc comment, "`term`: the raw normalized query".
fn search_term(parsed: &ParsedQuery) -> String {
    match parsed {
        ParsedQuery::Plain(text) => text.clone(),
        ParsedQuery::Acct { user, domain } => format!("{user}@{domain}"),
        ParsedQuery::Url(url) => url.clone(),
    }
}

/// Aggregates the unified-search business (Requirements 2.1, 2.2, 2.3, 2.5,
/// 5.4, 6.3, 6.5, 7.1, 9.5). See this module's doc comment for the full
/// reasoning behind every generic parameter, constructor dependency, and
/// design.md deviation.
pub struct SearchService<B, H, R, M>
where
    B: SearchBackend,
    H: FederationHttpClient,
    R: RemoteActorResolver,
    M: LocalMentionResolver,
{
    backend: B,
    remote_resolver: RemoteResolver<H, R, M>,
    hydrator: SearchHydrator,
    result_serializer: SearchResultSerializer,
}

impl<B, H, R, M> SearchService<B, H, R, M>
where
    B: SearchBackend,
    H: FederationHttpClient,
    R: RemoteActorResolver,
    M: LocalMentionResolver,
{
    /// Builds a `SearchService` from already-constructed collaborators —
    /// see this module's doc comment ("Constructor: `bundle, don't build`").
    pub fn new(
        backend: B,
        remote_resolver: RemoteResolver<H, R, M>,
        hydrator: SearchHydrator,
        result_serializer: SearchResultSerializer,
    ) -> Self {
        Self {
            backend,
            remote_resolver,
            hydrator,
            result_serializer,
        }
    }

    /// design.md's exact Service Interface signature (line ~443). See this
    /// module's doc comment for the full parse -> (optional resolve) ->
    /// match -> hydrate -> assemble pipeline this method wires together.
    ///
    /// # Errors
    /// A `422 Unprocessable Entity` [`AppError`] when `params.q` is empty or
    /// whitespace-only (Requirement 2.3). Any other [`AppError`] a
    /// collaborator (`SearchBackend`/`SearchHydrator`) itself returns is
    /// logged (Requirement 9.5) and propagated unchanged. A `RemoteResolver`
    /// failure never reaches this signature's `Err` case — see this
    /// module's doc comment, "`resolve_remote`'s own `Err` arm".
    pub async fn search(&self, params: SearchParams) -> Result<serde_json::Value, AppError> {
        let parsed = parse_query(&params.q).inspect_err(|err| {
            tracing::warn!(
                target_kind = "parse",
                failure_point = "query_parse",
                viewer = params.viewer.as_i64(),
                status = %err.status,
                "search query parsing rejected the request (Requirement 2.3)"
            );
        })?;
        let query_kind = query_kind_label(&parsed);

        let wants = |kind: SearchType| params.kind.is_none_or(|requested| requested == kind);

        // ---- optional remote resolution (Requirements 6.1, 6.3, 6.5) ----
        let mut resolved_account: Option<AccountRef> = None;
        let mut resolved_status: Option<Id> = None;
        if params.resolve && matches!(parsed, ParsedQuery::Acct { .. } | ParsedQuery::Url(_)) {
            match self
                .remote_resolver
                .resolve_remote(&parsed, params.viewer)
                .await
            {
                Ok(Resolved::Account(account_ref)) => resolved_account = Some(account_ref),
                Ok(Resolved::Status(status_id)) => resolved_status = Some(status_id),
                Ok(Resolved::None) => {}
                Err(err) => {
                    tracing::warn!(
                        query_kind,
                        target_kind = "remote_resolve",
                        failure_point = "remote_resolve",
                        viewer = params.viewer.as_i64(),
                        status = %err.status,
                        "remote resolution failed unexpectedly; excluding from results \
                         rather than failing the whole search (Requirement 6.4)"
                    );
                }
            }
        }

        // ---- SearchBackend matching, per requested type (Requirement 7.1) ---
        let term = search_term(&parsed);

        let mut account_matches: Vec<AccountRef> = Vec::new();
        if wants(SearchType::Accounts) {
            let query = AccountQuery {
                term: term.clone(),
                following_of: params.following.then_some(params.viewer),
                limit: params.limit,
                offset: params.offset,
            };
            account_matches = self
                .backend
                .search_accounts(&query)
                .await
                .inspect_err(|err| {
                    tracing::warn!(
                        query_kind,
                        target_kind = "accounts",
                        failure_point = "search_accounts",
                        viewer = params.viewer.as_i64(),
                        status = %err.status,
                        "account backend match failed"
                    );
                })?;
            if let Some(account_ref) = resolved_account {
                // No dedup guard needed here -- `hydrate_accounts` itself
                // deduplicates every `AccountRef` it receives (Requirement
                // 3.5, this module's doc comment).
                account_matches.insert(0, account_ref);
            }
        }

        let mut status_ids: Vec<Id> = Vec::new();
        if wants(SearchType::Statuses) {
            let query = StatusQuery {
                term: term.clone(),
                viewer: params.viewer,
                account_id: params.account_id,
                limit: params.limit,
                offset: params.offset,
            };
            status_ids = self
                .backend
                .search_statuses(&query)
                .await
                .inspect_err(|err| {
                    tracing::warn!(
                        query_kind,
                        target_kind = "statuses",
                        failure_point = "search_statuses",
                        viewer = params.viewer.as_i64(),
                        status = %err.status,
                        "status backend match failed"
                    );
                })?;
            if let Some(status_id) = resolved_status
                && !status_ids.contains(&status_id)
            {
                status_ids.insert(0, status_id);
            }
        }

        // `exclude_unreviewed` (Requirement 5.4) is accepted but does not
        // narrow this minimal implementation's hashtag matching -- there is
        // no "unreviewed" hashtag concept in `search_tags`/
        // `search_status_tags` (`migrations/0013_search.sql`) to filter by,
        // so accepting the parameter without altering behavior already
        // "本サーバーの最小実装に整合する結果を返す" (design.md's own
        // wording for this requirement).
        let mut hashtag_matches = Vec::new();
        if wants(SearchType::Hashtags) {
            let query = HashtagQuery {
                term: term.clone(),
                limit: params.limit,
                offset: params.offset,
            };
            hashtag_matches = self
                .backend
                .search_hashtags(&query)
                .await
                .inspect_err(|err| {
                    tracing::warn!(
                        query_kind,
                        target_kind = "hashtags",
                        failure_point = "search_hashtags",
                        viewer = params.viewer.as_i64(),
                        status = %err.status,
                        "hashtag backend match failed"
                    );
                })?;
        }

        // ---- SearchHydrator concretization (Requirements 1.2, 3.2-3.5, 4.2, 4.6, 5.2) ---
        let accounts_json = if wants(SearchType::Accounts) {
            self.hydrator
                .hydrate_accounts(&account_matches, params.viewer, params.following)
                .await
                .inspect_err(|err| {
                    tracing::warn!(
                        query_kind,
                        target_kind = "accounts",
                        failure_point = "hydrate_accounts",
                        viewer = params.viewer.as_i64(),
                        status = %err.status,
                        "account hydration failed"
                    );
                })?
        } else {
            Vec::new()
        };

        let statuses_json = if wants(SearchType::Statuses) {
            self.hydrator
                .hydrate_statuses(&status_ids, params.viewer, params.limit)
                .await
                .inspect_err(|err| {
                    tracing::warn!(
                        query_kind,
                        target_kind = "statuses",
                        failure_point = "hydrate_statuses",
                        viewer = params.viewer.as_i64(),
                        status = %err.status,
                        "status hydration failed"
                    );
                })?
        } else {
            Vec::new()
        };

        let hashtags_json = if wants(SearchType::Hashtags) {
            self.hydrator
                .hydrate_hashtags(&hashtag_matches)
                .await
                .inspect_err(|err| {
                    tracing::warn!(
                        query_kind,
                        target_kind = "hashtags",
                        failure_point = "hydrate_hashtags",
                        viewer = params.viewer.as_i64(),
                        status = %err.status,
                        "hashtag hydration failed"
                    );
                })?
        } else {
            Vec::new()
        };

        // ---- SearchResultSerializer assembly (Requirements 1.1, 1.4) ----
        Ok(self
            .result_serializer
            .build_search_results(accounts_json, statuses_json, hashtags_json))
    }
}
