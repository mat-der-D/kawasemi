-- 0002_actors.sql
--
-- actor-model: adds the core persistence for local actors, the management
-- layer's owner concept, and per-actor signing keys.
--
-- Purpose (design.md "Physical Data Model" / "Logical Data Model";
-- Requirements 1.2, 2.1, 5.3):
--   - `owners`: the management-layer-only concept that groups multiple
--     local actors under a single administrator (Requirement 2.1). `id` is
--     a plain `BIGINT PRIMARY KEY` with no `SERIAL`/`IDENTITY` default,
--     following 0001's convention: identifiers are always minted by the
--     application's own `IdGenerator` boundary, never by the database.
--   - `local_actors`: one row per local ActivityPub actor. `handle` carries
--     a table-level `UNIQUE` constraint (`local_actors_handle_unique`) so
--     the single-domain instance enforces handle uniqueness at the data
--     layer, not just in application logic (Requirement 1.2). `owner_id`
--     is a mandatory foreign key into `owners`, encoding the 1:N
--     owner-to-actor relationship (Requirement 2.1); it is intentionally
--     `NOT NULL` because every actor must belong to exactly one owner
--     (Requirement 2.2/2.3 are enforced by actor-model's service layer
--     before insertion, but the FK is the data-layer backstop). An index
--     on `owner_id` supports the owner-scoped listing this spec's
--     `ActorDirectory.list_actors_for_owner` requires.
--   - `actor_signing_keys`: one row per signing key ever issued to an
--     actor (active or retired), so rotation history is retained
--     (Requirement 5.4). `actor_id` is a mandatory foreign key into
--     `local_actors`. The partial unique index
--     `actor_signing_keys_active_unique` enforces "at most one active key
--     per actor" (Requirement 5.3) directly in the schema: it only
--     indexes rows where `status = 'active'`, so any number of `retired`
--     rows may coexist for the same actor, but a second `active` row for
--     the same actor is rejected by the database itself.
--
-- Out of scope for this migration: any Rust code referencing these tables
-- (actor-model's `src/actor/` module, added by a later task in this
-- feature) and any further schema evolution (profile fields, additional
-- indexes) not required by the acceptance criteria above.

CREATE TABLE owners (
    id          BIGINT PRIMARY KEY,            -- core-runtime IdGenerator 採番
    created_at  TIMESTAMPTZ NOT NULL
);

CREATE TABLE local_actors (
    id            BIGINT PRIMARY KEY,
    owner_id      BIGINT NOT NULL REFERENCES owners(id),
    handle        TEXT   NOT NULL,
    actor_type    TEXT   NOT NULL,             -- 'person' | 'service'
    display_name  TEXT   NOT NULL DEFAULT '',
    summary       TEXT   NOT NULL DEFAULT '',
    state         TEXT   NOT NULL,             -- 'active' | 'deactivated'
    created_at    TIMESTAMPTZ NOT NULL,
    updated_at    TIMESTAMPTZ NOT NULL,
    CONSTRAINT local_actors_handle_unique UNIQUE (handle)
);
CREATE INDEX local_actors_owner_idx ON local_actors(owner_id);

CREATE TABLE actor_signing_keys (
    id                  BIGINT PRIMARY KEY,
    actor_id            BIGINT NOT NULL REFERENCES local_actors(id),
    algorithm           TEXT   NOT NULL,        -- 'rsa-2048'
    public_key_pem      TEXT   NOT NULL,
    sealed_private_key  BYTEA  NOT NULL,        -- KeyCipher で封緘済み（平文を格納しない）
    status              TEXT   NOT NULL,        -- 'active' | 'retired'
    created_at          TIMESTAMPTZ NOT NULL
);
-- アクター毎に有効鍵は高々1（5.3）
CREATE UNIQUE INDEX actor_signing_keys_active_unique
    ON actor_signing_keys(actor_id) WHERE status = 'active';
