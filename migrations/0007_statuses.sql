-- 0007_statuses.sql
--
-- statuses-core: adds the persistence for the post (Status) core state
-- model: the status itself (with its self-referential reblog/reply/poll
-- pointers), its edit history, its media attachments, the four per-actor
-- interaction tables (favourite/bookmark/pin — reblog is represented as its
-- own dedicated `statuses` row, not a separate interaction table, per
-- design.md's "Logical Data Model"), the poll/poll-option/poll-vote trio,
-- the post-creation idempotency ledger, and hashtag persistence (`tags` /
-- `status_tags`).
--
-- Naming note: unlike several upstream specs' own migrations
-- (`0005_media.sql`, `0006_accounts.sql`), this file's number needs no
-- correction: design.md's "File Structure Plan" / "Physical Data Model"
-- already name it `migrations/0007_statuses.sql`, and — per this repo's
-- actual migration history (`0001_init_runtime.sql` core-runtime,
-- `0002_actors.sql` actor-model, `0003_oauth.sql` api-foundation,
-- `0004_federation.sql` federation-core, `0005_media.sql` media-pipeline,
-- `0006_accounts.sql` accounts-and-instance) — `0007` is genuinely the next
-- unused sequential slot. research.md's "Migration Numbering Coordination"
-- section separately notes that federation-core was tentatively assigned
-- `0008` in parallel spec generation; that turned out stale (federation-core
-- actually claimed the real `0004` slot, as `0004_federation.sql`'s own
-- naming-note documents), so no renumbering of any existing file is done or
-- needed here. Per `0001`'s "sequential numeric version prefix,
-- forward-only append" convention, this file only adds new tables.
--
-- Purpose (design.md "Logical Data Model" / "Physical Data Model" /
-- Boundary Commitments; Requirements 1.1, 3.6, 5.1, 7.4, 9.3, 10.4, 11.1,
-- 12.1, 13.1, 13.5):
--   - `statuses`: one row per post, local or remote, in the same model
--     (design.md model doc: "ローカル/リモート共通モデル"). `id` is a plain
--     `BIGINT PRIMARY KEY` with no `SERIAL`/`IDENTITY` default, following
--     0001-0006's convention: identifiers are always minted by the
--     application's own `IdGenerator` boundary, never by the database.
--     `actor_id` is a logical-only reference to actor-model's
--     `local_actors.id` (no `REFERENCES`, mirroring how 0003/0004/0005/0006
--     keep module boundaries between specs without a hard cross-spec FK —
--     statuses-core does not own `local_actors`). `uri` carries a
--     table-level `UNIQUE` constraint (one AP object URI per post).
--     `reblog_of_id`/`in_reply_to_id`/`in_reply_to_account_id`/`poll_id` are
--     nullable *logical* self-/forward-references with no `REFERENCES`
--     either: design.md's own Physical Data Model SQL block declares these
--     without a hard FK, and its "Consistency" note explains why — a
--     self-referential `ON DELETE CASCADE` on `reblog_of_id`/`in_reply_to_id`
--     would cascade in the wrong direction (deleting a post would delete
--     every boost/reply that references it, rather than the reverse), so
--     `StatusRepository::delete_status` instead explicitly (a) cascades the
--     deletion of any `reblog_of_id`-referencing boost row and (b)
--     decrements the parent's `replies_count` when the deleted post was
--     itself a reply (Requirement 7.4) — a data-layer backstop is
--     deliberately not attempted for these two columns. `poll_id` has no FK
--     either since `polls` does not yet exist at the point `statuses` is
--     created in this same file, and the relationship is already covered
--     from the other direction by `polls.status_id REFERENCES statuses(id)`
--     below. The three indexes (`statuses_actor_idx`/`statuses_in_reply_idx`/
--     `statuses_reblog_of_idx`) support actor-scoped listing, context
--     (ancestor/descendant) traversal (Requirement 6.2, read by a later
--     task), and boost-row lookups/cascade respectively.
--   - `status_edits`: one row per prior version of a post's editable fields
--     (Requirement 8.2, read/written by a later task). `status_id` is a
--     mandatory `REFERENCES statuses(id) ON DELETE CASCADE` (same-spec
--     reference, so a real FK is used, matching design.md's Logical Data
--     Model and the Consistency note's "物理 FK ON DELETE CASCADE で自動整合"
--     for this table). The index `status_edits_status_idx` supports the
--     history-listing query.
--   - `status_media`: the post-to-media attachment association (Requirement
--     3.4, populated by a later task). `status_id` is a mandatory
--     `REFERENCES statuses(id) ON DELETE CASCADE` (same-spec reference).
--     `media_id` is a logical-only reference to media-pipeline's `media.id`
--     (no `REFERENCES`, mirroring the cross-spec-boundary convention above —
--     statuses-core does not own `media`). `position` orders multiple
--     attachments on one post. Composite primary key `(status_id, media_id)`
--     means a given media item is attached to a given post at most once.
--   - `favourites` / `bookmarks` / `pins`: the three per-actor interaction
--     tables (Requirements 9.3/10.4 dedup via `favourites`, 11.1 via
--     `bookmarks`, 12.1 via `pins` — this task's own explicit instruction:
--     "fav/bookmark/pin は (actor_id, status_id) 一意"). `favourites`/`pins`
--     use `(actor_id, status_id)` directly as their primary key (no
--     separate surrogate id is ever needed to address a single favourite/
--     pin row). `bookmarks` instead uses its own `BIGINT PRIMARY KEY` `id`
--     with a `UNIQUE (actor_id, status_id)` constraint providing the same
--     dedup guarantee, because design.md's `InteractionRepository.
--     list_bookmarks` (Requirements 11.3) needs a monotonic, bookmark-
--     creation-ordered cursor value distinct from `status_id` to paginate
--     a given actor's bookmark list — `bookmarks_actor_idx` on
--     `(actor_id, id DESC)` is that cursor's supporting index. All three
--     tables' `status_id` is a mandatory `REFERENCES statuses(id) ON DELETE
--     CASCADE` (same-spec reference), matching the Consistency note's
--     "物理 FK ON DELETE CASCADE で自動整合" for these tables; `actor_id` stays
--     a logical-only cross-spec reference to `local_actors.id` in all three,
--     matching the same convention `statuses.actor_id` follows.
--   - `polls` / `poll_options` / `poll_votes`: the poll trio (Requirement
--     13.1, populated by a later task). `polls.status_id` is a mandatory
--     `REFERENCES statuses(id) ON DELETE CASCADE` (same-spec reference: a
--     poll cannot outlive the post it belongs to). `poll_options.poll_id`
--     is likewise a mandatory `REFERENCES polls(id) ON DELETE CASCADE`, with
--     composite primary key `(poll_id, idx)` (one row per option index per
--     poll). `poll_votes.poll_id` is the same kind of mandatory
--     `REFERENCES polls(id) ON DELETE CASCADE`; `actor_id` stays a
--     logical-only cross-spec reference (voter identity belongs to
--     actor-model). The primary key `(poll_id, actor_id, choice)` is this
--     task's own explicit instruction ("vote は (poll_id, actor_id, choice)
--     一意"): a single-choice vote is exactly one row, a multiple-choice
--     vote is one row per selected option, and re-selecting the same
--     `choice` twice is rejected as a duplicate-key violation (Requirement
--     13.5).
--   - `status_idempotency_keys`: the `Idempotency-Key` ledger (Requirement
--     5.1, populated by a later task). `actor_id` stays a logical-only
--     cross-spec reference; `status_id` is a mandatory `REFERENCES
--     statuses(id) ON DELETE CASCADE` (same-spec reference: an idempotency
--     record cannot outlive the post it resolves to). The primary key
--     `(actor_id, idempotency_key)` is this task's own explicit instruction
--     ("冪等は (actor_id, idempotency_key) 一意"): a resend of the same key by
--     the same actor is rejected as a duplicate-key violation, letting the
--     application read back the already-recorded `status_id` instead of
--     inserting a second post (Requirement 5.2, handled by a later task).
--   - `tags` / `status_tags`: hashtag persistence (this task's own explicit
--     instruction: "ハッシュタグ永続化用の tags / status_tags を作成";
--     design.md's Boundary Commitments, line "ハッシュタグの永続化（`tags` /
--     `status_tags` 関連テーブル）と、タグ関連付けの照会可能な読み取り境界" —
--     this spec owns persisting hashtag associations extracted from post
--     content (Requirement 3.6) so that downstream specs (timelines' tag
--     timeline, search's hashtag index) can consume them later; design.md's
--     own "Physical Data Model" SQL block omits the concrete table shape
--     for these two tables even though its prose commits to owning them, so
--     this migration supplies a shape consistent with every other table in
--     this file). `tags.id` is a plain application-minted `BIGINT PRIMARY
--     KEY`, following this file's own convention. `tags.name` holds the
--     normalized (lower-cased) hashtag text — normalization is an
--     application-layer concern of a later task's extraction logic, but the
--     `tags_name_unique` constraint on the stored value is what makes "the
--     same hashtag is persisted at most once" an enforced data-layer
--     invariant rather than an application-level convention alone.
--     `status_tags` is the many-to-many association: `status_id` is a
--     mandatory `REFERENCES statuses(id) ON DELETE CASCADE` and `tag_id` is
--     a mandatory `REFERENCES tags(id) ON DELETE CASCADE` (both same-spec
--     references), with composite primary key `(status_id, tag_id)` per
--     this task's own explicit instruction ("status_tags は (status_id,
--     tag_id) 一意"). The index `status_tags_tag_idx` on `tag_id` supports
--     the tag-to-posts lookup direction (this task's own explicit
--     instruction: "index... status_tags.tag_id for tag-timeline lookups")
--     that a tag timeline (downstream) or hashtag index (downstream) query
--     needs, without a full-table scan keyed only by the `(status_id,
--     tag_id)` primary key's leading `status_id` column.
--
-- Out of scope for this migration: any Rust code referencing these tables
-- (statuses-core's model/repository/service/serializer/activity-builder/
-- inbound-handler/endpoint modules, added by later tasks in this feature)
-- and any further schema evolution not required by the acceptance criteria
-- above.

CREATE TABLE statuses (
    id                      BIGINT PRIMARY KEY,           -- core-runtime IdGenerator 採番
    actor_id                BIGINT NOT NULL,              -- 投稿者（actor-model 論理参照）
    uri                     TEXT   NOT NULL UNIQUE,       -- AP オブジェクト URI（ローカルは ActorUrls 由来）
    url                     TEXT,
    content                 TEXT   NOT NULL DEFAULT '',
    visibility              TEXT   NOT NULL,              -- public/unlisted/private/direct
    sensitive               BOOLEAN NOT NULL DEFAULT FALSE,
    spoiler_text            TEXT   NOT NULL DEFAULT '',
    in_reply_to_id          BIGINT,                       -- statuses(id) 論理参照（自己参照のため FK なし）
    in_reply_to_account_id  BIGINT,
    reblog_of_id            BIGINT,                       -- ブースト元 statuses(id) 論理参照（自己参照のため FK なし）
    poll_id                 BIGINT,                       -- polls(id) 論理参照
    language                TEXT,
    reblogs_count           BIGINT NOT NULL DEFAULT 0,
    favourites_count        BIGINT NOT NULL DEFAULT 0,
    replies_count           BIGINT NOT NULL DEFAULT 0,
    local                   BOOLEAN NOT NULL,
    created_at              TIMESTAMPTZ NOT NULL,
    edited_at               TIMESTAMPTZ
);
CREATE INDEX statuses_actor_idx ON statuses(actor_id);
CREATE INDEX statuses_in_reply_idx ON statuses(in_reply_to_id);
CREATE INDEX statuses_reblog_of_idx ON statuses(reblog_of_id);

CREATE TABLE status_edits (
    id            BIGINT PRIMARY KEY,
    status_id     BIGINT NOT NULL REFERENCES statuses(id) ON DELETE CASCADE,
    content       TEXT   NOT NULL,
    spoiler_text  TEXT   NOT NULL,
    sensitive     BOOLEAN NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL
);
CREATE INDEX status_edits_status_idx ON status_edits(status_id);

CREATE TABLE status_media (                       -- 投稿への添付（media-pipeline の media を論理参照）
    status_id     BIGINT NOT NULL REFERENCES statuses(id) ON DELETE CASCADE,
    media_id      BIGINT NOT NULL,
    position      INT    NOT NULL,
    PRIMARY KEY (status_id, media_id)
);

CREATE TABLE favourites (
    actor_id      BIGINT NOT NULL,                -- お気に入りしたアクター（actor-model 論理参照）
    status_id     BIGINT NOT NULL REFERENCES statuses(id) ON DELETE CASCADE,
    created_at    TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (actor_id, status_id)              -- 重複防止（9.3, 10.4）
);

CREATE TABLE bookmarks (
    id            BIGINT PRIMARY KEY,             -- ブックマーク固有カーソル用
    actor_id      BIGINT NOT NULL,                -- actor-model 論理参照
    status_id     BIGINT NOT NULL REFERENCES statuses(id) ON DELETE CASCADE,
    created_at    TIMESTAMPTZ NOT NULL,
    UNIQUE (actor_id, status_id)                   -- 重複防止（11.1）
);
CREATE INDEX bookmarks_actor_idx ON bookmarks(actor_id, id DESC);

CREATE TABLE pins (
    actor_id      BIGINT NOT NULL,                -- actor-model 論理参照
    status_id     BIGINT NOT NULL REFERENCES statuses(id) ON DELETE CASCADE,
    created_at    TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (actor_id, status_id)              -- 重複防止（12.1）
);

CREATE TABLE polls (
    id            BIGINT PRIMARY KEY,
    status_id     BIGINT NOT NULL REFERENCES statuses(id) ON DELETE CASCADE,
    expires_at    TIMESTAMPTZ,
    multiple      BOOLEAN NOT NULL DEFAULT FALSE
);

CREATE TABLE poll_options (
    poll_id       BIGINT NOT NULL REFERENCES polls(id) ON DELETE CASCADE,
    idx           INT    NOT NULL,
    title         TEXT   NOT NULL,
    votes_count   BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (poll_id, idx)
);

CREATE TABLE poll_votes (
    poll_id       BIGINT NOT NULL REFERENCES polls(id) ON DELETE CASCADE,
    actor_id      BIGINT NOT NULL,                -- actor-model 論理参照
    choice        INT    NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (poll_id, actor_id, choice)        -- 単一選択は1行、複数選択は複数行（重複防止、13.5）
);

CREATE TABLE status_idempotency_keys (
    actor_id        BIGINT NOT NULL,               -- actor-model 論理参照
    idempotency_key TEXT   NOT NULL,
    status_id       BIGINT NOT NULL REFERENCES statuses(id) ON DELETE CASCADE,
    created_at      TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (actor_id, idempotency_key)         -- 冪等の重複防止（5.1, 5.2）
);

CREATE TABLE tags (
    id            BIGINT PRIMARY KEY,             -- core-runtime IdGenerator 採番
    name          TEXT   NOT NULL,                -- 正規化済み（小文字）ハッシュタグ名
    created_at    TIMESTAMPTZ NOT NULL,
    CONSTRAINT tags_name_unique UNIQUE (name)
);

CREATE TABLE status_tags (                        -- 投稿↔タグ関連付け
    status_id     BIGINT NOT NULL REFERENCES statuses(id) ON DELETE CASCADE,
    tag_id        BIGINT NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
    PRIMARY KEY (status_id, tag_id)                -- 重複防止
);
-- タグ→投稿の関連付け参照（タグタイムライン・ハッシュタグ検索、下流 spec 消費）を効率化
CREATE INDEX status_tags_tag_idx ON status_tags(tag_id);
