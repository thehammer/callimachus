-- Phase 12: Semantic Enrichment — purposes, contracts, blocks, themes.

-- Per-entity purpose (why it exists, separate from behavioral summary).
CREATE TABLE entity_purposes (
    entity_id     TEXT PRIMARY KEY REFERENCES entities(id) ON DELETE CASCADE,
    corpus_id     TEXT NOT NULL REFERENCES corpora(id) ON DELETE CASCADE,
    purpose       TEXT NOT NULL,
    model         TEXT,
    generated_at  TEXT NOT NULL
);
CREATE INDEX idx_entity_purposes_corpus ON entity_purposes (corpus_id);

-- Block-level blurbs for complex functions.
CREATE TABLE entity_blocks (
    id          TEXT PRIMARY KEY,
    entity_id   TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    corpus_id   TEXT NOT NULL,
    label       TEXT NOT NULL,
    description TEXT NOT NULL,
    position    INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_entity_blocks_entity ON entity_blocks (entity_id);

-- Per-entity contract signals + LLM semantic analysis.
CREATE TABLE entity_contracts (
    entity_id        TEXT PRIMARY KEY REFERENCES entities(id) ON DELETE CASCADE,
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
    model            TEXT,
    generated_at     TEXT NOT NULL
);
CREATE INDEX idx_entity_contracts_corpus ON entity_contracts (corpus_id);
CREATE INDEX idx_entity_contracts_panic  ON entity_contracts (corpus_id, has_panic_risk);
CREATE INDEX idx_entity_contracts_public ON entity_contracts (corpus_id, is_public);

-- Corpus-level architectural invariants.
CREATE TABLE themes (
    id           TEXT PRIMARY KEY,
    corpus_id    TEXT NOT NULL REFERENCES corpora(id) ON DELETE CASCADE,
    title        TEXT NOT NULL,
    statement    TEXT NOT NULL,
    confidence   REAL NOT NULL DEFAULT 0.5,
    model        TEXT,
    generated_at TEXT NOT NULL
);
CREATE INDEX idx_themes_corpus ON themes (corpus_id);
