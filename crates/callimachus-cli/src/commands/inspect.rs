use anyhow::Result;
use callimachus_core::corrections::CorrectionsEngine;
use callimachus_core::corrections::types::{Correction, CorrectionKind, EntityLinkKind};
use callimachus_core::indexing::EntityFilter;
use callimachus_core::query::service::QueryService;
use callimachus_core::query::types::ExplainComponentInput;
use callimachus_core::storage::StorageBackend;
use callimachus_core::types::ToolResult;
use callimachus_core::types::chunk::Chunk;
use callimachus_core::types::entity::Entity;
use callimachus_core::types::{EntityContract, SummaryTargetKind};
use serde::Serialize;
use std::sync::Arc;

/// Return entities for `corpus_id`, optionally filtered by name substring and/or kind.
/// Results are ordered by `appearance_count DESC` and capped at `limit` (default: 50).
/// Corrections are applied before returning, so merged/renamed entities are reflected.
pub fn run_entities(
    corpus_id: &str,
    filter: Option<&str>,
    kind: Option<&str>,
    min_confidence: Option<f32>,
    limit: Option<usize>,
    db: &dyn StorageBackend,
) -> Result<Vec<Entity>> {
    let engine = CorrectionsEngine::load(db, corpus_id)?;
    let mut entities = db.entity_list(corpus_id)?;
    engine.apply_to_entities(&mut entities);

    // Apply filters in Rust (not SQL) as per plan.
    if let Some(filter_str) = filter {
        let filter_lc = filter_str.to_lowercase();
        entities.retain(|e| {
            e.canonical_name.to_lowercase().contains(&filter_lc)
                || e.aliases
                    .iter()
                    .any(|a| a.to_lowercase().contains(&filter_lc))
        });
    }
    if let Some(k) = kind {
        entities.retain(|e| e.kind == k);
    }
    if let Some(min_conf) = min_confidence {
        entities.retain(|e| e.confidence >= min_conf);
    }

    let cap = limit.unwrap_or(50);
    entities.truncate(cap);

    Ok(entities)
}

/// One entity's LLM artifacts, as printed by `calli inspect artifacts`.
#[derive(Debug, Serialize)]
pub struct ArtifactRecord {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub location: Option<String>,
    /// Best summary: the entity's own, else the chunk at its location.
    pub summary: Option<String>,
    pub summary_model: Option<String>,
    pub purpose: Option<String>,
    pub purpose_model: Option<String>,
    pub contract: Option<EntityContract>,
    /// Most recent `generated_at` among this entity's artifacts.
    pub run_timestamp: Option<String>,
}

/// Collect the artifacts of every entity selected by `specs` (all if empty).
pub fn run_artifacts(
    corpus_id: &str,
    specs: &[String],
    db: &dyn StorageBackend,
) -> Result<Vec<ArtifactRecord>> {
    let filter = EntityFilter::build(specs, corpus_id, db)?;
    let mut entities: Vec<Entity> = db
        .entity_list(corpus_id)?
        .into_iter()
        .filter(|e| filter.matches(e, db))
        .collect();
    entities.sort_by(|a, b| a.canonical_name.cmp(&b.canonical_name));

    let mut records = Vec::with_capacity(entities.len());
    for e in entities {
        let location = e.first_location.as_ref().map(|l| l.uri());
        let summary = match db.summary_get(corpus_id, &SummaryTargetKind::Entity, &e.id)? {
            Some(s) => Some(s),
            None => match location.as_deref().map(|u| db.chunk_get_by_uri(u)) {
                Some(Ok(Some(chunk))) => {
                    db.summary_get(corpus_id, &SummaryTargetKind::Chunk, &chunk.id)?
                }
                _ => None,
            },
        };
        let purpose = db.purpose_get(corpus_id, &e.id)?;
        let contract = db.contract_get(corpus_id, &e.id)?;
        let run_timestamp = [
            summary.as_ref().map(|s| s.generated_at.clone()),
            purpose.as_ref().map(|p| p.generated_at.clone()),
            contract.as_ref().map(|c| c.generated_at.clone()),
        ]
        .into_iter()
        .flatten()
        .max();
        records.push(ArtifactRecord {
            id: e.id,
            name: e.canonical_name,
            kind: e.kind,
            location,
            summary_model: summary.as_ref().map(|s| s.model.clone()),
            summary: summary.map(|s| s.text),
            purpose_model: purpose.as_ref().map(|p| p.model.clone()),
            purpose: purpose.map(|p| p.purpose),
            contract,
            run_timestamp,
        });
    }
    Ok(records)
}

/// One text block per entity.
pub fn format_artifacts(records: &[ArtifactRecord]) -> String {
    let none = "(none)";
    let mut out = String::new();
    for r in records {
        out.push_str(&format!("{} [{}]  {}\n", r.name, r.kind, r.id));
        out.push_str(&format!(
            "  location: {}\n",
            r.location.as_deref().unwrap_or(none)
        ));
        out.push_str(&format!(
            "  summary:  {}{}\n",
            r.summary.as_deref().unwrap_or(none),
            model_suffix(&r.summary_model)
        ));
        out.push_str(&format!(
            "  purpose:  {}{}\n",
            r.purpose.as_deref().unwrap_or(none),
            model_suffix(&r.purpose_model)
        ));
        match &r.contract {
            Some(c) => {
                out.push_str(&format!("  contract: model={}\n", c.model));
                for (label, items) in [("assumptions", &c.assumptions), ("risks", &c.risks)] {
                    if !items.is_empty() {
                        out.push_str(&format!("    {label}: {}\n", items.join("; ")));
                    }
                }
                if let Some(n) = &c.caller_notes {
                    out.push_str(&format!("    caller_notes: {n}\n"));
                }
            }
            None => out.push_str(&format!("  contract: {none}\n")),
        }
        out.push_str(&format!(
            "  run:      {}\n\n",
            r.run_timestamp.as_deref().unwrap_or(none)
        ));
    }
    out
}

fn model_suffix(model: &Option<String>) -> String {
    model
        .as_ref()
        .map(|m| format!("  ({m})"))
        .unwrap_or_default()
}

/// Return the chunk with `location_uri == location`, or `None` if not found.
/// Never errors on a missing chunk — missing is not an error.
pub fn run_chunk(location: &str, db: &dyn StorageBackend) -> Result<Option<Chunk>> {
    Ok(db.chunk_get_by_uri(location)?)
}

/// Return all correction records for `corpus_id`, ordered by `applied_at ASC`.
pub fn run_corrections(corpus_id: &str, db: &dyn StorageBackend) -> Result<Vec<Correction>> {
    Ok(db.correction_list(corpus_id)?)
}

/// Assemble and print a diegesis (narrative explanation) for `entity` in `corpus_id`.
/// Uses BFS over call edges via the query service.
pub fn run_diegesis(
    corpus_id: &str,
    entity: &str,
    max_depth: Option<u8>,
    db: Arc<dyn StorageBackend>,
) -> Result<()> {
    let svc = QueryService::new(db);
    // Try the input as an entity ID first; fall back to module prefix (name) lookup.
    // This lets both `calli inspect diegesis <corpus> <entity-id>` and
    // `calli inspect diegesis <corpus> <entity-name>` work from the CLI.
    let input = ExplainComponentInput {
        corpus_id: corpus_id.to_string(),
        entity_id: None,
        module_prefix: Some(entity.to_string()),
        max_depth,
    };
    match svc.explain_component(input) {
        ToolResult::Ok(s) => {
            println!("{}", s.data.narrative);
            Ok(())
        }
        ToolResult::Err(e) => {
            let msg = match &e.error {
                callimachus_core::types::ToolError::NotFound { suggestions } => {
                    let hint = suggestions
                        .as_deref()
                        .map(|s| format!(": did you mean one of {:?}?", s))
                        .unwrap_or_default();
                    format!("entity not found{hint}")
                }
                callimachus_core::types::ToolError::Error { message, .. } => message.clone(),
                callimachus_core::types::ToolError::Ambiguous { candidates } => {
                    format!("ambiguous entity; candidates: {:?}", candidates)
                }
                callimachus_core::types::ToolError::InvalidInput { message } => {
                    format!("invalid input: {message}")
                }
            };
            anyhow::bail!("{msg}")
        }
    }
}

/// Print entity-link corrections for a collection, optionally filtered by kind.
/// Format: KIND | FROM | TO | NOTE
pub fn run_collection_links(
    collection_id: &str,
    kind_filter: Option<&str>,
    db: &dyn StorageBackend,
) -> Result<()> {
    // Validate collection exists.
    db.collection_require(collection_id)?;

    let corrections = db.correction_list_for_collection(collection_id)?;

    // Filter to EntityLink, optionally by kind.
    let links: Vec<_> = corrections
        .iter()
        .filter_map(|c| {
            if let CorrectionKind::EntityLink {
                corpus_a_id,
                entity_a_id,
                corpus_b_id,
                entity_b_id,
                kind,
                note,
            } = &c.kind
            {
                if kind_filter.is_some_and(|kf| kind.as_str() != kf) {
                    return None;
                }
                Some((
                    corpus_a_id,
                    entity_a_id,
                    corpus_b_id,
                    entity_b_id,
                    kind,
                    note,
                ))
            } else {
                None
            }
        })
        .collect();

    if links.is_empty() {
        println!("(no entity links)");
        return Ok(());
    }

    let sep = if *links[0].4 == EntityLinkKind::SameAs {
        "↔"
    } else {
        "→"
    };
    println!(
        "{:<12}  {:<42}  {}  {:<42}  NOTE",
        "KIND", "FROM", sep, "TO"
    );
    println!("{}", "-".repeat(110));
    for (ca, ea, cb, eb, kind, note) in links {
        let from = format!("{ca}/{ea}");
        let to = format!("{cb}/{eb}");
        let arrow = if *kind == EntityLinkKind::SameAs {
            "↔"
        } else {
            "→"
        };
        let note_str = note.as_deref().unwrap_or("");
        println!(
            "{:<12}  {:<42}  {arrow}  {:<42}  {note_str}",
            kind.as_str(),
            from,
            to
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use callimachus_core::corrections::types::CorrectionKind;
    use callimachus_core::storage::SqliteBackend;
    use callimachus_core::types::chunk::Chunk;
    use callimachus_core::types::corpus::Corpus;
    use callimachus_core::types::entity::Entity;
    use callimachus_core::types::location::Location;

    const CORPUS: &str = "test-corpus";

    fn seed_corpus(db: &dyn StorageBackend) {
        let corpus = Corpus::new(
            CORPUS.to_string(),
            "Test Corpus".to_string(),
            "book".to_string(),
            "/tmp/dummy".to_string(),
        );
        db.corpus_insert(&corpus).unwrap();
    }

    fn seed_entity(
        db: &dyn StorageBackend,
        id: &str,
        name: &str,
        kind: &str,
        confidence: f32,
        count: u32,
    ) -> Entity {
        let e = Entity {
            id: id.to_string(),
            corpus_id: CORPUS.to_string(),
            canonical_name: name.to_string(),
            kind: kind.to_string(),
            abstract_kind: String::new(),
            aliases: vec![],
            description: None,
            first_location: None,
            last_location: None,
            appearance_count: count,
            confidence,
            provenance: None,
            start_line: None,
            end_line: None,
        };
        db.entity_upsert(&e).unwrap();
        e
    }

    fn seed_chunk(db: &dyn StorageBackend, uri: &str) -> Chunk {
        let loc = Location::parse(uri).unwrap();
        let chunk = Chunk::new(
            CORPUS.to_string(),
            None,
            "chapter".to_string(),
            loc,
            format!("content of {uri}"),
        );
        db.chunk_upsert(&chunk).unwrap();
        chunk
    }

    fn seed_correction(db: &dyn StorageBackend, kind: CorrectionKind) {
        db.correction_insert(Some(CORPUS), None, &kind).unwrap();
    }

    /// Filtering by kind="character" returns only the 2 character entities, not the place.
    #[test]
    fn test_inspect_entities_filter_kind() {
        let db = SqliteBackend::open_in_memory().unwrap();
        seed_corpus(&db);
        seed_entity(&db, "e1", "Eisenhorn", "character", 0.9, 30);
        seed_entity(&db, "e2", "Ravenor", "character", 0.8, 20);
        seed_entity(&db, "e3", "Gershom", "place", 0.7, 5);

        let results = run_entities(CORPUS, None, Some("character"), None, None, &db).unwrap();

        assert_eq!(
            results.len(),
            2,
            "only the two character entities should be returned"
        );
        for e in &results {
            assert_eq!(
                e.kind, "character",
                "all returned entities should be characters"
            );
        }
    }

    /// Filtering by min_confidence=0.9 returns only the entity with confidence >= 0.9.
    #[test]
    fn test_inspect_entities_min_confidence() {
        let db = SqliteBackend::open_in_memory().unwrap();
        seed_corpus(&db);
        seed_entity(&db, "e1", "HighConf", "character", 0.95, 10);
        seed_entity(&db, "e2", "LowConf", "character", 0.5, 8);

        let results = run_entities(CORPUS, None, None, Some(0.9), None, &db).unwrap();

        assert_eq!(
            results.len(),
            1,
            "only the high-confidence entity should pass the filter"
        );
        assert_eq!(results[0].id, "e1");
    }

    /// run_chunk returns Some for a URI that exists in the database.
    #[test]
    fn test_inspect_chunk_valid() {
        let db = SqliteBackend::open_in_memory().unwrap();
        seed_corpus(&db);
        seed_chunk(&db, "calli://test-corpus/ch/1");

        let result = run_chunk("calli://test-corpus/ch/1", &db).unwrap();
        assert!(result.is_some(), "should find the seeded chunk");
    }

    /// run_chunk returns Ok(None) — not an error — for a URI that does not exist.
    #[test]
    fn test_inspect_chunk_invalid() {
        let db = SqliteBackend::open_in_memory().unwrap();
        seed_corpus(&db);

        let result = run_chunk("calli://test-corpus/ch/nonexistent", &db).unwrap();
        assert!(
            result.is_none(),
            "missing chunk should be Ok(None), not an error"
        );
    }

    /// run_corrections returns all stored correction records for the corpus.
    #[test]
    fn test_inspect_corrections_count() {
        let db = SqliteBackend::open_in_memory().unwrap();
        seed_corpus(&db);
        seed_entity(&db, "e1", "Alpha", "character", 0.8, 5);
        seed_entity(&db, "e2", "Beta", "character", 0.7, 3);

        seed_correction(
            &db,
            CorrectionKind::Rename {
                entity_id: "e1".to_string(),
                new_name: "NewAlpha".to_string(),
            },
        );
        seed_correction(
            &db,
            CorrectionKind::Merge {
                entity_a_id: "e1".to_string(),
                entity_b_id: "e2".to_string(),
                canonical_id: "e1".to_string(),
            },
        );

        let records = run_corrections(CORPUS, &db).unwrap();
        assert_eq!(records.len(), 2, "should return all 2 stored corrections");
    }
}
