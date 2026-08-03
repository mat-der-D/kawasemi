//! Unit tests for [`super::parse_query`] (task 1.3 completion definition:
//! "4 種別の判別と空クエリ拒否を網羅する単体テストが通る" -- all four
//! [`ParsedQuery`] discrimination cases plus empty-query rejection,
//! Requirements 2.3, 6.1, 6.2).

use axum::http::StatusCode;

use super::parse_query;
use crate::error::ErrorKind;
use crate::search::model::ParsedQuery;

/// Requirement 6.1: `acct:user@domain` is discriminated into
/// `ParsedQuery::Acct { user, domain }`.
#[test]
fn discriminates_acct_prefixed_form() {
    let parsed = parse_query("acct:alice@example.social").expect("valid acct query");
    assert_eq!(
        parsed,
        ParsedQuery::Acct {
            user: "alice".to_string(),
            domain: "example.social".to_string(),
        }
    );
}

/// Requirement 6.1: `@user@domain` (Mastodon mention shorthand) is
/// discriminated into the same `ParsedQuery::Acct` shape as the `acct:`
/// prefixed form.
#[test]
fn discriminates_at_mention_shorthand_form() {
    let parsed = parse_query("@bob@instance.example").expect("valid mention query");
    assert_eq!(
        parsed,
        ParsedQuery::Acct {
            user: "bob".to_string(),
            domain: "instance.example".to_string(),
        }
    );
}

/// Domain normalization: the domain segment is lowercased (DNS
/// case-insensitivity, matching `federation::endpoints::webfinger`'s own
/// documented rationale), while the user segment is left as typed.
#[test]
fn acct_domain_is_lowercased_user_is_not() {
    let parsed = parse_query("acct:Alice@EXAMPLE.Social").expect("valid acct query");
    assert_eq!(
        parsed,
        ParsedQuery::Acct {
            user: "Alice".to_string(),
            domain: "example.social".to_string(),
        }
    );
}

/// Requirement 6.2: an absolute URL with a scheme (`https://...`) is
/// discriminated into `ParsedQuery::Url`.
#[test]
fn discriminates_url_with_scheme() {
    let parsed =
        parse_query("https://instance.example/@bob/123456").expect("valid https URL query");
    assert_eq!(
        parsed,
        ParsedQuery::Url("https://instance.example/@bob/123456".to_string())
    );
}

/// `http://` (not just `https://`) is also discriminated as a URL.
#[test]
fn discriminates_plain_http_url() {
    let parsed = parse_query("http://instance.example/users/bob").expect("valid http URL query");
    assert_eq!(
        parsed,
        ParsedQuery::Url("http://instance.example/users/bob".to_string())
    );
}

/// Anything that is not `acct:`-prefixed, not `@user@domain`, and not an
/// absolute `http`/`https` URL is a plain-word query.
#[test]
fn discriminates_plain_word_query() {
    let parsed = parse_query("rustlang mastodon").expect("valid plain query");
    assert_eq!(parsed, ParsedQuery::Plain("rustlang mastodon".to_string()));
}

/// Normalization: leading/trailing whitespace around an otherwise plain
/// query is trimmed.
#[test]
fn plain_query_is_trimmed() {
    let parsed = parse_query("  hello world  ").expect("valid plain query");
    assert_eq!(parsed, ParsedQuery::Plain("hello world".to_string()));
}

/// Requirement 2.3: an empty query string is rejected with a 422
/// (`Unprocessable Entity`) client [`AppError`].
#[test]
fn rejects_empty_query() {
    let err = parse_query("").expect_err("empty query must be rejected");
    assert_eq!(err.kind, ErrorKind::Client);
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
}

/// Requirement 2.3: a whitespace-only query string is likewise rejected
/// (not just the exactly-empty string).
#[test]
fn rejects_whitespace_only_query() {
    let err = parse_query("   \t  \n ").expect_err("whitespace-only query must be rejected");
    assert_eq!(err.kind, ErrorKind::Client);
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
}

/// Edge case (documented in this module's own doc comment, "Malformed
/// `acct:`/mention forms fall back to `Plain`"): `@bob` with no second
/// `@` (no domain) is not rejected and not misclassified as `Acct` --
/// it falls back to a plain-word query, matching actual Mastodon search
/// behavior for a bare mention with no instance domain.
#[test]
fn bare_at_mention_without_domain_falls_back_to_plain() {
    let parsed = parse_query("@bob").expect("bare @mention is not an error");
    assert_eq!(parsed, ParsedQuery::Plain("@bob".to_string()));
}

/// Edge case: an `acct:` prefix without a well-formed `user@domain` pair
/// falls back to a plain-word query rather than erroring (same rationale
/// as the bare-mention case above).
#[test]
fn malformed_acct_prefix_falls_back_to_plain() {
    let parsed = parse_query("acct:notanaddress").expect("malformed acct is not an error");
    assert_eq!(parsed, ParsedQuery::Plain("acct:notanaddress".to_string()));
}

/// Edge case: a non-`http(s)` scheme (e.g. a bare `scheme:value` colon
/// query) is not misclassified as `Url` -- see this module's doc comment
/// ("Non-`http(s)` schemes are not treated as `Url`"). Falls back to
/// `Plain`.
#[test]
fn non_http_scheme_falls_back_to_plain() {
    let parsed = parse_query("mailto:bob@example.com").expect("mailto: is not an error");
    assert_eq!(
        parsed,
        ParsedQuery::Plain("mailto:bob@example.com".to_string())
    );
}
