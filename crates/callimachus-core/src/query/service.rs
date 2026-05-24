use crate::corrections::CorrectionsEngine;
use crate::query::search;
use crate::query::types::*;
use crate::storage::{EdgeDirection, StorageBackend};
use crate::types::{Entity, Location, Scope, SummaryTargetKind, ToolResult};
use callimachus_llm::LlmProvider;
use std::collections::HashSet;
use std::str::FromStr;
use std::sync::Arc;

/// Central query service. Holds a shared storage backend.
/// All methods take `&self` and return `ToolResult<T>`.
pub struct QueryService {
    db: Arc<dyn StorageBackend>,
    corrections: Option<CorrectionsEngine>,
    /// Optional embedding provider for semantic/hybrid search.
    /// When `None`, semantic and hybrid modes fall back to keyword search.
    embedder: Option<Arc<dyn LlmProvider>>,
}

impl QueryService {
    pub fn new(db: Arc<dyn StorageBackend>) -> Self {
        Self {
            db,
            corrections: None,
            embedder: None,
        }
    }

    /// Construct with a pre-loaded corrections engine. All entity-returning methods
    /// will apply corrections as an in-memory overlay before returning results.
    pub fn with_corrections(db: Arc<dyn StorageBackend>, corrections: CorrectionsEngine) -> Self {
        Self {
            db,
            corrections: Some(corrections),
            embedder: None,
        }
    }

    /// Attach an embedding provider for semantic and hybrid search.
    pub fn with_embedder(mut self, embedder: Arc<dyn LlmProvider>) -> Self {
        self.embedder = Some(embedder);
        self
    }

    /// Access the underlying storage backend (needed for constructing CollectionService).
    pub fn backend(&self) -> Arc<dyn StorageBackend> {
        Arc::clone(&self.db)
    }

    fn try_result<T, F: FnOnce() -> anyhow::Result<ToolResult<T>>>(&self, f: F) -> ToolResult<T> {
        match f() {
            Ok(result) => result,
            Err(e) => ToolResult::error("internal_error", e.to_string(), false),
        }
    }

    // ── corpus_list ─────────────────────────────────────────────────────────

    pub fn corpus_list(&self, _input: CorpusListInput) -> ToolResult<Vec<CorpusListEntry>> {
        self.try_result(|| {
            let corpora = self.db.corpus_list()?;
            let mut entries = Vec::with_capacity(corpora.len());
            for c in corpora {
                let chunk_count = self.db.chunk_count(&c.id).unwrap_or(0);
                let entity_count = self.db.entity_count(&c.id).unwrap_or(0);
                entries.push(CorpusListEntry {
                    id: c.id,
                    name: c.name,
                    kind: c.kind,
                    last_indexed: c.last_indexed_at,
                    chunk_count,
                    entity_count,
                });
            }
            Ok(ToolResult::ok(entries))
        })
    }

    // ── corpus_overview ──────────────────────────────────────────────────────

    pub fn corpus_overview(&self, input: CorpusOverviewInput) -> ToolResult<CorpusOverviewOutput> {
        self.try_result(|| {
            let corpus = match self.db.corpus_get(&input.corpus_id)? {
                Some(c) => c,
                None => {
                    return Ok(ToolResult::not_found(Some(vec![format!(
                        "No corpus with id '{}'",
                        input.corpus_id
                    )])));
                }
            };
            let chunk_count = self.db.chunk_count(&corpus.id).unwrap_or(0);
            let entity_count = self.db.entity_count(&corpus.id).unwrap_or(0);
            let mut top_entities = self.db.entity_top(&corpus.id, 10)?;
            if let Some(engine) = &self.corrections {
                engine.apply_to_entities(&mut top_entities);
                top_entities.truncate(10);
            }
            let summary_row =
                self.db
                    .summary_get(&corpus.id, &SummaryTargetKind::Corpus, &corpus.id)?;
            let summary = self
                .corrections
                .as_ref()
                .and_then(|e| e.override_summary("corpus", &corpus.id))
                .map(str::to_string)
                .or_else(|| summary_row.map(|s| s.text));
            Ok(ToolResult::ok(CorpusOverviewOutput {
                id: corpus.id.clone(),
                name: corpus.name.clone(),
                kind: corpus.kind.clone(),
                status: corpus.status.to_string(),
                chunk_count,
                entity_count,
                last_indexed: corpus.last_indexed_at.clone(),
                top_entities,
                summary,
            }))
        })
    }

    // ── search ───────────────────────────────────────────────────────────────

    pub fn search(&self, input: SearchInput) -> ToolResult<SearchOutput> {
        let embedder = self.embedder.clone();
        let db = self.db.as_ref();
        self.try_result(|| {
            let limit = input.limit.unwrap_or(20) as usize;
            let scope_ref: Option<&Scope> = input.scope.as_ref();
            let alpha = input.semantic_weight.unwrap_or(0.5).clamp(0.0, 1.0);

            let results = match &input.mode {
                SearchMode::Keyword => {
                    search::keyword_search(db, &input.corpus_id, &input.query, scope_ref, limit)?
                }
                SearchMode::Semantic => {
                    match &embedder {
                        Some(emb) if emb.supports_embeddings() => {
                            let vector = tokio::task::block_in_place(|| {
                                tokio::runtime::Handle::current()
                                    .block_on(emb.embed(&input.query))
                            });
                            match vector {
                                Ok(v) => search::semantic_search(
                                    db, &input.corpus_id, &v, scope_ref, limit,
                                )?,
                                Err(e) => {
                                    tracing::warn!(
                                        error = %e,
                                        "semantic search: embed failed; falling back to keyword"
                                    );
                                    search::keyword_search(
                                        db, &input.corpus_id, &input.query, scope_ref, limit,
                                    )?
                                }
                            }
                        }
                        _ => {
                            tracing::warn!(
                                "semantic search requested but no embedder configured; falling back to keyword"
                            );
                            search::keyword_search(
                                db, &input.corpus_id, &input.query, scope_ref, limit,
                            )?
                        }
                    }
                }
                SearchMode::Hybrid => {
                    match &embedder {
                        Some(emb) if emb.supports_embeddings() => {
                            let vector = tokio::task::block_in_place(|| {
                                tokio::runtime::Handle::current()
                                    .block_on(emb.embed(&input.query))
                            });
                            match vector {
                                Ok(v) => search::hybrid_search(
                                    db, &input.corpus_id, &input.query, &v, scope_ref, limit, alpha,
                                )?,
                                Err(e) => {
                                    tracing::warn!(
                                        error = %e,
                                        "hybrid search: embed failed; falling back to keyword"
                                    );
                                    search::keyword_search(
                                        db, &input.corpus_id, &input.query, scope_ref, limit,
                                    )?
                                }
                            }
                        }
                        _ => {
                            tracing::warn!(
                                "hybrid search requested but no embedder configured; falling back to keyword"
                            );
                            search::keyword_search(
                                db, &input.corpus_id, &input.query, scope_ref, limit,
                            )?
                        }
                    }
                }
            };

            let total = results.len();
            Ok(ToolResult::ok(SearchOutput { results, total }))
        })
    }

    // ── entity ───────────────────────────────────────────────────────────────

    pub fn entity(&self, input: EntityInput) -> ToolResult<Entity> {
        self.try_result(|| {
            // Resolve the requested id through merge corrections before DB lookup.
            let resolved_id = self
                .corrections
                .as_ref()
                .map(|e| e.resolve_entity_id(&input.name_or_id).to_string())
                .unwrap_or_else(|| input.name_or_id.clone());

            // Try by (resolved) ID first.
            if let Some(e) = self.db.entity_get_by_id(&resolved_id)? {
                let mut entities = vec![e];
                if let Some(engine) = &self.corrections {
                    engine.apply_to_entities(&mut entities);
                }
                if let Some(e) = entities.into_iter().next() {
                    return Ok(ToolResult::ok(e));
                }
            }
            // Try by name.
            let mut matches = self
                .db
                .entity_find_by_name(&input.corpus_id, &input.name_or_id)?;
            if let Some(engine) = &self.corrections {
                engine.apply_to_entities(&mut matches);
            }
            match matches.len() {
                0 => {
                    // Fuzzy suggestions via entity list filter.
                    let suggestions = self.fuzzy_entity_names(&input.corpus_id, &input.name_or_id);
                    Ok(ToolResult::not_found(Some(suggestions)))
                }
                1 => Ok(ToolResult::ok(matches.into_iter().next().unwrap())),
                _ => {
                    let candidates = matches.iter().map(|e| e.canonical_name.clone()).collect();
                    Ok(ToolResult::ambiguous(candidates))
                }
            }
        })
    }

    // ── entity_edges ─────────────────────────────────────────────────────────

    pub fn entity_edges(&self, input: EntityEdgesInput) -> ToolResult<EntityEdgesOutput> {
        self.try_result(|| {
            let direction = match EdgeDirection::from_str(&input.direction) {
                Ok(d) => d,
                Err(e) => {
                    return Ok(ToolResult::invalid_input(format!(
                        "invalid direction '{}': {}",
                        input.direction, e
                    )));
                }
            };
            let limit = input.limit.unwrap_or(50) as usize;
            let kind = input.kind.as_deref();
            let edges = self
                .db
                .edge_get_for_entity(&input.entity_id, direction, kind, limit)?;
            let count = edges.len();
            Ok(ToolResult::ok(EntityEdgesOutput {
                entity_id: input.entity_id,
                edges,
                count,
            }))
        })
    }

    // ── entity_meet ──────────────────────────────────────────────────────────

    pub fn entity_meet(&self, input: EntityMeetInput) -> ToolResult<EntityMeetOutput> {
        self.try_result(|| {
            // Resolve entity A.
            let id_a = match self.resolve_entity_id(&input.corpus_id, &input.entity_a)? {
                Some(id) => id,
                None => {
                    return Ok(ToolResult::not_found(Some(vec![format!(
                        "Entity '{}' not found",
                        input.entity_a
                    )])));
                }
            };
            // Resolve entity B.
            let id_b = match self.resolve_entity_id(&input.corpus_id, &input.entity_b)? {
                Some(id) => id,
                None => {
                    return Ok(ToolResult::not_found(Some(vec![format!(
                        "Entity '{}' not found",
                        input.entity_b
                    )])));
                }
            };

            // Find all location_uris for entity A.
            let locs_a = self.db.edge_location_uris_for_entity(&id_a)?;
            // Find all location_uris for entity B.
            let locs_b = self.db.edge_location_uris_for_entity(&id_b)?;

            // Intersect.
            let set_b: HashSet<&str> = locs_b.iter().map(|s| s.as_str()).collect();
            let mut common: Vec<String> = locs_a
                .into_iter()
                .filter(|l| set_b.contains(l.as_str()))
                .collect();

            if common.is_empty() {
                return Ok(ToolResult::not_found(Some(vec![format!(
                    "Entities '{}' and '{}' do not co-occur in any chunk",
                    input.entity_a, input.entity_b
                )])));
            }

            // Lexicographic sort → first is minimum.
            common.sort_unstable();

            let count = common.len() as u32;
            let first_uri = common[0].clone();
            let first_co_occurrence = Location::parse(&first_uri).unwrap_or_else(|_| Location {
                corpus_id: input.corpus_id.clone(),
                path: first_uri.clone(),
                uri: first_uri.clone(),
            });

            let all: Vec<Location> = common
                .into_iter()
                .map(|uri| {
                    Location::parse(&uri).unwrap_or_else(|_| Location {
                        corpus_id: input.corpus_id.clone(),
                        path: uri.clone(),
                        uri: uri.clone(),
                    })
                })
                .collect();

            Ok(ToolResult::ok(EntityMeetOutput {
                first_co_occurrence,
                all,
                count,
            }))
        })
    }

    // ── read ─────────────────────────────────────────────────────────────────

    pub fn read(&self, input: ReadInput) -> ToolResult<ReadOutput> {
        self.try_result(|| {
            let chunk = match self.db.chunk_get_by_uri(&input.location)? {
                Some(c) => c,
                None => {
                    return Ok(ToolResult::not_found(Some(vec![format!(
                        "No chunk at location '{}'",
                        input.location
                    )])));
                }
            };

            let corpus_id = &chunk.corpus_id;
            let uri = &chunk.location.uri;

            // Summary: look up in summary_store; let corrections override if set.
            let db_summary = self
                .db
                .summary_get(corpus_id, &SummaryTargetKind::Chunk, uri)?
                .map(|s| s.text);
            let summary = self
                .corrections
                .as_ref()
                .and_then(|e| e.override_summary("chunk", uri))
                .map(str::to_string)
                .or(db_summary);

            // Content: only if depth=Full.
            let content = match input.depth {
                ReadDepth::Full => Some(chunk.content.clone()),
                _ => None,
            };

            // Entities present: entities whose first or last location is this URI.
            let mut entities_present = self.db.entities_at_location(corpus_id, uri)?;
            if let Some(engine) = &self.corrections {
                engine.apply_to_entities(&mut entities_present);
            }

            // Child locations: chunks whose parent_path = this URI.
            let child_locations = self.db.chunk_children_by_uri(corpus_id, uri)?;

            Ok(ToolResult::ok(ReadOutput {
                location: chunk.location.clone(),
                kind: chunk.kind.clone(),
                summary,
                content,
                entities_present,
                child_locations,
            }))
        })
    }

    // ── summarize ────────────────────────────────────────────────────────────

    pub fn summarize(&self, input: SummarizeInput) -> ToolResult<SummarizeOutput> {
        self.try_result(|| {
            let (target_kind, target_id) = match &input.target {
                SummarizeTarget::Corpus => (SummaryTargetKind::Corpus, input.corpus_id.clone()),
                SummarizeTarget::Entity { entity_id } => {
                    (SummaryTargetKind::Entity, entity_id.clone())
                }
                SummarizeTarget::Location { location } => {
                    (SummaryTargetKind::Chunk, location.clone())
                }
                SummarizeTarget::Range { from, to: _ } => {
                    // For range, use the 'from' location as the target_id with kind=range.
                    (SummaryTargetKind::Range, from.clone())
                }
            };

            let target_kind_str = target_kind.to_string();
            // Check for an operator edit_summary correction first.
            if let Some(override_text) = self
                .corrections
                .as_ref()
                .and_then(|e| e.override_summary(&target_kind_str, &target_id))
            {
                return Ok(ToolResult::ok(SummarizeOutput {
                    text: override_text.to_string(),
                    generated_at: String::new(),
                    model: None,
                }));
            }

            match self
                .db
                .summary_get(&input.corpus_id, &target_kind, &target_id)?
            {
                Some(s) => Ok(ToolResult::ok(SummarizeOutput {
                    text: s.text,
                    generated_at: s.generated_at,
                    model: Some(s.model),
                })),
                None => Ok(ToolResult::not_found(Some(vec![
                    "Run `calli index <corpus_id> --pass=summarize` to generate summaries"
                        .to_string(),
                ]))),
            }
        })
    }

    // ── related ──────────────────────────────────────────────────────────────

    pub fn related(&self, input: RelatedInput) -> ToolResult<RelatedOutput> {
        self.try_result(|| {
            // Get entity set for the target chunk.
            let target_entity_ids: HashSet<String> = self
                .db
                .edge_entity_ids_at_location(&input.location)?
                .into_iter()
                .collect();
            if target_entity_ids.is_empty() {
                return Ok(ToolResult::ok(RelatedOutput { items: vec![] }));
            }

            // Get all chunks in the corpus.
            let corpus_id = input.corpus_id.as_str();
            let all_chunks = self.db.chunk_list(corpus_id)?;
            let limit = input.limit.unwrap_or(10) as usize;

            let mut scored: Vec<(f32, String)> = Vec::new();
            for chunk in &all_chunks {
                if chunk.location.uri == input.location {
                    continue;
                }
                let chunk_entity_ids: HashSet<String> = self
                    .db
                    .edge_entity_ids_at_location(&chunk.location.uri)?
                    .into_iter()
                    .collect();
                let score = jaccard(&target_entity_ids, &chunk_entity_ids);
                if score > 0.0 {
                    scored.push((score, chunk.location.uri.clone()));
                }
            }

            // Sort by score descending.
            scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            scored.truncate(limit);

            let items = scored
                .into_iter()
                .map(|(score, uri)| {
                    let location = Location::parse(&uri).unwrap_or_else(|_| Location {
                        corpus_id: corpus_id.to_string(),
                        path: uri.clone(),
                        uri: uri.clone(),
                    });
                    RelatedItem {
                        location,
                        relationship: "shared_entities".to_string(),
                        score,
                    }
                })
                .collect();

            Ok(ToolResult::ok(RelatedOutput { items }))
        })
    }

    // ── chapter_summary (composite) ──────────────────────────────────────────

    pub fn chapter_summary(&self, input: ChapterSummaryInput) -> ToolResult<ReadOutput> {
        let uri = self.try_result(|| {
            let uri = self.resolve_chapter_uri(&input.corpus_id, &input.chapter)?;
            Ok(ToolResult::ok(uri))
        });

        let uri_opt = match uri {
            ToolResult::Ok(s) => s.data,
            other => {
                return match other {
                    ToolResult::Err(e) => ToolResult::Err(e),
                    ToolResult::Ok(_) => unreachable!(),
                };
            }
        };

        match uri_opt {
            Some(location) => self.read(ReadInput {
                corpus_id: Some(input.corpus_id),
                location,
                depth: ReadDepth::Summary,
            }),
            None => ToolResult::not_found(Some(vec![format!(
                "Chapter '{}' not found in corpus '{}'",
                input.chapter, input.corpus_id
            )])),
        }
    }

    // ── character_profile (composite) ────────────────────────────────────────

    pub fn character_profile(
        &self,
        input: CharacterProfileInput,
    ) -> ToolResult<CharacterProfileOutput> {
        // Step 1: look up the entity.
        let entity = match self.entity(EntityInput {
            corpus_id: input.corpus_id.clone(),
            name_or_id: input.name.clone(),
        }) {
            ToolResult::Ok(s) => s.data,
            other => {
                return match other {
                    ToolResult::Err(e) => ToolResult::Err(e),
                    ToolResult::Ok(_) => unreachable!(),
                };
            }
        };

        // Step 2: get all edges.
        let edges_result = self.entity_edges(EntityEdgesInput {
            corpus_id: input.corpus_id.clone(),
            entity_id: entity.id.clone(),
            direction: "both".to_string(),
            kind: None,
            limit: Some(100),
        });
        let edges = match edges_result {
            ToolResult::Ok(s) => s.data.edges,
            _ => vec![],
        };

        // Step 3: get entity summary (best-effort).
        let summary = match self.summarize(SummarizeInput {
            corpus_id: input.corpus_id.clone(),
            target: SummarizeTarget::Entity {
                entity_id: entity.id.clone(),
            },
        }) {
            ToolResult::Ok(s) => Some(s.data.text),
            _ => None,
        };

        ToolResult::ok(CharacterProfileOutput {
            entity,
            edges,
            summary,
        })
    }

    // ── find_scene (composite) ───────────────────────────────────────────────

    pub fn find_scene(&self, input: FindSceneInput) -> ToolResult<FindSceneOutput> {
        // Step 1: entity_meet → first co-occurrence.
        let meet = match self.entity_meet(EntityMeetInput {
            corpus_id: input.corpus_id.clone(),
            entity_a: input.entity_a.clone(),
            entity_b: input.entity_b.clone(),
        }) {
            ToolResult::Ok(s) => s.data,
            other => {
                return match other {
                    ToolResult::Err(e) => ToolResult::Err(e),
                    ToolResult::Ok(_) => unreachable!(),
                };
            }
        };

        // Step 2: read the scene at that location.
        let read_out = match self.read(ReadInput {
            corpus_id: Some(input.corpus_id),
            location: meet.first_co_occurrence.uri.clone(),
            depth: ReadDepth::Full,
        }) {
            ToolResult::Ok(s) => s.data,
            other => {
                return match other {
                    ToolResult::Err(e) => ToolResult::Err(e),
                    ToolResult::Ok(_) => unreachable!(),
                };
            }
        };

        ToolResult::ok(FindSceneOutput {
            location: read_out.location,
            content: read_out.content.unwrap_or_default(),
            entities_present: read_out.entities_present,
        })
    }

    // ── private helpers ──────────────────────────────────────────────────────

    /// Attempt to resolve an entity name/id to a canonical entity ID.
    fn resolve_entity_id(
        &self,
        corpus_id: &str,
        name_or_id: &str,
    ) -> anyhow::Result<Option<String>> {
        if let Some(e) = self.db.entity_get_by_id(name_or_id)? {
            return Ok(Some(e.id));
        }
        let matches = self.db.entity_find_by_name(corpus_id, name_or_id)?;
        Ok(matches.into_iter().next().map(|e| e.id))
    }

    /// Fuzzy entity name suggestions (in-memory LIKE filter).
    fn fuzzy_entity_names(&self, corpus_id: &str, query: &str) -> Vec<String> {
        let pattern = query.to_lowercase();
        self.db
            .entity_list(corpus_id)
            .unwrap_or_default()
            .into_iter()
            .filter(|e| e.canonical_name.to_lowercase().contains(&pattern))
            .take(5)
            .map(|e| e.canonical_name)
            .collect()
    }

    /// Resolve a chapter descriptor ("3", "Three", or title) to a location URI.
    fn resolve_chapter_uri(
        &self,
        corpus_id: &str,
        chapter: &str,
    ) -> anyhow::Result<Option<String>> {
        // Try as integer chapter number.
        if let Ok(n) = chapter.trim().parse::<u32>() {
            let uri = format!("calli://{}/ch/{}", corpus_id, n);
            if self.db.chunk_get_by_uri(&uri)?.is_some() {
                return Ok(Some(uri));
            }
        }

        // Try ordinal words (one..twenty).
        if let Some(n) = word_to_chapter_number(chapter) {
            let uri = format!("calli://{}/ch/{}", corpus_id, n);
            if self.db.chunk_get_by_uri(&uri)?.is_some() {
                return Ok(Some(uri));
            }
        }

        // Fuzzy title match via FTS.
        let results = self.db.fts_search(corpus_id, chapter, 5)?;
        for r in &results {
            if let Some(chunk) = self.db.chunk_get_by_uri(&r.location_uri)?
                && chunk.kind == "chapter"
            {
                return Ok(Some(r.location_uri.clone()));
            }
        }

        Ok(None)
    }

    // ── Phase 12 query methods ────────────────────────────────────────────────

    /// Retrieve the contract and purpose for a specific entity.
    pub fn entity_contracts(
        &self,
        input: EntityContractsInput,
    ) -> ToolResult<EntityContractsOutput> {
        self.try_result(|| {
            let contract = match self.db.contract_get(&input.corpus_id, &input.entity_id)? {
                Some(c) => c,
                None => {
                    return Ok(ToolResult::not_found(Some(vec![format!(
                        "No contract for entity '{}'",
                        input.entity_id
                    )])));
                }
            };
            let purpose = self
                .db
                .purpose_get(&input.corpus_id, &input.entity_id)
                .ok()
                .flatten();
            let verified_by = self
                .db
                .edge_get_for_entity(
                    &input.entity_id,
                    EdgeDirection::Inbound,
                    Some("verified_by"),
                    100,
                )
                .unwrap_or_default();
            Ok(ToolResult::ok(EntityContractsOutput {
                entity_id: input.entity_id,
                contract,
                purpose,
                verified_by,
            }))
        })
    }

    /// Find entities whose contracts signal inconsistency or technical debt.
    pub fn find_inconsistencies(
        &self,
        input: FindInconsistenciesInput,
    ) -> ToolResult<FindInconsistenciesOutput> {
        self.try_result(|| {
            let contracts = self.db.contract_list_inconsistencies(&input.corpus_id)?;
            let count = contracts.len();
            Ok(ToolResult::ok(FindInconsistenciesOutput {
                contracts,
                count,
            }))
        })
    }

    /// Find entities with no inbound `calls` edges (potentially unreachable code).
    pub fn find_unreachable(
        &self,
        input: FindUnreachableInput,
    ) -> ToolResult<FindUnreachableOutput> {
        self.try_result(|| {
            let entities = self.db.entities_without_inbound_calls(&input.corpus_id)?;
            let count = entities.len();
            Ok(ToolResult::ok(FindUnreachableOutput { entities, count }))
        })
    }

    /// List corpus-level architectural themes, optionally with their edge sets.
    pub fn corpus_themes(&self, input: CorpusThemesInput) -> ToolResult<CorpusThemesOutput> {
        self.try_result(|| {
            let themes = self.db.theme_list(&input.corpus_id)?;
            let include = input.include_edges.unwrap_or(false);

            let mut upheld_by = Vec::new();
            let mut violated_by = Vec::new();

            if include {
                for theme in &themes {
                    let up = self
                        .db
                        .edge_get_for_entity(
                            &theme.id,
                            EdgeDirection::Inbound,
                            Some("upheld_by"),
                            200,
                        )
                        .unwrap_or_default();
                    upheld_by.extend(up);

                    let viol = self
                        .db
                        .edge_get_for_entity(
                            &theme.id,
                            EdgeDirection::Inbound,
                            Some("violated_by"),
                            200,
                        )
                        .unwrap_or_default();
                    violated_by.extend(viol);
                }
            }

            Ok(ToolResult::ok(CorpusThemesOutput {
                themes,
                upheld_by,
                violated_by,
            }))
        })
    }

    /// Find entities with no inbound `verified_by` edges (no test coverage).
    pub fn entities_without_tests(
        &self,
        input: EntitiesWithoutTestsInput,
    ) -> ToolResult<EntitiesWithoutTestsOutput> {
        self.try_result(|| {
            let entities = self.db.entities_without_verified_by(&input.corpus_id)?;
            let count = entities.len();
            Ok(ToolResult::ok(EntitiesWithoutTestsOutput {
                entities,
                count,
            }))
        })
    }

    /// Assemble a narrative explanation of a component via BFS on `calls` edges.
    /// No LLM calls — pure retrieval and assembly from pre-indexed data.
    pub fn explain_component(
        &self,
        input: ExplainComponentInput,
    ) -> ToolResult<ExplainComponentOutput> {
        self.try_result(|| {
            let max_depth = input.max_depth.unwrap_or(3);

            // Seed entities.
            let seeds: Vec<Entity> = if let Some(eid) = &input.entity_id {
                self.db
                    .entity_get_by_id(eid)?
                    .map(|e| vec![e])
                    .unwrap_or_default()
            } else if let Some(prefix) = &input.module_prefix {
                self.db.entity_find_by_name(&input.corpus_id, prefix)?
            } else {
                return Ok(ToolResult::error(
                    "invalid_input",
                    "provide entity_id or module_prefix",
                    false,
                ));
            };

            if seeds.is_empty() {
                return Ok(ToolResult::not_found(Some(vec![
                    "No seed entity found".to_string(),
                ])));
            }

            // BFS.
            let mut visited_ids: HashSet<String> = HashSet::new();
            let mut queue: std::collections::VecDeque<(Entity, u8)> =
                seeds.into_iter().map(|e| (e, 0u8)).collect();
            let mut nodes: Vec<ExplainNode> = Vec::new();

            while let Some((entity, depth)) = queue.pop_front() {
                if visited_ids.contains(&entity.id) {
                    continue;
                }
                visited_ids.insert(entity.id.clone());

                // Collect purpose + summary + blocks.
                let purpose_text = self
                    .db
                    .purpose_get(&input.corpus_id, &entity.id)
                    .ok()
                    .flatten()
                    .map(|p| p.purpose);

                let summary_text = self
                    .db
                    .summary_get(&input.corpus_id, &SummaryTargetKind::Entity, &entity.id)
                    .ok()
                    .flatten()
                    .map(|s| s.text);

                let blocks: Vec<ExplainBlock> = self
                    .db
                    .block_list_for_entity(&entity.id)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|b| ExplainBlock {
                        label: b.label,
                        description: b.description,
                    })
                    .collect();

                nodes.push(ExplainNode {
                    entity_id: entity.id.clone(),
                    name: entity.canonical_name.clone(),
                    kind: entity.kind.clone(),
                    purpose: purpose_text,
                    summary: summary_text,
                    blocks,
                    depth,
                });

                // Expand outbound `calls` edges if depth allows.
                if depth < max_depth {
                    let callees = self
                        .db
                        .edge_get_for_entity(&entity.id, EdgeDirection::Outbound, Some("calls"), 50)
                        .unwrap_or_default();

                    for edge in callees {
                        if let Ok(Some(callee)) = self.db.entity_get_by_id(&edge.to_entity_id)
                            && !visited_ids.contains(&callee.id)
                        {
                            queue.push_back((callee, depth + 1));
                        }
                    }
                }
            }

            // Assemble narrative — prepend Diegesis header using seed entity's name.
            let seed_name = nodes
                .first()
                .map(|n| n.name.as_str())
                .unwrap_or("Component");
            let mut narrative = format!("# Diegesis of {seed_name}\n\n");
            for node in &nodes {
                let indent = "  ".repeat(node.depth as usize);
                narrative.push_str(&format!("{indent}## {} (`{}`)\n", node.name, node.kind));
                if let Some(p) = &node.purpose {
                    narrative.push_str(&format!("{indent}{p}\n"));
                }
                if let Some(s) = &node.summary {
                    narrative.push_str(&format!("{indent}{s}\n"));
                }
                for (i, b) in node.blocks.iter().enumerate() {
                    narrative.push_str(&format!(
                        "{indent}  {}. {}: {}\n",
                        i + 1,
                        b.label,
                        b.description
                    ));
                }
                narrative.push('\n');
            }

            Ok(ToolResult::ok(ExplainComponentOutput {
                narrative: narrative.trim_end().to_string(),
                nodes,
            }))
        })
    }

    /// List entities across corpora filtered by abstract taxonomy kind.
    pub fn entity_search_by_abstract_kind(
        &self,
        input: EntitySearchByAbstractKindInput,
    ) -> ToolResult<EntitySearchByAbstractKindOutput> {
        self.try_result(|| {
            let corpus_id_refs: Vec<&str> = input.corpus_ids.iter().map(String::as_str).collect();
            let all_entities = self
                .db
                .entity_list_by_abstract_kind(&corpus_id_refs, &input.abstract_kind)?;
            let limit = input.limit.unwrap_or(50);
            let entities: Vec<Entity> = all_entities.into_iter().take(limit).collect();
            let count = entities.len();
            Ok(ToolResult::ok(EntitySearchByAbstractKindOutput {
                entities,
                count,
            }))
        })
    }

    /// Return the full kind_taxonomy table.
    pub fn list_abstract_kinds(
        &self,
        _input: ListAbstractKindsInput,
    ) -> ToolResult<ListAbstractKindsOutput> {
        self.try_result(|| {
            let raw = self.db.kind_taxonomy_list()?;
            let rows: Vec<TaxonomyRow> = raw
                .into_iter()
                .map(|(concrete_kind, corpus_kind, abstract_kind)| TaxonomyRow {
                    concrete_kind,
                    corpus_kind,
                    abstract_kind,
                })
                .collect();
            let count = rows.len();
            Ok(ToolResult::ok(ListAbstractKindsOutput { rows, count }))
        })
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

/// Jaccard similarity between two entity ID sets.
fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let intersection = a.intersection(b).count();
    let union = a.union(b).count();
    if union == 0 {
        return 0.0;
    }
    intersection as f32 / union as f32
}

fn word_to_chapter_number(word: &str) -> Option<u32> {
    match word.to_lowercase().as_str() {
        "one" => Some(1),
        "two" => Some(2),
        "three" => Some(3),
        "four" => Some(4),
        "five" => Some(5),
        "six" => Some(6),
        "seven" => Some(7),
        "eight" => Some(8),
        "nine" => Some(9),
        "ten" => Some(10),
        "eleven" => Some(11),
        "twelve" => Some(12),
        "thirteen" => Some(13),
        "fourteen" => Some(14),
        "fifteen" => Some(15),
        "sixteen" => Some(16),
        "seventeen" => Some(17),
        "eighteen" => Some(18),
        "nineteen" => Some(19),
        "twenty" => Some(20),
        _ => None,
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::SqliteBackend;
    use crate::types::{Chunk, Corpus, Edge, Entity, Location, Summary, SummaryTargetKind};

    fn make_service() -> (QueryService, Arc<dyn StorageBackend>) {
        let db: Arc<dyn StorageBackend> = Arc::new(SqliteBackend::open_in_memory().unwrap());
        let svc = QueryService::new(Arc::clone(&db));
        (svc, db)
    }

    fn seed_corpus(db: &dyn StorageBackend, id: &str) {
        let corpus = Corpus::new(
            id.into(),
            format!("Corpus {id}"),
            "book".into(),
            "/tmp".into(),
        );
        db.corpus_insert(&corpus).unwrap();
    }

    fn seed_chunk(
        db: &dyn StorageBackend,
        corpus_id: &str,
        path: &str,
        content: &str,
        kind: &str,
        parent: Option<&str>,
    ) -> Chunk {
        let loc = Location::new(corpus_id, path);
        let parent_uri = parent.map(|p| Location::new(corpus_id, p).uri);
        let chunk = Chunk::new(
            corpus_id.into(),
            parent_uri,
            kind.into(),
            loc,
            content.into(),
        );
        db.chunk_upsert(&chunk).unwrap();
        chunk
    }

    fn seed_entity(
        db: &dyn StorageBackend,
        corpus_id: &str,
        id: &str,
        name: &str,
        first_uri: Option<&str>,
    ) -> Entity {
        let mut e = Entity::new(id.into(), corpus_id.into(), name.into(), "character".into());
        e.appearance_count = 1;
        e.first_location = first_uri.and_then(|u| Location::parse(u).ok());
        e.last_location = first_uri.and_then(|u| Location::parse(u).ok());
        db.entity_upsert(&e).unwrap();
        e
    }

    fn seed_edge(
        db: &dyn StorageBackend,
        corpus_id: &str,
        id: &str,
        from: &str,
        to: &str,
        location_uri: &str,
        kind: &str,
    ) -> Edge {
        let loc = Location::parse(location_uri).unwrap();
        let edge = Edge::new(
            id.into(),
            corpus_id.into(),
            from.into(),
            to.into(),
            kind.into(),
            loc,
        );
        db.edge_upsert(&edge).unwrap();
        edge
    }

    // ── corpus_list ────────────────────────────────────────────────────────

    #[test]
    fn corpus_list_returns_all() {
        let (svc, db) = make_service();
        seed_corpus(db.as_ref(), "c1");
        seed_corpus(db.as_ref(), "c2");
        seed_chunk(db.as_ref(), "c1", "ch/1", "hello", "chapter", None);

        let result = svc.corpus_list(CorpusListInput::default());
        let ToolResult::Ok(s) = result else {
            panic!("expected Ok")
        };
        assert_eq!(s.data.len(), 2);
        let c1 = s.data.iter().find(|e| e.id == "c1").unwrap();
        assert_eq!(c1.chunk_count, 1);
    }

    // ── search ────────────────────────────────────────────────────────────

    #[test]
    fn search_returns_matching_chunks() {
        let (svc, db) = make_service();
        seed_corpus(db.as_ref(), "c1");
        seed_chunk(
            db.as_ref(),
            "c1",
            "ch/1",
            "Frodo found the ring",
            "chapter",
            None,
        );
        seed_chunk(
            db.as_ref(),
            "c1",
            "ch/2",
            "Samwise planted potatoes",
            "chapter",
            None,
        );
        seed_chunk(
            db.as_ref(),
            "c1",
            "ch/3",
            "Frodo carried the burden",
            "chapter",
            None,
        );
        db.fts_rebuild("c1").unwrap();

        let result = svc.search(SearchInput {
            corpus_id: "c1".into(),
            query: "Frodo".into(),
            mode: SearchMode::Keyword,
            scope: None,
            limit: Some(10),
            semantic_weight: None,
        });
        let ToolResult::Ok(s) = result else {
            panic!("expected Ok")
        };
        assert_eq!(s.data.results.len(), 2);
    }

    // ── entity ────────────────────────────────────────────────────────────

    #[test]
    fn entity_exact_name_match() {
        let (svc, db) = make_service();
        seed_corpus(db.as_ref(), "c1");
        seed_entity(db.as_ref(), "c1", "e1", "Gandalf", None);

        let result = svc.entity(EntityInput {
            corpus_id: "c1".into(),
            name_or_id: "Gandalf".into(),
        });
        let ToolResult::Ok(s) = result else {
            panic!("expected Ok")
        };
        assert_eq!(s.data.canonical_name, "Gandalf");
    }

    #[test]
    fn entity_by_id() {
        let (svc, db) = make_service();
        seed_corpus(db.as_ref(), "c1");
        seed_entity(db.as_ref(), "c1", "e-gandalf", "Gandalf", None);

        let result = svc.entity(EntityInput {
            corpus_id: "c1".into(),
            name_or_id: "e-gandalf".into(),
        });
        let ToolResult::Ok(s) = result else {
            panic!("expected Ok")
        };
        assert_eq!(s.data.id, "e-gandalf");
    }

    #[test]
    fn entity_not_found_returns_suggestions() {
        let (svc, db) = make_service();
        seed_corpus(db.as_ref(), "c1");
        seed_entity(db.as_ref(), "c1", "e1", "Gandalf", None);

        let result = svc.entity(EntityInput {
            corpus_id: "c1".into(),
            name_or_id: "Gandalff".into(),
        });
        let ToolResult::Err(e) = result else {
            panic!("expected Err")
        };
        assert!(matches!(
            e.error,
            crate::types::ToolError::NotFound {
                suggestions: Some(_)
            }
        ));
    }

    #[test]
    fn entity_ambiguous_returns_candidates() {
        let (svc, db) = make_service();
        seed_corpus(db.as_ref(), "c1");
        seed_entity(db.as_ref(), "c1", "e1", "Gandalf the Grey", None);
        seed_entity(db.as_ref(), "c1", "e2", "Gandalf the White", None);

        let result = svc.entity(EntityInput {
            corpus_id: "c1".into(),
            name_or_id: "Gandalf".into(),
        });
        let ToolResult::Err(e) = result else {
            panic!("expected Err")
        };
        assert!(matches!(e.error, crate::types::ToolError::Ambiguous { .. }));
    }

    // ── entity_edges ──────────────────────────────────────────────────────

    #[test]
    fn entity_edges_direction_both() {
        let (svc, db) = make_service();
        seed_corpus(db.as_ref(), "c1");
        seed_chunk(db.as_ref(), "c1", "ch/1", "...", "chapter", None);
        seed_entity(db.as_ref(), "c1", "e1", "Frodo", None);
        seed_entity(db.as_ref(), "c1", "e2", "Gandalf", None);
        let uri = Location::new("c1", "ch/1").uri;
        seed_edge(db.as_ref(), "c1", "edge1", "e1", "e2", &uri, "meets");
        seed_edge(db.as_ref(), "c1", "edge2", "e2", "e1", &uri, "advises");

        let result = svc.entity_edges(EntityEdgesInput {
            corpus_id: "c1".into(),
            entity_id: "e1".into(),
            direction: "both".into(),
            kind: None,
            limit: Some(20),
        });
        let ToolResult::Ok(s) = result else {
            panic!("expected Ok")
        };
        assert_eq!(s.data.count, 2);
    }

    #[test]
    fn entity_edges_invalid_direction() {
        let (svc, _db) = make_service();
        let result = svc.entity_edges(EntityEdgesInput {
            corpus_id: "c1".into(),
            entity_id: "e1".into(),
            direction: "sideways".into(),
            kind: None,
            limit: None,
        });
        assert!(matches!(result, ToolResult::Err(_)));
    }

    // ── entity_meet ───────────────────────────────────────────────────────

    #[test]
    fn entity_meet_finds_co_occurrence() {
        let (svc, db) = make_service();
        seed_corpus(db.as_ref(), "c1");
        seed_chunk(db.as_ref(), "c1", "ch/1", "first chapter", "chapter", None);
        seed_chunk(db.as_ref(), "c1", "ch/2", "second chapter", "chapter", None);
        seed_entity(db.as_ref(), "c1", "e1", "Frodo", None);
        seed_entity(db.as_ref(), "c1", "e2", "Gandalf", None);
        let uri1 = Location::new("c1", "ch/1").uri;
        let uri2 = Location::new("c1", "ch/2").uri;
        seed_edge(db.as_ref(), "c1", "ed1", "e1", "e2", &uri1, "meets");
        seed_edge(db.as_ref(), "c1", "ed2", "e1", "e2", &uri2, "meets");

        let result = svc.entity_meet(EntityMeetInput {
            corpus_id: "c1".into(),
            entity_a: "Frodo".into(),
            entity_b: "Gandalf".into(),
        });
        let ToolResult::Ok(s) = result else {
            panic!("expected Ok: {result:?}")
        };
        assert_eq!(s.data.count, 2);
        assert_eq!(s.data.first_co_occurrence.path, "ch/1");
    }

    // ── read ──────────────────────────────────────────────────────────────

    #[test]
    fn read_full_returns_content() {
        let (svc, db) = make_service();
        seed_corpus(db.as_ref(), "c1");
        let chunk = seed_chunk(
            db.as_ref(),
            "c1",
            "ch/1",
            "Full chapter text here",
            "chapter",
            None,
        );

        let result = svc.read(ReadInput {
            corpus_id: Some("c1".into()),
            location: chunk.location.uri.clone(),
            depth: ReadDepth::Full,
        });
        let ToolResult::Ok(s) = result else {
            panic!("expected Ok")
        };
        assert_eq!(s.data.content, Some("Full chapter text here".to_string()));
    }

    #[test]
    fn read_summary_omits_content() {
        let (svc, db) = make_service();
        seed_corpus(db.as_ref(), "c1");
        let chunk = seed_chunk(
            db.as_ref(),
            "c1",
            "ch/1",
            "Full chapter text here",
            "chapter",
            None,
        );

        let result = svc.read(ReadInput {
            corpus_id: Some("c1".into()),
            location: chunk.location.uri.clone(),
            depth: ReadDepth::Summary,
        });
        let ToolResult::Ok(s) = result else {
            panic!("expected Ok")
        };
        assert!(s.data.content.is_none());
    }

    #[test]
    fn read_scenes_includes_children() {
        let (svc, db) = make_service();
        seed_corpus(db.as_ref(), "c1");
        seed_chunk(db.as_ref(), "c1", "ch/1", "Chapter text", "chapter", None);
        seed_chunk(
            db.as_ref(),
            "c1",
            "ch/1/sc/1",
            "Scene 1",
            "scene",
            Some("ch/1"),
        );
        seed_chunk(
            db.as_ref(),
            "c1",
            "ch/1/sc/2",
            "Scene 2",
            "scene",
            Some("ch/1"),
        );
        let uri = Location::new("c1", "ch/1").uri;

        let result = svc.read(ReadInput {
            corpus_id: Some("c1".into()),
            location: uri,
            depth: ReadDepth::Scenes,
        });
        let ToolResult::Ok(s) = result else {
            panic!("expected Ok")
        };
        assert_eq!(s.data.child_locations.len(), 2);
    }

    #[test]
    fn read_entities_present_populated() {
        let (svc, db) = make_service();
        seed_corpus(db.as_ref(), "c1");
        let chunk = seed_chunk(db.as_ref(), "c1", "ch/1", "text", "chapter", None);
        seed_entity(db.as_ref(), "c1", "e1", "Frodo", Some(&chunk.location.uri));

        let result = svc.read(ReadInput {
            corpus_id: Some("c1".into()),
            location: chunk.location.uri.clone(),
            depth: ReadDepth::Full,
        });
        let ToolResult::Ok(s) = result else {
            panic!("expected Ok")
        };
        assert_eq!(s.data.entities_present.len(), 1);
    }

    // ── summarize ─────────────────────────────────────────────────────────

    #[test]
    fn summarize_returns_stored_text() {
        let (svc, db) = make_service();
        seed_corpus(db.as_ref(), "c1");
        let summary = Summary {
            id: "s1".into(),
            corpus_id: "c1".into(),
            target_kind: SummaryTargetKind::Corpus,
            target_id: "c1".into(),
            depth: "corpus".into(),
            text: "A rich tale".into(),
            model: "unknown".into(),
            model_tier: "unknown".into(),
            generated_at: chrono::Utc::now().to_rfc3339(),
            derived_at_version: None,
        };
        db.summary_upsert(&summary).unwrap();

        let result = svc.summarize(SummarizeInput {
            corpus_id: "c1".into(),
            target: SummarizeTarget::Corpus,
        });
        let ToolResult::Ok(s) = result else {
            panic!("expected Ok")
        };
        assert_eq!(s.data.text, "A rich tale");
    }

    #[test]
    fn summarize_not_stored_returns_not_found() {
        let (svc, db) = make_service();
        seed_corpus(db.as_ref(), "c1");
        let _ = db;

        let result = svc.summarize(SummarizeInput {
            corpus_id: "c1".into(),
            target: SummarizeTarget::Corpus,
        });
        let ToolResult::Err(e) = result else {
            panic!("expected Err")
        };
        assert!(matches!(e.error, crate::types::ToolError::NotFound { .. }));
    }

    // ── related ───────────────────────────────────────────────────────────

    #[test]
    fn related_ranks_by_shared_entities() {
        let (svc, db) = make_service();
        seed_corpus(db.as_ref(), "c1");
        seed_chunk(db.as_ref(), "c1", "ch/1", "target chunk", "chapter", None);
        seed_chunk(db.as_ref(), "c1", "ch/2", "related chunk", "chapter", None);
        seed_chunk(
            db.as_ref(),
            "c1",
            "ch/3",
            "unrelated chunk",
            "chapter",
            None,
        );
        seed_entity(db.as_ref(), "c1", "e1", "Frodo", None);
        let uri1 = Location::new("c1", "ch/1").uri;
        let uri2 = Location::new("c1", "ch/2").uri;
        seed_edge(db.as_ref(), "c1", "ed1", "e1", "e1", &uri1, "appears");
        seed_edge(db.as_ref(), "c1", "ed2", "e1", "e1", &uri2, "appears");

        let target_uri = Location::new("c1", "ch/1").uri;
        let result = svc.related(RelatedInput {
            corpus_id: "c1".into(),
            location: target_uri,
            limit: Some(5),
        });
        let ToolResult::Ok(s) = result else {
            panic!("expected Ok")
        };
        assert!(!s.data.items.is_empty());
        assert_eq!(s.data.items[0].location.path, "ch/2");
    }

    // ── composite tools ───────────────────────────────────────────────────

    #[test]
    fn find_scene_composes_correctly() {
        let (svc, db) = make_service();
        seed_corpus(db.as_ref(), "c1");
        seed_chunk(
            db.as_ref(),
            "c1",
            "ch/1/sc/1",
            "Frodo meets Gandalf here",
            "scene",
            Some("ch/1"),
        );
        seed_entity(db.as_ref(), "c1", "e1", "Frodo", None);
        seed_entity(db.as_ref(), "c1", "e2", "Gandalf", None);
        let uri = Location::new("c1", "ch/1/sc/1").uri;
        seed_edge(db.as_ref(), "c1", "ed1", "e1", "e2", &uri, "meets");

        let result = svc.find_scene(FindSceneInput {
            corpus_id: "c1".into(),
            entity_a: "Frodo".into(),
            entity_b: "Gandalf".into(),
        });
        let ToolResult::Ok(s) = result else {
            panic!("expected Ok: {result:?}")
        };
        assert_eq!(s.data.location.path, "ch/1/sc/1");
        assert!(s.data.content.contains("Frodo meets Gandalf"));
    }

    #[test]
    fn character_profile_composes_correctly() {
        let (svc, db) = make_service();
        seed_corpus(db.as_ref(), "c1");
        seed_chunk(db.as_ref(), "c1", "ch/1", "text", "chapter", None);
        let uri = Location::new("c1", "ch/1").uri;
        seed_entity(db.as_ref(), "c1", "e1", "Frodo", None);
        seed_entity(db.as_ref(), "c1", "e2", "Gandalf", None);
        seed_edge(db.as_ref(), "c1", "ed1", "e1", "e2", &uri, "meets");

        let result = svc.character_profile(CharacterProfileInput {
            corpus_id: "c1".into(),
            name: "Frodo".into(),
        });
        let ToolResult::Ok(s) = result else {
            panic!("expected Ok: {result:?}")
        };
        assert_eq!(s.data.entity.canonical_name, "Frodo");
        assert!(!s.data.edges.is_empty());
    }

    // ── explain_component / diegesis ──────────────────────────────────────────

    #[test]
    fn explain_component_narrative_starts_with_diegesis_of() {
        let (svc, db) = make_service();
        seed_corpus(db.as_ref(), "c1");
        seed_entity(db.as_ref(), "c1", "e-query-service", "QueryService", None);

        let result = svc.explain_component(ExplainComponentInput {
            corpus_id: "c1".into(),
            entity_id: Some("e-query-service".into()),
            module_prefix: None,
            max_depth: Some(1),
        });
        let ToolResult::Ok(s) = result else {
            panic!("expected Ok: {result:?}")
        };
        assert!(
            s.data.narrative.starts_with("# Diegesis of "),
            "narrative should start with '# Diegesis of ' but got: {:?}",
            &s.data.narrative[..s.data.narrative.len().min(50)]
        );
    }
}
