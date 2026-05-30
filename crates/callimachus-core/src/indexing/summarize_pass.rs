use std::sync::Arc;

use callimachus_llm::{LlmProvider, model_tier};
use uuid::Uuid;

use crate::{
    adapter::SourceAdapter,
    indexing::model_tier::{ModelTier, ModelTierRouter, RoutingInputs},
    storage::{StorageBackend, run_log::PassStats},
    types::{Corpus, Layer2CacheKey, Summary, SummaryTargetKind, chunk::hash_content},
};

use super::{layer2_cache, pipeline::IndexOptions};

pub async fn run(
    db: Arc<dyn StorageBackend>,
    corpus: &Corpus,
    adapter: Arc<dyn SourceAdapter>,
    llm_haiku: Arc<dyn LlmProvider>,
    llm_sonnet: Arc<dyn LlmProvider>,
    llm_opus: Arc<dyn LlmProvider>,
    opts: &IndexOptions,
) -> anyhow::Result<PassStats> {
    let mut stats = PassStats::default();
    let router = ModelTierRouter::new(&opts.tier_config);
    let mut tier_counts = [0u64; 3]; // [haiku, sonnet, opus]

    let mut all_chunks = db.chunk_list(&corpus.id)?;

    // Skip chunks whose source file is unchanged according to the manifest.
    if let Some(m) = opts.change_manifest.as_ref() {
        all_chunks.retain(|c| m.is_dirty_for_chunk(c));
    }

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
            // Count child summaries for tier routing.
            let (context_chunk, child_summary_count) = if level_idx > 0 {
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

                let count = child_summaries.len() as u32;
                if child_summaries.is_empty() {
                    ((*chunk).clone(), count)
                } else {
                    let mut c = (*chunk).clone();
                    c.content = child_summaries
                        .iter()
                        .enumerate()
                        .map(|(j, s)| format!("{} {}: {s}", child_kind, j + 1))
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    (c, count)
                }
            } else {
                ((*chunk).clone(), 0u32)
            };

            // Tier routing for chunks.
            // NOTE: Unlike entity passes, chunks lack entity-level signals (unsafe,
            // fallibility, etc.).  We derive a proxy from chunk properties:
            // body_lines ≈ content length, has_debt_markers from content scan,
            // and child_summary_count as a rough proxy for out-degree.
            // See docs/plans/tiered-model-selection.md for rationale.
            let chunk_lines = context_chunk.content.lines().count() as u32;
            let has_debt_markers = context_chunk.content.contains("FIXME")
                || context_chunk.content.contains("HACK")
                || context_chunk.content.contains("TODO");
            let chunk_routing = RoutingInputs {
                body_lines: chunk_lines,
                has_debt_markers,
                out_degree: child_summary_count,
                kind: level_kind.to_string(),
                ..RoutingInputs::default()
            };
            let tier = router.route(&chunk_routing);
            let llm: &dyn LlmProvider = match tier {
                ModelTier::Haiku => llm_haiku.as_ref(),
                ModelTier::Sonnet => llm_sonnet.as_ref(),
                ModelTier::Opus => llm_opus.as_ref(),
            };
            tier_counts[tier as usize] += 1;

            // Use the appropriate provider wrapper for make_summary.
            let llm_arc: Arc<dyn LlmProvider> = match tier {
                ModelTier::Haiku => Arc::clone(&llm_haiku),
                ModelTier::Sonnet => Arc::clone(&llm_sonnet),
                ModelTier::Opus => Arc::clone(&llm_opus),
            };

            // Layer-2 cache: a chunk summary is a deterministic function of the
            // (possibly rolled-up) input text, so we key on a hash of that
            // exact input plus depth + model. A hit skips the LLM call.
            let cache_key = Layer2CacheKey {
                artifact_kind: "summary".to_string(),
                entity_id: Some(format!("{level_kind}:{}", chunk.id)),
                content_hash: hash_content(&context_chunk.content),
                file_shape_hash: String::new(),
                model: llm.name().to_string(),
                stable_sampling: opts.stable_sampling,
            };
            let sha = opts
                .change_manifest
                .as_ref()
                .map(|m| m.current_version.as_str())
                .unwrap_or("");
            let summarized = match layer2_cache::cache_get::<String>(db.as_ref(), &cache_key) {
                Ok(Some(hit)) => Ok(Some(hit)),
                Ok(None) => match adapter.summarize(&context_chunk, llm, level_kind).await {
                    Ok(Some(text)) => {
                        if let Err(e) =
                            layer2_cache::cache_put(db.as_ref(), &cache_key, &text, sha)
                        {
                            tracing::warn!("summary cache_put failed for {}: {e}", chunk.id);
                        }
                        Ok(Some(text))
                    }
                    other => other,
                },
                Err(e) => Err(e),
            };

            match summarized {
                Ok(Some(text)) => {
                    if !opts.dry_run {
                        let version = opts
                            .change_manifest
                            .as_ref()
                            .map(|m| m.current_version.as_str());
                        let summary = make_summary(
                            corpus,
                            chunk.id.clone(),
                            level_kind,
                            text,
                            &llm_arc,
                            version,
                        );
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

    // Route the corpus-level summary to Sonnet (it's always a moderate-complexity task).
    let corpus_llm: &dyn LlmProvider = llm_sonnet.as_ref();
    tier_counts[ModelTier::Sonnet as usize] += 1;

    tracing::info!(
        "[summarize] tier distribution: haiku={} sonnet={} opus={}",
        tier_counts[ModelTier::Haiku as usize],
        tier_counts[ModelTier::Sonnet as usize],
        tier_counts[ModelTier::Opus as usize],
    );

    let corpus_cache_key = Layer2CacheKey {
        artifact_kind: "summary".to_string(),
        entity_id: Some(format!("corpus:{}", corpus.id)),
        content_hash: hash_content(&corpus_chunk.content),
        file_shape_hash: String::new(),
        model: corpus_llm.name().to_string(),
        stable_sampling: opts.stable_sampling,
    };
    let corpus_sha = opts
        .change_manifest
        .as_ref()
        .map(|m| m.current_version.as_str())
        .unwrap_or("");
    let corpus_summarized = match layer2_cache::cache_get::<String>(db.as_ref(), &corpus_cache_key) {
        Ok(Some(hit)) => Ok(Some(hit)),
        Ok(None) => match adapter.summarize(&corpus_chunk, corpus_llm, "corpus").await {
            Ok(Some(text)) => {
                if let Err(e) =
                    layer2_cache::cache_put(db.as_ref(), &corpus_cache_key, &text, corpus_sha)
                {
                    tracing::warn!("corpus summary cache_put failed: {e}");
                }
                Ok(Some(text))
            }
            other => other,
        },
        Err(e) => Err(e),
    };

    match corpus_summarized {
        Ok(Some(text)) => {
            if !opts.dry_run {
                let model = corpus_llm.name().to_string();
                let tier = model_tier(&model).to_string();
                let derived_at_version = opts
                    .change_manifest
                    .as_ref()
                    .map(|m| m.current_version.clone());
                let summary = Summary {
                    id: Uuid::new_v4().to_string(),
                    corpus_id: corpus.id.clone(),
                    target_kind: SummaryTargetKind::Corpus,
                    target_id: corpus.id.clone(),
                    depth: "corpus".to_string(),
                    text,
                    model,
                    model_tier: tier,
                    generated_at: chrono::Utc::now().to_rfc3339(),
                    derived_at_version,
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
    derived_at_version: Option<&str>,
) -> Summary {
    let model = llm.name().to_string();
    let tier = model_tier(&model).to_string();
    Summary {
        id: Uuid::new_v4().to_string(),
        corpus_id: corpus.id.clone(),
        target_kind: SummaryTargetKind::Chunk,
        target_id,
        depth: depth.to_string(),
        text,
        model,
        model_tier: tier,
        generated_at: chrono::Utc::now().to_rfc3339(),
        derived_at_version: derived_at_version.map(str::to_string),
    }
}
