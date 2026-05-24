-- Migration 012: provenance stamps + history mirror tables
--
-- This migration is FORWARD-ONLY (no down-migration; project convention).
-- rusqlite_migration wraps each .sql file in a transaction automatically.
--
-- Three parts:
--   Part 1 — add `derived_at_version` to every artifact head table.
--   Part 2 — create `*_history` mirror tables + indexes.
--   Part 3 — backfill `derived_at_version` from `corpora.last_indexed_version`
--            for all pre-existing rows (best-effort; rows whose corpus has a
--            NULL last_indexed_version stay NULL — acceptable).

-- ─────────────────────────────────────────────────────────────────────────────
-- Part 1: at-write SHA attribution on head tables
-- ─────────────────────────────────────────────────────────────────────────────

ALTER TABLE entities          ADD COLUMN derived_at_version TEXT;
ALTER TABLE edges             ADD COLUMN derived_at_version TEXT;
ALTER TABLE entity_purposes   ADD COLUMN derived_at_version TEXT;
ALTER TABLE entity_contracts  ADD COLUMN derived_at_version TEXT;
ALTER TABLE entity_blocks     ADD COLUMN derived_at_version TEXT;
ALTER TABLE summaries         ADD COLUMN derived_at_version TEXT;
ALTER TABLE themes            ADD COLUMN derived_at_version TEXT;
-- chunks already has `introduced_at_version` and `last_modified_at_version`
-- from migration 009; treat `last_modified_at_version` as the chunk equivalent.

CREATE INDEX idx_entities_derived          ON entities          (corpus_id, derived_at_version);
CREATE INDEX idx_edges_derived             ON edges             (corpus_id, derived_at_version);
CREATE INDEX idx_entity_purposes_derived   ON entity_purposes   (corpus_id, derived_at_version);
CREATE INDEX idx_entity_contracts_derived  ON entity_contracts  (corpus_id, derived_at_version);
CREATE INDEX idx_entity_blocks_derived     ON entity_blocks     (corpus_id, derived_at_version);
CREATE INDEX idx_summaries_derived         ON summaries         (corpus_id, derived_at_version);
CREATE INDEX idx_themes_derived            ON themes            (corpus_id, derived_at_version);

-- ─────────────────────────────────────────────────────────────────────────────
-- Part 2: history mirror tables
-- ─────────────────────────────────────────────────────────────────────────────
--
-- Shape = head columns + three history columns:
--   superseded_at_version : manifest current_version at time of snapshot
--                           (cascade-delete or upsert-replace trigger).
--   superseded_at         : ISO-8601 wall-clock timestamp of the snapshot.
--   history_id            : surrogate INTEGER PRIMARY KEY (rowid alias) so the
--                           same head PK can appear multiple times across re-
--                           supersessions.
--
-- No FK references from history → corpora/entities: history rows must outlive
-- the head rows they describe.

CREATE TABLE entities_history (
    history_id            INTEGER PRIMARY KEY,
    id                    TEXT NOT NULL,
    corpus_id             TEXT NOT NULL,
    canonical_name        TEXT NOT NULL,
    kind                  TEXT NOT NULL,
    aliases               TEXT NOT NULL,
    description           TEXT,
    first_location_uri    TEXT,
    last_location_uri     TEXT,
    appearance_count      INTEGER NOT NULL,
    confidence            REAL NOT NULL,
    derived_at_version    TEXT,
    superseded_at_version TEXT NOT NULL,
    superseded_at         TEXT NOT NULL
);
CREATE INDEX idx_entities_history_id         ON entities_history (corpus_id, id);
CREATE INDEX idx_entities_history_superseded ON entities_history (corpus_id, superseded_at_version);

CREATE TABLE edges_history (
    history_id            INTEGER PRIMARY KEY,
    id                    TEXT NOT NULL,
    corpus_id             TEXT NOT NULL,
    from_entity_id        TEXT NOT NULL,
    to_entity_id          TEXT NOT NULL,
    kind                  TEXT NOT NULL,
    location_uri          TEXT NOT NULL,
    confidence            REAL NOT NULL,
    derived_at_version    TEXT,
    superseded_at_version TEXT NOT NULL,
    superseded_at         TEXT NOT NULL
);
CREATE INDEX idx_edges_history_endpoints  ON edges_history (corpus_id, from_entity_id, to_entity_id);
CREATE INDEX idx_edges_history_superseded ON edges_history (corpus_id, superseded_at_version);

CREATE TABLE entity_purposes_history (
    history_id            INTEGER PRIMARY KEY,
    entity_id             TEXT NOT NULL,
    corpus_id             TEXT NOT NULL,
    purpose               TEXT NOT NULL,
    model                 TEXT NOT NULL,
    model_tier            TEXT NOT NULL,
    generated_at          TEXT NOT NULL,
    derived_at_version    TEXT,
    superseded_at_version TEXT NOT NULL,
    superseded_at         TEXT NOT NULL
);
CREATE INDEX idx_entity_purposes_history_entity     ON entity_purposes_history (corpus_id, entity_id);
CREATE INDEX idx_entity_purposes_history_superseded ON entity_purposes_history (corpus_id, superseded_at_version);

CREATE TABLE entity_contracts_history (
    history_id            INTEGER PRIMARY KEY,
    entity_id             TEXT NOT NULL,
    corpus_id             TEXT NOT NULL,
    is_public             INTEGER NOT NULL,
    is_must_use           INTEGER NOT NULL,
    is_deprecated         INTEGER NOT NULL,
    is_fallible           INTEGER NOT NULL,
    is_nullable           INTEGER NOT NULL,
    is_mutating           INTEGER NOT NULL,
    is_diverging          INTEGER NOT NULL,
    has_panic_risk        INTEGER NOT NULL,
    has_unsafe            INTEGER NOT NULL,
    is_incomplete         INTEGER NOT NULL,
    panic_call_count      INTEGER NOT NULL,
    debt_markers          TEXT NOT NULL,
    assumptions           TEXT NOT NULL,
    risks                 TEXT NOT NULL,
    intent_gap            TEXT,
    caller_notes          TEXT,
    model                 TEXT NOT NULL,
    model_tier            TEXT NOT NULL,
    generated_at          TEXT NOT NULL,
    derived_at_version    TEXT,
    superseded_at_version TEXT NOT NULL,
    superseded_at         TEXT NOT NULL
);
CREATE INDEX idx_entity_contracts_history_entity     ON entity_contracts_history (corpus_id, entity_id);
CREATE INDEX idx_entity_contracts_history_superseded ON entity_contracts_history (corpus_id, superseded_at_version);

CREATE TABLE entity_blocks_history (
    history_id            INTEGER PRIMARY KEY,
    id                    TEXT NOT NULL,
    entity_id             TEXT NOT NULL,
    corpus_id             TEXT NOT NULL,
    label                 TEXT NOT NULL,
    description           TEXT NOT NULL,
    position              INTEGER NOT NULL,
    derived_at_version    TEXT,
    superseded_at_version TEXT NOT NULL,
    superseded_at         TEXT NOT NULL
);
CREATE INDEX idx_entity_blocks_history_entity     ON entity_blocks_history (corpus_id, entity_id);
CREATE INDEX idx_entity_blocks_history_superseded ON entity_blocks_history (corpus_id, superseded_at_version);

CREATE TABLE summaries_history (
    history_id            INTEGER PRIMARY KEY,
    id                    TEXT NOT NULL,
    corpus_id             TEXT NOT NULL,
    target_kind           TEXT NOT NULL,
    target_id             TEXT NOT NULL,
    depth                 TEXT NOT NULL,
    text                  TEXT NOT NULL,
    model                 TEXT NOT NULL,
    model_tier            TEXT NOT NULL,
    generated_at          TEXT NOT NULL,
    derived_at_version    TEXT,
    superseded_at_version TEXT NOT NULL,
    superseded_at         TEXT NOT NULL
);
CREATE INDEX idx_summaries_history_target     ON summaries_history (corpus_id, target_kind, target_id);
CREATE INDEX idx_summaries_history_superseded ON summaries_history (corpus_id, superseded_at_version);

CREATE TABLE chunks_history (
    history_id                   INTEGER PRIMARY KEY,
    id                           TEXT NOT NULL,
    corpus_id                    TEXT NOT NULL,
    parent_path                  TEXT,
    kind                         TEXT NOT NULL,
    location_uri                 TEXT NOT NULL,
    content                      TEXT NOT NULL,
    byte_length                  INTEGER NOT NULL,
    created_at                   TEXT NOT NULL,
    semantic_processed           INTEGER NOT NULL,
    source_hash                  TEXT,
    introduced_at_version        TEXT,
    last_modified_at_version     TEXT,
    last_modified_commit_message TEXT,
    last_modified_author         TEXT,
    superseded_at_version        TEXT NOT NULL,
    superseded_at                TEXT NOT NULL
);
CREATE INDEX idx_chunks_history_location     ON chunks_history (corpus_id, location_uri);
CREATE INDEX idx_chunks_history_superseded   ON chunks_history (corpus_id, superseded_at_version);

CREATE TABLE themes_history (
    history_id            INTEGER PRIMARY KEY,
    id                    TEXT NOT NULL,
    corpus_id             TEXT NOT NULL,
    title                 TEXT NOT NULL,
    statement             TEXT NOT NULL,
    confidence            REAL NOT NULL,
    model                 TEXT,
    model_tier            TEXT NOT NULL,
    generated_at          TEXT NOT NULL,
    derived_at_version    TEXT,
    superseded_at_version TEXT NOT NULL,
    superseded_at         TEXT NOT NULL
);
CREATE INDEX idx_themes_history_superseded ON themes_history (corpus_id, superseded_at_version);

-- ─────────────────────────────────────────────────────────────────────────────
-- Part 3: backfill derived_at_version for pre-existing rows
-- ─────────────────────────────────────────────────────────────────────────────
--
-- The only SHA we know retroactively is corpora.last_indexed_version.
-- Stamp it onto every artifact row for the corresponding corpus.
-- Rows whose corpus has a NULL last_indexed_version remain NULL (acceptable —
-- they pre-date any indexed state).

UPDATE entities
    SET derived_at_version = (
        SELECT last_indexed_version FROM corpora WHERE corpora.id = entities.corpus_id
    )
    WHERE derived_at_version IS NULL;

UPDATE edges
    SET derived_at_version = (
        SELECT last_indexed_version FROM corpora WHERE corpora.id = edges.corpus_id
    )
    WHERE derived_at_version IS NULL;

UPDATE entity_purposes
    SET derived_at_version = (
        SELECT last_indexed_version FROM corpora WHERE corpora.id = entity_purposes.corpus_id
    )
    WHERE derived_at_version IS NULL;

UPDATE entity_contracts
    SET derived_at_version = (
        SELECT last_indexed_version FROM corpora WHERE corpora.id = entity_contracts.corpus_id
    )
    WHERE derived_at_version IS NULL;

UPDATE entity_blocks
    SET derived_at_version = (
        SELECT last_indexed_version FROM corpora WHERE corpora.id = entity_blocks.corpus_id
    )
    WHERE derived_at_version IS NULL;

UPDATE summaries
    SET derived_at_version = (
        SELECT last_indexed_version FROM corpora WHERE corpora.id = summaries.corpus_id
    )
    WHERE derived_at_version IS NULL;

UPDATE themes
    SET derived_at_version = (
        SELECT last_indexed_version FROM corpora WHERE corpora.id = themes.corpus_id
    )
    WHERE derived_at_version IS NULL;

-- chunks: backfill introduced_at_version / last_modified_at_version where NULL
-- (already populated by history pass on incremental runs since PR #26).
UPDATE chunks
    SET introduced_at_version = (
        SELECT last_indexed_version FROM corpora WHERE corpora.id = chunks.corpus_id
    )
    WHERE introduced_at_version IS NULL;

UPDATE chunks
    SET last_modified_at_version = introduced_at_version
    WHERE last_modified_at_version IS NULL;
