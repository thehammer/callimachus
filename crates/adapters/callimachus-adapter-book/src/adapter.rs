use callimachus_core::{
    adapter::{
        DiscoveredSource, EntityMerge, ExtractedSemantic, ExtractedStructure, LocationRef,
        SourceAdapter,
    },
    types::{Chunk, Entity},
};
use callimachus_llm::LlmProvider;

use crate::{chunker, extractor, resolver, summarizer};

/// Book adapter: handles EPUB, Markdown, and plain text.
pub struct BookAdapter;

impl BookAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for BookAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl SourceAdapter for BookAdapter {
    fn kind(&self) -> &str {
        "book"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    async fn discover(&self, source: &str) -> anyhow::Result<Vec<DiscoveredSource>> {
        let path = std::path::Path::new(source);
        let kind = match path.extension().and_then(|e| e.to_str()) {
            Some("epub") => "epub",
            Some("md") | Some("markdown") => "markdown",
            Some("txt") | Some("text") | None => "text",
            Some(other) => {
                tracing::warn!("unknown book extension .{other}; treating as text");
                "text"
            }
        };
        Ok(vec![DiscoveredSource {
            path: source.to_string(),
            kind: kind.to_string(),
            meta: serde_json::Value::Null,
        }])
    }

    fn summary_levels(&self) -> Vec<&'static str> {
        vec!["scene", "chapter"]
    }

    async fn chunk(&self, source: &DiscoveredSource) -> anyhow::Result<Vec<Chunk>> {
        match source.kind.as_str() {
            "epub" => chunker::chunk_epub(source),
            "markdown" => chunker::chunk_markdown(source),
            _ => chunker::chunk_text(source),
        }
    }

    async fn extract_structure(&self, chunk: &Chunk) -> anyhow::Result<ExtractedStructure> {
        // For books: scene chunks carry their parent chapter in `parent_path`.
        // The chunker already sets this, so structural extraction just reflects it.
        Ok(ExtractedStructure {
            parent_path: chunk.parent_path.clone(),
            child_paths: vec![],
            structural_entities: vec![],
            structural_edges: vec![],
        })
    }

    async fn extract_with_llm(
        &self,
        chunk: &Chunk,
        llm: &dyn LlmProvider,
    ) -> anyhow::Result<Option<ExtractedSemantic>> {
        // Only semantically process scene chunks; skip chapters (their scenes cover them).
        if chunk.kind != "scene" {
            return Ok(None);
        }
        let sem = extractor::extract(chunk, llm).await?;
        Ok(Some(sem))
    }

    async fn summarize(
        &self,
        chunk: &Chunk,
        llm: &dyn LlmProvider,
        depth: &str,
    ) -> anyhow::Result<Option<String>> {
        summarizer::summarize(chunk, llm, depth).await
    }

    async fn resolve_aliases(
        &self,
        entities: &[Entity],
        llm: &dyn LlmProvider,
    ) -> anyhow::Result<Vec<EntityMerge>> {
        resolver::resolve_aliases(entities, llm).await
    }

    fn format_location(&self, chunk: &Chunk) -> String {
        chunk.location.path.clone()
    }

    fn parse_location(&self, uri: &str) -> anyhow::Result<LocationRef> {
        // Expect format: "calli://<corpus_id>/<path>" or just "<path>"
        if let Ok(loc) = callimachus_core::types::Location::parse(uri) {
            return Ok(LocationRef {
                corpus_id: loc.corpus_id,
                path: loc.path,
            });
        }
        // Fall back: treat as a plain path, corpus_id unknown.
        Ok(LocationRef {
            corpus_id: String::new(),
            path: uri.to_string(),
        })
    }
}

// Structural types re-exported so tests can use them.
#[allow(dead_code)]
fn _assert_adapter_is_source_adapter(a: &BookAdapter) {
    let _: &dyn SourceAdapter = a;
}
