-- Migration 014: per-corpus backfill cursor.
--
-- FORWARD-ONLY (project convention). Ships with honest-provenance PR 2.
--
-- The backward history walk (`calli history backfill`) processes commits
-- newest-older → oldest-older. Before this column, a resumed backfill had to
-- *infer* where to continue from the on-disk `*_history` state — which is
-- ambiguous, and the source of the `history-backfill-resume-stuck-on-partial-shas`
-- bug. The cursor records the next SHA to process after each iteration completes,
-- so resume is unambiguous: read the cursor, skip everything newer.
--
-- `backfill_cursor` holds a version reference (`git:<oid>`), or NULL when no
-- backfill is in progress (fresh corpus, or a completed backfill that reached
-- the requested root).

ALTER TABLE corpora ADD COLUMN backfill_cursor TEXT;
