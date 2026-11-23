//! Callimachus adapter for **binary-export bundles**: per function the
//! decompiled C and the CPU disassembly, plus referenced tables, hardware
//! ports and RAM locations. See `docs/binary-export.md` for the format.

pub mod bundle;
pub mod content;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow, bail};
use callimachus_adapter_code::{
    CodeAdapter, ExtractedCodeStructure, analyse_c, edge_id, entity_id, summarizer::PromptOptions,
};
use callimachus_adapter_contract::{
    Chunk, DiscoveredSource, Edge, Entity, EntityMerge, ExtractedContract, ExtractedPurpose,
    ExtractedSemantic, ExtractedStructure, LocationRef, SourceAdapter,
};
use callimachus_llm::LlmProvider;

pub use bundle::{Bundle, merge_platform_config};
use bundle::{lists_address, parse_address};

/// Confidence for entities that come straight from the bundle.
const BUNDLE_CONFIDENCE: f32 = 0.9;

pub struct BinExportAdapter {
    /// Parsed bundles keyed by bundle path. `discover` refreshes the entry; the
    /// bundle-level jsonl files are otherwise read once per adapter instance.
    by_path: Mutex<HashMap<PathBuf, Arc<Bundle>>>,
    /// corpus id → bundle path, recorded when chunking.
    by_corpus: Mutex<HashMap<String, PathBuf>>,
    code: CodeAdapter,
}

impl BinExportAdapter {
    pub fn new() -> Self {
        Self {
            by_path: Mutex::default(),
            by_corpus: Mutex::default(),
            code: CodeAdapter::with_prompt_options(PromptOptions {
                // `trim_for_prompt` applies the real budget (C 4,000 + asm 3,000).
                code_cap: Some(content::C_BUDGET + content::ASM_BUDGET + 200),
                disassembly_marker: Some(content::DISASSEMBLY_MARKER),
            }),
        }
    }

    fn load(&self, root: &Path, refresh: bool) -> Result<Arc<Bundle>> {
        let mut cache = self.by_path.lock().unwrap();
        if !refresh && let Some(b) = cache.get(root) {
            return Ok(b.clone());
        }
        let bundle = Arc::new(Bundle::load(root)?);
        cache.insert(root.to_path_buf(), bundle.clone());
        Ok(bundle)
    }

    fn bundle_for_corpus(&self, corpus_id: &str) -> Result<Arc<Bundle>> {
        let path = self
            .by_corpus
            .lock()
            .unwrap()
            .get(corpus_id)
            .cloned()
            .ok_or_else(|| {
                anyhow!(
                    "binary-export bundle for corpus {corpus_id:?} is not loaded; \
                     run the chunk pass in the same invocation"
                )
            })?;
        self.load(&path, false)
    }
}

impl Default for BinExportAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl SourceAdapter for BinExportAdapter {
    fn kind(&self) -> &str {
        bundle::BUNDLE_KIND
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn summary_levels(&self) -> Vec<&'static str> {
        vec!["function"]
    }

    async fn discover(&self, source: &str) -> Result<Vec<DiscoveredSource>> {
        let root = Path::new(source);
        if !root.is_dir() {
            bail!("binary-export source must be a bundle folder: {source}");
        }
        // Validates format/kind and refreshes the cached bundle.
        self.load(root, true)?;
        Ok(vec![DiscoveredSource {
            path: source.to_string(),
            kind: "bundle".to_string(),
            meta: serde_json::Value::Object(Default::default()),
        }])
    }

    async fn chunk(&self, source: &DiscoveredSource) -> Result<Vec<Chunk>> {
        let corpus_id = source
            .meta
            .get("corpus_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let root = PathBuf::from(&source.path);
        let bundle = self.load(&root, false)?;
        self.by_corpus
            .lock()
            .unwrap()
            .insert(corpus_id.clone(), root);

        let mut chunks = Vec::with_capacity(bundle.functions.len());
        for f in &bundle.functions {
            let address = parse_address(&f.address)?;
            let c_text = match (&f.c, f.decompile_ok) {
                (Some(rel), true) => Some(bundle.read_text(rel)?),
                _ => None,
            };
            let asm_text = match &f.asm {
                Some(rel) => Some(bundle.read_text(rel)?),
                None => None,
            };
            let text = content::build_content(f, address, c_text.as_deref(), asm_text.as_deref());
            let mut chunk = Chunk::new(
                corpus_id.clone(),
                None,
                "function".to_string(),
                callimachus_adapter_contract::Location::new(
                    corpus_id.clone(),
                    content::location_path(address),
                ),
                text,
            );
            chunk.language = Some("c".to_string());
            chunks.push(chunk);
        }
        Ok(chunks)
    }

    async fn extract_structure(&self, chunk: &Chunk) -> Result<ExtractedStructure> {
        let bundle = self.bundle_for_corpus(&chunk.corpus_id)?;
        let address = content::address_of_path(&chunk.location.path)
            .ok_or_else(|| anyhow!("not a function chunk: {}", chunk.location.path))?;
        let func = bundle
            .function_at(address)
            .ok_or_else(|| anyhow!("no function at 0x{address:x} in bundle"))?;
        let corpus = chunk.corpus_id.as_str();
        let canonical = format!("0x{address:x}");
        let from_id = entity_id(corpus, &func.name);

        let mut entities: Vec<Entity> = Vec::new();
        let mut edges: Vec<Edge> = Vec::new();

        let bundle_entity = |name: &str, kind: &str, aliases: Vec<String>, desc: Option<String>| {
            let mut e = Entity::new(
                entity_id(corpus, name),
                corpus.to_string(),
                name.to_string(),
                kind.to_string(),
            );
            e.aliases = aliases;
            e.description = desc;
            e.first_location = Some(chunk.location.clone());
            e.last_location = Some(chunk.location.clone());
            e.appearance_count = 1;
            e.confidence = BUNDLE_CONFIDENCE;
            e
        };
        let bundle_edge = |kind: &str, to_id: &str| {
            let mut e = Edge::new(
                edge_id(corpus, &from_id, kind, to_id, "production"),
                corpus.to_string(),
                from_id.clone(),
                to_id.to_string(),
                kind.to_string(),
                chunk.location.clone(),
            );
            e.origin_scope = "production".to_string();
            e
        };

        for t in bundle
            .tables
            .iter()
            .filter(|t| lists_address(&t.readers, address))
        {
            let mut aliases = vec![canonical_of(&t.address)?];
            if let Some(entry) = &t.entry {
                aliases.push(format!("entry:{entry}"));
            }
            if let Some(n) = t.count {
                aliases.push(format!("count:{n}"));
            }
            let desc = t.kind.as_deref().map(|k| format!("{k} table"));
            let e = bundle_entity(&t.name, "table", aliases, desc);
            edges.push(bundle_edge("uses_table", &e.id));
            entities.push(e);
        }
        for p in &bundle.ports {
            let reads = lists_address(&p.readers, address);
            let writes = lists_address(&p.writers, address);
            if !reads && !writes {
                continue;
            }
            let mut aliases = vec![canonical_of(&p.address)?];
            aliases.extend(p.port.clone());
            let e = bundle_entity(&p.name, "port", aliases, None);
            if reads {
                edges.push(bundle_edge("reads", &e.id));
            }
            if writes {
                edges.push(bundle_edge("writes", &e.id));
            }
            entities.push(e);
        }
        for r in &bundle.ram {
            let reads = lists_address(&r.readers, address);
            let writes = lists_address(&r.writers, address);
            if !reads && !writes {
                continue;
            }
            let e = bundle_entity(&r.name, "variable", vec![canonical_of(&r.address)?], None);
            if reads {
                edges.push(bundle_edge("reads", &e.id));
            }
            if writes {
                edges.push(bundle_edge("writes", &e.id));
            }
            entities.push(e);
        }

        // C analysis of the C part. The synthetic per-chunk `file` entity and
        // its `defines` edges carry no information here, so they are dropped.
        let (c_part, _) = content::split_content(&chunk.content);
        let analysed: ExtractedCodeStructure = analyse_c(chunk, c_part)?;
        let file_ids: Vec<String> = analysed
            .entities
            .iter()
            .filter(|e| e.kind == "file")
            .map(|e| e.id.clone())
            .collect();
        let mut seen_entities: std::collections::HashSet<String> =
            entities.iter().map(|e| e.id.clone()).collect();
        let mut seen_edges: std::collections::HashSet<String> =
            edges.iter().map(|e| e.id.clone()).collect();

        let mut function_seen = false;
        for mut e in analysed.entities.into_iter().filter(|e| e.kind != "file") {
            if e.id == from_id {
                function_seen = true;
                if !e.aliases.contains(&canonical) {
                    e.aliases.push(canonical.clone());
                }
            }
            if seen_entities.insert(e.id.clone()) {
                entities.push(e);
            }
        }
        if !function_seen {
            let mut e = bundle_entity(&func.name, "function", vec![canonical.clone()], None);
            e.confidence = 1.0;
            if seen_entities.insert(e.id.clone()) {
                entities.push(e);
            }
        }
        for e in analysed
            .edges
            .into_iter()
            .filter(|e| !(e.kind == "defines" && file_ids.contains(&e.from_entity_id)))
        {
            if seen_edges.insert(e.id.clone()) {
                edges.push(e);
            }
        }

        Ok(ExtractedStructure {
            parent_path: chunk.parent_path.clone(),
            child_paths: vec![],
            structural_entities: entities,
            structural_edges: edges,
        })
    }

    async fn extract_with_llm(
        &self,
        _chunk: &Chunk,
        _llm: &dyn LlmProvider,
    ) -> Result<Option<ExtractedSemantic>> {
        Ok(None)
    }

    async fn summarize(
        &self,
        chunk: &Chunk,
        llm: &dyn LlmProvider,
        depth: &str,
    ) -> Result<Option<String>> {
        if depth == "function" {
            let mut trimmed = chunk.clone();
            trimmed.content = content::trim_for_prompt(&chunk.content);
            return self.code.summarize(&trimmed, llm, depth).await;
        }
        self.code.summarize(chunk, llm, depth).await
    }

    async fn resolve_aliases(
        &self,
        _entities: &[Entity],
        _llm: &dyn LlmProvider,
    ) -> Result<Vec<EntityMerge>> {
        Ok(vec![])
    }

    fn format_location(&self, chunk: &Chunk) -> String {
        chunk.location.path.clone()
    }

    fn parse_location(&self, uri: &str) -> Result<LocationRef> {
        let (corpus_id, path) = match uri.strip_prefix("calli://") {
            Some(rest) => rest
                .split_once('/')
                .map(|(c, p)| (c.to_string(), p.to_string()))
                .ok_or_else(|| anyhow!("invalid calli URI: {uri}"))?,
            None => (String::new(), uri.to_string()),
        };
        if content::address_of_path(&path).is_none() {
            bail!("not a binary-export location (expected fn/<addr8>): {uri}");
        }
        Ok(LocationRef { corpus_id, path })
    }

    async fn extract_purpose(
        &self,
        entity: &Entity,
        content: &str,
        summary: Option<&str>,
        llm: &dyn LlmProvider,
    ) -> Result<Option<ExtractedPurpose>> {
        let trimmed = content::trim_for_prompt(content);
        self.code
            .extract_purpose(entity, &trimmed, summary, llm)
            .await
    }

    async fn extract_contract(
        &self,
        entity: &Entity,
        content: &str,
        summary: Option<&str>,
        purpose: Option<&str>,
        signals: &serde_json::Value,
        llm: &dyn LlmProvider,
    ) -> Result<Option<ExtractedContract>> {
        let trimmed = content::trim_for_prompt(content);
        self.code
            .extract_contract(entity, &trimmed, summary, purpose, signals, llm)
            .await
    }
}

fn canonical_of(address: &str) -> Result<String> {
    Ok(format!(
        "0x{:x}",
        parse_address(address).context("bundle address")?
    ))
}
