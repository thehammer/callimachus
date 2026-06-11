-- Migration 016: edge origin scope (production vs test provenance).
--
-- Adds `origin_scope` to `edges` and `edges_history` so that edges derived
-- from test-only code (`#[cfg(test)] mod` / `#[test]` functions in Rust) are
-- distinguishable from production edges at the storage level.
--
-- FORWARD-ONLY (project convention). `rusqlite_migration` wraps this file in
-- a single transaction.
--
-- Allowed values: 'production' | 'test'
-- Pre-existing rows default to 'production'.  Rows re-derive correct scope on
-- the next reindex of each file.

ALTER TABLE edges         ADD COLUMN origin_scope TEXT NOT NULL DEFAULT 'production';
ALTER TABLE edges_history ADD COLUMN origin_scope TEXT NOT NULL DEFAULT 'production';
