//! Unit tests for `ObjectDocumentRegistry` and `OutboxSourceRegistry`
//! (Requirements 6.2, 6.6, 8.1, 8.2, 8.3), per task 3.5's observable
//! completion condition: "プロバイダ未登録時に解決要求が None を返し、スタ
//! ブプロバイダ登録後はその URL 空間の解決結果が返り、複数の `OutboxSource`
//! を登録すると収集結果が束ねられる".
//!
//! Pure in-memory logic — no DB, no HTTP; plain `#[tokio::test]` unit tests.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;

use serde_json::json;
use time::OffsetDateTime;

use super::*;
use crate::actor::keys::repository::{SigningKeyStatus, StoredSigningKey, insert_active_key};
use crate::actor::model::{ActorState, LocalActor};
use crate::actor::owner::create_owner;
use crate::actor::repository::insert_actor;
use crate::domain::Id;
use crate::federation::urls::ActorUrls;
use crate::test_harness::spawn_test_app;

fn handle(raw: &str) -> Handle {
    Handle::new(raw).expect("test handle must be valid")
}

/// A stub `ObjectDocumentProvider` that owns every URL starting with
/// `prefix` and always resolves to `body` for URLs it owns.
struct StubObjectProvider {
    prefix: &'static str,
    body: serde_json::Value,
}

impl StubObjectProvider {
    fn new(prefix: &'static str, body: serde_json::Value) -> Arc<Self> {
        Arc::new(Self { prefix, body })
    }
}

impl ObjectDocumentProvider for StubObjectProvider {
    fn can_resolve(&self, url: &str) -> bool {
        url.starts_with(self.prefix)
    }

    fn resolve<'a>(
        &'a self,
        _url: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<serde_json::Value>, AppError>> + Send + 'a>>
    {
        Box::pin(async move { Ok(Some(self.body.clone())) })
    }
}

/// A stub `ObjectDocumentProvider` that never claims any URL (used to prove
/// a non-matching provider is skipped, not stopped on).
struct NeverMatchesProvider;

impl ObjectDocumentProvider for NeverMatchesProvider {
    fn can_resolve(&self, _url: &str) -> bool {
        false
    }

    fn resolve<'a>(
        &'a self,
        _url: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<serde_json::Value>, AppError>> + Send + 'a>>
    {
        Box::pin(async move {
            panic!("resolve must never be called on a provider whose can_resolve returned false")
        })
    }
}

// --- 1: empty registry returns None ---

#[tokio::test]
async fn empty_object_document_registry_returns_none() {
    let registry = ObjectDocumentRegistry::new();

    let result = registry
        .resolve("https://kawasemi.example/objects/1")
        .await
        .expect("resolve must not error just because nothing is registered");

    assert_eq!(
        result, None,
        "an empty ObjectDocumentRegistry must answer None (safe default 404)"
    );
}

// --- 2: single matching provider ---

#[tokio::test]
async fn single_matching_provider_resolves_its_url_space() {
    let mut registry = ObjectDocumentRegistry::new();
    let body = json!({ "id": "https://kawasemi.example/objects/1", "type": "Note" });
    registry.register(StubObjectProvider::new(
        "https://kawasemi.example/objects/",
        body.clone(),
    ));

    let result = registry
        .resolve("https://kawasemi.example/objects/1")
        .await
        .expect("resolve must succeed");

    assert_eq!(
        result,
        Some(body),
        "a registered provider's own resolve() result must be returned for a URL it owns"
    );
}

// --- 3: non-matching URL still returns None ---

#[tokio::test]
async fn non_matching_url_returns_none_even_with_a_provider_registered() {
    let mut registry = ObjectDocumentRegistry::new();
    registry.register(StubObjectProvider::new(
        "https://kawasemi.example/objects/",
        json!({ "id": "https://kawasemi.example/objects/1" }),
    ));

    let result = registry
        .resolve("https://kawasemi.example/collections/outbox")
        .await
        .expect("resolve must succeed even for an unclaimed URL");

    assert_eq!(
        result, None,
        "a URL no registered provider claims must resolve to None (404), not error"
    );
}

// --- 4: first-match-wins ordering ---

#[tokio::test]
async fn first_registered_matching_provider_wins_over_a_later_one() {
    let mut registry = ObjectDocumentRegistry::new();
    let first_body = json!({ "source": "first" });
    let second_body = json!({ "source": "second" });
    // Both providers claim every URL (empty prefix matches everything).
    registry.register(StubObjectProvider::new("", first_body.clone()));
    registry.register(StubObjectProvider::new("", second_body.clone()));

    let result = registry
        .resolve("https://kawasemi.example/objects/1")
        .await
        .expect("resolve must succeed");

    assert_eq!(
        result,
        Some(first_body),
        "when multiple registered providers both claim a URL, the FIRST-registered one's \
         resolve() result must win, not the second's"
    );
}

// --- 5: a non-matching provider registered before a matching one is skipped ---

#[tokio::test]
async fn non_matching_provider_registered_first_is_skipped_for_a_later_matching_one() {
    let mut registry = ObjectDocumentRegistry::new();
    let body = json!({ "source": "second-provider" });
    registry.register(Arc::new(NeverMatchesProvider));
    registry.register(StubObjectProvider::new(
        "https://kawasemi.example/objects/",
        body.clone(),
    ));

    let result = registry
        .resolve("https://kawasemi.example/objects/1")
        .await
        .expect("resolve must succeed");

    assert_eq!(
        result,
        Some(body),
        "a provider whose can_resolve is false must be skipped (not short-circuit to None) \
         when a later provider matches"
    );
}

/// A stub `OutboxSource` that returns a fixed, distinguishable page and
/// records the `actor`/`page` it was called with.
struct StubOutboxSource {
    page_to_return: OutboxItemsPage,
    seen: Mutex<Option<(Handle, PageCursor)>>,
}

impl StubOutboxSource {
    fn new(page_to_return: OutboxItemsPage) -> Arc<Self> {
        Arc::new(Self {
            page_to_return,
            seen: Mutex::new(None),
        })
    }
}

impl OutboxSource for StubOutboxSource {
    fn outbox_page<'a>(
        &'a self,
        actor: &'a Handle,
        page: PageCursor,
    ) -> Pin<Box<dyn Future<Output = Result<OutboxItemsPage, AppError>> + Send + 'a>> {
        Box::pin(async move {
            *self.seen.lock().unwrap() = Some((actor.clone(), page));
            Ok(self.page_to_return.clone())
        })
    }
}

// --- 6: empty registry returns empty collection ---

#[tokio::test]
async fn empty_outbox_source_registry_collects_nothing() {
    let registry = OutboxSourceRegistry::new();

    let result = registry
        .collect(&handle("alice"), PageCursor::start())
        .await
        .expect("collect must not error just because nothing is registered");

    assert_eq!(
        result,
        Vec::new(),
        "an empty OutboxSourceRegistry must answer an empty Vec (safe default empty outbox)"
    );
}

// --- 7: multiple sources are all collected, distinguishable contents ---

#[tokio::test]
async fn multiple_registered_sources_are_all_collected() {
    let mut registry = OutboxSourceRegistry::new();
    let statuses_page = OutboxItemsPage {
        items: vec![json!({ "type": "Create", "source": "statuses-core" })],
        next: None,
    };
    let announces_page = OutboxItemsPage {
        items: vec![
            json!({ "type": "Announce", "source": "statuses-core-announce" }),
            json!({ "type": "Announce", "source": "statuses-core-announce-2" }),
        ],
        next: Some(PageCursor::token("announce-cursor-2")),
    };
    registry.register(StubOutboxSource::new(statuses_page.clone()));
    registry.register(StubOutboxSource::new(announces_page.clone()));

    let result = registry
        .collect(&handle("alice"), PageCursor::start())
        .await
        .expect("collect must succeed");

    assert_eq!(
        result.len(),
        2,
        "collect must return one OutboxItemsPage per registered source"
    );
    assert_eq!(
        result,
        vec![statuses_page, announces_page],
        "collect must return every registered source's own (distinguishable) result, unmerged, \
         in registration order"
    );
}

// --- 8: pass-through of the actor/page values ---

#[tokio::test]
async fn actor_and_page_are_passed_through_to_each_source_unchanged() {
    let mut registry = OutboxSourceRegistry::new();
    let source = StubOutboxSource::new(OutboxItemsPage {
        items: Vec::new(),
        next: None,
    });
    registry.register(Arc::clone(&source) as Arc<dyn OutboxSource>);

    let requested_handle = handle("bob");
    let requested_page = PageCursor::token("some-opaque-cursor-value");

    registry
        .collect(&requested_handle, requested_page.clone())
        .await
        .expect("collect must succeed");

    let seen = source
        .seen
        .lock()
        .unwrap()
        .clone()
        .expect("the registered source must have been called");
    assert_eq!(
        seen,
        (requested_handle, requested_page),
        "the registry must pass the exact actor and page values through to each registered \
         source, untouched (this registry does not interpret/parse the cursor)"
    );
}

// ==========================================================================
// ActivityPubDocumentBuilder (task 3.6, `Boundary: ActivityPubDocumentBuilder`,
// Requirements 6.1, 6.2, 6.5, 8.1, 8.2, 8.3)
//
// `build_actor_document` tests use a real, Postgres-backed `ActorDirectory`
// via `spawn_test_app` (mirroring `src/actor/directory/tests.rs`'s and
// `src/federation/outbound/target/tests.rs`'s own established fixture
// pattern) -- this task's own instructions call for a real directory, not a
// mock, since `ActorDirectory` has no narrow port introduced for it here
// (unlike task 3.4's `LocalActorLookup`). `build_outbox_page` tests are pure
// in-memory over stub `OutboxSource`s (this module's own `StubOutboxSource`,
// reused from the `OutboxSourceRegistry` tests above), but still construct
// their `ActivityPubDocumentBuilder` with a real `ActorDirectory` (via the
// same `spawn_test_app`), since the builder always requires one -- these
// tests just never call anything that would query it.
// ==========================================================================

/// The fixed, non-production domain every `ActivityPubDocumentBuilder` test
/// in this section builds its `ActorUrls` from.
fn test_urls() -> ActorUrls {
    ActorUrls::new("kawasemi.doc-builder-test.internal")
}

/// Creates a real owner fixture (mirrors `src/actor/directory/tests.rs`'s
/// own fixture helper of the same name).
async fn create_owner_fixture(pool: &sqlx::PgPool, owner_id: Id, now: OffsetDateTime) {
    create_owner(pool, owner_id, now)
        .await
        .expect("creating the owner fixture must succeed");
}

/// Creates a real active actor fixture under an already-existing owner.
async fn insert_actor_fixture(
    pool: &sqlx::PgPool,
    owner_id: Id,
    actor_id: Id,
    handle_str: &str,
    now: OffsetDateTime,
) -> LocalActor {
    let actor = LocalActor {
        id: actor_id,
        owner_id,
        handle: handle(handle_str),
        actor_type: ActorType::Person,
        display_name: "Doc Builder Test Actor".to_string(),
        summary: "an actor used to test ActivityPubDocumentBuilder".to_string(),
        state: ActorState::Active,
        created_at: now,
        updated_at: now,
    };
    let mut tx = pool
        .begin()
        .await
        .expect("opening a transaction for the actor fixture must succeed");
    insert_actor(&mut tx, &actor)
        .await
        .expect("inserting the actor fixture must succeed");
    tx.commit()
        .await
        .expect("committing the actor fixture transaction must succeed");
    actor
}

/// Creates a real active signing-key fixture for `actor_id`.
async fn insert_active_key_fixture(
    pool: &sqlx::PgPool,
    key_id: Id,
    actor_id: Id,
    now: OffsetDateTime,
) -> StoredSigningKey {
    let key = StoredSigningKey {
        id: key_id,
        actor_id,
        algorithm: "rsa-2048".to_string(),
        public_key_pem: "-----BEGIN PUBLIC KEY-----\ndoc-builder-test\n-----END PUBLIC KEY-----"
            .to_string(),
        sealed_private_key: b"sealed-opaque-bytes".to_vec(),
        status: SigningKeyStatus::Active,
        created_at: now,
    };
    let mut tx = pool
        .begin()
        .await
        .expect("opening a transaction for the key fixture must succeed");
    insert_active_key(&mut tx, &key)
        .await
        .expect("inserting the active key fixture must succeed");
    tx.commit()
        .await
        .expect("committing the key fixture transaction must succeed");
    key
}

// --- build_actor_document ---

/// Requirement 6.1: the built actor document includes `id`/`inbox`/`outbox`
/// matching `ActorUrls`' own construction, and a `publicKey` block carrying
/// the PEM from a real, fixture-inserted active signing key.
#[tokio::test]
async fn build_actor_document_includes_id_inbox_outbox_and_public_key() {
    let app = spawn_test_app().await;
    let urls = test_urls();
    let directory = Arc::new(ActorDirectory::new(app.pool.clone()));

    let owner_id = app.runtime.ids.next_id();
    let actor_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    create_owner_fixture(&app.pool, owner_id, now).await;
    let actor = insert_actor_fixture(&app.pool, owner_id, actor_id, "doc_alice", now).await;
    let key_id = app.runtime.ids.next_id();
    let key = insert_active_key_fixture(&app.pool, key_id, actor_id, now).await;

    let resolved = directory
        .resolve_actor_by_handle(&actor.handle)
        .await
        .expect("resolve_actor_by_handle must succeed")
        .expect("the fixture actor must resolve");

    let builder = ActivityPubDocumentBuilder::new(
        urls.clone(),
        Arc::clone(&directory),
        OutboxSourceRegistry::new(),
    );

    let doc = builder
        .build_actor_document(&resolved)
        .await
        .expect("build_actor_document must succeed");

    assert_eq!(doc["id"], json!(urls.actor_url(&actor.handle)));
    assert_eq!(doc["inbox"], json!(urls.inbox_url(&actor.handle)));
    assert_eq!(doc["outbox"], json!(urls.outbox_url(&actor.handle)));
    assert_eq!(
        doc["publicKey"]["id"],
        json!(urls.key_id(&actor.handle)),
        "publicKey.id must be this actor's keyId per ActorUrls::key_id"
    );
    assert_eq!(
        doc["publicKey"]["publicKeyPem"],
        json!(key.public_key_pem),
        "publicKey.publicKeyPem must carry the real active signing key's PEM"
    );

    app.cleanup().await;
}

/// Requirement 6.5: the built actor document never includes any
/// owner-identifying field or value -- checked negatively over the whole
/// serialized document. Note this deliberately does NOT assert the absence
/// of the substring `"owner"` wholesale: the ActivityPub-standard
/// `publicKey.owner` field (a back-reference to the actor's own public URL,
/// asserted present by a sibling test) legitimately contains that word and
/// is not the "オーナー情報" (management-layer administrator identity) this
/// requirement is about -- see this file's `ActivityPubDocumentBuilder`
/// re-export's own doc comment. What this test actually proves: no field
/// literally named `owner_id`/`ownerId` appears anywhere, and the internal
/// `owners` row's own numeric id (distinct from every other id used here)
/// is not present anywhere in the serialized output.
#[tokio::test]
async fn build_actor_document_never_includes_owner_identifying_information() {
    let app = spawn_test_app().await;
    let urls = test_urls();
    let directory = Arc::new(ActorDirectory::new(app.pool.clone()));

    let owner_id = app.runtime.ids.next_id();
    let actor_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    create_owner_fixture(&app.pool, owner_id, now).await;
    let actor = insert_actor_fixture(&app.pool, owner_id, actor_id, "doc_no_owner", now).await;
    let key_id = app.runtime.ids.next_id();
    insert_active_key_fixture(&app.pool, key_id, actor_id, now).await;

    let resolved = directory
        .resolve_actor_by_handle(&actor.handle)
        .await
        .expect("resolve_actor_by_handle must succeed")
        .expect("the fixture actor must resolve");

    let builder =
        ActivityPubDocumentBuilder::new(urls, Arc::clone(&directory), OutboxSourceRegistry::new());

    let doc = builder
        .build_actor_document(&resolved)
        .await
        .expect("build_actor_document must succeed");

    let serialized = serde_json::to_string(&doc).expect("document must serialize to JSON");
    assert!(
        !serialized.contains("owner_id") && !serialized.contains("ownerId"),
        "no field literally named owner_id/ownerId may appear anywhere in the actor document; \
         found in: {serialized}"
    );
    assert!(
        !serialized.contains(&owner_id.as_i64().to_string()),
        "the internal owners row's own numeric id must never appear anywhere in the actor \
         document (owner_id is distinct from actor_id/key_id in this fixture); found in: \
         {serialized}"
    );

    app.cleanup().await;
}

/// Requirement 9.1 (as applied to this builder's own output, per this
/// module's doc comment): `@context` is present and matches this spec's
/// established `ACTIVITYSTREAMS_CONTEXT` JSON-LD context value.
#[tokio::test]
async fn build_actor_document_includes_the_activitystreams_context() {
    let app = spawn_test_app().await;
    let urls = test_urls();
    let directory = Arc::new(ActorDirectory::new(app.pool.clone()));

    let owner_id = app.runtime.ids.next_id();
    let actor_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    create_owner_fixture(&app.pool, owner_id, now).await;
    let actor = insert_actor_fixture(&app.pool, owner_id, actor_id, "doc_context", now).await;

    let resolved = directory
        .resolve_actor_by_handle(&actor.handle)
        .await
        .expect("resolve_actor_by_handle must succeed")
        .expect("the fixture actor must resolve");

    let builder =
        ActivityPubDocumentBuilder::new(urls, Arc::clone(&directory), OutboxSourceRegistry::new());

    let doc = builder
        .build_actor_document(&resolved)
        .await
        .expect("build_actor_document must succeed");

    assert_eq!(
        doc["@context"],
        json!(crate::federation::jsonld::ACTIVITYSTREAMS_CONTEXT),
        "@context must match this spec's established JSON-LD context value"
    );

    app.cleanup().await;
}

/// An actor with no active signing key gets no `publicKey` field at all
/// (this task's documented decision -- see `ActivityPubDocumentBuilder`'s
/// own doc comment, "Actor document shape"), never an error and never a
/// null/empty placeholder.
#[tokio::test]
async fn build_actor_document_omits_public_key_when_actor_has_no_active_key() {
    let app = spawn_test_app().await;
    let urls = test_urls();
    let directory = Arc::new(ActorDirectory::new(app.pool.clone()));

    let owner_id = app.runtime.ids.next_id();
    let actor_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    create_owner_fixture(&app.pool, owner_id, now).await;
    let actor = insert_actor_fixture(&app.pool, owner_id, actor_id, "doc_keyless", now).await;

    let resolved = directory
        .resolve_actor_by_handle(&actor.handle)
        .await
        .expect("resolve_actor_by_handle must succeed")
        .expect("the fixture actor must resolve");

    let builder =
        ActivityPubDocumentBuilder::new(urls, Arc::clone(&directory), OutboxSourceRegistry::new());

    let doc = builder
        .build_actor_document(&resolved)
        .await
        .expect("build_actor_document must succeed even with no active signing key");

    assert!(
        doc.get("publicKey").is_none(),
        "an actor with no active signing key must get no publicKey field at all, found: {doc:?}"
    );

    app.cleanup().await;
}

// --- build_outbox_page ---

/// Requirements 8.1, 8.2, 8.3: with zero `OutboxSource`s registered, the
/// built page is a validly-shaped, empty `OrderedCollectionPage` -- nothing
/// fabricated.
#[tokio::test]
async fn build_outbox_page_with_no_registered_sources_yields_an_empty_but_validly_shaped_page() {
    let app = spawn_test_app().await;
    let urls = test_urls();
    let directory = Arc::new(ActorDirectory::new(app.pool.clone()));
    let builder =
        ActivityPubDocumentBuilder::new(urls.clone(), directory, OutboxSourceRegistry::new());

    let actor_handle = handle("outbox_empty");
    let page = builder
        .build_outbox_page(&actor_handle, PageCursor::start())
        .await
        .expect("build_outbox_page must succeed with nothing registered");

    assert_eq!(page["type"], json!("OrderedCollectionPage"));
    assert_eq!(
        page["orderedItems"],
        json!([]),
        "an empty registry must yield an empty orderedItems array, not a fabricated one"
    );
    assert_eq!(page["partOf"], json!(urls.outbox_url(&actor_handle)));
    assert_eq!(
        page["id"],
        json!(format!("{}?page=true", urls.outbox_url(&actor_handle)))
    );
    assert!(
        page.get("next").is_none(),
        "an empty registry must not fabricate a next cursor"
    );

    app.cleanup().await;
}

/// The task's own observable completion condition: with two or more stub
/// `OutboxSource`s registered (each returning distinguishable content), the
/// built page's `orderedItems` contains exactly the union of their supplied
/// items -- nothing invented, nothing dropped.
#[tokio::test]
async fn build_outbox_page_bundles_exactly_the_union_of_all_registered_sources() {
    let app = spawn_test_app().await;
    let urls = test_urls();
    let directory = Arc::new(ActorDirectory::new(app.pool.clone()));

    let mut sources = OutboxSourceRegistry::new();
    let item_a = json!({ "type": "Create", "source": "a", "published": "2024-01-01T00:00:00Z" });
    let item_b = json!({ "type": "Announce", "source": "b", "published": "2024-01-02T00:00:00Z" });
    sources.register(StubOutboxSource::new(OutboxItemsPage {
        items: vec![item_a.clone()],
        next: None,
    }));
    sources.register(StubOutboxSource::new(OutboxItemsPage {
        items: vec![item_b.clone()],
        next: None,
    }));

    let builder = ActivityPubDocumentBuilder::new(urls, directory, sources);

    let page = builder
        .build_outbox_page(&handle("outbox_union"), PageCursor::start())
        .await
        .expect("build_outbox_page must succeed");

    let items = page["orderedItems"]
        .as_array()
        .expect("orderedItems must be a JSON array");
    assert_eq!(
        items.len(),
        2,
        "exactly the union of both sources' items, nothing more"
    );
    assert!(items.contains(&item_a));
    assert!(items.contains(&item_b));

    app.cleanup().await;
}

/// Requirement 8.1's "順序付きコレクション": items collected from multiple
/// sources are ordered chronologically by their own `published` field, not
/// merely in registration order.
#[tokio::test]
async fn build_outbox_page_orders_items_chronologically_by_published() {
    let app = spawn_test_app().await;
    let urls = test_urls();
    let directory = Arc::new(ActorDirectory::new(app.pool.clone()));

    let newer = json!({ "type": "Create", "id": "newer", "published": "2024-06-01T00:00:00Z" });
    let older = json!({ "type": "Create", "id": "older", "published": "2024-01-01T00:00:00Z" });

    let mut sources = OutboxSourceRegistry::new();
    // Registered "newer first" -- the merge must still sort chronologically,
    // not just preserve registration order.
    sources.register(StubOutboxSource::new(OutboxItemsPage {
        items: vec![newer.clone()],
        next: None,
    }));
    sources.register(StubOutboxSource::new(OutboxItemsPage {
        items: vec![older.clone()],
        next: None,
    }));

    let builder = ActivityPubDocumentBuilder::new(urls, directory, sources);
    let page = builder
        .build_outbox_page(&handle("outbox_order"), PageCursor::start())
        .await
        .expect("build_outbox_page must succeed");

    let items = page["orderedItems"]
        .as_array()
        .expect("orderedItems must be a JSON array");
    assert_eq!(
        items,
        &vec![older, newer],
        "items must be ordered ascending by their own published timestamp, not registration order"
    );

    app.cleanup().await;
}

/// This builder's documented fallback: items missing `published` (or where
/// it is present but not a JSON string) sort after every dated item,
/// preserving their original relative (registration-then-page) order among
/// themselves.
#[tokio::test]
async fn build_outbox_page_sorts_items_missing_published_after_all_dated_items_preserving_order() {
    let app = spawn_test_app().await;
    let urls = test_urls();
    let directory = Arc::new(ActorDirectory::new(app.pool.clone()));

    let dated = json!({ "type": "Create", "id": "dated", "published": "2024-01-01T00:00:00Z" });
    let undated_first = json!({ "type": "Create", "id": "undated-first" });
    let undated_second = json!({ "type": "Create", "id": "undated-second", "published": 12345 });

    let mut sources = OutboxSourceRegistry::new();
    sources.register(StubOutboxSource::new(OutboxItemsPage {
        items: vec![undated_first.clone(), dated.clone()],
        next: None,
    }));
    sources.register(StubOutboxSource::new(OutboxItemsPage {
        items: vec![undated_second.clone()],
        next: None,
    }));

    let builder = ActivityPubDocumentBuilder::new(urls, directory, sources);
    let page = builder
        .build_outbox_page(&handle("outbox_missing_published"), PageCursor::start())
        .await
        .expect("build_outbox_page must succeed");

    let items = page["orderedItems"]
        .as_array()
        .expect("orderedItems must be a JSON array");
    assert_eq!(
        items,
        &vec![dated, undated_first, undated_second],
        "the one dated item must sort first; the two undated/malformed-published items must \
         sort after it, in their original relative (registration-then-page) order"
    );

    app.cleanup().await;
}

/// This builder's documented MVP next-cursor rule: the first non-`None`
/// `next` cursor reported by any registered source (in registration order)
/// is propagated as this page's own `next`.
#[tokio::test]
async fn build_outbox_page_propagates_the_first_non_none_next_cursor() {
    let app = spawn_test_app().await;
    let urls = test_urls();
    let directory = Arc::new(ActorDirectory::new(app.pool.clone()));

    let mut sources = OutboxSourceRegistry::new();
    sources.register(StubOutboxSource::new(OutboxItemsPage {
        items: vec![],
        next: None,
    }));
    sources.register(StubOutboxSource::new(OutboxItemsPage {
        items: vec![],
        next: Some(PageCursor::token("second-source-cursor")),
    }));
    sources.register(StubOutboxSource::new(OutboxItemsPage {
        items: vec![],
        next: Some(PageCursor::token("third-source-cursor")),
    }));

    let builder = ActivityPubDocumentBuilder::new(urls.clone(), directory, sources);
    let actor_handle = handle("outbox_next_cursor");
    let page = builder
        .build_outbox_page(&actor_handle, PageCursor::start())
        .await
        .expect("build_outbox_page must succeed");

    assert_eq!(
        page["next"],
        json!(format!(
            "{}?page=second-source-cursor",
            urls.outbox_url(&actor_handle)
        )),
        "the first registered source's non-None next cursor must win, not a later one's"
    );

    app.cleanup().await;
}
