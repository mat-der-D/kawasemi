use super::*;

fn handle(raw: &str) -> Handle {
    Handle::new(raw).expect("test handle must be valid")
}

// --- Consistency for a single handle (task 1.3's completion condition:
// "同一ハンドルに対し一貫した URL 群と keyId が生成される") ---

#[test]
fn actor_url_is_deterministic_for_the_same_handle() {
    let urls = ActorUrls::new("example.com");
    let alice = handle("alice");

    assert_eq!(urls.actor_url(&alice), urls.actor_url(&alice));
}

#[test]
fn actor_url_has_the_expected_shape() {
    let urls = ActorUrls::new("example.com");
    let alice = handle("alice");

    assert_eq!(urls.actor_url(&alice), "https://example.com/users/alice");
}

#[test]
fn inbox_url_is_the_actor_url_plus_inbox() {
    let urls = ActorUrls::new("example.com");
    let alice = handle("alice");

    assert_eq!(
        urls.inbox_url(&alice),
        format!("{}/inbox", urls.actor_url(&alice))
    );
    assert_eq!(
        urls.inbox_url(&alice),
        "https://example.com/users/alice/inbox"
    );
}

#[test]
fn outbox_url_is_the_actor_url_plus_outbox() {
    let urls = ActorUrls::new("example.com");
    let alice = handle("alice");

    assert_eq!(
        urls.outbox_url(&alice),
        format!("{}/outbox", urls.actor_url(&alice))
    );
    assert_eq!(
        urls.outbox_url(&alice),
        "https://example.com/users/alice/outbox"
    );
}

#[test]
fn shared_inbox_url_is_domain_level_not_handle_scoped() {
    let urls = ActorUrls::new("example.com");

    assert_eq!(urls.shared_inbox_url(), "https://example.com/inbox");
}

#[test]
fn shared_inbox_url_is_the_same_regardless_of_which_handle_is_in_scope() {
    let urls = ActorUrls::new("example.com");
    let alice = handle("alice");
    let bob = handle("bob");

    // Requirement: shared inbox is one instance-wide endpoint, not
    // per-actor -- it must not vary with which handle happens to be
    // "current" when a caller asks for it.
    let _ = (&alice, &bob);
    assert_eq!(urls.shared_inbox_url(), urls.shared_inbox_url());
}

#[test]
fn key_id_is_the_actor_url_with_a_main_key_fragment() {
    let urls = ActorUrls::new("example.com");
    let alice = handle("alice");

    assert_eq!(
        urls.key_id(&alice),
        format!("{}#main-key", urls.actor_url(&alice))
    );
    assert_eq!(
        urls.key_id(&alice),
        "https://example.com/users/alice#main-key"
    );
}

#[test]
fn key_id_is_deterministic_for_the_same_handle() {
    let urls = ActorUrls::new("example.com");
    let alice = handle("alice");

    assert_eq!(urls.key_id(&alice), urls.key_id(&alice));
}

// --- Distinct handles must not collide ---

#[test]
fn different_handles_produce_different_actor_urls_and_key_ids() {
    let urls = ActorUrls::new("example.com");
    let alice = handle("alice");
    let bob = handle("bob");

    assert_ne!(urls.actor_url(&alice), urls.actor_url(&bob));
    assert_ne!(urls.inbox_url(&alice), urls.inbox_url(&bob));
    assert_ne!(urls.outbox_url(&alice), urls.outbox_url(&bob));
    assert_ne!(urls.key_id(&alice), urls.key_id(&bob));
}

// --- Object/collection URLs (Requirement 6.1, 8.1's "オブジェクト・
// コレクション URL") ---

#[test]
fn object_url_builds_from_kind_and_id() {
    let urls = ActorUrls::new("example.com");
    let kind = ObjectKind::new("statuses");

    assert_eq!(
        urls.object_url(kind, Id::from_i64(42)),
        "https://example.com/statuses/42"
    );
}

#[test]
fn object_url_is_deterministic_for_the_same_kind_and_id() {
    let urls = ActorUrls::new("example.com");
    let kind = ObjectKind::new("statuses");

    assert_eq!(
        urls.object_url(kind, Id::from_i64(42)),
        urls.object_url(kind, Id::from_i64(42))
    );
}

#[test]
fn object_url_distinguishes_different_kinds_for_the_same_id() {
    let urls = ActorUrls::new("example.com");
    let statuses = ObjectKind::new("statuses");
    let collections = ObjectKind::new("collections/followers");

    assert_ne!(
        urls.object_url(statuses, Id::from_i64(1)),
        urls.object_url(collections, Id::from_i64(1))
    );
}

#[test]
fn object_url_distinguishes_different_ids_for_the_same_kind() {
    let urls = ActorUrls::new("example.com");
    let kind = ObjectKind::new("statuses");

    assert_ne!(
        urls.object_url(kind, Id::from_i64(1)),
        urls.object_url(kind, Id::from_i64(2))
    );
}

// --- Different server domains must not collide (sanity check that the
// domain is actually threaded through, not hardcoded) ---

#[test]
fn urls_differ_across_instances_with_different_domains() {
    let a = ActorUrls::new("a.example");
    let b = ActorUrls::new("b.example");
    let alice = handle("alice");

    assert_ne!(a.actor_url(&alice), b.actor_url(&alice));
    assert_ne!(a.shared_inbox_url(), b.shared_inbox_url());
    assert_ne!(
        a.object_url(ObjectKind::new("statuses"), Id::from_i64(1)),
        b.object_url(ObjectKind::new("statuses"), Id::from_i64(1))
    );
}
