use crate::indexing::model_tier::RoutingInputs;
use crate::types::{Chunk, Corpus, Edge, Entity};
use callimachus_llm::LlmProvider;

/// A discovered concrete input, ready for chunking.
#[derive(Debug, Clone)]
pub struct DiscoveredSource {
    pub path: String,
    /// "epub", "markdown", "text"
    pub kind: String,
    pub meta: serde_json::Value,
}

/// Structural extraction result (no LLM, parser-driven).
pub struct ExtractedStructure {
    pub parent_path: Option<String>,
    pub child_paths: Vec<String>,
    pub structural_entities: Vec<Entity>,
    pub structural_edges: Vec<Edge>,
}

/// LLM-driven semantic extraction result.
pub struct ExtractedSemantic {
    pub entities: Vec<Entity>,
    pub edges: Vec<Edge>,
    pub summary_text: Option<String>,
}

/// Instruction to merge two entities (alias resolution result).
pub struct EntityMerge {
    pub keep_id: String,
    pub absorb_id: String,
    pub reason: String,
}

/// A parsed location reference.
pub struct LocationRef {
    pub corpus_id: String,
    pub path: String,
}

// ── Phase 12 extracted types ─────────────────────────────────────────────────

/// Result of `extract_purpose`: why an entity exists and optional block blurbs.
pub struct ExtractedPurpose {
    pub purpose: String,
    pub blocks: Vec<ExtractedBlock>,
}

/// A named subsection of a complex function.
pub struct ExtractedBlock {
    pub label: String,
    pub description: String,
}

/// Result of `extract_contract`: LLM-inferred semantic contract data.
pub struct ExtractedContract {
    pub assumptions: Vec<String>,
    pub risks: Vec<String>,
    pub intent_gap: Option<String>,
    pub caller_notes: Option<String>,
    /// Names of entities verified by this entity (test → production function).
    pub verified_by_names: Vec<String>,
    /// Names of callees whose result this entity discards.
    pub discards_result_callees: Vec<String>,
}

/// Result of `extract_themes`: corpus-level architectural invariants.
pub struct ExtractedThemes {
    pub themes: Vec<ExtractedTheme>,
}

/// One architectural theme.
pub struct ExtractedTheme {
    pub title: String,
    pub statement: String,
    pub confidence: f32,
    pub upheld_by_entity_names: Vec<String>,
    pub violated_by_entity_names: Vec<String>,
}

/// The extension point every adapter implements.
///
/// Each adapter handles a specific content type (book, code, wiki, …).
/// The indexing pipeline calls these methods in order; adapters that
/// don't support a given step may return `None` or an empty collection.
#[async_trait::async_trait]
pub trait SourceAdapter: Send + Sync {
    /// Short stable identifier for this adapter, e.g. `"book"`.
    fn kind(&self) -> &str;
    /// Semver version string, e.g. `"0.1.0"`.
    fn version(&self) -> &str;

    /// Ordered list of chunk kinds to summarize, leaf → root.
    ///
    /// The summarize pass processes each level in order. For each level at
    /// index `i > 0`, child summaries from level `i-1` are collected and
    /// fed as context. A corpus-level summary is always attempted last.
    ///
    /// Default: empty — only the corpus-level summary is attempted.
    fn summary_levels(&self) -> Vec<&'static str> {
        vec![]
    }

    /// Expand a source path/URL into concrete inputs.
    async fn discover(&self, source: &str) -> anyhow::Result<Vec<DiscoveredSource>>;

    /// Yield chunks from a discovered source.
    async fn chunk(&self, source: &DiscoveredSource) -> anyhow::Result<Vec<Chunk>>;

    /// Structural extraction (parser-driven, no LLM). May return an empty result.
    async fn extract_structure(&self, chunk: &Chunk) -> anyhow::Result<ExtractedStructure>;

    /// LLM-driven semantic extraction. Return `None` to skip this chunk.
    async fn extract_with_llm(
        &self,
        chunk: &Chunk,
        llm: &dyn LlmProvider,
    ) -> anyhow::Result<Option<ExtractedSemantic>>;

    /// Summarize a chunk. Return `None` to skip.
    async fn summarize(
        &self,
        chunk: &Chunk,
        llm: &dyn LlmProvider,
        depth: &str,
    ) -> anyhow::Result<Option<String>>;

    /// Alias resolution: called once after all chunks are semantically processed.
    /// Return merge suggestions; the pipeline applies them via entity_store::merge.
    async fn resolve_aliases(
        &self,
        entities: &[Entity],
        llm: &dyn LlmProvider,
    ) -> anyhow::Result<Vec<EntityMerge>>;

    /// Canonical location URI path segment for a chunk (e.g. `"ch/3/sc/7"`).
    fn format_location(&self, chunk: &Chunk) -> String;

    /// Parse an adapter location path back into a `LocationRef`.
    fn parse_location(&self, uri: &str) -> anyhow::Result<LocationRef>;

    // ── Phase 12 methods (default: no-op) ────────────────────────────────────

    /// Extract why an entity exists (purpose) and optional block blurbs.
    /// Default: `Ok(None)` — adapters that don't model purpose skip this.
    async fn extract_purpose(
        &self,
        _entity: &Entity,
        _content: &str,
        _summary: Option<&str>,
        _llm: &dyn LlmProvider,
    ) -> anyhow::Result<Option<ExtractedPurpose>> {
        Ok(None)
    }

    /// Extract LLM-inferred semantic contract data for an entity.
    /// Default: `Ok(None)`.
    async fn extract_contract(
        &self,
        _entity: &Entity,
        _content: &str,
        _summary: Option<&str>,
        _purpose: Option<&str>,
        _signals: &serde_json::Value,
        _llm: &dyn LlmProvider,
    ) -> anyhow::Result<Option<ExtractedContract>> {
        Ok(None)
    }

    /// Return static routing signals for tier selection without any LLM call.
    ///
    /// Called by indexing passes before each LLM invocation to decide which
    /// model tier to use for the entity.  The default implementation returns
    /// all-zero/false inputs, causing the router to fall back to the configured
    /// `default` tier.  Code adapters override this by calling their static
    /// analysis (`analyze(language, content, name)`).
    fn static_routing_inputs(
        &self,
        _language: &str,
        _content: &str,
        _entity_name: &str,
    ) -> RoutingInputs {
        RoutingInputs::default()
    }

    /// Extract corpus-level architectural themes (opt-in).
    /// Default: `Ok(None)`.
    async fn extract_themes(
        &self,
        _corpus: &Corpus,
        _entities: &[Entity],
        _llm: &dyn LlmProvider,
    ) -> anyhow::Result<Option<ExtractedThemes>> {
        Ok(None)
    }
}
