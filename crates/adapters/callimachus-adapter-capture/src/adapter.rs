/// `SourceAdapter` implementation for webster network captures.
///
/// A capture is a directory (or the `events.jsonl` file inside it) produced by
/// the webster browser-automation extension.  The adapter turns each distinct
/// `(method, normalized_path)` pair into a Callimachus endpoint entity.
///
/// Location scheme: `ep/{METHOD}/{percent-encoded-normalized-path}`
/// Corpus kind:     `"capture"`
use std::path::{Path, PathBuf};

use callimachus_core::{
    adapter::{
        DiscoveredSource, EntityMerge, ExtractedSemantic, ExtractedStructure, LocationRef,
        SourceAdapter,
    },
    types::{Chunk, Entity},
};
use callimachus_llm::LlmProvider;

use crate::{chunker, extractor, summarizer};

/// Adapter for webster network captures.
pub struct CaptureAdapter;

impl CaptureAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CaptureAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl SourceAdapter for CaptureAdapter {
    fn kind(&self) -> &str {
        "capture"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn summary_levels(&self) -> Vec<&'static str> {
        vec!["endpoint"]
    }

    /// Discover the capture as a single source.
    ///
    /// Accepts either:
    /// - A path to an `events.jsonl` file.
    /// - A path to a directory containing `events.jsonl`.
    async fn discover(&self, source: &str) -> anyhow::Result<Vec<DiscoveredSource>> {
        let path = Path::new(source);

        let (capture_dir, events_path) = if path.is_dir() {
            let ep = path.join("events.jsonl");
            if !ep.exists() {
                anyhow::bail!("capture source directory does not contain events.jsonl: {source}");
            }
            (path.to_path_buf(), ep)
        } else if path.is_file() {
            let dir = path
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| PathBuf::from("."));
            (dir, path.to_path_buf())
        } else {
            anyhow::bail!("capture source path does not exist: {source}");
        };

        // Try to parse meta.json.
        let meta_json_path = capture_dir.join("meta.json");
        let meta_json: serde_json::Value = if meta_json_path.exists() {
            let text = std::fs::read_to_string(&meta_json_path)?;
            serde_json::from_str(&text).unwrap_or(serde_json::Value::Null)
        } else {
            serde_json::Value::Null
        };

        let source = DiscoveredSource {
            path: events_path.to_string_lossy().to_string(),
            kind: "capture".to_string(),
            meta: serde_json::json!({
                "events_path": events_path.to_string_lossy(),
                "capture_dir": capture_dir.to_string_lossy(),
                "meta_json": meta_json,
            }),
        };

        Ok(vec![source])
    }

    /// Chunk the entire capture into one chunk per distinct endpoint.
    async fn chunk(&self, source: &DiscoveredSource) -> anyhow::Result<Vec<Chunk>> {
        let events_path = source
            .meta
            .get("events_path")
            .and_then(|v| v.as_str())
            .unwrap_or(&source.path);

        let capture_dir = source
            .meta
            .get("capture_dir")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                Path::new(events_path)
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| PathBuf::from("."))
            });

        let meta_json = source
            .meta
            .get("meta_json")
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        let corpus_id = source
            .meta
            .get("corpus_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        let jsonl = std::fs::read_to_string(events_path)
            .map_err(|e| anyhow::anyhow!("failed to read events.jsonl at {events_path}: {e}"))?;

        chunker::chunk_events(&jsonl, corpus_id, &capture_dir, &meta_json)
    }

    /// Structural extraction: one entity + sequencing edges per chunk.
    async fn extract_structure(&self, chunk: &Chunk) -> anyhow::Result<ExtractedStructure> {
        extractor::extract_structure(chunk)
    }

    /// LLM-driven semantic extraction: describe the endpoint.
    async fn extract_with_llm(
        &self,
        chunk: &Chunk,
        llm: &dyn LlmProvider,
    ) -> anyhow::Result<Option<ExtractedSemantic>> {
        let description = summarizer::describe_endpoint(chunk, llm).await?;

        // Parse the structural entity id from chunk content to match it up.
        let content: serde_json::Value = serde_json::from_str(&chunk.content)
            .map_err(|e| anyhow::anyhow!("invalid chunk JSON: {e}"))?;
        let signature = content["signature"].as_str().unwrap_or("").to_string();
        let entity_id = format!("ep:{signature}");

        let mut entity = Entity::new(
            entity_id,
            chunk.corpus_id.clone(),
            signature,
            "endpoint".to_string(),
        );
        entity.description = Some(description);
        entity.confidence = 0.95;
        entity.first_location = Some(chunk.location.clone());
        entity.last_location = Some(chunk.location.clone());

        Ok(Some(ExtractedSemantic {
            entities: vec![entity],
            edges: vec![],
            summary_text: None,
        }))
    }

    /// Summarize a capture chunk.
    ///
    /// - `"endpoint"`: 1–2 sentence summary of what the endpoint does.
    /// - `"corpus"`:   API overview from aggregated endpoint summaries.
    async fn summarize(
        &self,
        chunk: &Chunk,
        llm: &dyn LlmProvider,
        depth: &str,
    ) -> anyhow::Result<Option<String>> {
        match depth {
            "endpoint" => {
                let s = summarizer::summarize_endpoint(chunk, llm).await?;
                Ok(Some(s))
            }
            "corpus" => {
                let s = summarizer::summarize_corpus(chunk, llm).await?;
                Ok(Some(s))
            }
            _ => Ok(None),
        }
    }

    /// No alias resolution needed for capture corpora (endpoints are unique by signature).
    async fn resolve_aliases(
        &self,
        _entities: &[Entity],
        _llm: &dyn LlmProvider,
    ) -> anyhow::Result<Vec<EntityMerge>> {
        Ok(vec![])
    }

    /// Location path for a capture chunk.
    ///
    /// Format: `ep/{METHOD}/{percent-encoded-normalized-path}`
    /// Example: `ep/GET/%2Fconnect%2Fcoordinatedcare%2Frestapi%2Fconfig%2Fuser%2F{id}`
    ///
    /// The chunker sets `chunk.location.path` to this value during construction,
    /// so we simply return it.
    fn format_location(&self, chunk: &Chunk) -> String {
        chunk.location.path.clone()
    }

    /// Parse a capture location URI back into a [`LocationRef`].
    fn parse_location(&self, uri: &str) -> anyhow::Result<LocationRef> {
        extractor::parse_location(uri)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_is_capture() {
        assert_eq!(CaptureAdapter::new().kind(), "capture");
    }

    #[test]
    fn summary_levels() {
        assert_eq!(CaptureAdapter::new().summary_levels(), vec!["endpoint"]);
    }

    #[test]
    fn parse_location_calli_uri() {
        let a = CaptureAdapter::new();
        let loc = a
            .parse_location("calli://mycap/ep/GET/%2Fapi%2Fusers%2F{id}")
            .unwrap();
        assert_eq!(loc.corpus_id, "mycap");
        assert_eq!(loc.path, "ep/GET/%2Fapi%2Fusers%2F{id}");
    }

    #[test]
    fn parse_location_plain_path() {
        let a = CaptureAdapter::new();
        let loc = a.parse_location("ep/POST/%2Fapi%2Forders").unwrap();
        assert!(loc.corpus_id.is_empty());
        assert_eq!(loc.path, "ep/POST/%2Fapi%2Forders");
    }
}
