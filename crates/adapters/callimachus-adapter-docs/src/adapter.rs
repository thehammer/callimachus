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

use crate::{chunker, docc::DoccPage, extractor, summarizer};

/// Docs adapter: handles Apple DocC JSON directory trees.
///
/// Each `.json` file in the source directory is treated as one DocC page.
/// The adapter expects the layout written by `fetch-apple-docs.py --format json`:
/// `<root>/<Framework>/<SymbolName>.json`
///
/// Construct with `DocsAdapter::with_root(path)` in production so that
/// `extract_structure` can read the raw JSON.  `DocsAdapter::new()` leaves
/// `root` as `None` and `extract_structure` will bail if called.
pub struct DocsAdapter {
    root: Option<String>,
}

impl DocsAdapter {
    pub fn new() -> Self {
        Self { root: None }
    }

    /// Construct an adapter that knows the corpus root directory.
    ///
    /// Required for the `extract_structure` trait method.
    pub fn with_root(root: &str) -> Self {
        Self {
            root: Some(root.to_string()),
        }
    }
}

impl Default for DocsAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl SourceAdapter for DocsAdapter {
    fn kind(&self) -> &str {
        "docs"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    /// Discover: returns one `DiscoveredSource` per `.json` file in the directory tree.
    ///
    /// Each JSON file is a self-contained DocC page. The adapter stores the root
    /// path in `meta.root` so that `chunk()` can derive framework/slug from
    /// the relative path.
    async fn discover(&self, source: &str) -> anyhow::Result<Vec<DiscoveredSource>> {
        let root = Path::new(source);
        if !root.exists() {
            anyhow::bail!("docs source path does not exist: {source}");
        }

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
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }

            sources.push(DiscoveredSource {
                path: path.to_string_lossy().to_string(),
                kind: "docc".to_string(),
                meta: serde_json::json!({ "root": source }),
            });
        }

        Ok(sources)
    }

    fn summary_levels(&self) -> Vec<&'static str> {
        vec!["section", "page"]
    }

    /// Chunk: produce page + section chunks for a single DocC JSON file.
    async fn chunk(&self, source: &DiscoveredSource) -> anyhow::Result<Vec<Chunk>> {
        let file_path = Path::new(&source.path);
        let root = source
            .meta
            .get("root")
            .and_then(|v| v.as_str())
            .unwrap_or(&source.path);
        let root_path = Path::new(root);

        let corpus_id = source
            .meta
            .get("corpus_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        let raw_text = std::fs::read_to_string(file_path)
            .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", file_path.display()))?;

        let raw: serde_json::Value = serde_json::from_str(&raw_text)
            .map_err(|e| anyhow::anyhow!("failed to parse JSON {}: {e}", file_path.display()))?;

        let page = DoccPage::from_value(&raw);
        let chunks = chunker::chunk_docs_file(corpus_id, file_path, root_path, &page, &raw);

        Ok(chunks)
    }

    /// Structural extraction from a docs chunk (no LLM).
    ///
    /// Requires the adapter to have been constructed with `DocsAdapter::with_root()`.
    /// The root path is used to reconstruct the original JSON file from the chunk's
    /// location path (`docs/<Framework>/<Slug>`).
    async fn extract_structure(&self, chunk: &Chunk) -> anyhow::Result<ExtractedStructure> {
        let root = self.root.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "DocsAdapter::extract_structure requires a root path; \
                 construct the adapter with DocsAdapter::with_root(corpus.source)"
            )
        })?;

        // chunk.location.path = "docs/AppKit/NSView" (possibly with "#anchor").
        // Strip the anchor, then strip the leading "docs/" prefix.
        let path_part = chunk
            .location
            .path
            .split('#')
            .next()
            .unwrap_or(&chunk.location.path);

        let relative = path_part.strip_prefix("docs/").ok_or_else(|| {
            anyhow::anyhow!(
                "unexpected docs chunk path format (expected 'docs/<Framework>/<Slug>'): {}",
                chunk.location.path
            )
        })?;

        let json_path = std::path::Path::new(root)
            .join(relative)
            .with_extension("json");

        let raw_text = std::fs::read_to_string(&json_path).map_err(|e| {
            anyhow::anyhow!("failed to read {}: {e}", json_path.display())
        })?;
        let raw: serde_json::Value = serde_json::from_str(&raw_text)?;
        let page = DoccPage::from_value(&raw);

        extractor::extract_structure(chunk, &page, &chunk.corpus_id)
    }

    /// Structural extraction given direct access to the DiscoveredSource.
    ///
    /// This is the path used by the smoke test and can be used by a future
    /// pipeline extension that passes sources alongside chunks.
    async fn extract_with_llm(
        &self,
        _chunk: &Chunk,
        _llm: &dyn LlmProvider,
    ) -> anyhow::Result<Option<ExtractedSemantic>> {
        // Semantic extraction is handled by extract_structure for docs.
        // Summaries are handled by the summarize pass.
        Ok(None)
    }

    /// Summarize a docs chunk.
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
                let summary =
                    summarizer::summarize_page_from_content(&page_title, &chunk.content, llm)
                        .await?;
                Ok(Some(summary))
            }
            "corpus" => {
                let summary = summarizer::summarize_corpus(&[], llm).await?;
                Ok(Some(summary))
            }
            _ => Ok(None),
        }
    }

    async fn resolve_aliases(
        &self,
        _entities: &[Entity],
        _llm: &dyn LlmProvider,
    ) -> anyhow::Result<Vec<EntityMerge>> {
        Ok(vec![])
    }

    fn format_location(&self, chunk: &Chunk) -> String {
        chunk.location.path.clone()
    }

    fn parse_location(&self, uri: &str) -> anyhow::Result<LocationRef> {
        if let Ok(loc) = callimachus_core::types::Location::parse(uri) {
            return Ok(LocationRef {
                corpus_id: loc.corpus_id,
                path: loc.path,
            });
        }
        Ok(LocationRef {
            corpus_id: String::new(),
            path: uri.to_string(),
        })
    }
}

/// Extract structure directly from a `DiscoveredSource` (used by the smoke test
/// and future pipeline extensions that want structured edges).
pub async fn extract_from_source(
    source: &DiscoveredSource,
) -> anyhow::Result<(Vec<Chunk>, ExtractedStructure)> {
    let file_path = Path::new(&source.path);
    let root = source
        .meta
        .get("root")
        .and_then(|v| v.as_str())
        .unwrap_or(&source.path);
    let root_path = Path::new(root);
    let corpus_id = source
        .meta
        .get("corpus_id")
        .and_then(|v| v.as_str())
        .unwrap_or("test");

    let raw_text = std::fs::read_to_string(file_path)?;
    let raw: serde_json::Value = serde_json::from_str(&raw_text)?;

    extractor::extract_from_value(&raw, corpus_id, file_path, root_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_is_docs() {
        let a = DocsAdapter::new();
        assert_eq!(a.kind(), "docs");
    }

    #[test]
    fn summary_levels() {
        let a = DocsAdapter::new();
        assert_eq!(a.summary_levels(), vec!["section", "page"]);
    }

    #[test]
    fn parse_location_calli_uri() {
        let a = DocsAdapter::new();
        let loc = a
            .parse_location("calli://my-docs/docs/AppKit/NSView")
            .unwrap();
        assert_eq!(loc.corpus_id, "my-docs");
        assert_eq!(loc.path, "docs/AppKit/NSView");
    }
}
