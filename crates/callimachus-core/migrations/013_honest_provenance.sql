-- Migration 013: honest provenance — tagged-union version stamps, unified
-- history identity, Layer-2 cache, artifact tombstones, embeddings history.
--
-- FORWARD-ONLY (project convention). `rusqlite_migration` wraps this file in a
-- single transaction.
--
-- This is the *substrate* migration for the honest-provenance refactor. It adds
-- schema only — no observable behaviour changes in this PR. Subsequent PRs wire
-- the walker / cascade / passes through the new columns and tables.
--
-- Tagged-union encoding (the `Provenance` enum in
-- `crates/callimachus-core/src/types/provenance.rs`):
--
--     Concrete(sha)        -> (derived_at_kind = 'concrete',       derived_at_sha = sha)
--     RangePredating(sha)  -> (derived_at_kind = 'range_predating', derived_at_sha = sha)
--
-- The legacy `derived_at_version TEXT` column (migration 012) is RETAINED as a
-- deprecated mirror for one more migration. It is dropped in a later PR once all
-- query paths read `derived_at_sha`. During this transition the history-table
-- uniqueness indexes key off
--   COALESCE(NULLIF(derived_at_sha,''), <legacy version column>, '')
-- so that:
--   * rows written by *not-yet-migrated* code (which still only populate
--     `derived_at_version`) remain distinct per commit and idempotent on
--     re-insert, keeping the existing test-suite green, and
--   * rows written by the new history layer (which populate `derived_at_sha`
--     directly) key off the real provenance SHA.
-- The later "drop legacy column" migration replaces these with plain
-- column-only unique indexes on (..., derived_at_kind, derived_at_sha).

-- ─────────────────────────────────────────────────────────────────────────────
-- Part 1: tagged-union provenance columns on every head table
-- ─────────────────────────────────────────────────────────────────────────────

ALTER TABLE entities         ADD COLUMN derived_at_kind TEXT NOT NULL DEFAULT 'concrete'
    CHECK (derived_at_kind IN ('concrete','range_predating'));
ALTER TABLE entities         ADD COLUMN derived_at_sha  TEXT NOT NULL DEFAULT '';

ALTER TABLE edges            ADD COLUMN derived_at_kind TEXT NOT NULL DEFAULT 'concrete'
    CHECK (derived_at_kind IN ('concrete','range_predating'));
ALTER TABLE edges            ADD COLUMN derived_at_sha  TEXT NOT NULL DEFAULT '';

ALTER TABLE entity_purposes  ADD COLUMN derived_at_kind TEXT NOT NULL DEFAULT 'concrete'
    CHECK (derived_at_kind IN ('concrete','range_predating'));
ALTER TABLE entity_purposes  ADD COLUMN derived_at_sha  TEXT NOT NULL DEFAULT '';

ALTER TABLE entity_contracts ADD COLUMN derived_at_kind TEXT NOT NULL DEFAULT 'concrete'
    CHECK (derived_at_kind IN ('concrete','range_predating'));
ALTER TABLE entity_contracts ADD COLUMN derived_at_sha  TEXT NOT NULL DEFAULT '';

ALTER TABLE entity_blocks    ADD COLUMN derived_at_kind TEXT NOT NULL DEFAULT 'concrete'
    CHECK (derived_at_kind IN ('concrete','range_predating'));
ALTER TABLE entity_blocks    ADD COLUMN derived_at_sha  TEXT NOT NULL DEFAULT '';

ALTER TABLE summaries        ADD COLUMN derived_at_kind TEXT NOT NULL DEFAULT 'concrete'
    CHECK (derived_at_kind IN ('concrete','range_predating'));
ALTER TABLE summaries        ADD COLUMN derived_at_sha  TEXT NOT NULL DEFAULT '';

ALTER TABLE themes           ADD COLUMN derived_at_kind TEXT NOT NULL DEFAULT 'concrete'
    CHECK (derived_at_kind IN ('concrete','range_predating'));
ALTER TABLE themes           ADD COLUMN derived_at_sha  TEXT NOT NULL DEFAULT '';

-- chunks tracks `introduced_at_version` / `last_modified_at_version` rather than
-- `derived_at_version`; it still gains the tagged-union pair for uniformity.
ALTER TABLE chunks           ADD COLUMN derived_at_kind TEXT NOT NULL DEFAULT 'concrete'
    CHECK (derived_at_kind IN ('concrete','range_predating'));
ALTER TABLE chunks           ADD COLUMN derived_at_sha  TEXT NOT NULL DEFAULT '';

-- Replace the legacy single-column provenance indexes with composite ones.
DROP INDEX IF EXISTS idx_entities_derived;
DROP INDEX IF EXISTS idx_edges_derived;
DROP INDEX IF EXISTS idx_entity_purposes_derived;
DROP INDEX IF EXISTS idx_entity_contracts_derived;
DROP INDEX IF EXISTS idx_entity_blocks_derived;
DROP INDEX IF EXISTS idx_summaries_derived;
DROP INDEX IF EXISTS idx_themes_derived;

CREATE INDEX idx_entities_provenance         ON entities         (corpus_id, derived_at_sha, derived_at_kind);
CREATE INDEX idx_edges_provenance            ON edges            (corpus_id, derived_at_sha, derived_at_kind);
CREATE INDEX idx_entity_purposes_provenance  ON entity_purposes  (corpus_id, derived_at_sha, derived_at_kind);
CREATE INDEX idx_entity_contracts_provenance ON entity_contracts (corpus_id, derived_at_sha, derived_at_kind);
CREATE INDEX idx_entity_blocks_provenance    ON entity_blocks    (corpus_id, derived_at_sha, derived_at_kind);
CREATE INDEX idx_summaries_provenance        ON summaries        (corpus_id, derived_at_sha, derived_at_kind);
CREATE INDEX idx_themes_provenance           ON themes           (corpus_id, derived_at_sha, derived_at_kind);
CREATE INDEX idx_chunks_provenance           ON chunks           (corpus_id, derived_at_sha, derived_at_kind);

-- ─────────────────────────────────────────────────────────────────────────────
-- Part 2: content_hash on entities; file_shape_hash + entity_id_list on chunks
-- ─────────────────────────────────────────────────────────────────────────────
--
-- `content_hash` is a hash of the entity's canonical textual body (the substrate
-- Layer-2 passes see). It is NOT part of entity identity (which stays
-- (corpus_id, canonical_name, kind) via entities.id); it is a refinable
-- observation used as part of the Layer-2 cache key.
ALTER TABLE entities ADD COLUMN content_hash TEXT NOT NULL DEFAULT '';

-- `file_shape_hash = sha256(json_canonical([entity.id for entity in file sorted
-- by source offset]))` — the file-grain shape used as the purpose-pass cache key.
-- `entity_id_list` is the ordered JSON array that hash is computed from.
ALTER TABLE chunks ADD COLUMN file_shape_hash TEXT NOT NULL DEFAULT '';
ALTER TABLE chunks ADD COLUMN entity_id_list  TEXT NOT NULL DEFAULT '[]';

-- ─────────────────────────────────────────────────────────────────────────────
-- Part 3: unified history identity — tagged-union columns + uniqueness index
-- ─────────────────────────────────────────────────────────────────────────────
--
-- Each existing `*_history` table (migration 012) gains the tagged-union pair
-- plus a `superseded_at_sha` mirror of `superseded_at_version`. entities_history
-- additionally gains `content_hash`. A uniqueness index per table is the
-- storage-side fix for the middle-out duplicate-row bug: one writer can no
-- longer produce divergent rows for the same artifact identity at the same SHA.

ALTER TABLE entities_history          ADD COLUMN derived_at_kind  TEXT NOT NULL DEFAULT 'concrete'
    CHECK (derived_at_kind IN ('concrete','range_predating'));
ALTER TABLE entities_history          ADD COLUMN derived_at_sha   TEXT NOT NULL DEFAULT '';
ALTER TABLE entities_history          ADD COLUMN superseded_at_sha TEXT;
ALTER TABLE entities_history          ADD COLUMN content_hash     TEXT NOT NULL DEFAULT '';

ALTER TABLE edges_history             ADD COLUMN derived_at_kind  TEXT NOT NULL DEFAULT 'concrete'
    CHECK (derived_at_kind IN ('concrete','range_predating'));
ALTER TABLE edges_history             ADD COLUMN derived_at_sha   TEXT NOT NULL DEFAULT '';
ALTER TABLE edges_history             ADD COLUMN superseded_at_sha TEXT;

ALTER TABLE entity_purposes_history   ADD COLUMN derived_at_kind  TEXT NOT NULL DEFAULT 'concrete'
    CHECK (derived_at_kind IN ('concrete','range_predating'));
ALTER TABLE entity_purposes_history   ADD COLUMN derived_at_sha   TEXT NOT NULL DEFAULT '';
ALTER TABLE entity_purposes_history   ADD COLUMN superseded_at_sha TEXT;

ALTER TABLE entity_contracts_history  ADD COLUMN derived_at_kind  TEXT NOT NULL DEFAULT 'concrete'
    CHECK (derived_at_kind IN ('concrete','range_predating'));
ALTER TABLE entity_contracts_history  ADD COLUMN derived_at_sha   TEXT NOT NULL DEFAULT '';
ALTER TABLE entity_contracts_history  ADD COLUMN superseded_at_sha TEXT;

ALTER TABLE entity_blocks_history     ADD COLUMN derived_at_kind  TEXT NOT NULL DEFAULT 'concrete'
    CHECK (derived_at_kind IN ('concrete','range_predating'));
ALTER TABLE entity_blocks_history     ADD COLUMN derived_at_sha   TEXT NOT NULL DEFAULT '';
ALTER TABLE entity_blocks_history     ADD COLUMN superseded_at_sha TEXT;

ALTER TABLE summaries_history         ADD COLUMN derived_at_kind  TEXT NOT NULL DEFAULT 'concrete'
    CHECK (derived_at_kind IN ('concrete','range_predating'));
ALTER TABLE summaries_history         ADD COLUMN derived_at_sha   TEXT NOT NULL DEFAULT '';
ALTER TABLE summaries_history         ADD COLUMN superseded_at_sha TEXT;

ALTER TABLE themes_history            ADD COLUMN derived_at_kind  TEXT NOT NULL DEFAULT 'concrete'
    CHECK (derived_at_kind IN ('concrete','range_predating'));
ALTER TABLE themes_history            ADD COLUMN derived_at_sha   TEXT NOT NULL DEFAULT '';
ALTER TABLE themes_history            ADD COLUMN superseded_at_sha TEXT;

ALTER TABLE chunks_history            ADD COLUMN derived_at_kind  TEXT NOT NULL DEFAULT 'concrete'
    CHECK (derived_at_kind IN ('concrete','range_predating'));
ALTER TABLE chunks_history            ADD COLUMN derived_at_sha   TEXT NOT NULL DEFAULT '';
ALTER TABLE chunks_history            ADD COLUMN superseded_at_sha TEXT;

-- ─────────────────────────────────────────────────────────────────────────────
-- Part 4: embeddings provenance + a history mirror (closes the
--         embeddings-no-history-archival bug at the schema level)
-- ─────────────────────────────────────────────────────────────────────────────
--
-- `embeddings` already has a `model` column (migration 003), so only the
-- provenance + surrounding-context-hash columns are added here.
ALTER TABLE embeddings ADD COLUMN derived_at_kind          TEXT NOT NULL DEFAULT 'concrete'
    CHECK (derived_at_kind IN ('concrete','range_predating'));
ALTER TABLE embeddings ADD COLUMN derived_at_sha           TEXT NOT NULL DEFAULT '';
ALTER TABLE embeddings ADD COLUMN surrounding_context_hash TEXT NOT NULL DEFAULT '';

CREATE INDEX idx_embeddings_provenance ON embeddings (corpus_id, derived_at_sha, derived_at_kind);

-- embeddings_history is a fresh table (no legacy writers), so it uses the new
-- tagged-union columns directly and a plain column-only uniqueness index.
CREATE TABLE embeddings_history (
    history_id            INTEGER PRIMARY KEY,
    id                    TEXT NOT NULL,
    corpus_id             TEXT NOT NULL,
    chunk_id              TEXT NOT NULL,
    model                 TEXT NOT NULL,
    vector                BLOB NOT NULL,
    dimensions            INTEGER NOT NULL,
    surrounding_context_hash TEXT NOT NULL DEFAULT '',
    created_at            TEXT NOT NULL,
    derived_at_kind       TEXT NOT NULL DEFAULT 'concrete'
        CHECK (derived_at_kind IN ('concrete','range_predating')),
    derived_at_sha        TEXT NOT NULL DEFAULT '',
    superseded_at_sha     TEXT,
    superseded_at         TEXT NOT NULL
);
CREATE UNIQUE INDEX uq_embeddings_history_identity
    ON embeddings_history (corpus_id, chunk_id, model, derived_at_kind, derived_at_sha);
CREATE INDEX idx_embeddings_history_chunk  ON embeddings_history (corpus_id, chunk_id);
CREATE INDEX idx_embeddings_history_at_sha ON embeddings_history (corpus_id, derived_at_sha);

-- ─────────────────────────────────────────────────────────────────────────────
-- Part 5: Layer-2 cache (memoises Layer-2 derivations across SHAs)
-- ─────────────────────────────────────────────────────────────────────────────
--
-- No callers in this PR; the Layer-2 passes consult it in a later PR.
CREATE TABLE layer2_cache (
    cache_key         TEXT PRIMARY KEY,  -- sha256(artifact_kind||entity_id||content_hash||file_shape_hash||model||stable_sampling)
    artifact_kind     TEXT NOT NULL,     -- 'purpose' | 'contract' | 'summary' | 'embedding' | 'theme'
    entity_id         TEXT,              -- nullable for corpus-level artifacts (themes)
    content_hash      TEXT NOT NULL,
    file_shape_hash   TEXT NOT NULL,
    model             TEXT NOT NULL,
    stable_sampling   INTEGER NOT NULL DEFAULT 0,
    payload           TEXT NOT NULL,     -- JSON; pass-specific
    created_at        TEXT NOT NULL,
    first_seen_at_sha TEXT NOT NULL,     -- audit only
    hit_count         INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_layer2_cache_lookup
    ON layer2_cache (artifact_kind, entity_id, content_hash, file_shape_hash, model);

-- ─────────────────────────────────────────────────────────────────────────────
-- Part 6: artifact tombstones (write-once "this artifact stopped existing here")
-- ─────────────────────────────────────────────────────────────────────────────
CREATE TABLE artifact_tombstones (
    tombstone_id    INTEGER PRIMARY KEY,
    corpus_id       TEXT NOT NULL,
    artifact_kind   TEXT NOT NULL,       -- 'chunk' | 'entity' | 'edge' | 'embedding'
    artifact_id     TEXT NOT NULL,
    derived_at_kind TEXT NOT NULL
        CHECK (derived_at_kind IN ('concrete','range_predating')),
    derived_at_sha  TEXT NOT NULL,
    reason          TEXT,                -- 'removed' | 'renamed_to=<id>' | 'absent_in_substrate' | …
    created_at      TEXT NOT NULL
);
CREATE UNIQUE INDEX uq_artifact_tombstones_identity
    ON artifact_tombstones (corpus_id, artifact_kind, artifact_id, derived_at_kind, derived_at_sha);
CREATE INDEX idx_artifact_tombstones_lookup
    ON artifact_tombstones (corpus_id, artifact_kind, artifact_id);

-- ─────────────────────────────────────────────────────────────────────────────
-- Part 7: drop location_uri from edge natural identity
-- ─────────────────────────────────────────────────────────────────────────────
--
-- `edges.id` / `edges_history.id` are deterministic hashes of
-- (corpus_id, from_entity_id, to_entity_id, kind). `location_uri` stays as a
-- non-key column capturing the most recent observation. Migration 012 created
-- no index that uses `location_uri` as part of edge identity (the edges_history
-- indexes are on endpoints and superseded_at_version), so there is no
-- identity-bearing index to drop here — the change is realised by the new
-- uniqueness index below, which excludes location_uri. Documented for the
-- reviewer; no destructive statement required.

-- ─────────────────────────────────────────────────────────────────────────────
-- Part 8: backfill provenance columns for pre-existing rows
-- ─────────────────────────────────────────────────────────────────────────────
--
-- Existing rows are presumed concrete from prior runs. derived_at_kind already
-- defaults to 'concrete'; copy the legacy SHA into derived_at_sha. This is only
-- honest if the pinakes was built without copy-forward; the recommended path for
-- a real pinakes is `calli history migrate-fresh` (a later PR). The SQL fallback
-- exists for test fixtures.

UPDATE entities         SET derived_at_sha = COALESCE(derived_at_version, '') WHERE derived_at_sha = '';
UPDATE edges            SET derived_at_sha = COALESCE(derived_at_version, '') WHERE derived_at_sha = '';
UPDATE entity_purposes  SET derived_at_sha = COALESCE(derived_at_version, '') WHERE derived_at_sha = '';
UPDATE entity_contracts SET derived_at_sha = COALESCE(derived_at_version, '') WHERE derived_at_sha = '';
UPDATE entity_blocks    SET derived_at_sha = COALESCE(derived_at_version, '') WHERE derived_at_sha = '';
UPDATE summaries        SET derived_at_sha = COALESCE(derived_at_version, '') WHERE derived_at_sha = '';
UPDATE themes           SET derived_at_sha = COALESCE(derived_at_version, '') WHERE derived_at_sha = '';
UPDATE chunks           SET derived_at_sha = COALESCE(last_modified_at_version, introduced_at_version, '') WHERE derived_at_sha = '';

UPDATE entities_history         SET derived_at_sha = COALESCE(derived_at_version, ''), superseded_at_sha = superseded_at_version WHERE derived_at_sha = '';
UPDATE edges_history            SET derived_at_sha = COALESCE(derived_at_version, ''), superseded_at_sha = superseded_at_version WHERE derived_at_sha = '';
UPDATE entity_purposes_history  SET derived_at_sha = COALESCE(derived_at_version, ''), superseded_at_sha = superseded_at_version WHERE derived_at_sha = '';
UPDATE entity_contracts_history SET derived_at_sha = COALESCE(derived_at_version, ''), superseded_at_sha = superseded_at_version WHERE derived_at_sha = '';
UPDATE entity_blocks_history    SET derived_at_sha = COALESCE(derived_at_version, ''), superseded_at_sha = superseded_at_version WHERE derived_at_sha = '';
UPDATE summaries_history        SET derived_at_sha = COALESCE(derived_at_version, ''), superseded_at_sha = superseded_at_version WHERE derived_at_sha = '';
UPDATE themes_history           SET derived_at_sha = COALESCE(derived_at_version, ''), superseded_at_sha = superseded_at_version WHERE derived_at_sha = '';
UPDATE chunks_history           SET derived_at_sha = COALESCE(last_modified_at_version, introduced_at_version, ''), superseded_at_sha = superseded_at_version WHERE derived_at_sha = '';

-- ─────────────────────────────────────────────────────────────────────────────
-- Part 3 (cont.): history uniqueness indexes
-- ─────────────────────────────────────────────────────────────────────────────
--
-- Created AFTER the Part 8 backfill so the indexed expression reflects populated
-- SHAs. The COALESCE(NULLIF(derived_at_sha,''), <legacy>, '') expression keeps
-- rows written by not-yet-migrated code (derived_at_sha = '') distinct per
-- commit via the legacy version column. derived_at_kind participates so a
-- Concrete row and a RangePredating row for the same identity+sha can coexist.

CREATE UNIQUE INDEX uq_entities_history_identity
    ON entities_history (corpus_id, id, derived_at_kind, COALESCE(NULLIF(derived_at_sha,''), derived_at_version, ''));
CREATE INDEX idx_entities_history_at_sha ON entities_history (corpus_id, derived_at_sha);

CREATE UNIQUE INDEX uq_edges_history_identity
    ON edges_history (corpus_id, id, derived_at_kind, COALESCE(NULLIF(derived_at_sha,''), derived_at_version, ''));
CREATE INDEX idx_edges_history_at_sha ON edges_history (corpus_id, derived_at_sha);

CREATE UNIQUE INDEX uq_entity_purposes_history_identity
    ON entity_purposes_history (corpus_id, entity_id, model, derived_at_kind, COALESCE(NULLIF(derived_at_sha,''), derived_at_version, ''));
CREATE INDEX idx_entity_purposes_history_at_sha ON entity_purposes_history (corpus_id, derived_at_sha);

CREATE UNIQUE INDEX uq_entity_contracts_history_identity
    ON entity_contracts_history (corpus_id, entity_id, model, derived_at_kind, COALESCE(NULLIF(derived_at_sha,''), derived_at_version, ''));
CREATE INDEX idx_entity_contracts_history_at_sha ON entity_contracts_history (corpus_id, derived_at_sha);

CREATE UNIQUE INDEX uq_entity_blocks_history_identity
    ON entity_blocks_history (corpus_id, id, derived_at_kind, COALESCE(NULLIF(derived_at_sha,''), derived_at_version, ''));
CREATE INDEX idx_entity_blocks_history_at_sha ON entity_blocks_history (corpus_id, derived_at_sha);

CREATE UNIQUE INDEX uq_summaries_history_identity
    ON summaries_history (corpus_id, id, derived_at_kind, COALESCE(NULLIF(derived_at_sha,''), derived_at_version, ''));
CREATE INDEX idx_summaries_history_at_sha ON summaries_history (corpus_id, derived_at_sha);

CREATE UNIQUE INDEX uq_themes_history_identity
    ON themes_history (corpus_id, id, derived_at_kind, COALESCE(NULLIF(derived_at_sha,''), derived_at_version, ''));
CREATE INDEX idx_themes_history_at_sha ON themes_history (corpus_id, derived_at_sha);

CREATE UNIQUE INDEX uq_chunks_history_identity
    ON chunks_history (corpus_id, id, derived_at_kind, COALESCE(NULLIF(derived_at_sha,''), last_modified_at_version, introduced_at_version, ''));
CREATE INDEX idx_chunks_history_at_sha ON chunks_history (corpus_id, derived_at_sha);
