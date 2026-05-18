use std::sync::Arc;

use callimachus_llm::LlmProvider;
use uuid::Uuid;

use crate::{
    adapter::SourceAdapter,
    storage::{StorageBackend, run_log::PassStats},
    types::{Corpus, Summary, SummaryTargetKind},
};

use super::pipeline::IndexOptions;

pub async fn run(
    db: Arc<dyn StorageBackend>,
    corpus: &Corpus,
    adapter: Arc<dyn SourceAdapter>,
    llm: Arc<dyn LlmProvider>,
    opts: &IndexOptions,
) -> anyhow::Result<PassStats> {
    let mut stats = PassStats::default();

    let all_chunks = db.chunk_list(&corpus.id)?;
    let levels = adapter.summary_levels();

    tracing::info!(
        "[summarize] {} levels declared by adapter: {:?}",
        levels.len(),
        levels
    );

    // ── Level-by-level summaries ──────────────────────────────────────────────
    for (level_idx, &level_kind) in levels.iter().enumerate() {
        let level_chunks: Vec<_> = all_chunks.iter().filter(|c| c.kind == level_kind).collect();
        let level_total = level_chunks.len() as u64;

        tracing::info!("[summarize] level '{level_kind}': {level_total} chunks");

        for (i, chunk) in level_chunks.iter().enumerate() {
            // Idempotent: skip if already summarized (unless --full).
            if !opts.full
                && db
                    .summary_get(&corpus.id, &SummaryTargetKind::Chunk, &chunk.id)?
                    .is_some()
            {
                stats.skipped += 1;
                log_progress(level_kind, i as u64 + 1, level_total);
                continue;
            }

            // Build context from child level summaries (if this is not the first level).
            let context_chunk = if level_idx > 0 {
                let child_kind = levels[level_idx - 1];
                let child_summaries: Vec<String> = all_chunks
                    .iter()
                    .filter(|c| {
                        c.kind == child_kind
                            && c.parent_path.as_deref() == Some(&chunk.location.path)
                    })
                    .filter_map(|child| {
                        db.summary_get(&corpus.id, &SummaryTargetKind::Chunk, &child.id)
                            .ok()
                            .flatten()
                            .map(|s| s.text)
                    })
                    .collect();

                if child_summaries.is_empty() {
                    (*chunk).clone()
                } else {
                    let mut c = (*chunk).clone();
                    c.content = child_summaries
                        .iter()
                        .enumerate()
                        .map(|(j, s)| format!("{} {}: {s}", child_kind, j + 1))
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    c
                }
            } else {
                (*chunk).clone()
            };

            match adapter
                .summarize(&context_chunk, llm.as_ref(), level_kind)
                .await
            {
                Ok(Some(text)) => {
                    if !opts.dry_run {
                        let summary =
                            make_summary(corpus, chunk.id.clone(), level_kind, text, &llm);
                        db.summary_upsert(&summary)?;
                    }
                    stats.processed += 1;
                }
                Ok(None) => {
                    stats.skipped += 1;
                }
                Err(e) => {
                    tracing::warn!("{level_kind} summary failed for {}: {e}", chunk.id);
                    stats.failed += 1;
                }
            }

            log_progress(level_kind, i as u64 + 1, level_total);
        }
    }

    // ── Corpus-level summary ──────────────────────────────────────────────────
    if !opts.full
        && db
            .summary_get(&corpus.id, &SummaryTargetKind::Corpus, &corpus.id)?
            .is_some()
    {
        stats.skipped += 1;
        return Ok(stats);
    }

    // Collect summaries from the deepest declared level (or fall back to entity descriptions).
    let top_level_summaries: Vec<String> = if let Some(&top_kind) = levels.last() {
        all_chunks
            .iter()
            .filter(|c| c.kind == top_kind)
            .filter_map(|ch| {
                db.summary_get(&corpus.id, &SummaryTargetKind::Chunk, &ch.id)
                    .ok()
                    .flatten()
                    .map(|s| s.text)
            })
            .collect()
    } else {
        // No levels declared — build corpus chunk from all-chunk content.
        vec![]
    };

    let corpus_chunk_content = if !top_level_summaries.is_empty() {
        let top_kind = levels.last().copied().unwrap_or("chunk");
        top_level_summaries
            .iter()
            .enumerate()
            .map(|(i, s)| format!("{} {}: {s}", top_kind, i + 1))
            .collect::<Vec<_>>()
            .join("\n\n")
    } else {
        // Fall back to entity descriptions (covers code corpora with no declared levels).
        let entities = db.entity_list(&corpus.id)?;
        let description_lines: Vec<String> = entities
            .iter()
            .filter_map(|e| {
                e.description
                    .as_deref()
                    .filter(|d| !d.is_empty())
                    .map(|d| format!("{} ({}): {}", e.canonical_name, e.kind, d))
            })
            .collect();

        if description_lines.is_empty() {
            tracing::info!(
                "[summarize] no summaries or entity descriptions available; skipping corpus summary"
            );
            return Ok(stats);
        }

        tracing::info!(
            "[summarize] synthesizing corpus summary from {} entity descriptions",
            description_lines.len()
        );
        description_lines.join("\n")
    };

    let corpus_chunk = {
        if let Some(representative) = all_chunks.first() {
            let mut c = representative.clone();
            c.id = corpus.id.clone();
            c.kind = "corpus".to_string();
            c.content = corpus_chunk_content;
            c
        } else {
            tracing::info!("[summarize] no chunks available; skipping corpus summary");
            return Ok(stats);
        }
    };

    match adapter
        .summarize(&corpus_chunk, llm.as_ref(), "corpus")
        .await
    {
        Ok(Some(text)) => {
            if !opts.dry_run {
                let summary = Summary {
                    id: Uuid::new_v4().to_string(),
                    corpus_id: corpus.id.clone(),
                    target_kind: SummaryTargetKind::Corpus,
                    target_id: corpus.id.clone(),
                    depth: "corpus".to_string(),
                    text,
                    model: Some(llm.name().to_string()),
                    generated_at: chrono::Utc::now().to_rfc3339(),
                };
                db.summary_upsert(&summary)?;
            }
            stats.processed += 1;
        }
        Ok(None) => {
            stats.skipped += 1;
        }
        Err(e) => {
            tracing::warn!("corpus summary failed: {e}");
            stats.failed += 1;
        }
    }

    Ok(stats)
}

fn log_progress(kind: &str, completed: u64, total: u64) {
    if total > 0 && completed.is_multiple_of(25) {
        tracing::info!("[summarize] {kind} {}/{} chunks", completed, total);
    }
}

fn make_summary(
    corpus: &Corpus,
    target_id: String,
    depth: &str,
    text: String,
    llm: &Arc<dyn LlmProvider>,
) -> Summary {
    Summary {
        id: Uuid::new_v4().to_string(),
        corpus_id: corpus.id.clone(),
        target_kind: SummaryTargetKind::Chunk,
        target_id,
        depth: depth.to_string(),
        text,
        model: Some(llm.name().to_string()),
        generated_at: chrono::Utc::now().to_rfc3339(),
    }
}
