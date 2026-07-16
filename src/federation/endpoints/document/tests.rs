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

use super::*;

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
