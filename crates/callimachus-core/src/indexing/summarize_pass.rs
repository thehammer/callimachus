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

    // ── 1. Scene-level summaries ──────────────────────────────────────────────
    let scene_chunks: Vec<_> = all_chunks.iter().filter(|c| c.kind == "scene").collect();
    let scene_total = scene_chunks.len() as u64;
    let chapter_count_for_log = all_chunks.iter().filter(|c| c.kind == "chapter").count();

    tracing::info!(
        "[summarize] {} scene chunks, {} chapter chunks",
        scene_total,
        chapter_count_for_log
    );
    if scene_total == 0 && chapter_count_for_log == 0 {
        tracing::info!(
            "[summarize] nothing to summarize at scene/chapter level — will attempt corpus-level summary from semantic data"
        );
    }

    for (i, chunk) in scene_chunks.iter().enumerate() {
        // Idempotent: skip if already summarized.
        if db
            .summary_get(&corpus.id, &SummaryTargetKind::Chunk, &chunk.id)?
            .is_some()
        {
            stats.skipped += 1;
            let completed = i as u64 + 1;
            if completed.is_multiple_of(25) {
                tracing::info!("[summarize] scene {}/{} chunks", completed, scene_total);
            }
            continue;
        }

        match adapter.summarize(chunk, llm.as_ref(), "scene").await {
            Ok(Some(text)) => {
                if !opts.dry_run {
                    let summary = make_summary(corpus, chunk.id.clone(), "scene", text, &llm);
                    db.summary_upsert(&summary)?;
                }
                stats.processed += 1;
            }
            Ok(None) => {
                stats.skipped += 1;
            }
            Err(e) => {
                tracing::warn!("scene summary failed for {}: {e}", chunk.id);
                stats.failed += 1;
            }
        }

        let completed = i as u64 + 1;
        if completed.is_multiple_of(25) {
            tracing::info!("[summarize] scene {}/{} chunks", completed, scene_total);
        }
    }

    // ── 2. Chapter-level summaries ────────────────────────────────────────────
    let chapter_chunks: Vec<_> = all_chunks.iter().filter(|c| c.kind == "chapter").collect();
    let chapter_total = chapter_chunks.len() as u64;

    for (i, chapter) in chapter_chunks.iter().enumerate() {
        if db
            .summary_get(&corpus.id, &SummaryTargetKind::Chunk, &chapter.id)?
            .is_some()
        {
            stats.skipped += 1;
            let completed = i as u64 + 1;
            if completed.is_multiple_of(25) {
                tracing::info!("[summarize] chapter {}/{} chunks", completed, chapter_total);
            }
            continue;
        }

        // Collect scene summaries for this chapter.
        let child_summaries: Vec<String> = all_chunks
            .iter()
            .filter(|c| {
                c.kind == "scene" && c.parent_path.as_deref() == Some(&chapter.location.path)
            })
            .filter_map(|scene| {
                db.summary_get(&corpus.id, &SummaryTargetKind::Chunk, &scene.id)
                    .ok()
                    .flatten()
                    .map(|s| s.text)
            })
            .collect();

        // Build a synthetic chunk whose content is the collected scene summaries.
        let context_chunk = if child_summaries.is_empty() {
            (*chapter).clone()
        } else {
            let mut c = (*chapter).clone();
            c.content = child_summaries
                .iter()
                .enumerate()
                .map(|(j, s)| format!("Scene {}: {s}", j + 1))
                .collect::<Vec<_>>()
                .join("\n\n");
            c
        };

        match adapter
            .summarize(&context_chunk, llm.as_ref(), "chapter")
            .await
        {
            Ok(Some(text)) => {
                if !opts.dry_run {
                    let summary = make_summary(corpus, chapter.id.clone(), "chapter", text, &llm);
                    db.summary_upsert(&summary)?;
                }
                stats.processed += 1;
            }
            Ok(None) => {
                stats.skipped += 1;
            }
            Err(e) => {
                tracing::warn!("chapter summary failed for {}: {e}", chapter.id);
                stats.failed += 1;
            }
        }

        let completed = i as u64 + 1;
        if completed.is_multiple_of(25) {
            tracing::info!("[summarize] chapter {}/{} chunks", completed, chapter_total);
        }
    }

    // ── 3. Corpus-level summary ───────────────────────────────────────────────
    if db
        .summary_get(&corpus.id, &SummaryTargetKind::Corpus, &corpus.id)?
        .is_some()
    {
        stats.skipped += 1;
        return Ok(stats);
    }

    // Collect chapter summaries.
    let all_chapter_summaries: Vec<String> = chapter_chunks
        .iter()
        .filter_map(|ch| {
            db.summary_get(&corpus.id, &SummaryTargetKind::Chunk, &ch.id)
                .ok()
                .flatten()
                .map(|s| s.text)
        })
        .collect();

    if !all_chapter_summaries.is_empty() {
        // Synthesize a corpus chunk.
        let corpus_chunk = {
            let representative = &all_chunks[0];
            let mut c = representative.clone();
            c.id = corpus.id.clone();
            c.kind = "corpus".to_string();
            c.content = all_chapter_summaries
                .iter()
                .enumerate()
                .map(|(i, s)| format!("Chapter {}: {s}", i + 1))
                .collect::<Vec<_>>()
                .join("\n\n");
            c
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
    } else {
        // Code corpora have no chapter chunks; fall back to entity descriptions
        // populated by the semantic pass.
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
            tracing::info!("[summarize] no entity descriptions available; skipping corpus summary");
        } else {
            tracing::info!(
                "[summarize] synthesizing corpus summary from {} entity descriptions",
                description_lines.len()
            );

            let corpus_chunk = {
                let representative = &all_chunks[0];
                let mut c = representative.clone();
                c.id = corpus.id.clone();
                c.kind = "corpus".to_string();
                c.content = description_lines.join("\n");
                c
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
        }
    }

    Ok(stats)
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
