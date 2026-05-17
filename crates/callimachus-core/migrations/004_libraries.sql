CREATE TABLE libraries (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE library_corpora (
    library_id TEXT NOT NULL,
    corpus_id TEXT NOT NULL,
    added_at TEXT NOT NULL,
    PRIMARY KEY (library_id, corpus_id),
    FOREIGN KEY (library_id) REFERENCES libraries(id) ON DELETE CASCADE,
    FOREIGN KEY (corpus_id) REFERENCES corpora(id) ON DELETE CASCADE
);

CREATE INDEX idx_library_corpora_library ON library_corpora(library_id);
CREATE INDEX idx_library_corpora_corpus ON library_corpora(corpus_id);
