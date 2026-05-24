use anyhow::Result;
use callimachus_core::corrections::CorrectionsEngine;
use callimachus_core::storage::StorageBackend;
use callimachus_core::types::SummaryTargetKind;
use serde_json::{Value, json};

/// Export all indexed data for `corpus_id` as JSONL to `writer`.
///
/// Line order:
/// 1. Chunk records (`"record_type":"chunk"`)
/// 2. Entity records — corrections applied (`"record_type":"entity"`)
/// 3. Edge records (`"record_type":"edge"`)
/// 4. Summary records — edit_summary corrections applied (`"record_type":"summary"`)
/// 5. Correction records (`"record_type":"correction"`)
///
/// Every line is a valid JSON object with a `record_type` discriminator field.
pub fn run_to_writer(
    corpus_id: &str,
    db: &dyn StorageBackend,
    writer: &mut impl std::io::Write,
) -> Result<()> {
    let engine = CorrectionsEngine::load(db, corpus_id)?;
    let mut line_count: u64 = 0;

    // 1. Chunks
    let chunks = db.chunk_list(corpus_id)?;
    for chunk in &chunks {
        let obj = json!({
            "record_type": "chunk",
            "id": chunk.id,
            "corpus_id": chunk.corpus_id,
            "location_uri": chunk.location.uri,
            "parent_path": chunk.parent_path,
            "kind": chunk.kind,
            "content": chunk.content,
        });
        writeln!(writer, "{}", obj)?;
        line_count += 1;
        if line_count.is_multiple_of(1000) {
            tracing::info!("export: {} lines written", line_count);
        }
    }

    // 2. Entities — with corrections applied
    let mut entities = db.entity_list(corpus_id)?;
    engine.apply_to_entities(&mut entities);
    for entity in &entities {
        let obj = json!({
            "record_type": "entity",
            "id": entity.id,
            "corpus_id": entity.corpus_id,
            "canonical_name": entity.canonical_name,
            "kind": entity.kind,
            "aliases": entity.aliases,
            "appearance_count": entity.appearance_count,
            "confidence": entity.confidence,
        });
        writeln!(writer, "{}", obj)?;
        line_count += 1;
        if line_count.is_multiple_of(1000) {
            tracing::info!("export: {} lines written", line_count);
        }
    }

    // 3. Edges
    let edges = db.edge_list(corpus_id)?;
    for edge in &edges {
        let obj = json!({
            "record_type": "edge",
            "id": edge.id,
            "corpus_id": edge.corpus_id,
            "from_entity_id": edge.from_entity_id,
            "to_entity_id": edge.to_entity_id,
            "kind": edge.kind,
            "location_uri": edge.location.uri,
        });
        writeln!(writer, "{}", obj)?;
        line_count += 1;
        if line_count.is_multiple_of(1000) {
            tracing::info!("export: {} lines written", line_count);
        }
    }

    // 4. Summaries — with edit_summary corrections applied
    let summaries = db.summary_list(corpus_id)?;
    for summary in &summaries {
        let target_kind_str = match summary.target_kind {
            SummaryTargetKind::Chunk => "chunk",
            SummaryTargetKind::Entity => "entity",
            SummaryTargetKind::Corpus => "corpus",
            SummaryTargetKind::Range => "range",
        };
        let text = engine
            .override_summary(target_kind_str, &summary.target_id)
            .unwrap_or(summary.text.as_str());
        let obj = json!({
            "record_type": "summary",
            "id": summary.id,
            "corpus_id": summary.corpus_id,
            "target_kind": target_kind_str,
            "target_id": summary.target_id,
            "text": text,
        });
        writeln!(writer, "{}", obj)?;
        line_count += 1;
        if line_count.is_multiple_of(1000) {
            tracing::info!("export: {} lines written", line_count);
        }
    }

    // 5. Corrections — raw, last
    let corrections = db.correction_list(corpus_id)?;
    for correction in &corrections {
        let payload: Value = serde_json::to_value(&correction.kind)?;
        let obj = json!({
            "record_type": "correction",
            "id": correction.id,
            "corpus_id": correction.corpus_id,
            "applied_at": correction.applied_at,
            "payload": payload,
        });
        writeln!(writer, "{}", obj)?;
        line_count += 1;
        if line_count.is_multiple_of(1000) {
            tracing::info!("export: {} lines written", line_count);
        }
    }

    tracing::info!("export: complete — {} total lines", line_count);
    Ok(())
}

// ---------------------------------------------------------------------------
// SCIP export
// ---------------------------------------------------------------------------

/// Export a code corpus as a SCIP (Stack graphs Index Protocol) JSON document.
///
/// SCIP is a standard code intelligence format from Sourcegraph.
/// This is a best-effort implementation covering documents and occurrences.
/// Full symbol table and type resolution are out of scope.
///
/// Only supported for code corpora (`kind = "code"`). Returns an error for
/// other corpus kinds.
///
/// See: <https://github.com/sourcegraph/scip>
pub fn run_scip_to_writer(
    corpus_id: &str,
    db: &dyn StorageBackend,
    writer: &mut impl std::io::Write,
) -> Result<()> {
    // Verify this is a code corpus.
    let corpus = db.corpus_require(corpus_id)?;
    if corpus.kind != "code" {
        anyhow::bail!(
            "SCIP export is only supported for code corpora (kind=code). \
             Corpus '{}' has kind '{}'.",
            corpus_id,
            corpus.kind
        );
    }

    let chunks = db.chunk_list(corpus_id)?;
    let entities = db.entity_list(corpus_id)?;
    let edges = db.edge_list(corpus_id)?;

    // Group entities by file URI (the portion before '#').
    let mut occurrences_by_file: std::collections::HashMap<String, Vec<serde_json::Value>> =
        std::collections::HashMap::new();

    for entity in &entities {
        if let Some(loc) = &entity.first_location {
            let file_key = loc.path.split('#').next().unwrap_or(&loc.path).to_string();
            let occ = json!({
                "symbol": format!("local {}", entity.canonical_name),
                "symbol_roles": 1,  // 1 = definition
                "kind": entity.kind,
            });
            occurrences_by_file.entry(file_key).or_default().push(occ);
        }
    }

    // Build documents: one per file chunk.
    let file_chunks: Vec<_> = chunks.iter().filter(|c| c.kind == "file").collect();

    let documents: Vec<Value> = file_chunks
        .iter()
        .map(|chunk| {
            let relative_path = chunk
                .location
                .path
                .strip_prefix("src/")
                .unwrap_or(&chunk.location.path);

            let occurrences = occurrences_by_file
                .get(&chunk.location.path)
                .cloned()
                .unwrap_or_default();

            json!({
                "relative_path": relative_path,
                "occurrences": occurrences,
                "language": detect_language(relative_path),
            })
        })
        .collect();

    // Build relationships from edges.
    let relationships: Vec<Value> = edges
        .iter()
        .filter(|e| {
            matches!(
                e.kind.as_str(),
                "calls" | "extends" | "implements" | "imports"
            )
        })
        .map(|e| {
            json!({
                "symbol": e.from_entity_id,
                "is_reference": e.kind != "defines",
                "references": [e.to_entity_id],
                "kind": e.kind,
            })
        })
        .collect();

    let scip_doc = json!({
        "metadata": {
            "version": 0,
            "tool_info": {
                "name": "callimachus",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "project_root": format!("file:///callimachus/{corpus_id}"),
            "text_document_encoding": "UTF8",
        },
        "documents": documents,
        "external_symbols": [],
        "relationships": relationships,
    });

    serde_json::to_writer_pretty(&mut *writer, &scip_doc)?;
    writeln!(writer)?;

    tracing::info!(
        "scip-export: {} documents, {} relationships",
        file_chunks.len(),
        relationships.len()
    );
    Ok(())
}

fn detect_language(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "rs" => "Rust",
        "ts" => "TypeScript",
        "tsx" => "TypeScript",
        "js" | "mjs" | "jsx" => "JavaScript",
        "py" => "Python",
        "go" => "Go",
        _ => "UnspecifiedLanguage",
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use callimachus_core::storage::SqliteBackend;
    use callimachus_core::types::chunk::Chunk;
    use callimachus_core::types::corpus::Corpus;
    use callimachus_core::types::edge::Edge;
    use callimachus_core::types::entity::Entity;
    use callimachus_core::types::location::Location;
    use callimachus_core::types::summary::{Summary, SummaryTargetKind};

    const CORPUS: &str = "export-corpus";

    fn seed_corpus(db: &dyn StorageBackend) {
        let corpus = Corpus::new(
            CORPUS.to_string(),
            "Export Corpus".to_string(),
            "book".to_string(),
            "/tmp/dummy".to_string(),
        );
        db.corpus_insert(&corpus).unwrap();
    }

    fn seed_chunk(db: &dyn StorageBackend, uri: &str) -> Chunk {
        let loc = Location::parse(uri).unwrap();
        let chunk = Chunk::new(
            CORPUS.to_string(),
            None,
            "chapter".to_string(),
            loc,
            format!("content of {}", uri),
        );
        db.chunk_upsert(&chunk).unwrap();
        chunk
    }

    fn seed_entity(db: &dyn StorageBackend, id: &str, name: &str) -> Entity {
        let e = Entity {
            id: id.to_string(),
            corpus_id: CORPUS.to_string(),
            canonical_name: name.to_string(),
            kind: "character".to_string(),
            abstract_kind: String::new(),
            aliases: vec![],
            description: None,
            first_location: None,
            last_location: None,
            appearance_count: 5,
            confidence: 0.8,
            derived_at_version: None,
        };
        db.entity_upsert(&e).unwrap();
        e
    }

    fn seed_edge(db: &dyn StorageBackend, id: &str, from: &str, to: &str, chunk_uri: &str) -> Edge {
        let loc = Location::parse(chunk_uri).unwrap();
        let edge = Edge::new(
            id.to_string(),
            CORPUS.to_string(),
            from.to_string(),
            to.to_string(),
            "meets".to_string(),
            loc,
        );
        db.edge_upsert(&edge).unwrap();
        edge
    }

    fn seed_summary(db: &dyn StorageBackend, target_id: &str) {
        let s = Summary {
            id: format!("summary-{}", chrono::Utc::now().timestamp_micros()),
            corpus_id: CORPUS.to_string(),
            target_kind: SummaryTargetKind::Chunk,
            target_id: target_id.to_string(),
            depth: "chapter".to_string(),
            text: "a summary".to_string(),
            model: "unknown".to_string(),
            model_tier: "unknown".to_string(),
            generated_at: chrono::Utc::now().to_rfc3339(),
            derived_at_version: None,
        };
        db.summary_upsert(&s).unwrap();
    }

    fn seed_correction(
        db: &dyn StorageBackend,
        kind: callimachus_core::corrections::types::CorrectionKind,
    ) {
        db.correction_insert(Some(CORPUS), None, &kind).unwrap();
    }

    fn run_export(db: &dyn StorageBackend) -> Vec<serde_json::Value> {
        let mut buf = Vec::new();
        run_to_writer(CORPUS, db, &mut buf).unwrap();
        let text = String::from_utf8(buf).unwrap();
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).expect("each line should be valid JSON"))
            .collect()
    }

    // -----------------------------------------------------------------------
    // Total record count
    // -----------------------------------------------------------------------

    /// Exporting a corpus with 2 chunks, 3 entities, 1 edge, 1 summary, and 1 correction
    /// produces exactly 8 JSONL lines, all parseable as JSON objects.
    #[test]
    fn test_export_all_records() {
        let db = SqliteBackend::open_in_memory().unwrap();
        seed_corpus(&db);
        seed_chunk(&db, "calli://export-corpus/ch/1");
        seed_chunk(&db, "calli://export-corpus/ch/2");
        let _e1 = seed_entity(&db, "e1", "Alpha");
        let _e2 = seed_entity(&db, "e2", "Beta");
        let _e3 = seed_entity(&db, "e3", "Gamma");
        seed_edge(&db, "edge1", "e1", "e2", "calli://export-corpus/ch/1");
        seed_summary(&db, "calli://export-corpus/ch/1");
        use callimachus_core::corrections::types::CorrectionKind;
        seed_correction(
            &db,
            CorrectionKind::Rename {
                entity_id: "e1".to_string(),
                new_name: "NewAlpha".to_string(),
            },
        );

        let lines = run_export(&db);

        assert_eq!(
            lines.len(),
            8,
            "expected 2 chunks + 3 entities + 1 edge + 1 summary + 1 correction = 8 lines"
        );

        for (i, line) in lines.iter().enumerate() {
            assert!(
                line.is_object(),
                "line {} should be a JSON object, got: {}",
                i,
                line
            );
        }
    }

    // -----------------------------------------------------------------------
    // Ordering: correction lines come last
    // -----------------------------------------------------------------------

    /// All `"record_type":"correction"` lines appear after all entity lines.
    #[test]
    fn test_export_corrections_last() {
        let db = SqliteBackend::open_in_memory().unwrap();
        seed_corpus(&db);
        seed_chunk(&db, "calli://export-corpus/ch/1");
        seed_entity(&db, "e1", "Alpha");
        use callimachus_core::corrections::types::CorrectionKind;
        seed_correction(
            &db,
            CorrectionKind::Rename {
                entity_id: "e1".to_string(),
                new_name: "NewAlpha".to_string(),
            },
        );

        let lines = run_export(&db);

        let last_entity_idx = lines
            .iter()
            .rposition(|l| l.get("record_type").and_then(|v| v.as_str()) == Some("entity"));
        let first_correction_idx = lines
            .iter()
            .position(|l| l.get("record_type").and_then(|v| v.as_str()) == Some("correction"));

        if let (Some(entity_pos), Some(correction_pos)) = (last_entity_idx, first_correction_idx) {
            assert!(
                correction_pos > entity_pos,
                "corrections (at index {}) should appear after entities (last at {})",
                correction_pos,
                entity_pos
            );
        }
    }

    // -----------------------------------------------------------------------
    // Corrections applied to entities before export
    // -----------------------------------------------------------------------

    /// When a Merge correction keeps entity A, only one entity line appears (B merged into A).
    #[test]
    fn test_export_applied_corrections() {
        let db = SqliteBackend::open_in_memory().unwrap();
        seed_corpus(&db);
        seed_entity(&db, "e1", "Alpha");
        seed_entity(&db, "e2", "Beta");
        use callimachus_core::corrections::types::CorrectionKind;
        seed_correction(
            &db,
            CorrectionKind::Merge {
                entity_a_id: "e1".to_string(),
                entity_b_id: "e2".to_string(),
                canonical_id: "e1".to_string(),
            },
        );

        let lines = run_export(&db);

        let entity_lines: Vec<&serde_json::Value> = lines
            .iter()
            .filter(|l| l.get("record_type").and_then(|v| v.as_str()) == Some("entity"))
            .collect();

        assert_eq!(
            entity_lines.len(),
            1,
            "after merge, only one entity line should be exported (B collapsed into A)"
        );
        let entity_id = entity_lines[0]
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("(missing)");
        assert_eq!(
            entity_id, "e1",
            "the surviving entity should be the canonical entity A"
        );
    }
}
