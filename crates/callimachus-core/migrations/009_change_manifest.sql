-- Stage 0: history-aware change detection columns.
--
-- corpora.last_indexed_version: the version reference (git SHA or v1-tree: hash)
-- at which the corpus was last successfully fully indexed.  NULL until the first
-- successful pipeline run that includes Pass::History.
ALTER TABLE corpora ADD COLUMN last_indexed_version TEXT;

-- chunks.source_hash: SHA-256 of the source file content at the time this chunk
-- was produced.  Used to detect whether a source file changed even when the
-- chunk ID (content hash) is stable.
ALTER TABLE chunks ADD COLUMN source_hash TEXT;

-- chunks.introduced_at_version: the corpus version at which this chunk first appeared.
ALTER TABLE chunks ADD COLUMN introduced_at_version TEXT;

-- chunks.last_modified_at_version: the corpus version at which this chunk's source
-- file was last seen dirty.
ALTER TABLE chunks ADD COLUMN last_modified_at_version TEXT;

-- chunks.last_modified_commit_message: the git commit message for the commit that
-- last touched this chunk's source file (populated only for git-backed code corpora).
ALTER TABLE chunks ADD COLUMN last_modified_commit_message TEXT;

-- chunks.last_modified_author: the author string for that commit.
ALTER TABLE chunks ADD COLUMN last_modified_author TEXT;

-- Index for fast "which chunks belong to a dirty source?" lookups.
CREATE INDEX IF NOT EXISTS idx_chunks_corpus_source_hash
    ON chunks (corpus_id, source_hash);
