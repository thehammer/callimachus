-- Migration 005: Replace Phase 10's library scaffolding with the recursive Collection model.

-- ── Collections ────────────────────────────────────────────────────────────

CREATE TABLE collections (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    kind        TEXT NOT NULL,        -- 'series' | 'catalog' | 'domain' | 'workspace' | free-form
    created_at  TEXT NOT NULL         -- ISO 8601
);

CREATE TABLE collection_members (
    collection_id TEXT NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    member_id     TEXT NOT NULL,
    member_type   TEXT NOT NULL CHECK (member_type IN ('corpus', 'collection')),
    added_at      TEXT NOT NULL,
    PRIMARY KEY (collection_id, member_id, member_type)
);

CREATE INDEX collection_members_member_idx
    ON collection_members (member_id, member_type);

-- ── Rebuild corrections to allow nullable corpus_id ─────────────────────────
-- The original schema declared `corpus_id TEXT NOT NULL`. Collection-scoped
-- corrections (EntityLink) have no corpus_id; they reference a collection
-- instead. SQLite does not support DROP CONSTRAINT, so we rebuild the table.

CREATE TABLE corrections_new (
    id            TEXT PRIMARY KEY,
    corpus_id     TEXT REFERENCES corpora(id) ON DELETE CASCADE,
    collection_id TEXT REFERENCES collections(id) ON DELETE CASCADE,
    kind          TEXT NOT NULL,
    payload       TEXT NOT NULL,
    applied_at    TEXT NOT NULL,
    CHECK ((corpus_id IS NOT NULL) != (collection_id IS NOT NULL))
);

INSERT INTO corrections_new (id, corpus_id, collection_id, kind, payload, applied_at)
SELECT id, corpus_id, NULL, kind, payload, applied_at FROM corrections;

DROP INDEX IF EXISTS idx_corrections_corpus;
DROP TABLE corrections;
ALTER TABLE corrections_new RENAME TO corrections;

CREATE INDEX idx_corrections_corpus     ON corrections (corpus_id);
CREATE INDEX idx_corrections_collection ON corrections (collection_id);

-- ── Drop Phase 10's library scaffolding ────────────────────────────────────

DROP TABLE IF EXISTS library_corpora;
DROP TABLE IF EXISTS libraries;
