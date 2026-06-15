//! A deliberately tiny adapter for the `plain` corpus kind.
//!
//! Its entire purpose is to prove the [`callimachus_adapter_contract`] seam:
//! it implements [`SourceAdapter`] using nothing but the contract crate — no
//! `callimachus-core`, no storage, no `rusqlite`. A binary can register it into
//! an [`AdapterRegistry`] and index a corpus of kind `"plain"` end-to-end with
//! **zero edits** to `callimachus-core` and zero edits to the host's adapter
//! selection logic. It stands in for the future out-of-repo adapters (jira,
//! sessions, docs) the PRD describes.
//!
//! Each `*.txt` file under the source path becomes one `document` chunk and one
//! structural `document` entity. No LLM is required, so it indexes cleanly
//! under a dry-run provider.

use std::path::Path;
use std::sync::Arc;

use callimachus_adapter_contract::{
    Chunk, DiscoveredSource, Entity, EntityMerge, ExtractedSemantic, ExtractedStructure,
    LlmProvider, Location, LocationRef, SourceAdapter, hash_content,
};

/// Minimal adapter for plain-text corpora.
#[derive(Debug, Default, Clone)]
pub struct TrivialAdapter;

impl TrivialAdapter {
    pub fn new() -> Self {
        Self
    }

    /// Convenience: an `Arc<dyn SourceAdapter>` ready to register.
    pub fn arc() -> Arc<dyn SourceAdapter> {
        Arc::new(Self::new())
    }
}

#[async_trait::async_trait]
impl SourceAdapter for TrivialAdapter {
    fn kind(&self) -> &str {
        "plain"
    }

    fn version(&self) -> &str {
        "0.1.0"
    }

    async fn discover(&self, source: &str) -> anyhow::Result<Vec<DiscoveredSource>> {
        let root = Path::new(source);
        let mut out = Vec::new();
        collect_txt(root, &mut out)?;
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    async fn chunk(&self, source: &DiscoveredSource) -> anyhow::Result<Vec<Chunk>> {
        let content = std::fs::read_to_string(&source.path)?;
        let corpus_id = source
            .meta
            .get("corpus_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let location = Location::new(corpus_id.clone(), source.path.clone());
        Ok(vec![Chunk::new(
            corpus_id,
            None,
            "document".to_string(),
            location,
            content,
        )])
    }

    async fn extract_structure(&self, chunk: &Chunk) -> anyhow::Result<ExtractedStructure> {
        // One structural entity per document — no LLM needed, so the corpus has
        // real content even under a dry-run provider.
        let id = hash_content(&format!("plain:{}", chunk.location.path));
        let mut entity = Entity::new(
            id,
            chunk.corpus_id.clone(),
            chunk.location.path.clone(),
            "document".to_string(),
        );
        entity.first_location = Some(chunk.location.clone());
        Ok(ExtractedStructure {
            parent_path: None,
            child_paths: vec![],
            structural_entities: vec![entity],
            structural_edges: vec![],
        })
    }

    async fn extract_with_llm(
        &self,
        _chunk: &Chunk,
        _llm: &dyn LlmProvider,
    ) -> anyhow::Result<Option<ExtractedSemantic>> {
        Ok(None)
    }

    async fn summarize(
        &self,
        _chunk: &Chunk,
        _llm: &dyn LlmProvider,
        _depth: &str,
    ) -> anyhow::Result<Option<String>> {
        Ok(None)
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
        let loc = Location::parse(uri)?;
        Ok(LocationRef {
            corpus_id: loc.corpus_id,
            path: loc.path,
        })
    }
}

/// Recursively collect `*.txt` files under `root` as `DiscoveredSource`s.
fn collect_txt(root: &Path, out: &mut Vec<DiscoveredSource>) -> anyhow::Result<()> {
    if root.is_file() {
        push_if_txt(root, out);
        return Ok(());
    }
    if !root.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_txt(&path, out)?;
        } else {
            push_if_txt(&path, out);
        }
    }
    Ok(())
}

fn push_if_txt(path: &Path, out: &mut Vec<DiscoveredSource>) {
    if path.extension().and_then(|e| e.to_str()) == Some("txt") {
        out.push(DiscoveredSource {
            path: path.to_string_lossy().to_string(),
            kind: "text".to_string(),
            meta: serde_json::Value::Object(Default::default()),
        });
    }
}
