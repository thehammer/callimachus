-- Migration 017: line spans on code entities and chunks.
--
-- Adds `start_line` / `end_line` (nullable INTEGER, 0-based) to the `entities`
-- and `chunks` head tables and their `*_history` mirrors so that code search
-- results carry exact file regions for GitHub deep-links.
--
-- Design decision (recorded for the PR):
--   * Spans live on BOTH entities and chunks.
--   * Entity spans record the defining location of each symbol (function, class,
--     method) within its source file — used by the /entity endpoint.
--   * Chunk spans record the file region covered by each code chunk — used by
--     /search results so callers can compose `#LN-LM` GitHub anchors directly
--     without a second round-trip.
--   * Book / wiki adapters never populate these columns; their rows stay NULL.
--   * Lines are 0-based (matching tree-sitter row numbers) so the web layer can
--     use `start_line + 1` for 1-based GitHub anchors.
--
-- FORWARD-ONLY (project convention). rusqlite_migration wraps this in a
-- single transaction.

-- ─────────────────────────────────────────────────────────────────────────────
-- Head tables
-- ─────────────────────────────────────────────────────────────────────────────

ALTER TABLE entities ADD COLUMN start_line INTEGER;
ALTER TABLE entities ADD COLUMN end_line   INTEGER;

ALTER TABLE chunks   ADD COLUMN start_line INTEGER;
ALTER TABLE chunks   ADD COLUMN end_line   INTEGER;

-- ─────────────────────────────────────────────────────────────────────────────
-- History mirror tables (must mirror every head column)
-- ─────────────────────────────────────────────────────────────────────────────

ALTER TABLE entities_history ADD COLUMN start_line INTEGER;
ALTER TABLE entities_history ADD COLUMN end_line   INTEGER;

ALTER TABLE chunks_history   ADD COLUMN start_line INTEGER;
ALTER TABLE chunks_history   ADD COLUMN end_line   INTEGER;
