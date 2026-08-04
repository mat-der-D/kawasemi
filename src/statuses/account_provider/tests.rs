//! Tests for this module's [`super::RequiredPolls`] — the [`PollResolver`]
//! `AccountStatusesProviderImpl` hands the assembler (Requirement 5.1; task
//! 4.5) — and for what one whole
//! [`AccountStatusesProviderImpl::list_statuses`] page renders.
//!
//! The resolver tests are scoped to the resolver rather than to
//! `list_statuses` as a whole: end-to-end coverage of the provider lives in
//! `tests/account_statuses_provider_it.rs`, which cannot reach the one
//! behavior that distinguishes *this* resolver from the four others
//! implementing the same trait — what happens to a `poll_id` whose `polls`
//! row is not there. A listable status never has one.
//!
//! The page test ([`list_statuses_keeps_every_rendered_material_and_order`])
//! exists for the opposite reason: it is a *characterization* of the whole
//! method's output, captured against the implementation that rendered its
//! page one status at a time, so that converting that loop into a single
//! batched assembly (Requirement 5.1) can be shown not to have moved
//! anything the caller can see (Requirements 1.1, 5.7).
//!
//! DB-backed against `crate::test_harness::spawn_test_app`, mirroring
//! `statuses/poll_repository/tests.rs`'s fixture convention: a poll needs a
//! real `statuses` row to reference (`polls.status_id` is a genuine FK), but
//! `statuses.actor_id` is not FK-constrained, so a bare id stands in for the
//! author wherever no rendered Account is needed.

use super::*;
use crate::actor::owner::create_owner;
use crate::actor::repository::insert_actor;
use crate::actor::{ActorState, ActorType, Handle};
use crate::api::pagination::PageParams;
use crate::domain::Visibility;
use crate::statuses::model::{Poll, PollOption, Tag};
use crate::statuses::poll_repository::PollTally;
use crate::statuses::tag_repository::{associate_tag, upsert_tag};
use crate::test_harness::{TestApp, spawn_test_app};

fn sample_status(app: &TestApp, actor_id: Id) -> Status {
    let id = app.runtime.ids.next_id();
    Status {
        id,
        actor_id,
        uri: format!("https://kawasemi.example/statuses/{}", id.as_i64()),
        url: None,
        content: "what should we have for lunch?".to_string(),
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
        created_at: app.runtime.clock.now(),
        edited_at: None,
    }
}

/// Inserts a `polls` row carrying `titles` as options `idx 0..N`, attached
/// to a fresh `statuses` row.
async fn insert_test_poll(app: &TestApp, titles: &[&str]) -> Poll {
    let status = sample_status(app, app.runtime.ids.next_id());
    status_repository::insert_status(&app.pool, &status)
        .await
        .expect("insert_status must succeed for a fresh id/uri");

    let poll = Poll {
        id: app.runtime.ids.next_id(),
        status_id: status.id,
        expires_at: None,
        multiple: false,
    };
    let options: Vec<PollOption> = titles
        .iter()
        .enumerate()
        .map(|(idx, title)| PollOption {
            poll_id: poll.id,
            idx: idx as i32,
            title: (*title).to_string(),
            votes_count: 0,
        })
        .collect();
    poll_repository::insert_poll(&app.pool, &poll, &options)
        .await
        .expect("insert_poll must succeed for a fresh poll");
    poll
}

fn resolved_ids(resolved: &[(Id, Poll, PollTally)]) -> Vec<Id> {
    resolved.iter().map(|(id, _, _)| *id).collect()
}

fn option_titles(tally: &PollTally) -> Vec<&str> {
    tally
        .options
        .iter()
        .map(|option| option.title.as_str())
        .collect()
}

/// This module supplies the **strict** [`PollResolver`]: a `poll_id`
/// matching no `polls` row is this module's own [`not_found`], not a
/// silently poll-less status. Asserted on the exact status *and* message
/// because `notifications::service`'s equally strict resolver raises a
/// differently worded 404 for the same condition ("poll not found"), and the
/// two are deliberately not unified — design.md's `PollResolver` section,
/// Risks: "統一しない（統一すれば振る舞いが変わる）".
///
/// Checked with the dangling id in both positions: the resolver must raise
/// whether or not a resolvable poll precedes it.
#[tokio::test]
async fn resolve_many_raises_this_modules_not_found_for_a_dangling_poll_id() {
    let app = spawn_test_app().await;
    let polls = RequiredPolls {
        pool: app.pool.clone(),
    };

    let existing = insert_test_poll(&app, &["Yes", "No"]).await;
    let dangling = Id::from_i64(i64::MAX - 41);

    for requested in [[existing.id, dangling], [dangling, existing.id]] {
        let err = polls
            .resolve_many(&requested, None)
            .await
            .expect_err("a dangling poll id must fail the strict resolver");
        assert_eq!(err.status, StatusCode::NOT_FOUND);
        assert_eq!(err.public_message, "status not found");
    }

    app.cleanup().await;
}

/// The strict resolver is not trivially failing: every id that does resolve
/// comes back, in `poll_ids` order rather than whatever order the rows
/// arrive in. Requested in an order that is neither ascending nor descending
/// by id, so a lookup that let a `HashMap`'s iteration order through could
/// not pass by luck.
#[tokio::test]
async fn resolve_many_returns_every_existing_poll_in_the_requested_order() {
    let app = spawn_test_app().await;
    let polls = RequiredPolls {
        pool: app.pool.clone(),
    };

    let first = insert_test_poll(&app, &["a"]).await;
    let second = insert_test_poll(&app, &["b"]).await;
    let third = insert_test_poll(&app, &["c"]).await;

    let requested = [third.id, first.id, second.id];
    let resolved = polls
        .resolve_many(&requested, None)
        .await
        .expect("resolve_many must succeed for three existing polls");

    assert_eq!(resolved_ids(&resolved), requested.to_vec());
    assert_eq!(option_titles(&resolved[0].2), vec!["c"]);
    assert_eq!(option_titles(&resolved[1].2), vec!["a"]);
    assert_eq!(option_titles(&resolved[2].2), vec!["b"]);

    app.cleanup().await;
}

/// `viewer` reaches the tally: their own selections come back in
/// `own_votes`, and an unauthenticated read gets an empty one while still
/// seeing the same public `voters_count`.
#[tokio::test]
async fn resolve_many_reports_the_viewers_own_votes() {
    let app = spawn_test_app().await;
    let polls = RequiredPolls {
        pool: app.pool.clone(),
    };

    let poll = insert_test_poll(&app, &["Yes", "No"]).await;
    let viewer = app.runtime.ids.next_id();
    poll_repository::record_vote(&app.pool, poll.id, viewer, &[1], app.runtime.clock.now())
        .await
        .expect("record_vote must succeed");

    let seen = polls
        .resolve_many(&[poll.id], Some(viewer))
        .await
        .expect("resolve_many must succeed");
    assert_eq!(seen[0].2.own_votes, vec![1]);
    assert_eq!(seen[0].2.voters_count, 1);

    let anonymous = polls
        .resolve_many(&[poll.id], None)
        .await
        .expect("resolve_many must succeed without a viewer");
    assert!(anonymous[0].2.own_votes.is_empty());
    assert_eq!(anonymous[0].2.voters_count, 1);

    app.cleanup().await;
}

// -- `list_statuses` page characterization (Requirements 1.1, 5.1, 5.7) ----

/// Creates a real owner + local actor row, returning the actor's `Id` — the
/// same helper `search/hydrator/tests.rs` and `render_assembler/tests.rs`
/// keep their own copies of. A rendered page embeds an Account per author,
/// so these authors have to be resolvable, unlike the bare ids the
/// `RequiredPolls` fixtures above stand up.
async fn create_test_actor(app: &TestApp, handle: &str) -> Id {
    let now = app.runtime.clock.now();
    let owner_id = app.runtime.ids.next_id();
    create_owner(&app.pool, owner_id, now)
        .await
        .expect("creating the owner must succeed");

    let actor_id = app.runtime.ids.next_id();
    let actor = crate::actor::model::LocalActor {
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

/// Seeds a locally-registered custom emoji, mirroring
/// `render_assembler/tests.rs`'s identical test-local helper.
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
async fn insert(app: &TestApp, status: &Status) -> Id {
    status_repository::insert_status(&app.pool, status)
        .await
        .expect("insert_status must succeed for a fresh id/uri");
    status.id
}

/// The provider under test, wired to the same live handles
/// `crate::statuses::register_account_ports` gives the production one.
fn build_provider(app: &TestApp) -> AccountStatusesProviderImpl {
    AccountStatusesProviderImpl::new(
        app.pool.clone(),
        app.runtime.clone(),
        app.state.config().server.domain.clone(),
        app.state.accounts().service(),
        app.state.media().store().clone(),
        app.state.statuses().relationship_query_registry(),
    )
}

/// A query with every filter off and no cursor — the whole of `target`'s
/// visible output, first page.
fn unfiltered_query(target: Id, viewer: Option<Id>) -> StatusesQuery {
    StatusesQuery {
        target: AccountRef::Local(target),
        viewer,
        page: PageParams::default(),
        pinned: false,
        only_media: false,
        exclude_replies: false,
        exclude_reblogs: false,
    }
}

/// Pulls one field out of every element of a JSON array, tolerating a
/// non-array (a `null` `poll` indexes to `null`, not a panic).
fn field_list(items: &Value, field: &str) -> Vec<Value> {
    items
        .as_array()
        .map(|items| items.iter().map(|item| item[field].clone()).collect())
        .unwrap_or_default()
}

/// Projects exactly what this provider's own assembly resolves — the
/// author's Account, the tags, the emoji, the poll, the viewer's interaction
/// state — recursing into `reblog`, whose presence or absence is this
/// module's own visibility judgment rather than the assembler's.
///
/// Deliberately a projection rather than a whole-document snapshot, for the
/// same reason `render_assembler/tests.rs::material_fingerprint` is: the
/// rest of Status JSON is `status_to_json`'s contract, already pinned by
/// that module's golden tests.
fn material_fingerprint(json: &Value) -> Value {
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
            ref reblog => material_fingerprint(reblog),
        },
    })
}

/// The characterization one whole page is measured against: five statuses by
/// one author, covering every shape the render loop had to handle one at a
/// time.
///
/// - a boost whose target **is** visible, which must nest a fully rendered
///   `reblog` carrying the target's own author, poll-less content, and the
///   viewer's own favourite of it;
/// - a boost whose target is **not** visible to the viewer, which must
///   render `reblog: null` — dropped entirely, never partially, and judged
///   against the *target's* author rather than the booster's;
/// - two statuses by the same author, so that resolving that author once for
///   the page cannot be told apart from resolving them twice;
/// - a status with a poll, whose option titles carry a shortcode of their
///   own that the page's emoji resolution has to reach;
/// - content mentioning `:zulu:` before `:alpha:`, so that an `emojis` list
///   in the repository's `ORDER BY shortcode` order is distinguishable from
///   one in first-appearance order.
///
/// Ordering is asserted separately from content: `list_by_actor` returns
/// newest-id-first, and a page that renders the right five statuses in the
/// wrong order violates Requirement 5.7 just as much as one that renders
/// them wrong.
#[tokio::test]
async fn list_statuses_keeps_every_rendered_material_and_order() {
    let app = spawn_test_app().await;
    let author = create_test_actor(&app, "listpage_author").await;
    let other = create_test_actor(&app, "listpage_other").await;

    for shortcode in ["alpha", "zulu", "yes"] {
        seed_custom_emoji(&app, shortcode).await;
    }

    // Two by the same author, the first carrying both a tag and two
    // shortcodes deliberately out of alphabetical order.
    let plain_a = insert(
        &app,
        &Status {
            content: "first :zulu: and :alpha:".to_string(),
            ..sample_status(&app, author)
        },
    )
    .await;
    attach_tag(&app, plain_a, "kawasemi").await;
    let plain_b = insert(
        &app,
        &Status {
            content: "second by the same author".to_string(),
            ..sample_status(&app, author)
        },
    )
    .await;

    // A boost of a visible post, favourited by the viewer so the reblog's
    // own interaction state is non-default.
    let visible_target = insert(
        &app,
        &Status {
            content: "boosted publicly".to_string(),
            ..sample_status(&app, other)
        },
    )
    .await;
    interaction_repository::add_favourite(
        &app.pool,
        author,
        visible_target,
        app.runtime.clock.now(),
    )
    .await
    .expect("add_favourite must succeed");
    let boost_of_visible = insert(
        &app,
        &Status {
            content: String::new(),
            reblog_of_id: Some(visible_target),
            ..sample_status(&app, author)
        },
    )
    .await;

    // A boost of a post the viewer may not see: `private`, authored by
    // someone the default `NoRelationshipQuery` reports the viewer as not
    // following.
    let hidden_target = insert(
        &app,
        &Status {
            content: "boosted privately".to_string(),
            visibility: Visibility::Private,
            ..sample_status(&app, other)
        },
    )
    .await;
    let boost_of_hidden = insert(
        &app,
        &Status {
            content: String::new(),
            reblog_of_id: Some(hidden_target),
            ..sample_status(&app, author)
        },
    )
    .await;

    // A status with a poll whose option title carries its own shortcode.
    let poll_id = app.runtime.ids.next_id();
    let polled = insert(
        &app,
        &Status {
            content: "lunch?".to_string(),
            poll_id: Some(poll_id),
            ..sample_status(&app, author)
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

    let page = build_provider(&app)
        .list_statuses(&unfiltered_query(author, Some(author)))
        .await
        .expect("list_statuses must succeed");

    let ids: Vec<&str> = page
        .items
        .iter()
        .map(|item| item["id"].as_str().expect("id must be a string"))
        .collect();
    assert_eq!(
        ids,
        vec![
            polled.as_i64().to_string(),
            boost_of_hidden.as_i64().to_string(),
            boost_of_visible.as_i64().to_string(),
            plain_b.as_i64().to_string(),
            plain_a.as_i64().to_string(),
        ],
        "the page keeps `list_by_actor`'s newest-first order"
    );

    let fingerprints: Vec<Value> = page.items.iter().map(material_fingerprint).collect();
    let author_id = serde_json::json!(author.as_i64().to_string());
    let other_id = serde_json::json!(other.as_i64().to_string());
    assert_eq!(
        fingerprints,
        vec![
            serde_json::json!({
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
            }),
            serde_json::json!({
                "id": boost_of_hidden.as_i64().to_string(),
                "account": author_id,
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
            }),
            serde_json::json!({
                "id": boost_of_visible.as_i64().to_string(),
                "account": author_id,
                "tags": [],
                "emojis": [],
                "poll_options": [],
                "poll_emojis": [],
                "favourited": false,
                "reblogged": false,
                "bookmarked": false,
                "pinned": false,
                "muted": false,
                "reblog": {
                    "id": visible_target.as_i64().to_string(),
                    "account": other_id,
                    "tags": [],
                    "emojis": [],
                    "poll_options": [],
                    "poll_emojis": [],
                    "favourited": true,
                    // The viewer *is* the booster, so the target comes back
                    // flagged as one they have boosted.
                    "reblogged": true,
                    "bookmarked": false,
                    "pinned": false,
                    "muted": false,
                    "reblog": Value::Null,
                },
            }),
            serde_json::json!({
                "id": plain_b.as_i64().to_string(),
                "account": author_id,
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
            }),
            serde_json::json!({
                "id": plain_a.as_i64().to_string(),
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
            }),
        ]
    );

    app.cleanup().await;
}
