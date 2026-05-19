-- Migration 010: Multi-model artifact storage
--
-- Widens entity_purposes, entity_contracts, and summaries from
-- "one artifact per entity" to "one artifact per (entity, model)".
-- Adds model_tier to all four artifact tables so the read path can
-- serve the best available artifact without callers specifying a model.
--
-- IMPORTANT: Disabling foreign keys is required while we DROP and recreate
-- tables that have FK references. rusqlite_migration wraps each migration
-- in a transaction, but FK enforcement is session-level, so we manage it here.

PRAGMA foreign_keys = OFF;

-- ── entity_purposes ──────────────────────────────────────────────────────────
-- Old PK: entity_id only. New PK: (entity_id, model).

CREATE TABLE entity_purposes_new (
    entity_id     TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    corpus_id     TEXT NOT NULL REFERENCES corpora(id) ON DELETE CASCADE,
    purpose       TEXT NOT NULL,
    model         TEXT NOT NULL,
    model_tier    TEXT NOT NULL DEFAULT 'unknown',
    generated_at  TEXT NOT NULL,
    PRIMARY KEY (entity_id, model)
);

INSERT INTO entity_purposes_new (entity_id, corpus_id, purpose, model, model_tier, generated_at)
    SELECT entity_id, corpus_id, purpose, COALESCE(model, 'unknown'), 'unknown', generated_at
    FROM entity_purposes;

DROP TABLE entity_purposes;
ALTER TABLE entity_purposes_new RENAME TO entity_purposes;

CREATE INDEX idx_entity_purposes_corpus ON entity_purposes (corpus_id);
CREATE INDEX idx_entity_purposes_tier   ON entity_purposes (entity_id, model_tier);

-- ── entity_contracts ─────────────────────────────────────────────────────────
-- Old PK: entity_id only. New PK: (entity_id, model).

CREATE TABLE entity_contracts_new (
    entity_id        TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    corpus_id        TEXT NOT NULL REFERENCES corpora(id) ON DELETE CASCADE,
    is_public        INTEGER NOT NULL DEFAULT 0,
    is_must_use      INTEGER NOT NULL DEFAULT 0,
    is_deprecated    INTEGER NOT NULL DEFAULT 0,
    is_fallible      INTEGER NOT NULL DEFAULT 0,
    is_nullable      INTEGER NOT NULL DEFAULT 0,
    is_mutating      INTEGER NOT NULL DEFAULT 0,
    is_diverging     INTEGER NOT NULL DEFAULT 0,
    has_panic_risk   INTEGER NOT NULL DEFAULT 0,
    has_unsafe       INTEGER NOT NULL DEFAULT 0,
    is_incomplete    INTEGER NOT NULL DEFAULT 0,
    panic_call_count INTEGER NOT NULL DEFAULT 0,
    debt_markers     TEXT NOT NULL DEFAULT '[]',
    assumptions      TEXT NOT NULL DEFAULT '[]',
    risks            TEXT NOT NULL DEFAULT '[]',
    intent_gap       TEXT,
    caller_notes     TEXT,
    model            TEXT NOT NULL,
    model_tier       TEXT NOT NULL DEFAULT 'unknown',
    generated_at     TEXT NOT NULL,
    PRIMARY KEY (entity_id, model)
);

INSERT INTO entity_contracts_new SELECT
    entity_id, corpus_id, is_public, is_must_use, is_deprecated, is_fallible,
    is_nullable, is_mutating, is_diverging, has_panic_risk, has_unsafe,
    is_incomplete, panic_call_count, debt_markers, assumptions, risks,
    intent_gap, caller_notes, COALESCE(model, 'unknown'), 'unknown', generated_at
    FROM entity_contracts;

DROP TABLE entity_contracts;
ALTER TABLE entity_contracts_new RENAME TO entity_contracts;

CREATE INDEX idx_entity_contracts_corpus ON entity_contracts (corpus_id);
CREATE INDEX idx_entity_contracts_panic  ON entity_contracts (corpus_id, has_panic_risk);
CREATE INDEX idx_entity_contracts_public ON entity_contracts (corpus_id, is_public);
CREATE INDEX idx_entity_contracts_tier   ON entity_contracts (entity_id, model_tier);

-- ── summaries ─────────────────────────────────────────────────────────────────
-- Keep UUID PK; add UNIQUE(corpus_id, target_kind, target_id, model) + model_tier.
-- Backfill NULL model before rebuild.

UPDATE summaries SET model = 'unknown' WHERE model IS NULL;

CREATE TABLE summaries_new (
    id          TEXT PRIMARY KEY,
    corpus_id   TEXT NOT NULL,
    target_kind TEXT NOT NULL,
    target_id   TEXT NOT NULL,
    depth       TEXT NOT NULL,
    text        TEXT NOT NULL,
    model       TEXT NOT NULL,
    model_tier  TEXT NOT NULL DEFAULT 'unknown',
    generated_at TEXT NOT NULL,
    FOREIGN KEY (corpus_id) REFERENCES corpora(id) ON DELETE CASCADE,
    UNIQUE (corpus_id, target_kind, target_id, model)
);

-- If duplicates exist (same target+model), keep the latest generated_at row.
INSERT INTO summaries_new
    SELECT id, corpus_id, target_kind, target_id, depth, text, model, 'unknown', generated_at
    FROM summaries s1
    WHERE generated_at = (
        SELECT MAX(generated_at) FROM summaries s2
        WHERE s2.corpus_id   = s1.corpus_id
          AND s2.target_kind = s1.target_kind
          AND s2.target_id   = s1.target_id
          AND s2.model       = s1.model
    );

DROP TABLE summaries;
ALTER TABLE summaries_new RENAME TO summaries;

CREATE INDEX idx_summaries_corpus ON summaries (corpus_id);
CREATE INDEX idx_summaries_target ON summaries (corpus_id, target_kind, target_id);

-- ── themes ───────────────────────────────────────────────────────────────────
-- No PK change needed (themes already use UUID PK; multiple per corpus is fine).
-- Just add model_tier and backfill NULL model.

ALTER TABLE themes ADD COLUMN model_tier TEXT NOT NULL DEFAULT 'unknown';
UPDATE themes SET model = 'unknown' WHERE model IS NULL;

PRAGMA foreign_keys = ON;
