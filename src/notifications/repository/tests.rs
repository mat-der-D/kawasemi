//! Integration-style, DB-backed tests for `NotificationRepository`
//! (Requirements 2.1, 2.2, 2.3, 2.4, 3.1, 4.1, 4.2, 4.4, 8.1, 8.2), per task
//! 1.3's own completion definition: "同一重複排除キーの二重挿入が冪等にな
//! り、消去済みにした通知と同一キーの新規挿入は新規作成として扱われ（取り
//! 消し→再実行の再通知）、消去済みが一覧から除外され、他者宛が単一取得で
//! None になることをリポジトリ単体で確認できる状態".
//!
//! Mirrors `social_graph/repository/tests.rs`'s established convention
//! (that spec's own first-repository task, 1.3, the closest structural
//! precedent to this one): reuses `crate::test_harness::db_fixture::spawn_test_db` for
//! an isolated, already-migrated schema and a deterministic
//! `RuntimeContext`; every id/`AccountRef` used here is a plain synthetic
//! value minted from `db.runtime.ids` — `notifications` holds only
//! *logical* references (`migrations/0009_notifications.sql`'s own doc
//! comment), so nothing in this repository depends on a real actor/status
//! row existing.

use time::Duration;

use crate::api::pagination::PageParams;
use crate::domain::{AccountRef, Id};
use crate::notifications::model::{Notification, NotificationType};
use crate::test_harness::db_fixture::spawn_test_db;

use super::{InsertOutcome, ListFilter, clear, dismiss, find_for_recipient, insert_dedup, list};

/// Builds a fresh, not-yet-persisted [`Notification`] with caller-minted
/// `id`/`created_at` (mirrors this module's own doc comment,
/// "IDs/timestamps": callers mint via `RuntimeContext`, this repository
/// never does).
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

// -- insert_dedup ---------------------------------------------------------

/// Requirement 8.1/8.2: a first `insert_dedup` for a fresh dedup key
/// persists the row and reports `Created`.
#[tokio::test]
async fn insert_dedup_creates_a_new_notification() {
    let db = spawn_test_db().await;
    let recipient = db.runtime.ids.next_id();
    let origin = AccountRef::Remote(db.runtime.ids.next_id());
    let id = db.runtime.ids.next_id();
    let now = db.runtime.clock.now();

    let notification =
        sample_notification(id, recipient, NotificationType::Follow, origin, None, now);

    let outcome = insert_dedup(&db.pool, &notification)
        .await
        .expect("insert_dedup must succeed for a fresh dedup key");

    assert_eq!(outcome, InsertOutcome::Created(notification.clone()));

    let fetched = find_for_recipient(&db.pool, id, recipient)
        .await
        .expect("find_for_recipient must succeed")
        .expect("the just-inserted notification must be found");
    assert_eq!(fetched, notification);

    db.cleanup().await;
}

/// Requirement 8.1, 8.2: a second `insert_dedup` for the identical dedup key
/// (`recipient`, `kind`, `origin`, `status_id`), while the first row remains
/// not-yet-dismissed, is silently ignored (`Duplicate`) — proving the
/// "同一重複排除キーの二重挿入が冪等になり" half of this task's completion
/// definition. The duplicate's own (different, unused) caller-minted id
/// never lands in the table.
#[tokio::test]
async fn insert_dedup_is_idempotent_for_a_repeated_dedup_key() {
    let db = spawn_test_db().await;
    let recipient = db.runtime.ids.next_id();
    let origin = AccountRef::Remote(db.runtime.ids.next_id());
    let now = db.runtime.clock.now();

    let first_id = db.runtime.ids.next_id();
    let first = sample_notification(
        first_id,
        recipient,
        NotificationType::Favourite,
        origin,
        Some(db.runtime.ids.next_id()),
        now,
    );
    let first_outcome = insert_dedup(&db.pool, &first)
        .await
        .expect("first insert_dedup must succeed");
    assert_eq!(first_outcome, InsertOutcome::Created(first.clone()));

    // Same (recipient, kind, origin, status_id) key, different id/timestamp
    // — the upstream event resend case (Requirement 8.2's "上流イベントが
    // 再送・再受信される間...冪等").
    let second_id = db.runtime.ids.next_id();
    let second = sample_notification(
        second_id,
        recipient,
        NotificationType::Favourite,
        origin,
        first.status_id,
        now + Duration::seconds(1),
    );
    let second_outcome = insert_dedup(&db.pool, &second)
        .await
        .expect("second insert_dedup for the same key must succeed idempotently");
    assert_eq!(second_outcome, InsertOutcome::Duplicate);

    // Only the first row persisted: the duplicate's own id never landed.
    let via_first_id = find_for_recipient(&db.pool, first_id, recipient)
        .await
        .expect("find_for_recipient must succeed");
    assert_eq!(via_first_id, Some(first));

    let via_second_id = find_for_recipient(&db.pool, second_id, recipient)
        .await
        .expect("find_for_recipient must succeed");
    assert_eq!(
        via_second_id, None,
        "the ignored duplicate's own caller-minted id must not exist as a row"
    );

    db.cleanup().await;
}

/// Requirement 8.1: once the existing row with a given dedup key has been
/// dismissed, a fresh `insert_dedup` with the identical key inserts a new
/// row rather than being treated as a duplicate — proving this task's
/// completion definition's "消去済みにした通知と同一キーの新規挿入は新規
/// 作成として扱われ（取り消し→再実行の再通知）" half.
#[tokio::test]
async fn insert_dedup_allows_a_fresh_insert_after_the_existing_row_is_dismissed() {
    let db = spawn_test_db().await;
    let recipient = db.runtime.ids.next_id();
    let origin = AccountRef::Local(db.runtime.ids.next_id());
    let now = db.runtime.clock.now();

    let first_id = db.runtime.ids.next_id();
    let first = sample_notification(
        first_id,
        recipient,
        NotificationType::Follow,
        origin,
        None,
        now,
    );
    let first_outcome = insert_dedup(&db.pool, &first)
        .await
        .expect("first insert_dedup must succeed");
    assert_eq!(first_outcome, InsertOutcome::Created(first.clone()));

    let dismissed = dismiss(&db.pool, first_id, recipient)
        .await
        .expect("dismiss must succeed");
    assert!(
        dismissed,
        "dismiss must report that it found/updated the row"
    );

    // Same dedup key (recipient, Follow, origin, None) — unfollow -> re-follow.
    let second_id = db.runtime.ids.next_id();
    let second = sample_notification(
        second_id,
        recipient,
        NotificationType::Follow,
        origin,
        None,
        now + Duration::seconds(5),
    );
    let second_outcome = insert_dedup(&db.pool, &second)
        .await
        .expect("insert_dedup after dismissal must succeed");
    assert_eq!(
        second_outcome,
        InsertOutcome::Created(second.clone()),
        "a fresh insert with the same key must be treated as newly created once the prior \
         row with that key is dismissed"
    );

    // The re-created notification is visible via find_for_recipient
    // (not-dismissed); the original dismissed row is not.
    let via_second_id = find_for_recipient(&db.pool, second_id, recipient)
        .await
        .expect("find_for_recipient must succeed");
    assert_eq!(via_second_id, Some(second));

    let via_first_id = find_for_recipient(&db.pool, first_id, recipient)
        .await
        .expect("find_for_recipient must succeed");
    assert_eq!(
        via_first_id, None,
        "the original, now-dismissed row must not be returned by find_for_recipient"
    );

    db.cleanup().await;
}

// -- find_for_recipient -----------------------------------------------------

/// Requirement 3.1: a notification addressed to a different recipient must
/// not be found via `find_for_recipient` — proving this task's completion
/// definition's "他者宛が単一取得で None になる" half.
#[tokio::test]
async fn find_for_recipient_returns_none_for_another_actors_notification() {
    let db = spawn_test_db().await;
    let recipient = db.runtime.ids.next_id();
    let someone_else = db.runtime.ids.next_id();
    let origin = AccountRef::Remote(db.runtime.ids.next_id());
    let now = db.runtime.clock.now();

    let id = db.runtime.ids.next_id();
    let notification =
        sample_notification(id, recipient, NotificationType::Mention, origin, None, now);
    insert_dedup(&db.pool, &notification)
        .await
        .expect("insert_dedup must succeed");

    let found = find_for_recipient(&db.pool, id, someone_else)
        .await
        .expect("find_for_recipient must succeed");
    assert_eq!(
        found, None,
        "a notification addressed to a different recipient must not be found"
    );

    db.cleanup().await;
}

/// Requirement 3.1: a nonexistent notification id returns `None`.
#[tokio::test]
async fn find_for_recipient_returns_none_for_a_nonexistent_id() {
    let db = spawn_test_db().await;
    let recipient = db.runtime.ids.next_id();
    let nonexistent_id = db.runtime.ids.next_id();

    let found = find_for_recipient(&db.pool, nonexistent_id, recipient)
        .await
        .expect("find_for_recipient must succeed");
    assert_eq!(found, None);

    db.cleanup().await;
}

/// Requirement 4.4: a dismissed notification is excluded from
/// `find_for_recipient`, even for its own recipient.
#[tokio::test]
async fn find_for_recipient_excludes_a_dismissed_notification_for_its_own_recipient() {
    let db = spawn_test_db().await;
    let recipient = db.runtime.ids.next_id();
    let origin = AccountRef::Local(db.runtime.ids.next_id());
    let now = db.runtime.clock.now();

    let id = db.runtime.ids.next_id();
    let notification = sample_notification(
        id,
        recipient,
        NotificationType::Reblog,
        origin,
        Some(db.runtime.ids.next_id()),
        now,
    );
    insert_dedup(&db.pool, &notification)
        .await
        .expect("insert_dedup must succeed");

    dismiss(&db.pool, id, recipient)
        .await
        .expect("dismiss must succeed");

    let found = find_for_recipient(&db.pool, id, recipient)
        .await
        .expect("find_for_recipient must succeed");
    assert_eq!(
        found, None,
        "a dismissed notification must not be returned by find_for_recipient, even for its \
         own recipient"
    );

    db.cleanup().await;
}

// -- list -------------------------------------------------------------------

/// Requirement 2.1: `list` returns only the requesting recipient's
/// notifications, newest-first.
#[tokio::test]
async fn list_scopes_to_the_recipient_and_orders_newest_first() {
    let db = spawn_test_db().await;
    let recipient = db.runtime.ids.next_id();
    let someone_else = db.runtime.ids.next_id();
    let origin = AccountRef::Remote(db.runtime.ids.next_id());
    let now = db.runtime.clock.now();

    let mine_first = sample_notification(
        db.runtime.ids.next_id(),
        recipient,
        NotificationType::Follow,
        origin,
        None,
        now,
    );
    insert_dedup(&db.pool, &mine_first).await.unwrap();

    let not_mine = sample_notification(
        db.runtime.ids.next_id(),
        someone_else,
        NotificationType::Follow,
        origin,
        None,
        now,
    );
    insert_dedup(&db.pool, &not_mine).await.unwrap();

    let mine_second = sample_notification(
        db.runtime.ids.next_id(),
        recipient,
        NotificationType::Favourite,
        origin,
        Some(db.runtime.ids.next_id()),
        now,
    );
    insert_dedup(&db.pool, &mine_second).await.unwrap();

    let page = list(
        &db.pool,
        recipient,
        &PageParams::default(),
        &ListFilter::default(),
    )
    .await
    .expect("list must succeed");

    assert_eq!(page.items, vec![mine_second, mine_first]);

    db.cleanup().await;
}

/// Requirement 2.4: a dismissed notification is excluded from `list` —
/// proving this task's completion definition's "消去済みが一覧から除外
/// され" half.
#[tokio::test]
async fn list_excludes_dismissed_notifications() {
    let db = spawn_test_db().await;
    let recipient = db.runtime.ids.next_id();
    let origin = AccountRef::Remote(db.runtime.ids.next_id());
    let now = db.runtime.clock.now();

    let kept = sample_notification(
        db.runtime.ids.next_id(),
        recipient,
        NotificationType::Follow,
        origin,
        None,
        now,
    );
    insert_dedup(&db.pool, &kept).await.unwrap();

    let dismissed_one = sample_notification(
        db.runtime.ids.next_id(),
        recipient,
        NotificationType::FollowRequest,
        origin,
        None,
        now,
    );
    insert_dedup(&db.pool, &dismissed_one).await.unwrap();
    dismiss(&db.pool, dismissed_one.id, recipient)
        .await
        .expect("dismiss must succeed");

    let page = list(
        &db.pool,
        recipient,
        &PageParams::default(),
        &ListFilter::default(),
    )
    .await
    .expect("list must succeed");

    assert_eq!(page.items, vec![kept]);

    db.cleanup().await;
}

/// Requirement 2.2: `types` includes only the listed kinds; `exclude_types`
/// excludes the listed kinds.
#[tokio::test]
async fn list_applies_types_and_exclude_types_filters() {
    let db = spawn_test_db().await;
    let recipient = db.runtime.ids.next_id();
    let origin = AccountRef::Remote(db.runtime.ids.next_id());
    let now = db.runtime.clock.now();

    let follow = sample_notification(
        db.runtime.ids.next_id(),
        recipient,
        NotificationType::Follow,
        origin,
        None,
        now,
    );
    insert_dedup(&db.pool, &follow).await.unwrap();

    let favourite = sample_notification(
        db.runtime.ids.next_id(),
        recipient,
        NotificationType::Favourite,
        origin,
        Some(db.runtime.ids.next_id()),
        now,
    );
    insert_dedup(&db.pool, &favourite).await.unwrap();

    let reblog = sample_notification(
        db.runtime.ids.next_id(),
        recipient,
        NotificationType::Reblog,
        origin,
        Some(db.runtime.ids.next_id()),
        now,
    );
    insert_dedup(&db.pool, &reblog).await.unwrap();

    // types: only Follow.
    let types_filter = ListFilter {
        types: Some(vec![NotificationType::Follow]),
        ..Default::default()
    };
    let types_page = list(&db.pool, recipient, &PageParams::default(), &types_filter)
        .await
        .expect("list with types filter must succeed");
    assert_eq!(types_page.items, vec![follow.clone()]);

    // exclude_types: everything but Reblog.
    let exclude_filter = ListFilter {
        exclude_types: Some(vec![NotificationType::Reblog]),
        ..Default::default()
    };
    let exclude_page = list(&db.pool, recipient, &PageParams::default(), &exclude_filter)
        .await
        .expect("list with exclude_types filter must succeed");
    assert_eq!(exclude_page.items, vec![favourite, follow]);

    db.cleanup().await;
}

/// Requirement 2.3: `account_id` narrows to notifications whose origin
/// matches the given (already-resolved) `AccountRef`.
#[tokio::test]
async fn list_applies_account_id_filter() {
    let db = spawn_test_db().await;
    let recipient = db.runtime.ids.next_id();
    let origin_a = AccountRef::Remote(db.runtime.ids.next_id());
    let origin_b = AccountRef::Local(db.runtime.ids.next_id());
    let now = db.runtime.clock.now();

    let from_a = sample_notification(
        db.runtime.ids.next_id(),
        recipient,
        NotificationType::Follow,
        origin_a,
        None,
        now,
    );
    insert_dedup(&db.pool, &from_a).await.unwrap();

    let from_b = sample_notification(
        db.runtime.ids.next_id(),
        recipient,
        NotificationType::Follow,
        origin_b,
        None,
        now,
    );
    insert_dedup(&db.pool, &from_b).await.unwrap();

    let filter = ListFilter {
        account_id: Some(origin_a),
        ..Default::default()
    };
    let page = list(&db.pool, recipient, &PageParams::default(), &filter)
        .await
        .expect("list with account_id filter must succeed");
    assert_eq!(page.items, vec![from_a]);

    db.cleanup().await;
}

/// Requirement 2.1: `list` respects cursor pagination (`max_id`/`limit`).
#[tokio::test]
async fn list_paginates_by_notification_id_cursor() {
    let db = spawn_test_db().await;
    let recipient = db.runtime.ids.next_id();
    let origin = AccountRef::Remote(db.runtime.ids.next_id());
    let now = db.runtime.clock.now();

    let mut created = Vec::new();
    for _ in 0..5 {
        // Each notification gets its own `status_id`, so every one has a
        // distinct dedup key (recipient, kind, origin, status_id) — without
        // this, `insert_dedup` would correctly collapse them all into a
        // single row (this module's own dedup semantics under test
        // elsewhere), leaving fewer than 5 rows for pagination to page
        // through here.
        let n = sample_notification(
            db.runtime.ids.next_id(),
            recipient,
            NotificationType::Favourite,
            origin,
            Some(db.runtime.ids.next_id()),
            now,
        );
        insert_dedup(&db.pool, &n).await.unwrap();
        created.push(n);
    }
    // Newest-first order matches insertion order reversed (ids increase
    // monotonically via the deterministic `IdGenerator`).
    created.reverse();

    let first_page = list(
        &db.pool,
        recipient,
        &PageParams {
            limit: Some(2),
            ..Default::default()
        },
        &ListFilter::default(),
    )
    .await
    .expect("first page list must succeed");
    assert_eq!(first_page.items, created[0..2]);

    let second_page = list(
        &db.pool,
        recipient,
        &PageParams {
            max_id: first_page.next_cursor.clone(),
            limit: Some(2),
            ..Default::default()
        },
        &ListFilter::default(),
    )
    .await
    .expect("second page list must succeed");
    assert_eq!(second_page.items, created[2..4]);

    db.cleanup().await;
}

// -- dismiss / clear ----------------------------------------------------

/// Requirement 4.2: `dismiss` on a notification the caller does not own
/// (wrong recipient) reports `false` and does not mutate the row.
#[tokio::test]
async fn dismiss_returns_false_for_another_actors_notification() {
    let db = spawn_test_db().await;
    let recipient = db.runtime.ids.next_id();
    let someone_else = db.runtime.ids.next_id();
    let origin = AccountRef::Remote(db.runtime.ids.next_id());
    let now = db.runtime.clock.now();

    let id = db.runtime.ids.next_id();
    let notification =
        sample_notification(id, recipient, NotificationType::Follow, origin, None, now);
    insert_dedup(&db.pool, &notification).await.unwrap();

    let result = dismiss(&db.pool, id, someone_else)
        .await
        .expect("dismiss must succeed even when it finds no matching row");
    assert!(
        !result,
        "dismiss must report false for another actor's notification"
    );

    // Still retrievable (untouched) by its real recipient.
    let still_there = find_for_recipient(&db.pool, id, recipient)
        .await
        .expect("find_for_recipient must succeed");
    assert_eq!(still_there, Some(notification));

    db.cleanup().await;
}

/// Requirement 4.2: `dismiss` on an existing, owned notification excludes
/// it from a subsequent `list`.
#[tokio::test]
async fn dismiss_excludes_the_notification_from_a_subsequent_list() {
    let db = spawn_test_db().await;
    let recipient = db.runtime.ids.next_id();
    let origin = AccountRef::Remote(db.runtime.ids.next_id());
    let now = db.runtime.clock.now();

    let id = db.runtime.ids.next_id();
    let notification =
        sample_notification(id, recipient, NotificationType::Follow, origin, None, now);
    insert_dedup(&db.pool, &notification).await.unwrap();

    let result = dismiss(&db.pool, id, recipient)
        .await
        .expect("dismiss must succeed");
    assert!(result);

    let page = list(
        &db.pool,
        recipient,
        &PageParams::default(),
        &ListFilter::default(),
    )
    .await
    .expect("list must succeed");
    assert!(page.items.is_empty());

    db.cleanup().await;
}

/// Requirement 4.1: `clear` dismisses every one of the recipient's
/// notifications, and leaves other recipients' notifications untouched.
#[tokio::test]
async fn clear_dismisses_all_of_the_recipients_notifications_only() {
    let db = spawn_test_db().await;
    let recipient = db.runtime.ids.next_id();
    let someone_else = db.runtime.ids.next_id();
    let origin = AccountRef::Remote(db.runtime.ids.next_id());
    let now = db.runtime.clock.now();

    for _ in 0..3 {
        let n = sample_notification(
            db.runtime.ids.next_id(),
            recipient,
            NotificationType::Follow,
            origin,
            None,
            now,
        );
        insert_dedup(&db.pool, &n).await.unwrap();
    }
    let other_notification = sample_notification(
        db.runtime.ids.next_id(),
        someone_else,
        NotificationType::Follow,
        origin,
        None,
        now,
    );
    insert_dedup(&db.pool, &other_notification).await.unwrap();

    clear(&db.pool, recipient)
        .await
        .expect("clear must succeed");

    let mine = list(
        &db.pool,
        recipient,
        &PageParams::default(),
        &ListFilter::default(),
    )
    .await
    .expect("list must succeed");
    assert!(
        mine.items.is_empty(),
        "clear must dismiss all of the recipient's notifications"
    );

    let theirs = list(
        &db.pool,
        someone_else,
        &PageParams::default(),
        &ListFilter::default(),
    )
    .await
    .expect("list must succeed");
    assert_eq!(
        theirs.items,
        vec![other_notification],
        "clear must not affect another recipient's notifications"
    );

    db.cleanup().await;
}
