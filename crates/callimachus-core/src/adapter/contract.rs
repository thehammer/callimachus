use crate::indexing::change_manifest::{ChangeKind, ChangedSource};
use crate::indexing::model_tier::RoutingInputs;
use crate::types::{Chunk, Corpus, Edge, Entity};
use callimachus_llm::LlmProvider;
use sha2::{Digest, Sha256};

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

/// Result of `extract_contract`: static signals merged with LLM-inferred semantics.
///
/// Static signal fields are populated by the adapter's deterministic analysis
/// (tree-sitter / regex) before the LLM call; they should always be accurate
/// regardless of whether the LLM succeeds.  LLM-inferred fields may be empty
/// when the model returns nothing useful.
#[derive(Default)]
pub struct ExtractedContract {
    // ── Static signals (adapter-populated, no LLM) ───────────────────────────
    pub is_public: bool,
    pub is_must_use: bool,
    pub is_deprecated: bool,
    pub is_fallible: bool,
    pub is_nullable: bool,
    pub is_mutating: bool,
    pub is_diverging: bool,
    pub has_panic_risk: bool,
    pub has_unsafe: bool,
    pub is_incomplete: bool,
    pub panic_call_count: u32,
    pub debt_markers: Vec<String>,
    // ── LLM-inferred semantics ───────────────────────────────────────────────
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

    // ── Stage 0 methods (default: hash-of-hashes) ────────────────────────────

    /// Version reference for the corpus's current state.
    ///
    /// Default implementation: walks `source_path`, computes SHA-256 of each
    /// file's content, sorts entries by path, then hashes the concatenated
    /// `"{path}\0{hex_hash}\n"` lines.  Returns the result as
    /// `"v1-tree:<hex-digest>"`.
    ///
    /// Adapters that have access to a faster or more precise version signal
    /// (e.g. git commit SHA) should override this.
    fn current_version(&self, source_path: &str) -> anyhow::Result<String> {
        default_current_version(source_path)
    }

    /// Files that changed between two version refs.
    ///
    /// `from_version = None` ⇒ return all source paths (first-run case).
    ///
    /// Default implementation: returns all files as `Added` when
    /// `from_version` is None or when the versions differ (conservative but
    /// correct).  Adapters with git access should override this to return
    /// only the actual diff.
    ///
    /// Note: the default impl is deliberately conservative — it never misses
    /// a changed file, but it may return files that haven't actually changed.
    /// The CodeAdapter override uses `git2` diff for precise results.
    fn changed_sources(
        &self,
        source_path: &str,
        from_version: Option<&str>,
        to_version: &str,
    ) -> anyhow::Result<Vec<ChangedSource>> {
        default_changed_sources(source_path, from_version, to_version)
    }
}

// ── Default Stage-0 helpers ───────────────────────────────────────────────────

/// Default implementation of `current_version`: SHA-256 hash-of-hashes over
/// every file under `source_path`.
pub fn default_current_version(source_path: &str) -> anyhow::Result<String> {
    use std::io::Read;

    let root = std::path::Path::new(source_path);
    if !root.exists() {
        return Ok("v1-tree:empty".to_string());
    }

    // Collect (relative_path, file_hash) pairs, sorted for determinism.
    let mut entries: Vec<(String, String)> = Vec::new();

    if root.is_file() {
        let mut content = Vec::new();
        std::fs::File::open(root)?.read_to_end(&mut content)?;
        let hash = hex::encode(Sha256::digest(&content));
        entries.push((source_path.to_string(), hash));
    } else {
        walk_files(root, root, &mut entries)?;
    }

    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut outer = Sha256::new();
    for (path, hash) in &entries {
        outer.update(path.as_bytes());
        outer.update(b"\0");
        outer.update(hash.as_bytes());
        outer.update(b"\n");
    }
    Ok(format!("v1-tree:{}", hex::encode(outer.finalize())))
}

fn walk_files(
    root: &std::path::Path,
    dir: &std::path::Path,
    out: &mut Vec<(String, String)>,
) -> anyhow::Result<()> {
    use std::io::Read;

    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        // Skip hidden files/dirs (e.g. .git).
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with('.'))
        {
            continue;
        }
        if path.is_dir() {
            walk_files(root, &path, out)?;
        } else if path.is_file() {
            let rel = path.strip_prefix(root).unwrap_or(&path);
            let rel_str = rel.to_string_lossy().to_string();
            let mut content = Vec::new();
            std::fs::File::open(&path)?.read_to_end(&mut content)?;
            let hash = hex::encode(Sha256::digest(&content));
            out.push((rel_str, hash));
        }
    }
    Ok(())
}

/// Default implementation of `changed_sources`: conservative — returns all
/// files as Added whenever `from_version` is None or differs from `to_version`.
pub fn default_changed_sources(
    source_path: &str,
    from_version: Option<&str>,
    to_version: &str,
) -> anyhow::Result<Vec<ChangedSource>> {
    // If versions match, nothing changed.
    if from_version == Some(to_version) {
        return Ok(vec![]);
    }
    // Otherwise, return every source path as Added (conservative).
    collect_all_as_added(source_path)
}

fn collect_all_as_added(source_path: &str) -> anyhow::Result<Vec<ChangedSource>> {
    let root = std::path::Path::new(source_path);
    if !root.exists() {
        return Ok(vec![]);
    }
    if root.is_file() {
        return Ok(vec![ChangedSource {
            path: source_path.to_string(),
            kind: ChangeKind::Added,
            commit_meta: None,
        }]);
    }
    let mut paths: Vec<(String, String)> = Vec::new();
    walk_files(root, root, &mut paths)?;
    Ok(paths
        .into_iter()
        .map(|(path, _)| ChangedSource {
            path,
            kind: ChangeKind::Added,
            commit_meta: None,
        })
        .collect())
}
