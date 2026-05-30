-- Migration 015: drop the legacy `derived_at_version` and `superseded_at_version`
-- columns now that all query paths read `derived_at_sha`.
--
-- FORWARD-ONLY (project convention). `rusqlite_migration` wraps this file in a
-- single transaction.
--
-- Continuation of the honest-provenance refactor (PRs #36–#41). Migration 013
-- retained `derived_at_version TEXT` as a deprecated mirror while the write path
-- was being rewired. This migration completes the cleanup:
--
--   1. Drop the COALESCE-based uniqueness indexes from 013 (SQLite refuses to
--      drop a column referenced by an index expression).
--   2. Drop `derived_at_version` from all head tables (entities, edges,
--      entity_purposes, entity_contracts, entity_blocks, summaries, themes).
--      NOTE: `chunks` tracks `introduced_at_version` / `last_modified_at_version`,
--      not `derived_at_version` — those are NOT dropped here.
--   3. Drop `derived_at_version` from all history tables (includes chunks_history).
--   4. Drop `superseded_at_version` from all history tables (NOT embeddings_history
--      — it was created fresh in 013 with only `superseded_at_sha`).
--   5. Recreate uniqueness indexes as plain column-only indexes on
--      (..., derived_at_kind, derived_at_sha).

-- ─────────────────────────────────────────────────────────────────────────────
-- Part 1: drop all indexes referencing derived_at_version or superseded_at_version
-- (SQLite cannot drop a column used in an index expression)
-- ─────────────────────────────────────────────────────────────────────────────

-- COALESCE-based uniqueness indexes from migration 013:
DROP INDEX IF EXISTS uq_entities_history_identity;
DROP INDEX IF EXISTS uq_edges_history_identity;
DROP INDEX IF EXISTS uq_entity_purposes_history_identity;
DROP INDEX IF EXISTS uq_entity_contracts_history_identity;
DROP INDEX IF EXISTS uq_entity_blocks_history_identity;
DROP INDEX IF EXISTS uq_summaries_history_identity;
DROP INDEX IF EXISTS uq_themes_history_identity;
DROP INDEX IF EXISTS uq_chunks_history_identity;

-- Plain superseded_at_version lookup indexes from migration 012:
DROP INDEX IF EXISTS idx_entities_history_superseded;
DROP INDEX IF EXISTS idx_edges_history_superseded;
DROP INDEX IF EXISTS idx_entity_purposes_history_superseded;
DROP INDEX IF EXISTS idx_entity_contracts_history_superseded;
DROP INDEX IF EXISTS idx_entity_blocks_history_superseded;
DROP INDEX IF EXISTS idx_summaries_history_superseded;
DROP INDEX IF EXISTS idx_chunks_history_superseded;
DROP INDEX IF EXISTS idx_themes_history_superseded;

-- ─────────────────────────────────────────────────────────────────────────────
-- Part 2: drop `derived_at_version` from head tables
-- (chunks is excluded — it never had this column)
-- ─────────────────────────────────────────────────────────────────────────────

ALTER TABLE entities         DROP COLUMN derived_at_version;
ALTER TABLE edges            DROP COLUMN derived_at_version;
ALTER TABLE entity_purposes  DROP COLUMN derived_at_version;
ALTER TABLE entity_contracts DROP COLUMN derived_at_version;
ALTER TABLE entity_blocks    DROP COLUMN derived_at_version;
ALTER TABLE summaries        DROP COLUMN derived_at_version;
ALTER TABLE themes           DROP COLUMN derived_at_version;

-- ─────────────────────────────────────────────────────────────────────────────
-- Part 3: drop `derived_at_version` from history tables
-- (chunks_history is EXCLUDED — it was created in 012 without derived_at_version;
--  it has introduced_at_version and last_modified_at_version instead)
-- ─────────────────────────────────────────────────────────────────────────────

-- Note: chunks_history was created in migration 012 with only
-- `introduced_at_version` and `last_modified_at_version` — no `derived_at_version`.
-- It therefore does NOT appear in this list.
ALTER TABLE entities_history         DROP COLUMN derived_at_version;
ALTER TABLE edges_history            DROP COLUMN derived_at_version;
ALTER TABLE entity_purposes_history  DROP COLUMN derived_at_version;
ALTER TABLE entity_contracts_history DROP COLUMN derived_at_version;
ALTER TABLE entity_blocks_history    DROP COLUMN derived_at_version;
ALTER TABLE summaries_history        DROP COLUMN derived_at_version;
ALTER TABLE themes_history           DROP COLUMN derived_at_version;

-- ─────────────────────────────────────────────────────────────────────────────
-- Part 4: drop `superseded_at_version` from history tables
-- (embeddings_history was created in 013 without this column — skip it)
-- ─────────────────────────────────────────────────────────────────────────────

ALTER TABLE entities_history         DROP COLUMN superseded_at_version;
ALTER TABLE edges_history            DROP COLUMN superseded_at_version;
ALTER TABLE entity_purposes_history  DROP COLUMN superseded_at_version;
ALTER TABLE entity_contracts_history DROP COLUMN superseded_at_version;
ALTER TABLE entity_blocks_history    DROP COLUMN superseded_at_version;
ALTER TABLE summaries_history        DROP COLUMN superseded_at_version;
ALTER TABLE themes_history           DROP COLUMN superseded_at_version;
ALTER TABLE chunks_history           DROP COLUMN superseded_at_version;

-- ─────────────────────────────────────────────────────────────────────────────
-- Part 5: recreate uniqueness indexes as plain column-only indexes
-- ─────────────────────────────────────────────────────────────────────────────

-- Most history tables: key on (corpus_id, id, derived_at_kind, derived_at_sha)
CREATE UNIQUE INDEX uq_entities_history_identity
    ON entities_history (corpus_id, id, derived_at_kind, derived_at_sha);
CREATE UNIQUE INDEX uq_edges_history_identity
    ON edges_history (corpus_id, id, derived_at_kind, derived_at_sha);
CREATE UNIQUE INDEX uq_entity_blocks_history_identity
    ON entity_blocks_history (corpus_id, id, derived_at_kind, derived_at_sha);
CREATE UNIQUE INDEX uq_summaries_history_identity
    ON summaries_history (corpus_id, id, derived_at_kind, derived_at_sha);
CREATE UNIQUE INDEX uq_themes_history_identity
    ON themes_history (corpus_id, id, derived_at_kind, derived_at_sha);
CREATE UNIQUE INDEX uq_chunks_history_identity
    ON chunks_history (corpus_id, id, derived_at_kind, derived_at_sha);

-- Purposes and contracts key on (corpus_id, entity_id, model, derived_at_kind, derived_at_sha)
CREATE UNIQUE INDEX uq_entity_purposes_history_identity
    ON entity_purposes_history (corpus_id, entity_id, model, derived_at_kind, derived_at_sha);
CREATE UNIQUE INDEX uq_entity_contracts_history_identity
    ON entity_contracts_history (corpus_id, entity_id, model, derived_at_kind, derived_at_sha);

-- Recreate the supersession lookup indexes on superseded_at_sha (replacing the
-- migration-012 versions that keyed off superseded_at_version):
CREATE INDEX idx_entities_history_superseded          ON entities_history         (corpus_id, superseded_at_sha);
CREATE INDEX idx_edges_history_superseded             ON edges_history            (corpus_id, superseded_at_sha);
CREATE INDEX idx_entity_purposes_history_superseded   ON entity_purposes_history  (corpus_id, superseded_at_sha);
CREATE INDEX idx_entity_contracts_history_superseded  ON entity_contracts_history (corpus_id, superseded_at_sha);
CREATE INDEX idx_entity_blocks_history_superseded     ON entity_blocks_history    (corpus_id, superseded_at_sha);
CREATE INDEX idx_summaries_history_superseded         ON summaries_history        (corpus_id, superseded_at_sha);
CREATE INDEX idx_chunks_history_superseded            ON chunks_history           (corpus_id, superseded_at_sha);
CREATE INDEX idx_themes_history_superseded            ON themes_history           (corpus_id, superseded_at_sha);
