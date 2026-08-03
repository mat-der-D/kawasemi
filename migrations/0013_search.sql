-- 0013_search.sql
--
-- search task 1.1 (検索用テーブルのマイグレーションを追加する; Requirement 8:
-- 拡張・日本語対応の後付けマイグレーション経路, acceptance criteria 8.1, 8.2,
-- 8.3). Adds this spec's own owned persistent state: the hashtag read
-- index (`search_tags` / `search_status_tags`) later tasks'
-- `HashtagIndexRepository` (task 2.1) reads and writes, and the derivation
-- cursor (`search_index_watermark`) `HashtagIndexer` (task 2.2) uses for its
-- on-demand catch-up scan. This migration only adds schema — no Rust code
-- in this task.
--
-- Migration numbering: `0010` (design.md's/tasks.md's literal filename) is
-- NOT used here. `migrations/0012_social_graph.sql`'s own Implementation
-- Notes (and `.kiro/specs/social-graph/tasks.md`'s task 1.1 note) record
-- that `0008`-`0010` were reserved-but-unmaterialized placeholders assigned
-- in parallel by other specs' design docs, never actually free slots. By
-- the time this task ran, this repo's real migration history was
-- `0001_init_runtime.sql` (core-runtime), `0002_actors.sql` (actor-model),
-- `0003_oauth.sql` (api-foundation), `0004_federation.sql`
-- (federation-core), `0005_media.sql` (media-pipeline), `0006_accounts.sql`
-- (accounts-and-instance), `0007_statuses.sql` (statuses-core),
-- `0009_notifications.sql` (notifications),
-- `0011_status_mentions_and_remote_attachments.sql` (statuses-core),
-- `0012_social_graph.sql` (social-graph) — with `0008` and `0010` still
-- unmaterialized (reserved, not free) and `0011`/`0012` already applied
-- after them. sqlx's migrator (`src/migrate.rs`) applies embedded
-- migrations in strictly ascending, never-reused filename-version order
-- tracked in `_sqlx_migrations`; reusing `0010` now, after `0011`/`0012`
-- are already applied in any environment that has run them, would be an
-- out-of-order/reused version and would break the migrator. This migration
-- therefore claims the next actually-free slot instead: `0013`.
--
-- Purpose (design.md "Logical Data Model" / "Physical Data Model";
-- Requirement 8: 拡張・日本語対応の後付けマイグレーション経路, acceptance
-- criteria 8.1, 8.2, 8.3):
--   - `search_tags`: one row per distinct normalized hashtag name this spec
--     has derived from upstream (statuses-core) posts' retained hashtag
--     data. `id` is a plain `BIGINT PRIMARY KEY` with no `SERIAL`/`IDENTITY`
--     default, following 0001-0007/0009/0011/0012's established convention:
--     identifiers are always minted by the application's own core-runtime
--     `IdGenerator` boundary, never by the database. `name` is `UNIQUE`
--     (design.md: "正規化名 UNIQUE で upsert" — later tasks' upsert relies on
--     this for `ON CONFLICT (name) DO UPDATE`). `last_status_at` /
--     `statuses_count` are the minimal usage aggregation design.md's
--     "Logical Data Model" calls for (used for Tag `history`/freshness);
--     both are nullable-friendly minimal aggregates, not a full analytics
--     table (out of scope: trends, per design.md's Boundary Context).
--     `updated_at` is `Clock`-sourced (deterministic, per design.md's
--     "Temporal").
--   - `search_tags_name_idx`: a standard-PostgreSQL prefix-match index on
--     `search_tags(name)` using the built-in `text_pattern_ops` opclass
--     (task 1.1's own explicit instruction). `text_pattern_ops` is a
--     core PostgreSQL opclass (no `CREATE EXTENSION` required) that makes a
--     plain btree index usable for `LIKE 'prefix%'`/`~^` prefix matching
--     regardless of the database's collation — this is precisely the
--     "standard PostgreSQL only, no `pg_bigm`" prefix-match mechanism
--     Requirement 8.1/8.4 calls for hashtag search (5.1) to use by default.
--   - `search_status_tags`: the tag<->post association this spec derives
--     read-only from statuses-core's retained per-post hashtag data.
--     `tag_id` takes a real `REFERENCES search_tags(id) ON DELETE CASCADE`
--     (both tables are owned by this same spec/migration, so a hard FK is
--     safe and keeps the index consistent if a tag row is ever pruned).
--     `status_id` is a logical-only reference to statuses-core's
--     `statuses(id)` (no `REFERENCES`, mirroring this repo's already-
--     established cross-module-boundary convention of never taking a hard
--     `REFERENCES` across a spec boundary — see `migrations/0009_
--     notifications.sql`'s doc comment for the identical pattern applied to
--     `notifications.status_id`) — search does not own `statuses`.
--     `(tag_id, status_id)` is the primary key (design.md: "(tag_id,
--     status_id) は一意"), which also gives task 2.1's `upsert_tag_usage` an
--     `ON CONFLICT (tag_id, status_id) DO NOTHING`-style idempotent upsert
--     target so re-scanning the same post's tags never double-counts.
--   - `search_status_tags_status_idx` on `status_id` (task 1.1's own
--     explicit instruction): backs lookups/joins keyed by post id (e.g.
--     when a post's hashtag associations need to be located directly by
--     `status_id`, not by tag), mirroring this repo's existing per-column
--     lookup-index convention for association tables.
--   - `search_index_watermark`: the `HashtagIndexer`'s (task 2.2) on-demand
--     catch-up-scan cursor (design.md: "最終処理済み statuses.created_at/id
--     を保持"). `id BOOLEAN PRIMARY KEY DEFAULT TRUE` plus the
--     `search_index_watermark_singleton CHECK (id)` constraint (task 1.1's
--     own explicit instruction) together enforce that the table can hold at
--     most one row, ever, in a way that is provably stronger than the
--     primary key alone: a bare `BOOLEAN PRIMARY KEY` still permits two
--     *distinct* rows (`id = TRUE` and `id = FALSE`, since they differ from
--     each other), which would silently break the "single cursor" contract;
--     the `CHECK (id)` closes exactly that gap by rejecting any row whose
--     `id` is not `TRUE` outright, so the only way to "update" the
--     watermark is an `UPDATE`/`UPSERT` against the sole `id = TRUE` row,
--     never an `INSERT` of a second one. `status_created_at`/`status_id`
--     are both nullable (unset = no watermark yet = task 2.2's "初回は全件
--     走査" backfill case). `updated_at` is `Clock`-sourced.
--
-- Extension-free default / future extension-based path (Requirement 8.1,
-- 8.3, 8.4): this migration deliberately contains no `CREATE EXTENSION`
-- statement anywhere (no `pg_bigm` or otherwise) — every index above uses
-- only standard, built-in PostgreSQL facilities (a plain btree with the
-- built-in `text_pattern_ops` opclass), so the default distribution
-- continues to require nothing beyond "the app + PostgreSQL" (design.md's
-- "配布方針"). A future, wholly independent migration (a higher-numbered
-- file than this one) may add `CREATE EXTENSION pg_bigm;` plus a
-- `CREATE INDEX ... USING gin (... gin_bigm_ops)` on `search_tags.name` (or
-- an equivalent Japanese-aware index) purely additively, without altering
-- or dropping anything this migration creates and without changing
-- `PgSearchBackend`'s/`GET /api/v2/search`'s API contract (8.3, 8.4) — see
-- the commented-out illustrative sketch at the bottom of this file, which
-- is documentation only and intentionally never executes as part of this
-- (or any other) migration.
--
-- Out of scope for this migration: any Rust code referencing `search_tags`/
-- `search_status_tags`/`search_index_watermark` (search's model/repository/
-- indexer/backend/service/endpoint modules, added by later tasks in this
-- feature, starting with task 1.2's domain types and task 2.1's
-- `HashtagIndexRepository`) and any further schema evolution not required
-- by task 1.1's acceptance bullets above, including the future Japanese-
-- extension index sketched (but not created) below.

CREATE TABLE search_tags (
    id             BIGINT PRIMARY KEY,             -- core-runtime IdGenerator 採番
    name           TEXT   NOT NULL UNIQUE,         -- 正規化ハッシュタグ名（小文字化等）
    last_status_at TIMESTAMPTZ,                    -- history/鮮度用の最小集計
    statuses_count BIGINT NOT NULL DEFAULT 0,      -- 使用件数の最小集計
    updated_at     TIMESTAMPTZ NOT NULL
);

-- 前方一致の標準索引（text_pattern_ops、拡張不要）
CREATE INDEX search_tags_name_idx ON search_tags (name text_pattern_ops);

CREATE TABLE search_status_tags (
    tag_id     BIGINT NOT NULL REFERENCES search_tags (id) ON DELETE CASCADE,
    status_id  BIGINT NOT NULL,                    -- statuses-core statuses(id) 論理参照（read-only 導出）
    created_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (tag_id, status_id)
);

CREATE INDEX search_status_tags_status_idx ON search_status_tags (status_id);

-- ハッシュタグインデクサ（HashtagIndexer）の導出カーソル（シングルトン）。
-- id は BOOLEAN PRIMARY KEY DEFAULT TRUE とし、search_index_watermark_singleton
-- CHECK (id) で「id が TRUE の行以外は挿入させない」ことを強制する。これにより
-- PRIMARY KEY 単体では防げない「id = FALSE の別行が共存し得る」抜け穴を塞ぎ、
-- テーブル全体で常にたかだか 1 行のみが存在することを保証する。
CREATE TABLE search_index_watermark (
    id                 BOOLEAN PRIMARY KEY DEFAULT TRUE,
    status_created_at  TIMESTAMPTZ,                -- 最終処理済み statuses.created_at（未保持=初回/バックフィル）
    status_id          BIGINT,                     -- 最終処理済み statuses.id（tie-break）
    updated_at         TIMESTAMPTZ NOT NULL,
    CONSTRAINT search_index_watermark_singleton CHECK (id)
);

-- 後付け（任意・別マイグレーション、初期配布には含めない。ドキュメント目的のみ
-- で本マイグレーションの一部としては実行されない）:
--
--   CREATE EXTENSION IF NOT EXISTS pg_bigm;
--   CREATE INDEX search_tags_name_bigm_idx
--       ON search_tags USING gin (name gin_bigm_ops);
--
-- 既定 PgSearchBackend の SQL 契約・GET /api/v2/search の API 契約は不変のまま、
-- 拡張バックエンド（将来 spec）がこのインデックスを利用する。
