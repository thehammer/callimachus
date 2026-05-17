use anyhow::Result;
use callimachus_core::corrections::CorrectionsEngine;
use callimachus_core::corrections::types::{Correction, CorrectionKind, EntityLinkKind};
use callimachus_core::storage::StorageBackend;
use callimachus_core::types::chunk::Chunk;
use callimachus_core::types::entity::Entity;

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

/// Return the chunk with `location_uri == location`, or `None` if not found.
/// Never errors on a missing chunk — missing is not an error.
pub fn run_chunk(location: &str, db: &dyn StorageBackend) -> Result<Option<Chunk>> {
    Ok(db.chunk_get_by_uri(location)?)
}

/// Return all correction records for `corpus_id`, ordered by `applied_at ASC`.
pub fn run_corrections(corpus_id: &str, db: &dyn StorageBackend) -> Result<Vec<Correction>> {
    Ok(db.correction_list(corpus_id)?)
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
            aliases: vec![],
            description: None,
            first_location: None,
            last_location: None,
            appearance_count: count,
            confidence,
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
