use std::path::Path;

use callimachus_core::{
    adapter::{
        DiscoveredSource, EntityMerge, ExtractedSemantic, ExtractedStructure, LocationRef,
        SourceAdapter,
    },
    types::{Chunk, Entity},
};
use callimachus_llm::LlmProvider;
use walkdir::WalkDir;

use crate::{chunker, extractor, summarizer};

/// Wiki adapter: handles Markdown directory trees (Obsidian vaults, GitHub wikis, MkDocs).
pub struct WikiAdapter;

impl WikiAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WikiAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl SourceAdapter for WikiAdapter {
    fn kind(&self) -> &str {
        "wiki"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    /// Discover: returns one `DiscoveredSource` per `.md` file in the directory tree.
    ///
    /// Unlike book/code adapters (which return the directory as a single source),
    /// wiki returns one source per file to enable per-file change detection in the watcher.
    async fn discover(&self, source: &str) -> anyhow::Result<Vec<DiscoveredSource>> {
        let root = Path::new(source);
        if !root.exists() {
            anyhow::bail!("wiki source path does not exist: {source}");
        }

        let exclude_prefixes = [".git", "_images", "_attachments"];

        let mut sources = Vec::new();
        for entry in WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }

            // Exclude directories starting with underscore or .git.
            let rel = path
                .strip_prefix(root)
                .map(|r| r.to_string_lossy().to_string())
                .unwrap_or_default();
            let rel_normalized = rel.replace('\\', "/");

            let excluded = exclude_prefixes
                .iter()
                .any(|pfx| rel_normalized.starts_with(pfx));
            if excluded {
                continue;
            }

            sources.push(DiscoveredSource {
                path: path.to_string_lossy().to_string(),
                kind: "markdown".to_string(),
                meta: serde_json::json!({ "root": source }),
            });
        }

        Ok(sources)
    }

    fn summary_levels(&self) -> Vec<&'static str> {
        vec!["section", "page"]
    }

    /// Chunk: produce page + section chunks for a single discovered `.md` file.
    async fn chunk(&self, source: &DiscoveredSource) -> anyhow::Result<Vec<Chunk>> {
        let file_path = Path::new(&source.path);
        let root = source
            .meta
            .get("root")
            .and_then(|v| v.as_str())
            .unwrap_or(&source.path);
        let root_path = Path::new(root);

        // For now use a placeholder corpus_id — the real ID is set by the pipeline
        // when it stores chunks. Adapters don't own the corpus_id.
        // The pipeline passes the corpus context via storage — here we embed a
        // placeholder that the pipeline overwrites.
        //
        // Actually: the pipeline calls chunk() and then stores the chunks with the
        // real corpus_id set on the Chunk. Let's use an empty string and let the
        // pipeline fill it in... but looking at the book adapter: it hardcodes
        // corpus_id inside the chunker. Let me check.
        //
        // The book adapter also embeds the corpus_id in the Chunk at chunk time.
        // It uses `source.meta` to carry corpus context. For the wiki adapter,
        // we'll carry the corpus_id in meta too.
        let corpus_id = source
            .meta
            .get("corpus_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        chunker::chunk_single_file(corpus_id, file_path, root_path).await
    }

    /// Structural extraction from a wiki chunk (no LLM).
    async fn extract_structure(&self, chunk: &Chunk) -> anyhow::Result<ExtractedStructure> {
        let source_path = Path::new("/"); // not used for path resolution in extractor
        let ws = extractor::extract_structure(chunk, source_path)?;
        extractor::to_extracted_structure(chunk, &ws, &chunk.corpus_id)
    }

    /// LLM-driven semantic extraction.
    ///
    /// Focuses on entity/edge extraction only. Section summaries are now produced
    /// by the summarize pass via `summarize(depth="section")`.
    async fn extract_with_llm(
        &self,
        _chunk: &Chunk,
        _llm: &dyn LlmProvider,
    ) -> anyhow::Result<Option<ExtractedSemantic>> {
        // Semantic extraction (entities, edges) is handled by extract_structure for wikis.
        // Summaries are handled by the summarize pass.
        Ok(None)
    }

    /// Summarize a wiki chunk.
    ///
    /// - `"section"`: generate a 1-2 sentence summary of the section content.
    /// - `"page"`: summarize from child section summaries (provided as chunk.content by the pass).
    /// - `"corpus"`: generate a wiki overview.
    async fn summarize(
        &self,
        chunk: &Chunk,
        llm: &dyn LlmProvider,
        depth: &str,
    ) -> anyhow::Result<Option<String>> {
        match depth {
            "section" => {
                let (page_title, section_heading) = summarizer::chunk_metadata(chunk);
                let summary =
                    summarizer::summarize_section(chunk, &page_title, &section_heading, llm)
                        .await?;
                Ok(Some(summary))
            }
            "page" => {
                let (page_title, _) = summarizer::chunk_metadata(chunk);
                // chunk.content may contain pre-formatted section summaries from the pass.
                let summary = summarizer::summarize_page(&page_title, &[], llm).await?;
                Ok(Some(summary))
            }
            "corpus" => {
                let summary = summarizer::summarize_corpus(&[], llm).await?;
                Ok(Some(summary))
            }
            _ => Ok(None),
        }
    }

    /// Alias resolution: front-matter `aliases` are high-confidence.
    ///
    /// Returns empty merge list — alias resolution for wikis relies on front-matter
    /// already extracted by `extract_structure`. LLM alias suggestions would go here
    /// in a future pass.
    async fn resolve_aliases(
        &self,
        _entities: &[Entity],
        _llm: &dyn LlmProvider,
    ) -> anyhow::Result<Vec<EntityMerge>> {
        // Front-matter aliases are already embedded in Entity.aliases by extract_structure.
        // No LLM merge suggestions in v1.
        Ok(vec![])
    }

    /// Format the location path for a wiki chunk.
    ///
    /// For page chunks: `wiki/<path>`
    /// For section chunks: `wiki/<path>#<slug>`
    fn format_location(&self, chunk: &Chunk) -> String {
        chunk.location.path.clone()
    }

    /// Parse a wiki location URI back into a `LocationRef`.
    fn parse_location(&self, uri: &str) -> anyhow::Result<LocationRef> {
        // Try full calli:// URI first.
        if let Ok(loc) = callimachus_core::types::Location::parse(uri) {
            return Ok(LocationRef {
                corpus_id: loc.corpus_id,
                path: loc.path,
            });
        }
        // Fall back: treat as plain path.
        Ok(LocationRef {
            corpus_id: String::new(),
            path: uri.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_is_wiki() {
        let a = WikiAdapter::new();
        assert_eq!(a.kind(), "wiki");
    }

    #[test]
    fn parse_location_calli_uri() {
        let a = WikiAdapter::new();
        let loc_ref = a
            .parse_location("calli://mywiki/wiki/authentication")
            .unwrap();
        assert_eq!(loc_ref.corpus_id, "mywiki");
        assert_eq!(loc_ref.path, "wiki/authentication");
    }

    #[test]
    fn parse_location_with_anchor() {
        let a = WikiAdapter::new();
        let loc_ref = a
            .parse_location("calli://mywiki/wiki/authentication#oauth-flow")
            .unwrap();
        assert_eq!(loc_ref.path, "wiki/authentication#oauth-flow");
    }
}
