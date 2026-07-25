//! Unit tests for [`is_visible`] and [`NoRelationshipQuery`] (Requirements
//! 4.1, 6.1, 6.3, 6.4, 9.5, 12.4), per task 3.1's observable completion
//! condition: "public/unlisted/private/direct について可視/不可視が単一関
//! 数で判定され、未認証文脈で公開のみ可視になり、既定 `RelationshipQuery`
//! 実装で `private` 判定が非フォロワー扱いになる".
//!
//! Pure in-memory logic (`is_visible` is a plain sync fn) — no DB. Plain
//! `#[test]` for `is_visible`, `#[tokio::test]` only for the two
//! `NoRelationshipQuery` trait-method tests (async trait methods).

use time::macros::datetime;

use super::*;

fn author() -> Id {
    Id::from_i64(10)
}

fn other_viewer() -> Id {
    Id::from_i64(20)
}

fn status_with_visibility(visibility: Visibility) -> Status {
    Status {
        id: Id::from_i64(1),
        actor_id: author(),
        uri: "https://kawasemi.example/statuses/1".to_string(),
        url: Some("https://kawasemi.example/@author/1".to_string()),
        content: "hello".to_string(),
        visibility,
        sensitive: false,
        spoiler_text: String::new(),
        in_reply_to_id: None,
        in_reply_to_account_id: None,
        reblog_of_id: None,
        poll_id: None,
        language: Some("en".to_string()),
        reblogs_count: 0,
        favourites_count: 0,
        replies_count: 0,
        local: true,
        created_at: datetime!(2026-07-24 00:00:00 UTC),
        edited_at: None,
    }
}

const NON_FOLLOWER: ViewerRelation = ViewerRelation { is_follower: false };
const FOLLOWER: ViewerRelation = ViewerRelation { is_follower: true };

// -- Public: visible to everyone -----------------------------------------

#[test]
fn public_is_visible_to_unauthenticated_viewer() {
    let status = status_with_visibility(Visibility::Public);
    assert!(is_visible(&status, None, &NON_FOLLOWER));
}

#[test]
fn public_is_visible_to_authenticated_non_follower() {
    let status = status_with_visibility(Visibility::Public);
    assert!(is_visible(&status, Some(other_viewer()), &NON_FOLLOWER));
}

#[test]
fn public_is_visible_to_the_author() {
    let status = status_with_visibility(Visibility::Public);
    assert!(is_visible(&status, Some(author()), &NON_FOLLOWER));
}

// -- Unlisted: visible to any authenticated viewer, not unauthenticated --

#[test]
fn unlisted_is_invisible_to_unauthenticated_viewer() {
    // Requirement 6.4: unauthenticated retrieval returns *only* `Public`
    // posts, so `Unlisted` must not leak to `viewer: None`.
    let status = status_with_visibility(Visibility::Unlisted);
    assert!(!is_visible(&status, None, &NON_FOLLOWER));
}

#[test]
fn unlisted_is_visible_to_any_authenticated_viewer() {
    let status = status_with_visibility(Visibility::Unlisted);
    assert!(is_visible(&status, Some(other_viewer()), &NON_FOLLOWER));
}

#[test]
fn unlisted_is_visible_to_the_author() {
    let status = status_with_visibility(Visibility::Unlisted);
    assert!(is_visible(&status, Some(author()), &NON_FOLLOWER));
}

// -- Private: visible to the author and to followers only ----------------

#[test]
fn private_is_invisible_to_unauthenticated_viewer() {
    let status = status_with_visibility(Visibility::Private);
    assert!(!is_visible(&status, None, &NON_FOLLOWER));
}

#[test]
fn private_is_invisible_to_an_authenticated_non_follower() {
    let status = status_with_visibility(Visibility::Private);
    assert!(!is_visible(&status, Some(other_viewer()), &NON_FOLLOWER));
}

#[test]
fn private_is_visible_to_an_authenticated_follower() {
    let status = status_with_visibility(Visibility::Private);
    assert!(is_visible(&status, Some(other_viewer()), &FOLLOWER));
}

#[test]
fn private_is_visible_to_the_author_even_when_rel_reports_non_follower() {
    // The author is not "their own follower" in any real ViewerRelation
    // resolution, so the author-bypass must not depend on `rel` at all.
    let status = status_with_visibility(Visibility::Private);
    assert!(is_visible(&status, Some(author()), &NON_FOLLOWER));
}

// -- Direct: visible to the author only (see module doc comment) ---------

#[test]
fn direct_is_invisible_to_unauthenticated_viewer() {
    let status = status_with_visibility(Visibility::Direct);
    assert!(!is_visible(&status, None, &NON_FOLLOWER));
}

#[test]
fn direct_is_invisible_to_an_authenticated_non_mentioned_viewer() {
    let status = status_with_visibility(Visibility::Direct);
    assert!(!is_visible(&status, Some(other_viewer()), &NON_FOLLOWER));
}

#[test]
fn direct_is_invisible_to_a_follower_who_is_not_the_author() {
    // Being a follower confers no special access to a `direct` post —
    // `Direct`'s branch does not consult `rel` at all.
    let status = status_with_visibility(Visibility::Direct);
    assert!(!is_visible(&status, Some(other_viewer()), &FOLLOWER));
}

#[test]
fn direct_is_visible_to_the_author() {
    let status = status_with_visibility(Visibility::Direct);
    assert!(is_visible(&status, Some(author()), &NON_FOLLOWER));
}

// -- Single function covers all 4 visibility kinds (Requirement 4.1) -----

#[test]
fn is_visible_exhaustively_covers_all_four_visibility_kinds_via_one_function() {
    // Every kind is decided by the very same `is_visible` call — no
    // per-kind dispatch happens at any caller. Table-driven over an
    // exhaustively-matched local closure proves no kind is silently
    // skipped: adding a fifth `Visibility` variant would fail to compile
    // this match (mirrors `model.rs`'s own `visibility_ordinal` technique).
    fn expected_for_author(v: Visibility) -> bool {
        match v {
            Visibility::Public
            | Visibility::Unlisted
            | Visibility::Private
            | Visibility::Direct => {
                true // the author always sees their own post
            }
        }
    }

    for visibility in [
        Visibility::Public,
        Visibility::Unlisted,
        Visibility::Private,
        Visibility::Direct,
    ] {
        let status = status_with_visibility(visibility);
        assert_eq!(
            is_visible(&status, Some(author()), &NON_FOLLOWER),
            expected_for_author(visibility),
            "author visibility mismatch for {visibility:?}"
        );
    }
}

// -- NoRelationshipQuery: safe default (non-follower, empty followers) ---

#[tokio::test]
async fn no_relationship_query_reports_the_viewer_as_a_non_follower() {
    let query = NoRelationshipQuery;
    let rel = query
        .viewer_relation(author(), Some(other_viewer()))
        .await
        .expect("the default RelationshipQuery must never fail");
    assert_eq!(rel, ViewerRelation { is_follower: false });
}

#[tokio::test]
async fn no_relationship_query_reports_non_follower_even_for_an_unauthenticated_viewer() {
    let query = NoRelationshipQuery;
    let rel = query
        .viewer_relation(author(), None)
        .await
        .expect("the default RelationshipQuery must never fail");
    assert!(!rel.is_follower);
}

#[tokio::test]
async fn no_relationship_query_reports_an_empty_followers_set() {
    let query = NoRelationshipQuery;
    let followers = query
        .followers_of(author())
        .await
        .expect("the default RelationshipQuery must never fail");
    assert!(followers.is_empty());
}

// -- Composition: is_visible + NoRelationshipQuery's default is fail-safe
// for `private` when social-graph is not wired in --------------------------

#[tokio::test]
async fn private_post_is_invisible_to_a_non_author_under_the_default_relationship_query() {
    // Requirement 4.1 / design.md: "social-graph 未配線でも安全側の結果
    // （非フォロワー扱い）で単独動作する" — end-to-end through the port.
    let query = NoRelationshipQuery;
    let status = status_with_visibility(Visibility::Private);
    let rel = query
        .viewer_relation(status.actor_id, Some(other_viewer()))
        .await
        .unwrap();
    assert!(!is_visible(&status, Some(other_viewer()), &rel));
}
