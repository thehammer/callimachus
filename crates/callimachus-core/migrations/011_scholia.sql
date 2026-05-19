-- Migration 009: Rename `corrections` table to `scholia`.
--
-- "Scholia" (singular: scholion) is the Greek scholarly term for marginal
-- annotations — an exact fit for the post-indexing corrections model.
-- The Rust types (Correction, CorrectionKind) and StorageBackend trait methods
-- (correction_insert, correction_list, …) are unchanged; only the storage
-- layer SQL is updated.

ALTER TABLE corrections RENAME TO scholia;
DROP INDEX IF EXISTS idx_corrections_corpus;
DROP INDEX IF EXISTS idx_corrections_collection;
CREATE INDEX IF NOT EXISTS idx_scholia_corpus ON scholia(corpus_id);
CREATE INDEX IF NOT EXISTS idx_scholia_collection ON scholia(collection_id);
