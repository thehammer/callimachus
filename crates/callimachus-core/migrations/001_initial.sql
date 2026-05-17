CREATE TABLE IF NOT EXISTS corpora (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    source TEXT NOT NULL,
    config TEXT NOT NULL DEFAULT '{}',
    status TEXT NOT NULL DEFAULT 'registered',
    created_at TEXT NOT NULL,
    last_indexed_at TEXT
);

CREATE TABLE IF NOT EXISTS chunks (
    id TEXT PRIMARY KEY,
    corpus_id TEXT NOT NULL,
    parent_path TEXT,
    kind TEXT NOT NULL,
    location_uri TEXT NOT NULL,
    content TEXT NOT NULL,
    byte_length INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    semantic_processed INTEGER NOT NULL DEFAULT 0,
    FOREIGN KEY (corpus_id) REFERENCES corpora(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_chunks_corpus ON chunks (corpus_id);
CREATE INDEX IF NOT EXISTS idx_chunks_parent ON chunks (corpus_id, parent_path);

CREATE TABLE IF NOT EXISTS entities (
    id TEXT PRIMARY KEY,
    corpus_id TEXT NOT NULL,
    canonical_name TEXT NOT NULL,
    kind TEXT NOT NULL,
    aliases TEXT NOT NULL DEFAULT '[]',
    description TEXT,
    first_location_uri TEXT,
    last_location_uri TEXT,
    appearance_count INTEGER NOT NULL DEFAULT 0,
    confidence REAL NOT NULL DEFAULT 0.5,
    FOREIGN KEY (corpus_id) REFERENCES corpora(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_entities_corpus ON entities (corpus_id);
CREATE INDEX IF NOT EXISTS idx_entities_name ON entities (corpus_id, canonical_name);

CREATE TABLE IF NOT EXISTS edges (
    id TEXT PRIMARY KEY,
    corpus_id TEXT NOT NULL,
    from_entity_id TEXT NOT NULL,
    to_entity_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    location_uri TEXT NOT NULL,
    confidence REAL NOT NULL DEFAULT 0.5,
    FOREIGN KEY (corpus_id) REFERENCES corpora(id) ON DELETE CASCADE,
    FOREIGN KEY (from_entity_id) REFERENCES entities(id) ON DELETE CASCADE,
    FOREIGN KEY (to_entity_id) REFERENCES entities(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_edges_corpus ON edges (corpus_id);
CREATE INDEX IF NOT EXISTS idx_edges_from ON edges (from_entity_id);
CREATE INDEX IF NOT EXISTS idx_edges_to ON edges (to_entity_id);

CREATE TABLE IF NOT EXISTS summaries (
    id TEXT PRIMARY KEY,
    corpus_id TEXT NOT NULL,
    target_kind TEXT NOT NULL,
    target_id TEXT NOT NULL,
    depth TEXT NOT NULL,
    text TEXT NOT NULL,
    model TEXT,
    generated_at TEXT NOT NULL,
    FOREIGN KEY (corpus_id) REFERENCES corpora(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_summaries_corpus ON summaries (corpus_id);
CREATE INDEX IF NOT EXISTS idx_summaries_target ON summaries (corpus_id, target_kind, target_id);

CREATE TABLE IF NOT EXISTS corrections (
    id TEXT PRIMARY KEY,
    corpus_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    payload TEXT NOT NULL,
    applied_at TEXT NOT NULL,
    FOREIGN KEY (corpus_id) REFERENCES corpora(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_corrections_corpus ON corrections (corpus_id);

CREATE TABLE IF NOT EXISTS runs (
    id TEXT PRIMARY KEY,
    corpus_id TEXT NOT NULL,
    pass TEXT NOT NULL,
    started_at TEXT NOT NULL,
    finished_at TEXT,
    status TEXT NOT NULL DEFAULT 'running',
    stats TEXT NOT NULL DEFAULT '{}',
    provider TEXT,
    FOREIGN KEY (corpus_id) REFERENCES corpora(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_runs_corpus ON runs (corpus_id);
CREATE INDEX IF NOT EXISTS idx_runs_corpus_pass ON runs (corpus_id, pass);
