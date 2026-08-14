//! DB-backed integration tests for
//! [`kawasemi::notifications::service::NotificationService`]'s four public
//! methods — `list` (pagination, `types`/`account_id` filtering, dismissed
//! exclusion, `status` embedding), `show` (other-recipient/nonexistent ->
//! 404), `dismiss` and `clear` — driven directly against the service, on a
//! real `spawn_test_app`-backed Postgres schema.
//!
//! These tests were moved here from `src/notifications/service/tests.rs` by
//! `.kiro/specs/test-placement-migration` task 4.2, so that steering
//! `structure.md`'s test layout rule ("DB込みの実起動インスタンスを要する検証
//! は `tests/` 直下の `*_it.rs` に置く") holds in fact and not only on paper.
//! Every assertion is unchanged from before the move; only import
//! qualification changed (`crate::` -> `kawasemi::`, and the glob over the
//! production module expanded into explicit imports). The source file
//! retains the three `RequiredPolls` unit tests, which use the lighter
//! `spawn_test_db` fixture and therefore were never in scope for this move;
//! the two fixture helpers they share with this file (`sample_status`,
//! `create_test_status`) are consequently duplicated rather than moved.
//!
//! ## Relationship to the router-level notification integration tests
//! `tests/notifications_endpoints_it.rs` covers the same module's HTTP
//! surface (auth, scopes, query-parameter wiring, `Link` headers,
//! `account_id` string resolution), and `tests/notification_list_it.rs`,
//! `tests/notification_show_dismiss_it.rs` and
//! `tests/notification_contract_it.rs` drive the fully-wired production
//! router. This file's entry point is different: it calls
//! `NotificationService`'s own methods, so it is the component-level check
//! of the retrieval/dismissal business itself. Both sets are kept as-is;
//! neither subsumes the other.
//!
//! The fixture plumbing below mirrors
//! `src/social_graph/follow_request_service/tests.rs`'s own established
//! conventions (`create_test_actor` is an exact copy of that module's own
//! helper of the same name) and seeds `Notification` rows directly via
//! `kawasemi::notifications::repository::insert_dedup` — this service never
//! generates notifications itself, that being `NotificationGenerator`'s
//! boundary, so the tests seed rows rather than routing through a generator
//! this module does not depend on.

use axum::http::StatusCode;
use serde_json::Value;
use sqlx::PgPool;

use kawasemi::actor::owner::create_owner;
use kawasemi::actor::repository::insert_actor;
use kawasemi::actor::{ActorState, ActorType, Handle};
use kawasemi::api::pagination::PageParams;
use kawasemi::domain::{AccountRef, Id, Visibility};
use kawasemi::notifications::model::{Notification, NotificationType};
use kawasemi::notifications::repository::{self, ListFilter, insert_dedup};
use kawasemi::notifications::service::NotificationService;
use kawasemi::oauth::model::RequestActorContext;
use kawasemi::runtime::RuntimeContext;
use kawasemi::statuses::interaction_repository;
use kawasemi::statuses::model::{Poll, PollOption, Status, Tag};
use kawasemi::statuses::poll_repository;
use kawasemi::statuses::status_repository::{self, insert_status};
use kawasemi::statuses::tag_repository::{associate_tag, upsert_tag};
use kawasemi::test_harness::{TestApp, spawn_test_app};

/// Creates a real owner + local actor row, returning the actor's `Id` — an
/// exact copy of `follow_request_service/tests.rs::create_test_actor`.
async fn create_test_actor(app: &TestApp, handle: &str) -> Id {
    let now = app.runtime.clock.now();
    let owner_id = app.runtime.ids.next_id();
    create_owner(&app.pool, owner_id, now)
        .await
        .expect("creating the owner must succeed");

    let actor_id = app.runtime.ids.next_id();
    let actor = kawasemi::actor::model::LocalActor {
        id: actor_id,
        owner_id,
        handle: Handle::new(handle).expect("test handle must be valid"),
        actor_type: ActorType::Person,
        display_name: "Test Actor".to_string(),
        summary: "a test actor".to_string(),
        state: ActorState::Active,
        created_at: now,
        updated_at: now,
    };
    let mut tx = app
        .pool
        .begin()
        .await
        .expect("opening a transaction must succeed");
    insert_actor(&mut tx, &actor)
        .await
        .expect("insert_actor must succeed");
    tx.commit().await.expect("committing must succeed");

    actor_id
}

fn sample_status(id: Id, actor_id: Id, created_at: time::OffsetDateTime) -> Status {
    Status {
        id,
        actor_id,
        uri: format!("https://kawasemi.example/statuses/{}", id.as_i64()),
        url: None,
        content: "hello from a notification-embedded status".to_string(),
        visibility: Visibility::Public,
        sensitive: false,
        spoiler_text: String::new(),
        in_reply_to_id: None,
        in_reply_to_account_id: None,
        reblog_of_id: None,
        poll_id: None,
        language: None,
        reblogs_count: 0,
        favourites_count: 0,
        replies_count: 0,
        local: true,
        created_at,
        edited_at: None,
    }
}

async fn create_test_status(pool: &PgPool, runtime: &RuntimeContext, actor_id: Id) -> Id {
    let id = runtime.ids.next_id();
    let now = runtime.clock.now();
    insert_status(pool, &sample_status(id, actor_id, now))
        .await
        .expect("insert_status must succeed");
    id
}

#[allow(clippy::too_many_arguments)]
fn sample_notification(
    id: Id,
    recipient_id: Id,
    kind: NotificationType,
    origin: AccountRef,
    status_id: Option<Id>,
    created_at: time::OffsetDateTime,
) -> Notification {
    Notification {
        id,
        recipient_id,
        kind,
        origin,
        status_id,
        dismissed: false,
        created_at,
    }
}

async fn seed_notification(
    app: &TestApp,
    recipient_id: Id,
    kind: NotificationType,
    origin: AccountRef,
    status_id: Option<Id>,
) -> Id {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    let notification = sample_notification(id, recipient_id, kind, origin, status_id, now);
    insert_dedup(&app.pool, &notification)
        .await
        .expect("insert_dedup must succeed");
    id
}

fn build_service(app: &TestApp) -> NotificationService {
    NotificationService::new(
        app.pool.clone(),
        app.runtime.clone(),
        app.state.config().server.domain.clone(),
        app.state.accounts().service(),
        app.state.media().store().clone(),
    )
}

fn ctx_for(actor_id: Id) -> RequestActorContext {
    RequestActorContext {
        actor_id,
        scopes: kawasemi::oauth::model::ScopeSet::default(),
    }
}

// -- list -------------------------------------------------------------------

/// Requirement 2.1: a recipient's list only ever contains their own
/// notifications, even when another recipient also has some.
#[tokio::test]
async fn list_returns_only_the_requesting_recipients_notifications() {
    let app = spawn_test_app().await;
    let service = build_service(&app);

    let recipient = create_test_actor(&app, "list_recipient").await;
    let other_recipient = create_test_actor(&app, "list_other_recipient").await;
    let origin = create_test_actor(&app, "list_origin").await;

    seed_notification(
        &app,
        recipient,
        NotificationType::Follow,
        AccountRef::Local(origin),
        None,
    )
    .await;
    seed_notification(
        &app,
        other_recipient,
        NotificationType::Follow,
        AccountRef::Local(origin),
        None,
    )
    .await;

    let page = service
        .list(
            &ctx_for(recipient),
            PageParams::default(),
            ListFilter::default(),
        )
        .await
        .expect("list must succeed");

    assert_eq!(
        page.items.len(),
        1,
        "only the requesting recipient's own notification"
    );
    assert_eq!(
        page.items[0].get("type").and_then(|v| v.as_str()),
        Some("follow")
    );

    app.cleanup().await;
}

/// Requirement 2.4: a dismissed notification never appears in `list`.
#[tokio::test]
async fn list_excludes_dismissed_notifications() {
    let app = spawn_test_app().await;
    let service = build_service(&app);

    let recipient = create_test_actor(&app, "list_dismiss_recipient").await;
    let origin = create_test_actor(&app, "list_dismiss_origin").await;

    let notification_id = seed_notification(
        &app,
        recipient,
        NotificationType::Follow,
        AccountRef::Local(origin),
        None,
    )
    .await;

    let dismissed = repository::dismiss(&app.pool, notification_id, recipient)
        .await
        .expect("dismiss must succeed");
    assert!(dismissed);

    let page = service
        .list(
            &ctx_for(recipient),
            PageParams::default(),
            ListFilter::default(),
        )
        .await
        .expect("list must succeed");

    assert!(
        page.items.is_empty(),
        "a dismissed notification must not appear in list"
    );

    app.cleanup().await;
}

/// Requirement 2.2: `types`/`exclude_types` narrow the result set.
#[tokio::test]
async fn list_applies_types_filter() {
    let app = spawn_test_app().await;
    let service = build_service(&app);

    let recipient = create_test_actor(&app, "list_types_recipient").await;
    let origin = create_test_actor(&app, "list_types_origin").await;

    seed_notification(
        &app,
        recipient,
        NotificationType::Follow,
        AccountRef::Local(origin),
        None,
    )
    .await;
    seed_notification(
        &app,
        recipient,
        NotificationType::FollowRequest,
        AccountRef::Local(origin),
        None,
    )
    .await;

    let filter = ListFilter {
        types: Some(vec![NotificationType::Follow]),
        ..ListFilter::default()
    };
    let page = service
        .list(&ctx_for(recipient), PageParams::default(), filter)
        .await
        .expect("list must succeed");

    assert_eq!(page.items.len(), 1);
    assert_eq!(
        page.items[0].get("type").and_then(|v| v.as_str()),
        Some("follow")
    );

    app.cleanup().await;
}

/// Requirement 2.3: `account_id` (already-resolved `AccountRef`) narrows to
/// notifications from that origin only.
#[tokio::test]
async fn list_applies_account_id_filter() {
    let app = spawn_test_app().await;
    let service = build_service(&app);

    let recipient = create_test_actor(&app, "list_account_recipient").await;
    let origin_a = create_test_actor(&app, "list_account_origin_a").await;
    let origin_b = create_test_actor(&app, "list_account_origin_b").await;

    seed_notification(
        &app,
        recipient,
        NotificationType::Follow,
        AccountRef::Local(origin_a),
        None,
    )
    .await;
    seed_notification(
        &app,
        recipient,
        NotificationType::Follow,
        AccountRef::Local(origin_b),
        None,
    )
    .await;

    let filter = ListFilter {
        account_id: Some(AccountRef::Local(origin_a)),
        ..ListFilter::default()
    };
    let page = service
        .list(&ctx_for(recipient), PageParams::default(), filter)
        .await
        .expect("list must succeed");

    assert_eq!(page.items.len(), 1);
    assert_eq!(
        page.items[0]
            .get("account")
            .and_then(|a| a.get("id"))
            .and_then(|v| v.as_str()),
        Some(origin_a.as_i64().to_string()).as_deref()
    );

    app.cleanup().await;
}

/// Requirement 1.2: a post-related notification embeds the related status,
/// rendered from the recipient's own viewpoint.
#[tokio::test]
async fn list_embeds_related_status_for_post_related_kinds() {
    let app = spawn_test_app().await;
    let service = build_service(&app);

    let recipient = create_test_actor(&app, "list_status_recipient").await;
    let origin = create_test_actor(&app, "list_status_origin").await;
    let status_id = create_test_status(&app.pool, &app.runtime, recipient).await;

    seed_notification(
        &app,
        recipient,
        NotificationType::Favourite,
        AccountRef::Local(origin),
        Some(status_id),
    )
    .await;

    let page = service
        .list(
            &ctx_for(recipient),
            PageParams::default(),
            ListFilter::default(),
        )
        .await
        .expect("list must succeed");

    assert_eq!(page.items.len(), 1);
    let status = page.items[0]
        .get("status")
        .expect("status field must be present");
    assert!(!status.is_null(), "favourite notifications embed a status");
    assert_eq!(
        status.get("id").and_then(|v| v.as_str()),
        Some(status_id.as_i64().to_string()).as_deref()
    );

    app.cleanup().await;
}

/// Requirement 1.4: `follow`/`follow_request` never embed a status.
#[tokio::test]
async fn list_null_status_for_follow_kinds() {
    let app = spawn_test_app().await;
    let service = build_service(&app);

    let recipient = create_test_actor(&app, "list_follow_null_recipient").await;
    let origin = create_test_actor(&app, "list_follow_null_origin").await;

    seed_notification(
        &app,
        recipient,
        NotificationType::Follow,
        AccountRef::Local(origin),
        None,
    )
    .await;

    let page = service
        .list(
            &ctx_for(recipient),
            PageParams::default(),
            ListFilter::default(),
        )
        .await
        .expect("list must succeed");

    assert_eq!(page.items.len(), 1);
    assert!(
        page.items[0]
            .get("status")
            .expect("status key present")
            .is_null()
    );

    app.cleanup().await;
}

// -- show ---------------------------------------------------------------------

/// Requirement 3.1: a recipient can fetch their own notification by id.
#[tokio::test]
async fn show_returns_the_notification_for_its_own_recipient() {
    let app = spawn_test_app().await;
    let service = build_service(&app);

    let recipient = create_test_actor(&app, "show_recipient").await;
    let origin = create_test_actor(&app, "show_origin").await;
    let notification_id = seed_notification(
        &app,
        recipient,
        NotificationType::Follow,
        AccountRef::Local(origin),
        None,
    )
    .await;

    let json = service
        .show(&ctx_for(recipient), notification_id)
        .await
        .expect("show must succeed for the owning recipient");

    assert_eq!(
        json.get("id").and_then(|v| v.as_str()),
        Some(notification_id.as_i64().to_string()).as_deref()
    );

    app.cleanup().await;
}

/// Requirement 3.2: another recipient's notification 404s.
#[tokio::test]
async fn show_404_for_another_recipients_notification() {
    let app = spawn_test_app().await;
    let service = build_service(&app);

    let recipient = create_test_actor(&app, "show_other_recipient").await;
    let owner = create_test_actor(&app, "show_owner").await;
    let origin = create_test_actor(&app, "show_other_origin").await;
    let notification_id = seed_notification(
        &app,
        owner,
        NotificationType::Follow,
        AccountRef::Local(origin),
        None,
    )
    .await;

    let err = service
        .show(&ctx_for(recipient), notification_id)
        .await
        .expect_err("another recipient's notification must 404");
    assert_eq!(err.status, StatusCode::NOT_FOUND);

    app.cleanup().await;
}

/// Requirement 3.2: a nonexistent id 404s.
#[tokio::test]
async fn show_404_for_a_nonexistent_notification() {
    let app = spawn_test_app().await;
    let service = build_service(&app);
    let recipient = create_test_actor(&app, "show_nonexistent_recipient").await;

    let err = service
        .show(&ctx_for(recipient), app.runtime.ids.next_id())
        .await
        .expect_err("a nonexistent notification must 404");
    assert_eq!(err.status, StatusCode::NOT_FOUND);

    app.cleanup().await;
}

// -- dismiss / clear ------------------------------------------------------------

/// Requirements 4.2, 4.4: dismissing a notification removes it from
/// subsequent retrieval.
#[tokio::test]
async fn dismiss_excludes_from_subsequent_show_and_list() {
    let app = spawn_test_app().await;
    let service = build_service(&app);

    let recipient = create_test_actor(&app, "dismiss_recipient").await;
    let origin = create_test_actor(&app, "dismiss_origin").await;
    let notification_id = seed_notification(
        &app,
        recipient,
        NotificationType::Follow,
        AccountRef::Local(origin),
        None,
    )
    .await;

    service
        .dismiss(&ctx_for(recipient), notification_id)
        .await
        .expect("dismiss must succeed for the owning recipient");

    let show_err = service
        .show(&ctx_for(recipient), notification_id)
        .await
        .expect_err("a dismissed notification must no longer be retrievable");
    assert_eq!(show_err.status, StatusCode::NOT_FOUND);

    let page = service
        .list(
            &ctx_for(recipient),
            PageParams::default(),
            ListFilter::default(),
        )
        .await
        .expect("list must succeed");
    assert!(page.items.is_empty());

    app.cleanup().await;
}

/// Requirement 4.3: dismissing another recipient's notification 404s.
#[tokio::test]
async fn dismiss_404_for_another_recipients_notification() {
    let app = spawn_test_app().await;
    let service = build_service(&app);

    let recipient = create_test_actor(&app, "dismiss_other_recipient").await;
    let owner = create_test_actor(&app, "dismiss_owner").await;
    let origin = create_test_actor(&app, "dismiss_other_origin").await;
    let notification_id = seed_notification(
        &app,
        owner,
        NotificationType::Follow,
        AccountRef::Local(origin),
        None,
    )
    .await;

    let err = service
        .dismiss(&ctx_for(recipient), notification_id)
        .await
        .expect_err("another recipient's notification must 404 on dismiss");
    assert_eq!(err.status, StatusCode::NOT_FOUND);

    app.cleanup().await;
}

/// Requirement 4.3: dismissing a nonexistent id 404s.
#[tokio::test]
async fn dismiss_404_for_a_nonexistent_notification() {
    let app = spawn_test_app().await;
    let service = build_service(&app);
    let recipient = create_test_actor(&app, "dismiss_nonexistent_recipient").await;

    let err = service
        .dismiss(&ctx_for(recipient), app.runtime.ids.next_id())
        .await
        .expect_err("a nonexistent notification must 404 on dismiss");
    assert_eq!(err.status, StatusCode::NOT_FOUND);

    app.cleanup().await;
}

/// Requirements 4.1, 4.4: `clear` dismisses every one of the recipient's
/// notifications, excluding them from subsequent `list`.
#[tokio::test]
async fn clear_dismisses_every_notification_for_the_recipient() {
    let app = spawn_test_app().await;
    let service = build_service(&app);

    let recipient = create_test_actor(&app, "clear_recipient").await;
    let origin = create_test_actor(&app, "clear_origin").await;
    seed_notification(
        &app,
        recipient,
        NotificationType::Follow,
        AccountRef::Local(origin),
        None,
    )
    .await;
    seed_notification(
        &app,
        recipient,
        NotificationType::FollowRequest,
        AccountRef::Local(origin),
        None,
    )
    .await;

    service
        .clear(&ctx_for(recipient))
        .await
        .expect("clear must succeed");

    let page = service
        .list(
            &ctx_for(recipient),
            PageParams::default(),
            ListFilter::default(),
        )
        .await
        .expect("list must succeed");
    assert!(page.items.is_empty());

    app.cleanup().await;
}

/// Requirement 4.1: `clear` is idempotent — succeeds even with nothing to
/// clear.
#[tokio::test]
async fn clear_succeeds_when_the_recipient_has_no_notifications() {
    let app = spawn_test_app().await;
    let service = build_service(&app);
    let recipient = create_test_actor(&app, "clear_empty_recipient").await;

    service
        .clear(&ctx_for(recipient))
        .await
        .expect("clear must succeed even with no notifications");

    app.cleanup().await;
}

// -- one whole list page ---------------------------------------------------

/// Seeds a locally-registered custom emoji — an exact copy of
/// `statuses/account_provider/tests.rs`'s and `statuses/render_assembler/
/// tests.rs`'s identical test-local helper.
async fn seed_custom_emoji(app: &TestApp, shortcode: &str) {
    let now = app.runtime.clock.now();
    let url = format!("https://example.test/emoji/{shortcode}.png");
    sqlx::query(
        "INSERT INTO custom_emojis \
             (shortcode, domain, url, static_url, visible_in_picker, category, updated_at) \
         VALUES ($1, '', $2, $2, TRUE, NULL, $3)",
    )
    .bind(shortcode)
    .bind(&url)
    .bind(now)
    .execute(&app.pool)
    .await
    .expect("seeding a custom_emojis row must succeed");
}

/// Registers `name` as a tag and associates it with `status_id`.
async fn attach_tag(app: &TestApp, status_id: Id, name: &str) {
    let tag = Tag {
        id: app.runtime.ids.next_id(),
        name: name.to_string(),
        created_at: app.runtime.clock.now(),
    };
    let tag = upsert_tag(&app.pool, &tag)
        .await
        .expect("upsert_tag must succeed");
    associate_tag(&app.pool, status_id, tag.id)
        .await
        .expect("associate_tag must succeed");
}

/// Inserts `status` and hands back its id.
async fn insert_status_row(app: &TestApp, status: &Status) -> Id {
    insert_status(&app.pool, status)
        .await
        .expect("insert_status must succeed for a fresh id/uri");
    status.id
}

/// Pulls one field out of every element of a JSON array, tolerating a
/// non-array (a `null` `poll` indexes to `null`, not a panic).
fn field_list(items: &Value, field: &str) -> Vec<Value> {
    items
        .as_array()
        .map(|items| items.iter().map(|item| item[field].clone()).collect())
        .unwrap_or_default()
}

/// Projects exactly what the embedded post's own assembly resolves — the
/// author's Account, the tags, the emoji, the poll, the recipient's
/// interaction state — recursing into `reblog`, whose presence is this
/// module's own (deliberately un-re-checked) boost resolution rather than
/// the assembler's.
///
/// Deliberately a projection rather than a whole-document snapshot, for the
/// same reason `statuses/render_assembler/tests.rs::material_fingerprint`
/// is: the rest of Status JSON is `status_to_json`'s contract, already
/// pinned by that module's own golden tests.
fn status_fingerprint(json: &Value) -> Value {
    serde_json::json!({
        "id": json["id"],
        "account": json["account"]["id"],
        "tags": field_list(&json["tags"], "name"),
        "emojis": field_list(&json["emojis"], "shortcode"),
        "poll_options": field_list(&json["poll"]["options"], "title"),
        "poll_emojis": field_list(&json["poll"]["emojis"], "shortcode"),
        "favourited": json["favourited"],
        "reblogged": json["reblogged"],
        "bookmarked": json["bookmarked"],
        "pinned": json["pinned"],
        "muted": json["muted"],
        "reblog": match json["reblog"] {
            Value::Null => Value::Null,
            ref reblog => status_fingerprint(reblog),
        },
    })
}

/// The envelope plus its nested post: `notification_to_json`'s own outer
/// shell is what carries the null discipline, so `status` is projected as an
/// explicit `Value::Null` rather than an absent key.
fn notification_fingerprint(json: &Value) -> Value {
    serde_json::json!({
        "id": json["id"],
        "type": json["type"],
        "account": json["account"]["id"],
        "status": match json["status"] {
            Value::Null => Value::Null,
            ref status => status_fingerprint(status),
        },
    })
}

/// The characterization one whole notification page is measured against:
/// six notifications covering every shape the render loop had to handle one
/// at a time.
///
/// - a `follow`, which has no related post at all — `status: null`;
/// - a `favourite` whose related post has since been **deleted**, which
///   degrades to the same `status: null` without becoming an error (this
///   module's own "Dangling references are not errors");
/// - a `reblog` whose related post is a boost, whose target is `private`
///   and authored by someone the recipient does not follow — it must still
///   nest a fully rendered `reblog`, because this module deliberately runs
///   **no** visibility re-check on a boost target (see
///   [`super::NotificationService::render_page`]'s own doc comment). A
///   `reblog: null` here would mean a check had been introduced;
/// - two notifications whose related posts share one author, so that
///   resolving that author once for the page cannot be told apart from
///   resolving them twice, and one of them favourited by the recipient so
///   the interaction state is not uniformly `false`;
/// - a `poll` whose related post carries a poll whose option title has a
///   shortcode of its own, which the page's single emoji resolution has to
///   reach;
/// - content mentioning `:zulu:` before `:alpha:`, so that an `emojis` list
///   in the repository's `ORDER BY shortcode` order is distinguishable from
///   one in first-appearance order.
///
/// Ordering is asserted separately from content: `repository::list` returns
/// `ORDER BY id DESC`, and a page that renders the right six notifications
/// in the wrong order is just as wrong as one that renders them wrong.
#[tokio::test]
async fn list_renders_a_mixed_page_of_every_status_shape_in_order() {
    let app = spawn_test_app().await;
    let service = build_service(&app);
    let now = app.runtime.clock.now();

    let recipient = create_test_actor(&app, "mixedpage_recipient").await;
    let origin_a = create_test_actor(&app, "mixedpage_origin_a").await;
    let origin_b = create_test_actor(&app, "mixedpage_origin_b").await;
    let author = create_test_actor(&app, "mixedpage_author").await;
    let other = create_test_actor(&app, "mixedpage_other").await;

    for shortcode in ["alpha", "zulu", "yes"] {
        seed_custom_emoji(&app, shortcode).await;
    }

    // Two posts by the same author, the first carrying both a tag and two
    // shortcodes deliberately out of alphabetical order.
    let shared_a = insert_status_row(
        &app,
        &Status {
            content: "first :zulu: and :alpha:".to_string(),
            ..sample_status(app.runtime.ids.next_id(), author, now)
        },
    )
    .await;
    attach_tag(&app, shared_a, "kawasemi").await;
    let shared_b = insert_status_row(
        &app,
        &Status {
            content: "second by the same author".to_string(),
            ..sample_status(app.runtime.ids.next_id(), author, now)
        },
    )
    .await;
    interaction_repository::add_favourite(&app.pool, recipient, shared_b, now)
        .await
        .expect("add_favourite must succeed");

    // A boost whose target the recipient would *not* pass a visibility check
    // for. This module runs none, so it renders in full regardless.
    let boost_target = insert_status_row(
        &app,
        &Status {
            content: "boosted privately".to_string(),
            visibility: Visibility::Private,
            ..sample_status(app.runtime.ids.next_id(), other, now)
        },
    )
    .await;
    let boost = insert_status_row(
        &app,
        &Status {
            content: String::new(),
            reblog_of_id: Some(boost_target),
            ..sample_status(app.runtime.ids.next_id(), other, now)
        },
    )
    .await;

    // A post with a poll whose option title carries its own shortcode.
    let poll_id = app.runtime.ids.next_id();
    let polled = insert_status_row(
        &app,
        &Status {
            content: "lunch?".to_string(),
            poll_id: Some(poll_id),
            ..sample_status(app.runtime.ids.next_id(), author, now)
        },
    )
    .await;
    poll_repository::insert_poll(
        &app.pool,
        &Poll {
            id: poll_id,
            status_id: polled,
            expires_at: None,
            multiple: false,
        },
        &[
            PollOption {
                poll_id,
                idx: 0,
                title: ":yes: pizza".to_string(),
                votes_count: 0,
            },
            PollOption {
                poll_id,
                idx: 1,
                title: "sushi".to_string(),
                votes_count: 0,
            },
        ],
    )
    .await
    .expect("insert_poll must succeed");

    // Referenced by a notification, then hard-deleted out from under it.
    let doomed = insert_status_row(
        &app,
        &Status {
            content: "deleted before the page is rendered".to_string(),
            ..sample_status(app.runtime.ids.next_id(), other, now)
        },
    )
    .await;

    // Seeded oldest-first; `ORDER BY id DESC` hands them back reversed.
    let followed = seed_notification(
        &app,
        recipient,
        NotificationType::Follow,
        AccountRef::Local(origin_a),
        None,
    )
    .await;
    let favourited_deleted = seed_notification(
        &app,
        recipient,
        NotificationType::Favourite,
        AccountRef::Local(origin_a),
        Some(doomed),
    )
    .await;
    let reblogged = seed_notification(
        &app,
        recipient,
        NotificationType::Reblog,
        AccountRef::Local(origin_b),
        Some(boost),
    )
    .await;
    let mentioned = seed_notification(
        &app,
        recipient,
        NotificationType::Mention,
        AccountRef::Local(origin_a),
        Some(shared_a),
    )
    .await;
    let favourited = seed_notification(
        &app,
        recipient,
        NotificationType::Favourite,
        AccountRef::Local(origin_b),
        Some(shared_b),
    )
    .await;
    let polled_notification = seed_notification(
        &app,
        recipient,
        NotificationType::Poll,
        AccountRef::Local(origin_a),
        Some(polled),
    )
    .await;

    status_repository::delete_status(&app.pool, doomed)
        .await
        .expect("delete_status must succeed");

    let page = service
        .list(
            &ctx_for(recipient),
            PageParams::default(),
            ListFilter::default(),
        )
        .await
        .expect("list must succeed");

    let ids: Vec<&str> = page
        .items
        .iter()
        .map(|item| item["id"].as_str().expect("id must be a string"))
        .collect();
    assert_eq!(
        ids,
        vec![
            polled_notification.as_i64().to_string(),
            favourited.as_i64().to_string(),
            mentioned.as_i64().to_string(),
            reblogged.as_i64().to_string(),
            favourited_deleted.as_i64().to_string(),
            followed.as_i64().to_string(),
        ],
        "the page keeps `repository::list`'s newest-first order"
    );

    let fingerprints: Vec<Value> = page.items.iter().map(notification_fingerprint).collect();
    let origin_a_id = serde_json::json!(origin_a.as_i64().to_string());
    let origin_b_id = serde_json::json!(origin_b.as_i64().to_string());
    let author_id = serde_json::json!(author.as_i64().to_string());
    let other_id = serde_json::json!(other.as_i64().to_string());
    assert_eq!(
        fingerprints,
        vec![
            serde_json::json!({
                "id": polled_notification.as_i64().to_string(),
                "type": "poll",
                "account": origin_a_id,
                "status": {
                    "id": polled.as_i64().to_string(),
                    "account": author_id,
                    "tags": [],
                    "emojis": [],
                    "poll_options": [":yes: pizza", "sushi"],
                    "poll_emojis": ["yes"],
                    "favourited": false,
                    "reblogged": false,
                    "bookmarked": false,
                    "pinned": false,
                    "muted": false,
                    "reblog": Value::Null,
                },
            }),
            serde_json::json!({
                "id": favourited.as_i64().to_string(),
                "type": "favourite",
                "account": origin_b_id,
                "status": {
                    "id": shared_b.as_i64().to_string(),
                    "account": author_id,
                    "tags": [],
                    "emojis": [],
                    "poll_options": [],
                    "poll_emojis": [],
                    "favourited": true,
                    "reblogged": false,
                    "bookmarked": false,
                    "pinned": false,
                    "muted": false,
                    "reblog": Value::Null,
                },
            }),
            serde_json::json!({
                "id": mentioned.as_i64().to_string(),
                "type": "mention",
                "account": origin_a_id,
                "status": {
                    "id": shared_a.as_i64().to_string(),
                    "account": author_id,
                    "tags": ["kawasemi"],
                    "emojis": ["alpha", "zulu"],
                    "poll_options": [],
                    "poll_emojis": [],
                    "favourited": false,
                    "reblogged": false,
                    "bookmarked": false,
                    "pinned": false,
                    "muted": false,
                    "reblog": Value::Null,
                },
            }),
            serde_json::json!({
                "id": reblogged.as_i64().to_string(),
                "type": "reblog",
                "account": origin_b_id,
                "status": {
                    "id": boost.as_i64().to_string(),
                    "account": other_id,
                    "tags": [],
                    "emojis": [],
                    "poll_options": [],
                    "poll_emojis": [],
                    "favourited": false,
                    "reblogged": false,
                    "bookmarked": false,
                    "pinned": false,
                    "muted": false,
                    // Rendered in full despite being `private` and authored
                    // by someone the recipient does not follow.
                    "reblog": {
                        "id": boost_target.as_i64().to_string(),
                        "account": other_id,
                        "tags": [],
                        "emojis": [],
                        "poll_options": [],
                        "poll_emojis": [],
                        "favourited": false,
                        "reblogged": false,
                        "bookmarked": false,
                        "pinned": false,
                        "muted": false,
                        "reblog": Value::Null,
                    },
                },
            }),
            serde_json::json!({
                "id": favourited_deleted.as_i64().to_string(),
                "type": "favourite",
                "account": origin_a_id,
                // The referenced post is gone; the notification survives it.
                "status": Value::Null,
            }),
            serde_json::json!({
                "id": followed.as_i64().to_string(),
                "type": "follow",
                "account": origin_a_id,
                "status": Value::Null,
            }),
        ]
    );

    app.cleanup().await;
}

/// A `follow` notification carrying a `status_id` still resolves that post,
/// even though [`notification_to_json`] discards the result — so a post
/// whose poll row is missing makes the whole page fail rather than silently
/// rendering, exactly as it did before the page was batched.
///
/// This pins the one thing an "only render what the envelope will keep"
/// shortcut would quietly change. It is the current behaviour, not a
/// desirable one; it is characterized so that batching cannot move it.
#[tokio::test]
async fn list_still_resolves_a_status_the_envelope_will_discard() {
    let app = spawn_test_app().await;
    let service = build_service(&app);
    let now = app.runtime.clock.now();

    let recipient = create_test_actor(&app, "discarded_recipient").await;
    let origin = create_test_actor(&app, "discarded_origin").await;

    // A post pointing at a `polls` row that does not exist — the strict
    // `RequiredPolls` resolver's own not-found condition.
    let dangling = insert_status_row(
        &app,
        &Status {
            content: "a poll that is not there".to_string(),
            poll_id: Some(Id::from_i64(i64::MAX - 97)),
            ..sample_status(app.runtime.ids.next_id(), origin, now)
        },
    )
    .await;
    seed_notification(
        &app,
        recipient,
        NotificationType::Follow,
        AccountRef::Local(origin),
        Some(dangling),
    )
    .await;

    let err = service
        .list(
            &ctx_for(recipient),
            PageParams::default(),
            ListFilter::default(),
        )
        .await
        .expect_err("a dangling poll on a related post fails the whole page");
    assert_eq!(err.status, StatusCode::NOT_FOUND);
    assert_eq!(err.public_message, "poll not found");

    app.cleanup().await;
}

/// `show` renders one notification exactly as `list` renders it inside a
/// page — the two must not be able to drift, since both go through the same
/// assembly.
#[tokio::test]
async fn show_and_list_render_the_same_notification_identically() {
    let app = spawn_test_app().await;
    let service = build_service(&app);
    let now = app.runtime.clock.now();

    let recipient = create_test_actor(&app, "sameshape_recipient").await;
    let origin = create_test_actor(&app, "sameshape_origin").await;

    seed_custom_emoji(&app, "alpha").await;
    let target = insert_status_row(
        &app,
        &Status {
            content: "boosted :alpha:".to_string(),
            ..sample_status(app.runtime.ids.next_id(), origin, now)
        },
    )
    .await;
    let boost = insert_status_row(
        &app,
        &Status {
            content: String::new(),
            reblog_of_id: Some(target),
            ..sample_status(app.runtime.ids.next_id(), origin, now)
        },
    )
    .await;
    let notification_id = seed_notification(
        &app,
        recipient,
        NotificationType::Reblog,
        AccountRef::Local(origin),
        Some(boost),
    )
    .await;

    let page = service
        .list(
            &ctx_for(recipient),
            PageParams::default(),
            ListFilter::default(),
        )
        .await
        .expect("list must succeed");
    let shown = service
        .show(&ctx_for(recipient), notification_id)
        .await
        .expect("show must succeed");

    assert_eq!(page.items, vec![shown]);

    app.cleanup().await;
}
