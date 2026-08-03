-- 0006_accounts.sql
--
-- accounts-and-instance: adds the persistence for this spec's own owned
-- state: local-actor profile extensions, normalized/cached remote accounts,
-- the custom-emoji read model, and the single-row operational instance
-- settings.
--
-- Naming note: the task text (`.kiro/specs/accounts-and-instance/tasks.md`
-- task 1.1) and design.md's "File Structure Plan"/"Physical Data Model" name
-- this file `0005_accounts.sql`. That number is stale: this repo already has
-- `migrations/0001_init_runtime.sql` (core-runtime), `0002_actors.sql`
-- (actor-model), `0003_oauth.sql` (api-foundation), `0004_federation.sql`
-- (federation-core), and `0005_media.sql` (media-pipeline, whose own naming-
-- note comment documents this same discrepancy — each spec's design.md was
-- assigned a migration number in parallel by `/kiro-spec-batch`, and those
-- numbers never reflected real implementation order; media-pipeline claimed
-- the real `0005` slot first, having been implemented before
-- accounts-and-instance). Per `0001_init_runtime.sql`'s "sequential numeric
-- version prefix, forward-only append" convention, and because sqlx's
-- migrator (`sqlx::migrate!("./migrations")`, see `src/migrate.rs`) applies
-- migrations in ascending version order and rejects out-of-order insertion,
-- this file uses the real next sequential slot, `0006`, instead. Only the
-- filename numeral differs from design.md; the table/column/index/
-- constraint substance below is otherwise taken as-is from design.md's
-- `0005_accounts.sql` SQL block.
--
-- Purpose (design.md "Logical Data Model" / "Physical Data Model";
-- Requirements 6.5, 7.2, 8.2, 9.1):
--   - `account_profiles`: the local-actor profile extension (1:1 logical
--     reference to actor-model's `local_actors.id`, no `REFERENCES` — this
--     spec does not own `local_actors`, mirroring how 0004/0005 keep module
--     boundaries between specs without a hard cross-spec FK). `actor_id` is
--     the primary key (one profile per local actor). `display_name`/`note`
--     are the supply source for the Account/CredentialAccount fields of the
--     same name (design.md model doc; task 1.1's explicit instruction) and
--     default to `''` so a freshly-created actor has a valid, non-null
--     profile row from the start. `avatar_media_id`/`header_media_id` are
--     logical-only references to media-pipeline's `media.id` (same
--     no-hard-FK convention). `fields` holds the `ProfileField[]` JSON
--     array. `locked`/`bot`/`discoverable` and the `source_*` columns
--     (`source_privacy`/`source_sensitive`/`source_language`) hold the
--     remaining `AccountProfile`/`CredentialSource` fields design.md's model
--     doc specifies.
--   - `remote_accounts`: one row per normalized, cached remote account
--     (Requirement 7.2: ActivityPub actor documents are normalized into
--     Account-contract fields and held). `id` is a plain `BIGINT PRIMARY
--     KEY` with no `SERIAL`/`IDENTITY` default, following 0001-0005's
--     convention: identifiers are always minted by the application's own
--     `IdGenerator` boundary, never by the database. `actor_uri` carries a
--     table-level `UNIQUE` constraint (task 1.1's explicit instruction;
--     Requirement 7.2) so a given remote actor is normalized/cached at most
--     once. `fetched_at` is the staleness-check timestamp (7.3). The index
--     `remote_accounts_handle_idx` on `(username, domain)` supports
--     `username@domain`-keyed lookups (Requirement 1.3's `acct` resolution
--     path).
--   - `custom_emojis`: the read-only custom-emoji model (Requirement 9.1-
--     9.3: this spec only reads; population is a later spec's
--     responsibility). Composite primary key `(shortcode, domain)` (task
--     1.1's explicit instruction) — `domain = ''` denotes a local emoji, so
--     the same shortcode may exist once locally and again per distinct
--     remote domain without colliding.
--   - `instance_settings`: the single-row table of operational, admin-
--     writable-but-not-by-this-spec settings (Requirement 8.2: `title`/
--     `description`/`contact`/`rules`/`registrations` etc. are read from
--     here and reflected into Instance(v2); this spec only reads/defaults,
--     never writes — see design.md's Data Contracts). `id INTEGER PRIMARY
--     KEY DEFAULT 1` plus the `instance_settings_singleton CHECK (id = 1)`
--     constraint (task 1.1's explicit instruction) enforce that at most one
--     row, with `id = 1`, can ever exist. `thumbnail`/`languages` (task
--     1.1's explicit instruction) supply Instance(v2)'s same-named fields
--     (Requirement 8.1); `languages` is a JSONB array of strings so an unset
--     value can default to `'[]'` rather than NULL (Requirement 8.3's "safe
--     initial default" for every item, mirroring `fields`/`rules`'s own
--     JSONB-array-with-`[]`-default convention elsewhere in this file).
--
-- Out of scope for this migration: any Rust code referencing these tables
-- (accounts-and-instance's model/repository/serializer/service/endpoint
-- modules, added by later tasks in this feature) and any further schema
-- evolution not required by the acceptance criteria above.

CREATE TABLE account_profiles (
    actor_id         BIGINT PRIMARY KEY,            -- local_actors(id) 論理参照（1:1）
    display_name     TEXT   NOT NULL DEFAULT '',    -- Account/CredentialAccount の display_name 供給元
    note             TEXT   NOT NULL DEFAULT '',    -- Account/CredentialAccount の note 供給元
    avatar_media_id  BIGINT,                        -- media(id) 論理参照（任意）
    header_media_id  BIGINT,                        -- media(id) 論理参照（任意）
    fields           JSONB  NOT NULL DEFAULT '[]',  -- ProfileField[]
    locked           BOOLEAN NOT NULL DEFAULT FALSE,
    bot              BOOLEAN NOT NULL DEFAULT FALSE,
    discoverable     BOOLEAN NOT NULL DEFAULT FALSE,
    source_privacy   TEXT   NOT NULL DEFAULT 'public',
    source_sensitive BOOLEAN NOT NULL DEFAULT FALSE,
    source_language  TEXT,
    updated_at       TIMESTAMPTZ NOT NULL
);

CREATE TABLE remote_accounts (
    id            BIGINT PRIMARY KEY,              -- core-runtime IdGenerator 採番
    actor_uri     TEXT   NOT NULL UNIQUE,
    username      TEXT   NOT NULL,
    domain        TEXT   NOT NULL,
    display_name  TEXT   NOT NULL DEFAULT '',
    note          TEXT   NOT NULL DEFAULT '',
    url           TEXT   NOT NULL,
    avatar_url    TEXT,
    header_url    TEXT,
    fields        JSONB  NOT NULL DEFAULT '[]',
    bot           BOOLEAN NOT NULL DEFAULT FALSE,
    locked        BOOLEAN NOT NULL DEFAULT FALSE,
    fetched_at    TIMESTAMPTZ NOT NULL
);
-- username@domain 形式の acct 解決を効率化するインデックス（1.3）
CREATE INDEX remote_accounts_handle_idx ON remote_accounts(username, domain);

CREATE TABLE custom_emojis (
    shortcode         TEXT   NOT NULL,
    domain            TEXT   NOT NULL DEFAULT '',  -- '' = ローカル
    url               TEXT   NOT NULL,
    static_url        TEXT   NOT NULL,
    visible_in_picker BOOLEAN NOT NULL DEFAULT TRUE,
    category          TEXT,
    updated_at        TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (shortcode, domain)
);

CREATE TABLE instance_settings (
    id                               INTEGER PRIMARY KEY DEFAULT 1,   -- 単一行（id=1 固定）
    title                            TEXT   NOT NULL DEFAULT '',
    description                      TEXT   NOT NULL DEFAULT '',
    contact_email                    TEXT   NOT NULL DEFAULT '',
    contact_account_id               BIGINT,                          -- local_actors(id) 論理参照（任意）
    rules                            JSONB  NOT NULL DEFAULT '[]',
    registrations_enabled            BOOLEAN NOT NULL DEFAULT FALSE,
    registrations_approval_required  BOOLEAN NOT NULL DEFAULT FALSE,
    registrations_message            TEXT,
    thumbnail                        TEXT,                            -- Instance(v2).thumbnail 供給元（未設定 = null）
    languages                        JSONB  NOT NULL DEFAULT '[]',    -- Instance(v2).languages 供給元（String[]、未設定 = []）
    updated_at                       TIMESTAMPTZ NOT NULL,
    CONSTRAINT instance_settings_singleton CHECK (id = 1)
);
