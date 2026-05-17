use anyhow::{Result, bail};
use callimachus_core::corrections::types::{CorrectionKind, EntityLinkKind, SplitGranularity};
use callimachus_core::storage::StorageBackend;

/// Sub-commands for `calli correct <corpus_id> <sub>`.
#[derive(Debug)]
pub enum CorrectSubcommand {
    Merge {
        entity_a_id: String,
        entity_b_id: String,
    },
    Unmerge {
        entity_id: String,
        split_by: String,
    },
    Rename {
        entity_id: String,
        new_name: String,
    },
    Alias {
        entity_id: String,
        add: Vec<String>,
        remove: Vec<String>,
    },
    /// target is either a `calli://` URI (chunk), a corpus ID, or an entity ID.
    EditSummary {
        target: String,
        text: String,
    },
}

/// Entity-link correction (collection-scoped).
#[derive(Debug)]
pub struct EntityLinkArgs {
    pub collection_id: String,
    pub corpus_a: String,
    pub entity_a: String,
    pub corpus_b: String,
    pub entity_b: String,
    pub kind: String,
    pub note: Option<String>,
}

/// Apply an entity-link correction to collection `args.collection_id`.
///
/// Validates: collection exists; both corpora are reachable from it; both
/// entities exist in their respective corpora; `kind` is a known `EntityLinkKind`.
pub fn run_entity_link(args: EntityLinkArgs, db: &dyn StorageBackend) -> Result<()> {
    // 1. Validate collection exists.
    db.collection_require(&args.collection_id)?;

    // 2. Both corpora must be reachable.
    let reachable = db.collection_resolve_corpus_ids(&args.collection_id)?;
    if !reachable.contains(&args.corpus_a) {
        bail!(
            "corpus '{}' is not reachable from collection '{}'",
            args.corpus_a,
            args.collection_id
        );
    }
    if !reachable.contains(&args.corpus_b) {
        bail!(
            "corpus '{}' is not reachable from collection '{}'",
            args.corpus_b,
            args.collection_id
        );
    }

    // 3. Both entities must exist.
    validate_entity_in_corpus(db, &args.entity_a, &args.corpus_a)?;
    validate_entity_in_corpus(db, &args.entity_b, &args.corpus_b)?;

    // 4. Parse kind.
    let link_kind = EntityLinkKind::from_str(&args.kind)
        .ok_or_else(|| anyhow::anyhow!(
            "unknown entity link kind '{}'; expected one of: same_as, implements, exemplifies, references, contrasts",
            args.kind
        ))?;

    let kind = CorrectionKind::EntityLink {
        corpus_a_id: args.corpus_a.clone(),
        entity_a_id: args.entity_a.clone(),
        corpus_b_id: args.corpus_b.clone(),
        entity_b_id: args.entity_b.clone(),
        kind: link_kind,
        note: args.note,
    };

    db.correction_insert(None, Some(&args.collection_id), &kind)?;
    println!(
        "✓ EntityLink ({}) recorded: {}/{} → {}/{}",
        link_kind.as_str(),
        args.corpus_a,
        args.entity_a,
        args.corpus_b,
        args.entity_b
    );
    Ok(())
}

/// Apply a correction to corpus `corpus_id`, persisting it to the database.
///
/// Validates that referenced entities exist and belong to the same corpus before
/// writing. Returns `Err` if validation fails.
pub fn run(corpus_id: &str, sub: CorrectSubcommand, db: &dyn StorageBackend) -> Result<()> {
    // Ensure the corpus exists.
    db.corpus_require(corpus_id)?;

    let kind = match sub {
        CorrectSubcommand::Merge {
            entity_a_id,
            entity_b_id,
        } => {
            validate_entity_in_corpus(db, &entity_a_id, corpus_id)?;
            validate_entity_in_corpus(db, &entity_b_id, corpus_id)?;
            CorrectionKind::Merge {
                canonical_id: entity_a_id.clone(),
                entity_a_id,
                entity_b_id,
            }
        }

        CorrectSubcommand::Unmerge {
            entity_id,
            split_by,
        } => {
            validate_entity_in_corpus(db, &entity_id, corpus_id)?;
            let granularity = match split_by.as_str() {
                "chapter" => SplitGranularity::Chapter,
                _ => SplitGranularity::Scene,
            };
            CorrectionKind::Unmerge {
                entity_id,
                split_by: granularity,
            }
        }

        CorrectSubcommand::Rename {
            entity_id,
            new_name,
        } => {
            validate_entity_in_corpus(db, &entity_id, corpus_id)?;
            CorrectionKind::Rename {
                entity_id,
                new_name,
            }
        }

        CorrectSubcommand::Alias {
            entity_id,
            add,
            remove,
        } => {
            validate_entity_in_corpus(db, &entity_id, corpus_id)?;
            CorrectionKind::Alias {
                entity_id,
                add,
                remove,
            }
        }

        CorrectSubcommand::EditSummary { target, text } => {
            let (target_kind, target_id) = classify_target(corpus_id, &target);
            CorrectionKind::EditSummary {
                target_kind,
                target_id,
                text,
            }
        }
    };

    let id = db.correction_insert(Some(corpus_id), None, &kind)?;
    println!("✓ Correction recorded (id: {id})");
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn validate_entity_in_corpus(
    db: &dyn StorageBackend,
    entity_id: &str,
    corpus_id: &str,
) -> Result<()> {
    match db.entity_get_by_id(entity_id)? {
        None => bail!("entity not found: {entity_id}"),
        Some(e) if e.corpus_id != corpus_id => bail!(
            "entity {entity_id} belongs to corpus {:?}, not {:?}",
            e.corpus_id,
            corpus_id
        ),
        Some(_) => Ok(()),
    }
}

/// Classify a target string as (target_kind, target_id).
/// - `calli://...` URIs → "chunk"
/// - The corpus_id itself → "corpus"
/// - Anything else → "entity"
fn classify_target(corpus_id: &str, target: &str) -> (String, String) {
    if target.starts_with("calli://") {
        ("chunk".to_string(), target.to_string())
    } else if target == corpus_id {
        ("corpus".to_string(), target.to_string())
    } else {
        ("entity".to_string(), target.to_string())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use callimachus_core::corrections::types::CorrectionKind;
    use callimachus_core::storage::SqliteBackend;
    use callimachus_core::types::corpus::Corpus;
    use callimachus_core::types::entity::Entity;

    fn seed_corpus(db: &dyn StorageBackend, id: &str) {
        let corpus = Corpus::new(
            id.to_string(),
            format!("{id} corpus"),
            "book".to_string(),
            "/tmp/dummy".to_string(),
        );
        db.corpus_insert(&corpus).unwrap();
    }

    fn seed_entity(db: &dyn StorageBackend, id: &str, corpus_id: &str, name: &str) {
        let mut e = Entity::new(
            id.to_string(),
            corpus_id.to_string(),
            name.to_string(),
            "character".to_string(),
        );
        e.appearance_count = 1;
        db.entity_upsert(&e).unwrap();
    }

    /// Merging with a nonexistent entity_a returns an error containing "not found".
    #[test]
    fn test_merge_invalid_entity_a() {
        let db = SqliteBackend::open_in_memory().unwrap();
        seed_corpus(&db, "corp1");
        seed_entity(&db, "b", "corp1", "Beta");

        let sub = CorrectSubcommand::Merge {
            entity_a_id: "nonexistent".to_string(),
            entity_b_id: "b".to_string(),
        };
        let err = run("corp1", sub, &db).unwrap_err();
        let msg = format!("{err:?}");
        assert!(
            msg.to_lowercase().contains("not found"),
            "expected 'not found' in error, got: {msg}"
        );
    }

    /// Merging entities from different corpora returns an error.
    #[test]
    fn test_merge_mismatched_corpora() {
        let db = SqliteBackend::open_in_memory().unwrap();
        seed_corpus(&db, "corp1");
        seed_corpus(&db, "corp2");
        seed_entity(&db, "a", "corp1", "Alpha");
        seed_entity(&db, "b", "corp2", "Beta");

        let sub = CorrectSubcommand::Merge {
            entity_a_id: "a".to_string(),
            entity_b_id: "b".to_string(),
        };
        let result = run("corp1", sub, &db);
        assert!(result.is_err(), "cross-corpus merge should be rejected");
    }

    /// A successful merge stores exactly one correction record with kind Merge.
    #[test]
    fn test_merge_inserts_correction() {
        let db = SqliteBackend::open_in_memory().unwrap();
        seed_corpus(&db, "corp1");
        seed_entity(&db, "a", "corp1", "Alpha");
        seed_entity(&db, "b", "corp1", "Beta");

        let sub = CorrectSubcommand::Merge {
            entity_a_id: "a".to_string(),
            entity_b_id: "b".to_string(),
        };
        run("corp1", sub, &db).unwrap();

        let records = db.correction_list("corp1").unwrap();
        assert_eq!(records.len(), 1, "one correction should be stored");
        assert!(
            matches!(records[0].kind, CorrectionKind::Merge { .. }),
            "correction should be a Merge variant"
        );
    }

    /// A successful rename stores exactly one correction record with kind Rename.
    #[test]
    fn test_rename_inserts_correction() {
        let db = SqliteBackend::open_in_memory().unwrap();
        seed_corpus(&db, "corp1");
        seed_entity(&db, "a", "corp1", "Alpha");

        let sub = CorrectSubcommand::Rename {
            entity_id: "a".to_string(),
            new_name: "NewAlpha".to_string(),
        };
        run("corp1", sub, &db).unwrap();

        let records = db.correction_list("corp1").unwrap();
        assert_eq!(records.len(), 1, "one correction should be stored");
        assert!(
            matches!(records[0].kind, CorrectionKind::Rename { .. }),
            "correction should be a Rename variant"
        );
    }

    /// When the target is a calli:// URI, the stored correction targets "chunk".
    #[test]
    fn test_edit_summary_chunk_uri() {
        let db = SqliteBackend::open_in_memory().unwrap();
        seed_corpus(&db, "corp1");

        let sub = CorrectSubcommand::EditSummary {
            target: "calli://xenos/ch/3".to_string(),
            text: "a custom summary".to_string(),
        };
        run("corp1", sub, &db).unwrap();

        let records = db.correction_list("corp1").unwrap();
        assert_eq!(records.len(), 1);
        match &records[0].kind {
            CorrectionKind::EditSummary { target_kind, .. } => {
                assert_eq!(target_kind, "chunk");
            }
            other => panic!("expected EditSummary correction, got {other:?}"),
        }
    }

    /// When the target is a plain entity ID (not a URI), the stored correction targets "entity".
    #[test]
    fn test_edit_summary_entity_id() {
        let db = SqliteBackend::open_in_memory().unwrap();
        seed_corpus(&db, "corp1");
        seed_entity(&db, "a", "corp1", "Alpha");

        let sub = CorrectSubcommand::EditSummary {
            target: "a".to_string(),
            text: "a custom entity summary".to_string(),
        };
        run("corp1", sub, &db).unwrap();

        let records = db.correction_list("corp1").unwrap();
        assert_eq!(records.len(), 1);
        match &records[0].kind {
            CorrectionKind::EditSummary { target_kind, .. } => {
                assert_eq!(target_kind, "entity");
            }
            other => panic!("expected EditSummary correction, got {other:?}"),
        }
    }
}
