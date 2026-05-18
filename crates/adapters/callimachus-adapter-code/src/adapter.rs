use std::path::Path;

use anyhow::Result;
use callimachus_core::{
    adapter::{
        DiscoveredSource, EntityMerge, ExtractedBlock, ExtractedContract, ExtractedPurpose,
        ExtractedSemantic, ExtractedStructure, ExtractedTheme, ExtractedThemes, LocationRef,
        SourceAdapter,
    },
    types::{Chunk, Corpus, Entity},
};
use callimachus_llm::{CompletionRequest, LlmProvider};

use callimachus_core::adapter::default_current_version;
use callimachus_core::indexing::change_manifest::ChangedSource;

use crate::{
    chunker::{ChunkOptions, chunk_directory},
    contracts, extractor, git, languages, summarizer,
};

// ── CodeAdapter ───────────────────────────────────────────────────────────────

/// Adapter for source code repositories.
///
/// Supports Rust, TypeScript/JavaScript, Python, and Go.
/// Uses tree-sitter for structural chunking and entity extraction.
pub struct CodeAdapter;

impl CodeAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl SourceAdapter for CodeAdapter {
    fn kind(&self) -> &str {
        "code"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    /// For a code corpus the entire source directory is one discovered source.
    async fn discover(&self, source: &str) -> Result<Vec<DiscoveredSource>> {
        Ok(vec![DiscoveredSource {
            path: source.to_string(),
            kind: "directory".to_string(),
            // Use an empty object so the chunk_pass can inject corpus_id via
            // `meta.entry("corpus_id").or_insert(...)`.
            meta: serde_json::Value::Object(Default::default()),
        }])
    }

    fn summary_levels(&self) -> Vec<&'static str> {
        vec!["function", "file"]
    }

    /// Chunk the source directory using tree-sitter.
    async fn chunk(&self, source: &DiscoveredSource) -> Result<Vec<Chunk>> {
        let corpus_id = source
            .meta
            .get("corpus_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        let opts = build_chunk_options(&source.meta);
        let path = Path::new(&source.path);

        chunk_directory(path, &corpus_id, &opts).await
    }

    /// Structural extraction — parse the chunk with tree-sitter.
    async fn extract_structure(&self, chunk: &Chunk) -> Result<ExtractedStructure> {
        // Detect language from the chunk location URI.
        let ext = extract_extension_from_chunk(chunk);
        let lang = match ext.and_then(languages::for_extension) {
            Some(l) => l,
            None => {
                return Ok(ExtractedStructure {
                    parent_path: chunk.parent_path.clone(),
                    child_paths: vec![],
                    structural_entities: vec![],
                    structural_edges: vec![],
                });
            }
        };

        let code_structure = extractor::extract_structure(chunk, lang)?;

        Ok(ExtractedStructure {
            parent_path: chunk.parent_path.clone(),
            child_paths: vec![],
            structural_entities: code_structure.entities,
            structural_edges: code_structure.edges,
        })
    }

    /// LLM-driven semantic extraction — extract structure for function/class/interface chunks.
    ///
    /// Note: chunk summaries are now generated in the summarize pass via `summarize()`,
    /// not here. This method focuses purely on entity/edge extraction.
    async fn extract_with_llm(
        &self,
        chunk: &Chunk,
        llm: &dyn LlmProvider,
    ) -> Result<Option<ExtractedSemantic>> {
        match chunk.kind.as_str() {
            "function" | "class" | "interface" => {}
            _ => return Ok(None), // skip file, module, etc.
        }

        // Structural extraction only — no summarization here.
        let _ = llm; // LLM not used in this path currently; retained for API compatibility.

        Ok(Some(ExtractedSemantic {
            entities: vec![],
            edges: vec![],
            summary_text: None,
        }))
    }

    /// Summarize a code chunk.
    ///
    /// Called by the generic summarize pass for each declared level plus `"corpus"`.
    /// - `"function"` / `"class"` / `"interface"` — per-symbol summary via `summarize_chunk`.
    /// - `"file"` — file-level overview via `summarize_file`.
    /// - `"corpus"` — repository overview via `summarize_corpus`.
    async fn summarize(
        &self,
        chunk: &Chunk,
        llm: &dyn LlmProvider,
        depth: &str,
    ) -> Result<Option<String>> {
        match depth {
            "function" | "class" | "interface" => {
                // Per-symbol summary.
                let ext = extract_extension_from_chunk(chunk);
                let structure = if let Some(lang) = ext.and_then(languages::for_extension) {
                    extractor::extract_structure(chunk, lang).unwrap_or_default()
                } else {
                    extractor::ExtractedCodeStructure::default()
                };
                summarizer::summarize_chunk(chunk, &structure, llm).await
            }
            "file" => summarizer::summarize_file(chunk, llm).await,
            "corpus" => {
                let doc_comments: Vec<String> = chunk
                    .content
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .map(|l| l.to_string())
                    .collect();

                if doc_comments.is_empty() {
                    return Ok(None);
                }

                summarizer::summarize_corpus(&doc_comments, &chunk.corpus_id, llm).await
            }
            _ => Ok(None),
        }
    }

    /// Code entity names are canonical — no alias merging needed.
    async fn resolve_aliases(
        &self,
        _entities: &[Entity],
        _llm: &dyn LlmProvider,
    ) -> Result<Vec<EntityMerge>> {
        Ok(vec![])
    }

    /// Format a location path from a chunk: `src/<relative-path>#<symbol>`
    fn format_location(&self, chunk: &Chunk) -> String {
        chunk.location.path.clone()
    }

    /// Parse a location path back into a `LocationRef`.
    fn parse_location(&self, uri: &str) -> Result<LocationRef> {
        // Handle full URI: `calli://corpus_id/path...`
        let (corpus_id, path) = if let Some(rest) = uri.strip_prefix("calli://") {
            rest.split_once('/')
                .map(|(c, p)| (c.to_string(), p.to_string()))
                .ok_or_else(|| anyhow::anyhow!("invalid calli URI: {uri}"))?
        } else {
            // Bare path — no corpus_id.
            ("".to_string(), uri.to_string())
        };

        Ok(LocationRef { corpus_id, path })
    }

    // ── Phase 12 methods ──────────────────────────────────────────────────────

    /// Generate a purpose statement for a code entity.
    ///
    /// Runs static analysis to decide whether to request block blurbs, then
    /// calls the LLM (claude-haiku-3-5) with a tailored prompt.
    async fn extract_purpose(
        &self,
        entity: &Entity,
        content: &str,
        summary: Option<&str>,
        llm: &dyn LlmProvider,
    ) -> Result<Option<ExtractedPurpose>> {
        // Run static analysis to determine complexity.
        // Infer language from the entity's location URI so we don't run the
        // Rust tree-sitter grammar on PHP/JS/TS/Vue content (the external scanner
        // can loop indefinitely on non-Rust input).
        let lang = entity
            .first_location
            .as_ref()
            .map(|loc| loc.uri.as_str())
            .unwrap_or("")
            .rsplit('.')
            .next()
            .map(|ext| match ext {
                "rs" => "rust",
                _ => "other",
            })
            .unwrap_or("other");
        let signals = contracts::analyze(lang, content, &entity.canonical_name);
        let wants_blocks = signals.branch_count >= 3 || signals.body_lines >= 20;

        let summary_section = summary
            .map(|s| format!("\nExisting summary:\n{s}\n"))
            .unwrap_or_default();

        let blocks_instruction = if wants_blocks {
            r#"
Also break the function body into 2-5 named logical blocks. For each block provide:
- "label": a short snake_case identifier (e.g. "validate_inputs", "build_response")
- "description": one sentence explaining what that block does

Return JSON:
{
  "purpose": "...",
  "blocks": [
    {"label": "...", "description": "..."},
    ...
  ]
}"#
        } else {
            r#"Return JSON: {"purpose": "...", "blocks": []}"#
        };

        let prompt = format!(
            r#"You are analyzing a code entity to extract its *purpose* — why it exists in the system, not what it mechanically does.

Entity: {name}
Kind: {kind}
{summary_section}
Source:
<code>
{code}
</code>

Write a single sentence explaining the *purpose* of this entity. Focus on the business/design intent rather than mechanics.
{blocks_instruction}"#,
            name = entity.canonical_name,
            kind = entity.kind,
            code = &content[..content.floor_char_boundary(4000.min(content.len()))],
        );

        let req = CompletionRequest {
            prompt,
            model: Some("claude-haiku-4-5-20251001".to_string()),
            max_tokens: Some(500),
            chunk_id: None,
        };

        let response = llm.complete(req).await?;
        let text = response.text.trim();

        if text.is_empty() || text == "[dry-run]" {
            return Ok(Some(ExtractedPurpose {
                purpose: format!("[auto] {} `{}`", entity.kind, entity.canonical_name),
                blocks: vec![],
            }));
        }

        // Parse JSON response.
        let clean = strip_markdown_fences(text);
        match serde_json::from_str::<serde_json::Value>(&clean) {
            Ok(v) => {
                let purpose = v
                    .get("purpose")
                    .and_then(|p| p.as_str())
                    .unwrap_or(text)
                    .to_string();
                let blocks: Vec<ExtractedBlock> = v
                    .get("blocks")
                    .and_then(|b| b.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|b| {
                                Some(ExtractedBlock {
                                    label: b.get("label")?.as_str()?.to_string(),
                                    description: b.get("description")?.as_str()?.to_string(),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                Ok(Some(ExtractedPurpose { purpose, blocks }))
            }
            Err(_) => {
                // LLM returned plain text; treat as purpose with no blocks.
                Ok(Some(ExtractedPurpose {
                    purpose: text.to_string(),
                    blocks: vec![],
                }))
            }
        }
    }

    /// Extract LLM-inferred semantic contract data for a code entity.
    async fn extract_contract(
        &self,
        entity: &Entity,
        content: &str,
        summary: Option<&str>,
        purpose: Option<&str>,
        _signals: &serde_json::Value,
        llm: &dyn LlmProvider,
    ) -> Result<Option<ExtractedContract>> {
        // Detect language from the entity's URI extension (same pattern as extract_purpose).
        let lang = entity
            .first_location
            .as_ref()
            .map(|loc| loc.uri.as_str())
            .unwrap_or("")
            .rsplit('.')
            .next()
            .map(|ext| match ext {
                "rs" => "rust",
                "php" => "php",
                "ts" | "tsx" => "typescript",
                "js" | "jsx" => "javascript",
                "vue" => "vue",
                _ => "other",
            })
            .unwrap_or("other");

        // Run language-aware static analysis.
        let static_signals = contracts::analyze(lang, content, &entity.canonical_name);

        let summary_section = summary
            .map(|s| format!("Summary: {s}\n"))
            .unwrap_or_default();
        let purpose_section = purpose
            .map(|p| format!("Purpose: {p}\n"))
            .unwrap_or_default();

        let signals_text = format!(
            "is_public={} is_fallible={} is_nullable={} is_mutating={} \
             has_panic_risk={} has_unsafe={} is_incomplete={} is_test={}",
            static_signals.is_public,
            static_signals.is_fallible,
            static_signals.is_nullable,
            static_signals.is_mutating,
            static_signals.has_panic_risk,
            static_signals.has_unsafe,
            static_signals.is_incomplete,
            static_signals.is_test,
        );

        let language = lang;

        let language_note = match lang {
            "php" => {
                "Note: 'has_panic_risk' means unhandled exception risk (findOrFail, abort(), throw new). 'is_mutating' covers Eloquent writes and $this-> assignments."
            }
            "typescript" | "javascript" | "tsx" | "jsx" | "vue" => {
                "Note: 'has_panic_risk' means non-null assertion (!) usage or unguarded ref.value access. 'is_fallible' covers async/Promise and explicit throw."
            }
            _ => "",
        };
        let language_note_section = if language_note.is_empty() {
            String::new()
        } else {
            format!("{language_note}\n")
        };

        let prompt = format!(
            r#"You are analysing a {language} code entity to extract its behavioural contract.

Entity: {name}
Kind: {kind}
{summary_section}{purpose_section}{language_note_section}Static signals: {signals_text}

Source:
<code>
{code}
</code>

Return JSON matching this schema exactly:
{{
  "assumptions": ["..."],
  "risks": ["..."],
  "intent_gap": "..." | null,
  "caller_notes": "..." | null,
  "verified_by_names": ["..."],
  "discards_result_callees": ["..."]
}}

- assumptions: things the caller must guarantee for correct behavior
- risks: conditions under which this entity may fail, panic, or produce wrong output
- intent_gap: a brief description if the code does something unexpected given its name/summary (null if none)
- caller_notes: any caveats callers should know about (null if none)
- verified_by_names: names of other entities that test/verify this one (usually empty unless this is a test)
- discards_result_callees: names of callees whose Result/Option return value is ignored"#,
            language = language,
            name = entity.canonical_name,
            kind = entity.kind,
            language_note_section = language_note_section,
            code = &content[..content.floor_char_boundary(4000.min(content.len()))],
        );

        let req = CompletionRequest {
            prompt,
            model: Some("claude-haiku-4-5-20251001".to_string()),
            max_tokens: Some(600),
            chunk_id: None,
        };

        let response = llm.complete(req).await?;
        let text = response.text.trim();

        if text.is_empty() || text == "[dry-run]" {
            return Ok(Some(ExtractedContract {
                assumptions: vec![],
                risks: vec![],
                intent_gap: None,
                caller_notes: None,
                verified_by_names: vec![],
                discards_result_callees: vec![],
            }));
        }

        let clean = strip_markdown_fences(text);
        match serde_json::from_str::<serde_json::Value>(&clean) {
            Ok(v) => Ok(Some(ExtractedContract {
                assumptions: json_str_array(&v, "assumptions"),
                risks: json_str_array(&v, "risks"),
                intent_gap: v
                    .get("intent_gap")
                    .and_then(|x| x.as_str())
                    .map(|s| s.to_string()),
                caller_notes: v
                    .get("caller_notes")
                    .and_then(|x| x.as_str())
                    .map(|s| s.to_string()),
                verified_by_names: json_str_array(&v, "verified_by_names"),
                discards_result_callees: json_str_array(&v, "discards_result_callees"),
            })),
            Err(_) => Ok(Some(ExtractedContract {
                assumptions: vec![],
                risks: vec![text.to_string()],
                intent_gap: None,
                caller_notes: None,
                verified_by_names: vec![],
                discards_result_callees: vec![],
            })),
        }
    }

    /// Extract corpus-level architectural themes.
    async fn extract_themes(
        &self,
        corpus: &Corpus,
        entities: &[Entity],
        llm: &dyn LlmProvider,
    ) -> Result<Option<ExtractedThemes>> {
        // Summarise entity names for the prompt.
        let entity_list: String = entities
            .iter()
            .take(200)
            .map(|e| format!("  - {} ({})", e.canonical_name, e.kind))
            .collect::<Vec<_>>()
            .join("\n");

        let prompt = format!(
            r#"You are analysing a software corpus to identify architectural themes and invariants.

Corpus: {name}
Kind: {kind}
Entity count: {count}

Entities (sample):
{entity_list}

Identify 3–7 architectural themes or invariants that characterise this codebase.
For each theme return:
- "title": a short noun phrase (e.g. "Error Propagation via Result")
- "statement": one sentence describing the invariant
- "confidence": a float 0.0–1.0
- "upheld_by_entity_names": entity names that exemplify/uphold this theme
- "violated_by_entity_names": entity names that violate this theme (often empty)

Return JSON:
{{
  "themes": [
    {{
      "title": "...",
      "statement": "...",
      "confidence": 0.8,
      "upheld_by_entity_names": ["..."],
      "violated_by_entity_names": []
    }}
  ]
}}"#,
            name = corpus.name,
            kind = corpus.kind,
            count = entities.len(),
        );

        let req = CompletionRequest {
            prompt,
            model: Some("claude-sonnet-4-5-20250929".to_string()),
            max_tokens: Some(1500),
            chunk_id: None,
        };

        let response = llm.complete(req).await?;
        let text = response.text.trim();

        if text.is_empty() || text == "[dry-run]" {
            return Ok(None);
        }

        let clean = strip_markdown_fences(text);
        match serde_json::from_str::<serde_json::Value>(&clean) {
            Ok(v) => {
                let themes: Vec<ExtractedTheme> = v
                    .get("themes")
                    .and_then(|t| t.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|t| {
                                Some(ExtractedTheme {
                                    title: t.get("title")?.as_str()?.to_string(),
                                    statement: t.get("statement")?.as_str()?.to_string(),
                                    confidence: t
                                        .get("confidence")
                                        .and_then(|c| c.as_f64())
                                        .unwrap_or(0.5)
                                        as f32,
                                    upheld_by_entity_names: json_str_array(
                                        t,
                                        "upheld_by_entity_names",
                                    ),
                                    violated_by_entity_names: json_str_array(
                                        t,
                                        "violated_by_entity_names",
                                    ),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                Ok(Some(ExtractedThemes { themes }))
            }
            Err(_) => Ok(None),
        }
    }

    /// Populate [`RoutingInputs`] from tree-sitter static analysis of the source.
    ///
    /// Uses the same `contracts::analyze` call as `extract_contract` so the
    /// signals are consistent with what the LLM prompt will see.  Non-code
    /// adapters inherit the default `RoutingInputs::default()` (all zeros/false).
    fn static_routing_inputs(
        &self,
        language: &str,
        content: &str,
        entity_name: &str,
    ) -> callimachus_core::indexing::model_tier::RoutingInputs {
        let s = contracts::analyze(language, content, entity_name);
        callimachus_core::indexing::model_tier::RoutingInputs {
            has_unsafe: s.has_unsafe,
            is_fallible: s.is_fallible,
            is_public: s.is_public,
            is_mutating: s.is_mutating,
            panic_call_count: s.panic_call_count,
            has_debt_markers: !s.debt_markers.is_empty(),
            body_lines: s.body_lines,
            kind: String::new(), // caller fills in from entity.kind
            in_degree: 0,        // caller fills in from storage
            out_degree: 0,       // caller fills in from storage
        }
    }

    // ── Stage 0 overrides ────────────────────────────────────────────────────

    /// Returns `"git:<full-oid>"` when the source path is a git repo with a HEAD
    /// commit, otherwise falls back to the default SHA-256 hash-of-hashes.
    fn current_version(&self, source_path: &str) -> anyhow::Result<String> {
        let p = std::path::Path::new(source_path);
        match git::current_git_version(p)? {
            Some(v) => Ok(v),
            None => default_current_version(source_path),
        }
    }

    /// Returns the list of files that changed between two corpus versions.
    ///
    /// When both versions are `"git:<oid>"` strings and the path is a git repo,
    /// uses `git2` diff for precise results.  Falls back to the default
    /// (all-paths-as-Added) behaviour otherwise.
    fn changed_sources(
        &self,
        source_path: &str,
        from_version: Option<&str>,
        to_version: &str,
    ) -> anyhow::Result<Vec<ChangedSource>> {
        // If no prior version, everything is new.
        let from = match from_version {
            Some(f) => f,
            None => {
                return callimachus_core::adapter::default_changed_sources(
                    source_path,
                    None,
                    to_version,
                );
            }
        };

        // If versions match, nothing changed.
        if from == to_version {
            return Ok(vec![]);
        }

        // Try git diff if both versions look like git refs.
        if from.starts_with("git:") && to_version.starts_with("git:") {
            let p = std::path::Path::new(source_path);
            if let Some(changed) = git::diff_between(p, from, to_version)? {
                return Ok(changed);
            }
        }

        // Fallback: return all files.
        callimachus_core::adapter::default_changed_sources(source_path, Some(from), to_version)
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Extract the file extension from a chunk's location path.
///
/// Location path looks like: `src/foo/bar.rs` or `src/foo/bar.rs#MyFunc`
fn extract_extension_from_chunk(chunk: &Chunk) -> Option<&str> {
    let path = chunk.location.path.split('#').next().unwrap_or("");
    path.rsplit('.').next()
}

/// Build chunk options from corpus config JSON.
fn build_chunk_options(meta: &serde_json::Value) -> ChunkOptions {
    let mut opts = ChunkOptions::default();

    if let Some(max) = meta.get("max_chunk_bytes").and_then(|v| v.as_u64()) {
        opts.max_chunk_bytes = max as usize;
    }
    if let Some(globs) = meta.get("include_globs").and_then(|v| v.as_array()) {
        opts.include_globs = globs
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
    }
    if let Some(globs) = meta.get("exclude_globs").and_then(|v| v.as_array()) {
        // Merge with defaults — user can override entirely if desired.
        let user_globs: Vec<String> = globs
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
        if !user_globs.is_empty() {
            opts.exclude_globs = user_globs;
        }
    }
    if let Some(b) = meta.get("no_git_filter").and_then(|v| v.as_bool()) {
        opts.no_git_filter = b;
    }

    opts
}

/// Strip ```json ... ``` fences that LLMs often wrap their output in.
fn strip_markdown_fences(s: &str) -> String {
    let s = s.trim();
    // Strip leading ```json or ``` fence.
    let s = if let Some(rest) = s.strip_prefix("```json") {
        rest
    } else if let Some(rest) = s.strip_prefix("```") {
        rest
    } else {
        s
    };
    // Strip trailing ``` fence.
    let s = if let Some(rest) = s.strip_suffix("```") {
        rest
    } else {
        s
    };
    s.trim().to_string()
}

/// Extract a `Vec<String>` from a JSON object's array field.
fn json_str_array(v: &serde_json::Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.as_str().map(|x| x.to_string()))
                .collect()
        })
        .unwrap_or_default()
}
