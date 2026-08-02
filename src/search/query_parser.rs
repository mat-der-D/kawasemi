//! `QueryParser` (design.md "Search Domain / ドメイン層" -> "model /
//! QueryParser"; Requirements 2.3, 6.1, 6.2; task 1.3, `Boundary:
//! QueryParser`): discriminates and normalizes a raw search query string
//! (`q`) into a [`ParsedQuery`] (defined by task 1.2 in
//! `crate::search::model`, not redefined here).
//!
//! design.md's own sketch (`##### 型定義（抜粋）`, line 316):
//! `pub fn parse_query(q: &str) -> Result<ParsedQuery, AppError>; // 空/空白は AppError(422)`.
//!
//! ## Discrimination order and rules
//! [`parse_query`] classifies the *trimmed* input, in this order:
//! 1. `acct:user@domain` (WebFinger acct URI syntax, RFC 7565) -- an
//!    `acct:` prefix followed by exactly one `@`-separated user/domain
//!    pair, mirroring `federation::endpoints::webfinger::parse_acct_resource`'s
//!    established `strip_prefix`/`split_once` technique for the same
//!    syntax.
//! 2. `@user@domain` (Mastodon mention shorthand) -- a leading `@`
//!    followed by the same user/domain pair shape.
//! 3. An absolute URL with an `http`/`https` scheme and a host -- parsed
//!    via [`reqwest::Url`] (this crate's existing `reqwest` dependency
//!    already re-exports the `url` crate's parser as `reqwest::Url`, so
//!    this reuses an existing dependency rather than adding a new one).
//! 4. Anything else is a plain-word query (`ParsedQuery::Plain`).
//!
//! Empty or whitespace-only input (after trimming) is rejected with a
//! `422 Unprocessable Entity` [`AppError`] (Requirement 2.3), built with
//! `AppError::client(StatusCode::UNPROCESSABLE_ENTITY, ..)` -- the
//! established convention for a caller-facing "bad input" error elsewhere
//! in this crate (e.g. `statuses::status_service`, `media::service`,
//! `social_graph::follow_service`).
//!
//! See this module's own doc comment sections below and this task's
//! status report `CONCERNS` for how ambiguous edge cases not pinned by
//! design.md (malformed `acct:`/mention forms, non-http(s) schemes, case
//! folding) are resolved.
//!
//! ## Normalization
//! - The whole input is trimmed of leading/trailing whitespace before any
//!   classification (also serves the empty/whitespace-only rejection).
//! - `Acct { user, domain }`: `user` and `domain` are individually
//!   trimmed; `domain` is lowercased (DNS domains are conventionally
//!   case-insensitive -- the same rationale
//!   `federation::endpoints::webfinger`'s own doc comment states for its
//!   `eq_ignore_ascii_case` domain comparison). `user` is left
//!   case-as-typed: this crate has no established precedent for folding
//!   local-part case, and folding it risks breaking WebFinger lookups
//!   against remote instances whose local part is genuinely
//!   case-sensitive.
//! - `Url(String)`: stored as [`reqwest::Url`]'s own normalized
//!   `to_string()` form (lowercased scheme/host, percent-encoding, etc. --
//!   whatever normalization the URL parser itself performs), not the raw
//!   input substring.
//! - `Plain(String)`: the trimmed input, verbatim (no case folding --
//!   matching-engine case-insensitivity, e.g. SQL `ILIKE`, is `PgSearchBackend`'s
//!   job (task 3.1), not the parser's).
//!
//! ## Malformed `acct:`/mention forms fall back to `Plain`, not an error
//! Requirement 2.3 only requires rejecting *empty/whitespace-only* input;
//! design.md's own `parse_query` signature comment repeats exactly that
//! ("空/空白は AppError(422)"), naming no other error case. A query that
//! merely *starts with* `acct:` or `@` but does not resolve into a valid
//! `user@domain` pair (e.g. `acct:notanaddress`, `@bob` with no second
//! `@`, `@a@b@c` with more than one `@` after the leading one) is
//! therefore treated as a plain-word query rather than rejected -- the
//! most conservative reading available (never reject a query design.md
//! does not say to reject) and consistent with actual Mastodon search
//! behavior, where `@bob` alone (no domain) is treated as ordinary
//! account-search text, not an error.
//!
//! ## Non-`http(s)` schemes are not treated as `Url`
//! design.md's own words are "URL（スキーム付き）" (a URL with a scheme),
//! which taken completely literally would admit any RFC 3986 scheme
//! (`mailto:`, `data:`, arbitrary custom schemes like `foo:bar`). This
//! parser narrows that to `http`/`https` with a present host: Requirement
//! 6.2's own remit is resolving "リモートアクターまたはリモート投稿の
//! URL" via federation fetch, and every URL this crate itself ever builds
//! for an actor/object (`federation::urls::ActorUrls`) is `https://`;
//! there is no fetchable federation resource behind a `mailto:` or
//! host-less scheme. Narrowing to `http`/`https`+host avoids
//! misclassifying an ordinary plain-word query that happens to contain a
//! bare `scheme:value` colon (e.g. a search for `note:to-self`) as a
//! (federation-unresolvable) URL. Flagged as a CONCERN for reviewer
//! double-check since it narrows design.md's literal wording.

#[cfg(test)]
mod tests;

use axum::http::StatusCode;

use crate::error::AppError;
use crate::search::model::ParsedQuery;

/// Discriminates and normalizes a raw search query string into a
/// [`ParsedQuery`] (Requirements 2.3, 6.1, 6.2). See this module's doc
/// comment for the full discrimination order, normalization rules, and
/// edge-case resolutions.
///
/// # Errors
/// Returns a `422 Unprocessable Entity` [`AppError`] when `q` is empty or
/// consists only of whitespace (Requirement 2.3).
pub fn parse_query(q: &str) -> Result<ParsedQuery, AppError> {
    let trimmed = q.trim();
    if trimmed.is_empty() {
        return Err(AppError::client(
            StatusCode::UNPROCESSABLE_ENTITY,
            "search query must not be empty",
        ));
    }

    if let Some(parsed) = parse_acct_prefixed(trimmed) {
        return Ok(parsed);
    }

    if let Some(parsed) = parse_mention_shorthand(trimmed) {
        return Ok(parsed);
    }

    if let Some(parsed) = parse_absolute_url(trimmed) {
        return Ok(parsed);
    }

    Ok(ParsedQuery::Plain(trimmed.to_string()))
}

/// Matches `acct:user@domain` (WebFinger acct URI syntax). Mirrors
/// `federation::endpoints::webfinger::parse_acct_resource`'s
/// `strip_prefix`/`split_once` technique for the same underlying syntax,
/// but consumes/normalizes into a [`ParsedQuery::Acct`] rather than
/// borrowing.
fn parse_acct_prefixed(q: &str) -> Option<ParsedQuery> {
    let rest = q.strip_prefix("acct:")?;
    split_user_domain(rest)
}

/// Matches `@user@domain` (Mastodon mention shorthand for a remote
/// handle). A bare `@user` with no second `@` is deliberately *not*
/// matched here -- see this module's doc comment ("Malformed `acct:`/
/// mention forms fall back to `Plain`").
fn parse_mention_shorthand(q: &str) -> Option<ParsedQuery> {
    let rest = q.strip_prefix('@')?;
    split_user_domain(rest)
}

/// Splits `rest` on the first `@` into `(user, domain)` and builds a
/// [`ParsedQuery::Acct`], rejecting (`None`) empty segments or a domain
/// segment that itself contains another `@` (e.g. `a@b@c` after the
/// leading prefix is stripped -- an ambiguous form this parser does not
/// attempt to resolve, falling back to `Plain` instead).
fn split_user_domain(rest: &str) -> Option<ParsedQuery> {
    let (user, domain) = rest.split_once('@')?;
    let user = user.trim();
    let domain = domain.trim();
    if user.is_empty() || domain.is_empty() || domain.contains('@') {
        return None;
    }
    Some(ParsedQuery::Acct {
        user: user.to_string(),
        domain: domain.to_lowercase(),
    })
}

/// Matches an absolute `http`/`https` URL with a present host. See this
/// module's doc comment ("Non-`http(s)` schemes are not treated as
/// `Url`") for why the scheme is narrowed beyond design.md's literal "a
/// URL with a scheme" wording.
fn parse_absolute_url(q: &str) -> Option<ParsedQuery> {
    let url = reqwest::Url::parse(q).ok()?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return None;
    }
    url.host_str()?;
    Some(ParsedQuery::Url(url.to_string()))
}
