CREATE TABLE IF NOT EXISTS embeddings (
    id TEXT PRIMARY KEY,
    corpus_id TEXT NOT NULL,
    chunk_id TEXT NOT NULL,
    model TEXT NOT NULL,
    vector BLOB NOT NULL,        -- raw f32 bytes, little-endian
    dimensions INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    FOREIGN KEY (corpus_id) REFERENCES corpora(id) ON DELETE CASCADE,
    FOREIGN KEY (chunk_id) REFERENCES chunks(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_embeddings_corpus ON embeddings(corpus_id);
CREATE INDEX IF NOT EXISTS idx_embeddings_chunk ON embeddings(chunk_id);
