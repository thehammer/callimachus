-- Migration 019: detected source language on chunks.
--
-- Adds `language` (nullable TEXT) to the `chunks` head table and its
-- `chunks_history` mirror.  The code adapter stamps the language label it
-- detected (extension, shebang, well-known filename; e.g. `bash`, `python`,
-- `make`) on every chunk it emits, so downstream passes that only see a chunk
-- URI or body (structure, summarize, purpose, contract) resolve the same
-- language for extensionless files such as `bin/tool` without re-reading the
-- file.
--
-- No backfill: NULL means "detect from the location path", which is exactly the
-- behaviour of rows written before this migration (they all have extensions).
-- Book / wiki adapters never populate the column.
--
-- FORWARD-ONLY (project convention). rusqlite_migration wraps this in a
-- single transaction.

ALTER TABLE chunks         ADD COLUMN language TEXT;
ALTER TABLE chunks_history ADD COLUMN language TEXT;
