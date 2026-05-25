use std::sync::Arc;

use callimachus_llm::{LlmProvider, model_tier};
use uuid::Uuid;

use crate::{
    adapter::SourceAdapter,
    storage::{StorageBackend, run_log::PassStats},
    types::{Corpus, Edge, Entity, Theme},
};

use super::pipeline::IndexOptions;

const MIN_ENTITIES_FOR_THEMES: u64 = 20;

/// Opt-in corpus-level theme detection pass.
///
/// Only runs when there are at least `MIN_ENTITIES_FOR_THEMES` entities
/// in the corpus. Uses `claude-sonnet-4-5` via the adapter's `extract_themes`.
pub async fn run(
    db: Arc<dyn StorageBackend>,
    corpus: &Corpus,
    adapter: Arc<dyn SourceAdapter>,
    llm: Arc<dyn LlmProvider>,
    opts: &IndexOptions,
) -> anyhow::Result<PassStats> {
    let mut stats = PassStats::default();

    if opts.dry_run {
        return Ok(stats);
    }

    // Skip when the manifest says nothing changed — theme detection is
    // corpus-wide and there's nothing useful to do if no sources are dirty.
    if opts
        .change_manifest
        .as_ref()
        .is_some_and(|m| !m.all_dirty && m.dirty_count() == 0)
    {
        tracing::info!("[theme] no dirty sources — skipping theme detection");
        return Ok(stats);
    }

    // Pre-condition: need enough entities to infer themes.
    // When a ReadView is attached (HistoryBackfill mode), read from there so
    // the count reflects the historical state at the target commit SHA.
    let entity_count = if let Some(rv) = opts.read_view.as_ref() {
        rv.entity_count()?
    } else {
        db.entity_count(&corpus.id)?
    };
    if entity_count < MIN_ENTITIES_FOR_THEMES {
        stats.skipped += 1;
        return Ok(stats);
    }

    let all_entities: Vec<Entity> = if let Some(rv) = opts.read_view.as_ref() {
        rv.entity_list()?
    } else {
        db.entity_list(&corpus.id)?
    };

    match adapter
        .extract_themes(corpus, &all_entities, llm.as_ref())
        .await
    {
        Ok(Some(extracted)) => {
            let now = chrono::Utc::now().to_rfc3339();
            let version = opts
                .change_manifest
                .as_ref()
                .map(|m| m.current_version.clone());

            for et in &extracted.themes {
                let slug = slugify(&et.title);
                let theme_id = format!("{}:theme:{}", corpus.id, slug);

                // Insert a theme entity row (kind=theme) so FK is satisfied.
                let mut theme_entity = Entity::new(
                    theme_id.clone(),
                    corpus.id.clone(),
                    et.title.clone(),
                    "theme".to_string(),
                );
                theme_entity.derived_at_version = version.clone();
                db.entity_upsert(&theme_entity)?;

                // Insert theme record.
                let model = llm.name().to_string();
                let tier = model_tier(&model).to_string();
                let theme = Theme {
                    id: theme_id.clone(),
                    corpus_id: corpus.id.clone(),
                    title: et.title.clone(),
                    statement: et.statement.clone(),
                    confidence: et.confidence,
                    model,
                    model_tier: tier,
                    generated_at: now.clone(),
                    derived_at_version: version.clone(),
                };
                db.theme_upsert(&theme)?;

                // Resolve upheld_by entity names → edges.
                for name in &et.upheld_by_entity_names {
                    let matched = if let Some(rv) = opts.read_view.as_ref() {
                        rv.entity_find_by_name(name)?
                    } else {
                        db.entity_find_by_name(&corpus.id, name)?
                    };
                    for entity in matched.iter() {
                        let edge = Edge {
                            id: Uuid::new_v4().to_string(),
                            corpus_id: corpus.id.clone(),
                            from_entity_id: entity.id.clone(),
                            to_entity_id: theme_id.clone(),
                            kind: "upheld_by".to_string(),
                            location: crate::types::Location::new(&corpus.id, ""),
                            confidence: 0.5,
                            derived_at_version: version.clone(),
                        };
                        db.edge_upsert(&edge)?;
                    }
                }

                // Resolve violated_by entity names → edges.
                for name in &et.violated_by_entity_names {
                    let matched = if let Some(rv) = opts.read_view.as_ref() {
                        rv.entity_find_by_name(name)?
                    } else {
                        db.entity_find_by_name(&corpus.id, name)?
                    };
                    for entity in matched.iter() {
                        let edge = Edge {
                            id: Uuid::new_v4().to_string(),
                            corpus_id: corpus.id.clone(),
                            from_entity_id: entity.id.clone(),
                            to_entity_id: theme_id.clone(),
                            kind: "violated_by".to_string(),
                            location: crate::types::Location::new(&corpus.id, ""),
                            confidence: 0.5,
                            derived_at_version: version.clone(),
                        };
                        db.edge_upsert(&edge)?;
                    }
                }

                stats.processed += 1;
            }
        }
        Ok(None) => {
            stats.skipped += 1;
        }
        Err(e) => {
            tracing::warn!("theme pass failed for corpus {}: {e}", corpus.id);
            stats.failed += 1;
        }
    }

    Ok(stats)
}

/// Convert a title to a URL-safe slug: lowercase, spaces → hyphens, strip non-alphanumeric.
fn slugify(title: &str) -> String {
    title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}
