-- 0011_status_mentions_and_remote_attachments.sql
--
-- statuses-core task 10.3 (要件14.2の未充足解消: リモート Create(Note) 取り込
-- みで添付・メンションを反映する). Adds exactly the two small tables needed
-- to close this task's own narrowly-scoped gap in `ingest_note_object`
-- (`src/statuses/inbound_handlers.rs`) — see that function's own doc comment
-- for the full ingestion-side reasoning.
--
-- Migration numbering: `migrations/` currently holds 0001-0007 as real
-- files. `0008`/`0009`/`0010` are not yet materialized as files in this repo
-- but are already reserved by other specs' own research.md coordination
-- notes (confirmed by cross-spec grep at authoring time): federation-core
-- owns `0008_federation.sql` (federation-core/research.md "Decision:
-- federation-core のマイグレーションを...0008_federation.sql へ改番する。
-- 0008 スロットは以後 federation-core が所有する。"), notifications owns
-- `0009_notifications.sql` (notifications/research.md), search owns
-- `0010_search.sql` (search/research.md's own confirmed non-colliding
-- sequence table, which also independently corroborates the federation-
-- core/notifications assignments above). statuses-core's own task 1.1
-- Implementation Notes entry (`tasks.md`) already documents that `0008` is
-- off-limits to this spec for the identical reason. No other spec's
-- tasks.md/research.md claims `0011` or higher as of this migration, so
-- `0011` is the next genuinely free slot for a statuses-core-owned table
-- addition, following `0001`'s "sequential numeric version prefix,
-- forward-only append" convention.
--
-- Purpose (Requirement 14.2's "返信・可視性・添付・メンションを反映する";
-- task 10.3's own scoping — see `inbound_handlers.rs`'s doc comment,
-- "What this handler does *not* persist" section (now resolved) for the
-- full boundary reasoning this migration exists to support):
--   - `status_mentions`: one row per (status, locally-resolved mentioned
--     actor) pair, extracted from an inbound remote `Note`'s `tag`-array
--     `Mention` entries (`inbound_handlers.rs::extract_tag_mentions`) and
--     resolved through the same `LocalMentionResolver` port task 10.2 added
--     for notification purposes (`inbound_handlers.rs::resolve_mentions`) —
--     not a second, forked resolution path. Only mentions that resolve to a
--     *local* actor are persisted here, matching this crate's single,
--     already-established "mention resolution: local only" convention
--     (`status_service.rs`'s own doc comment of that exact title, applied
--     symmetrically to the remote-ingestion direction). `actor_id` is a
--     logical-only reference to actor-model's `local_actors.id` (no
--     `REFERENCES`, mirroring `statuses.actor_id`'s own identical
--     cross-spec-boundary convention in `migrations/0007_statuses.sql`).
--     `status_id` is a same-spec reference and does get a real
--     `REFERENCES ... ON DELETE CASCADE` (a mentioned-actor row cannot
--     outlive the post that mentions it). Composite primary key
--     `(status_id, actor_id)` — a given actor is mentioned by a given post
--     at most once (a `Note`'s `tag` array carrying the same `Mention`
--     `name` twice collapses to one row, matching
--     `extract_tag_mentions`'s own exact-key dedup).
--   - `status_remote_attachments`: one row per attachment entry reflected
--     from an inbound remote `Note`'s `attachment` property, stored as
--     lightweight, statuses-core-owned metadata (`url`/`media_type`/
--     `description`) rather than a real media-pipeline `media`/
--     `status_media` row — deliberately mirroring
--     `accounts-and-instance`'s already-implemented precedent for the
--     identical class of problem: `RemoteAccountFetcher::fetch_and_normalize`
--     (`src/accounts/remote_fetcher.rs`) stores a remote actor's
--     avatar/header image as a plain URL string
--     (`account_profiles.avatar_url`/`header_url`,
--     `migrations/0006_accounts.sql`), never as a full local `Media` row,
--     because there is no capability anywhere in this crate to fetch a
--     remote URL's bytes and turn them into a real, owned `media` row
--     outside of `MediaService::accept_upload`'s own raw-upload-bytes entry
--     point (media-pipeline's boundary, not this spec's). `status_media`'s
--     `media_id` (`migrations/0007_statuses.sql`) stays a logical FK to
--     media-pipeline's own real `media.id` and is deliberately never
--     fabricated by this migration's tables — `status_remote_attachments`
--     is a wholly separate, additive table, not a variant/extension of
--     `status_media`. `status_id` is a same-spec `REFERENCES ... ON DELETE
--     CASCADE`. `position` preserves the `Note.attachment` array's own
--     order (mirrors `status_media.position`'s identical purpose);
--     composite primary key `(status_id, position)` needs no separate
--     surrogate id since a given post's attachment order is already a
--     unique key. `media_type`/`description` are nullable: an
--     ActivityStreams attachment entry's `mediaType`/`name` (used as the
--     conventional Mastodon-compatible attachment description field) are
--     both genuinely optional per the vocabulary.
--
-- Out of scope for this migration: any further schema evolution beyond
-- these two tables, and any Rust code referencing them (added by task 10.3
-- itself in the same run this migration was authored for).

CREATE TABLE status_mentions (                    -- リモート Note の tag[]Mention のうちローカル解決分
    status_id     BIGINT NOT NULL REFERENCES statuses(id) ON DELETE CASCADE,
    actor_id      BIGINT NOT NULL,                -- ローカルアクター（actor-model 論理参照）
    PRIMARY KEY (status_id, actor_id)              -- 重複防止
);

CREATE TABLE status_remote_attachments (          -- リモート Note の attachment[] を軽量メタデータとして反映（media-pipeline の Media 実体は生成しない）
    status_id     BIGINT NOT NULL REFERENCES statuses(id) ON DELETE CASCADE,
    position      INT    NOT NULL,
    url           TEXT   NOT NULL,
    media_type    TEXT,
    description   TEXT,
    PRIMARY KEY (status_id, position)
);
