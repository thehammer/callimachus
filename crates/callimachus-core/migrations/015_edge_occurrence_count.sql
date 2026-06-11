-- Migration 015: edge occurrence counts.
--
-- Adds `occurrence_count` to `edges` and `edges_history` so that multiple
-- source sites producing the same logical edge (same from/kind/to/scope) are
-- represented as a single row whose count reflects the number of sites.
--
-- FORWARD-ONLY (project convention). `rusqlite_migration` wraps this file in
-- a single transaction.
--
-- Pre-existing rows keep `occurrence_count = 1`.  Duplicate rows created
-- before this migration (when edge ids were random UUIDs) are NOT
-- retroactively collapsed here — they collapse naturally on the next reindex
-- of each file via the cascade-delete + deterministic-id + overwrite-upsert
-- cycle introduced alongside this migration.

ALTER TABLE edges         ADD COLUMN occurrence_count INTEGER NOT NULL DEFAULT 1;
ALTER TABLE edges_history ADD COLUMN occurrence_count INTEGER NOT NULL DEFAULT 1;
