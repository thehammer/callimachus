use std::sync::Arc;

use callimachus_llm::{LlmError, LlmProvider, model_tier};
use futures::StreamExt;
use uuid::Uuid;

use crate::{
    adapter::SourceAdapter,
    indexing::model_tier::{ModelTier, ModelTierRouter, TierConfig},
    storage::{StorageBackend, run_log::PassStats},
    types::{Corpus, Entity, EntityBlock, EntityPurpose},
};

use super::{change_manifest::file_path_from_uri, pipeline::IndexOptions};

const MAX_RETRIES: u32 = 8;

/// Kinds of entities for which we extract purpose.
const PURPOSE_KINDS: &[&str] = &["function", "method", "class", "interface", "module"];

// ─── Per-entity outcome ───────────────────────────────────────────────────────

/// Result of processing one entity in the concurrent phase.
enum PurposeOutcome {
    Skip,
    Failed(String),
    Extracted {
        tier: ModelTier,
        purpose: EntityPurpose,
        blocks: Vec<BlockData>,
        edges: Vec<crate::types::Edge>,
        block_entities: Vec<Entity>,
    },
}

struct BlockData {
    entity_block: EntityBlock,
}

// ─── Main pass ────────────────────────────────────────────────────────────────

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
    // Keep tier_config for owned clone inside closures.
    let tier_config = opts.tier_config.clone();
    let mut tier_counts = [0u64; 3]; // [haiku, sonnet, opus]

    if opts.dry_run {
        return Ok(stats);
    }

    // Determine concurrency width.
    let concurrency = opts
        .concurrency
        .or_else(|| {
            llm_haiku
                .concurrency_limiter()
                .map(|l| l.initial() as usize)
                .filter(|&n| n > 0)
        })
        .unwrap_or(4);

    let all_entities = db.entity_list(&corpus.id)?;
    let candidates: Vec<Entity> = all_entities
        .into_iter()
        .filter(|e| PURPOSE_KINDS.contains(&e.kind.as_str()))
        .filter(|e| {
            opts.change_manifest
                .as_ref()
                .map(|m| {
                    e.first_location
                        .as_ref()
                        .map(|loc| m.is_dirty(file_path_from_uri(&loc.uri)))
                        .unwrap_or(true)
                })
                .unwrap_or(true)
        })
        .collect();
    let total = candidates.len() as u64;

    // ── Concurrent phase: read-only + LLM ────────────────────────────────────

    let ctx = Arc::new(PassContext {
        db: Arc::clone(&db),
        corpus_id: corpus.id.clone(),
        adapter: Arc::clone(&adapter),
        llm_haiku: Arc::clone(&llm_haiku),
        llm_sonnet: Arc::clone(&llm_sonnet),
        llm_opus: Arc::clone(&llm_opus),
        tier_config,
        full: opts.full,
    });

    let outcomes: Vec<PurposeOutcome> = futures::stream::iter(candidates.iter())
        .map(|entity| {
            let ctx = Arc::clone(&ctx);
            let entity = entity.clone();
            async move { process_entity(&ctx, &entity).await }
        })
        .buffer_unordered(concurrency)
        .collect()
        .await;

    // ── Serial sink: DB writes + stats ────────────────────────────────────────

    for (i, outcome) in outcomes.into_iter().enumerate() {
        match outcome {
            PurposeOutcome::Skip => {
                stats.skipped += 1;
            }
            PurposeOutcome::Failed(msg) => {
                tracing::warn!("purpose pass failed for entity {}/{total}: {msg}", i + 1);
                stats.failed += 1;
            }
            PurposeOutcome::Extracted {
                tier,
                purpose,
                blocks,
                edges,
                block_entities,
            } => {
                db.purpose_upsert(&purpose)?;
                for be in &block_entities {
                    db.entity_upsert(be)?;
                }
                for block in &blocks {
                    db.block_upsert(&block.entity_block)?;
                }
                for edge in &edges {
                    db.edge_upsert(edge)?;
                }
                tier_counts[tier as usize] += 1;
                stats.processed += 1;
            }
        }

        let completed = i as u64 + 1;
        if completed.is_multiple_of(25) {
            tracing::info!("[purpose] {}/{} entities", completed, total);
        }
    }

    // ── Populate concurrency stats into PassStats ─────────────────────────────
    if let Some(limiter) = llm_haiku.concurrency_limiter() {
        let cs = limiter.stats();
        stats.requests_made = Some(cs.requests_made);
        stats.avg_concurrency = Some(cs.avg_concurrency);
        stats.peak_concurrency = Some(cs.peak_concurrency);
        tracing::info!(
            "[purpose] avg_concurrency={:.1} peak={} requests={}",
            cs.avg_concurrency,
            cs.peak_concurrency,
            cs.requests_made,
        );
        limiter.reset();
    }

    tracing::info!(
        "[purpose] tier distribution: haiku={} sonnet={} opus={}",
        tier_counts[ModelTier::Haiku as usize],
        tier_counts[ModelTier::Sonnet as usize],
        tier_counts[ModelTier::Opus as usize],
    );

    Ok(stats)
}

// ─── Per-entity async work (read-only + LLM) ─────────────────────────────────

struct PassContext {
    db: Arc<dyn StorageBackend>,
    corpus_id: String,
    adapter: Arc<dyn SourceAdapter>,
    llm_haiku: Arc<dyn LlmProvider>,
    llm_sonnet: Arc<dyn LlmProvider>,
    llm_opus: Arc<dyn LlmProvider>,
    tier_config: TierConfig,
    full: bool,
}

async fn process_entity(ctx: &PassContext, entity: &Entity) -> PurposeOutcome {
    let db = &ctx.db;
    let corpus_id = ctx.corpus_id.as_str();
    let adapter = &ctx.adapter;
    let llm_haiku = &ctx.llm_haiku;
    let llm_sonnet = &ctx.llm_sonnet;
    let llm_opus = &ctx.llm_opus;
    let tier_config = &ctx.tier_config;
    let full = ctx.full;
    // Fetch content via first location.
    let content = match entity.first_location.as_ref() {
        Some(loc) => match db.chunk_get_by_uri(&loc.uri) {
            Ok(Some(chunk)) => chunk.content,
            _ => return PurposeOutcome::Skip,
        },
        None => return PurposeOutcome::Skip,
    };

    // Compute routing inputs.
    let in_deg = db.entity_in_degree(corpus_id, &entity.id).unwrap_or(0);
    let out_deg = db.entity_out_degree(corpus_id, &entity.id).unwrap_or(0);
    let language = entity
        .first_location
        .as_ref()
        .map(|l| l.uri.as_str())
        .map(detect_language_from_uri)
        .unwrap_or("unknown");
    let mut routing = adapter.static_routing_inputs(language, &content, &entity.canonical_name);
    routing.kind = entity.kind.clone();
    routing.in_degree = in_deg;
    routing.out_degree = out_deg;

    let router = ModelTierRouter::new(tier_config);
    let tier = router.route(&routing);
    let llm: &dyn LlmProvider = match tier {
        ModelTier::Haiku => llm_haiku.as_ref(),
        ModelTier::Sonnet => llm_sonnet.as_ref(),
        ModelTier::Opus => llm_opus.as_ref(),
    };

    tracing::debug!(
        "[purpose] entity={} tier={} kind={} in_deg={} out_deg={} fallible={} panics={}",
        entity.id,
        tier,
        entity.kind,
        in_deg,
        out_deg,
        routing.is_fallible,
        routing.panic_call_count,
    );

    // Idempotency: skip if this exact model already produced an artifact.
    let model_name = llm.name();
    if !full
        && db
            .purpose_get_for_model(corpus_id, &entity.id, model_name)
            .is_ok_and(|r| r.is_some())
    {
        return PurposeOutcome::Skip;
    }

    // Fetch existing summary text if available.
    let summary_opt = db
        .summary_get(
            corpus_id,
            &crate::types::SummaryTargetKind::Entity,
            &entity.id,
        )
        .ok()
        .flatten()
        .map(|s| s.text);

    // Call adapter with retry.
    match extract_with_retry(
        adapter.as_ref(),
        entity,
        &content,
        summary_opt.as_deref(),
        llm,
    )
    .await
    {
        Ok(Some(extracted)) => {
            let now = chrono::Utc::now().to_rfc3339();
            let model = llm.name().to_string();
            let tier_str = model_tier(&model).to_string();
            let purpose = EntityPurpose {
                entity_id: entity.id.clone(),
                corpus_id: corpus_id.to_string(),
                purpose: extracted.purpose,
                model,
                model_tier: tier_str,
                generated_at: now.clone(),
            };

            let mut block_entities = Vec::new();
            let mut blocks = Vec::new();
            let mut edges = Vec::new();

            for (i, block) in extracted.blocks.iter().enumerate() {
                let block_entity_id = format!("{}:block:{}", entity.id, i);
                let block_entity = Entity::new(
                    block_entity_id.clone(),
                    corpus_id.to_string(),
                    block.label.clone(),
                    "block".to_string(),
                );
                block_entities.push(block_entity);

                let eb = EntityBlock {
                    id: Uuid::new_v4().to_string(),
                    entity_id: entity.id.clone(),
                    corpus_id: corpus_id.to_string(),
                    label: block.label.clone(),
                    description: block.description.clone(),
                    position: i as i64,
                };
                blocks.push(BlockData { entity_block: eb });

                let edge = crate::types::Edge {
                    id: Uuid::new_v4().to_string(),
                    corpus_id: corpus_id.to_string(),
                    from_entity_id: entity.id.clone(),
                    to_entity_id: block_entity_id,
                    kind: "defines".to_string(),
                    location: crate::types::Location::new(corpus_id, ""),
                    confidence: 0.5,
                };
                edges.push(edge);
            }

            PurposeOutcome::Extracted {
                tier,
                purpose,
                blocks,
                edges,
                block_entities,
            }
        }
        Ok(None) => PurposeOutcome::Skip,
        Err(e) => PurposeOutcome::Failed(format!("{}: {e}", entity.id)),
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn detect_language_from_uri(uri: &str) -> &'static str {
    let path = uri.split('#').next().unwrap_or(uri);
    if path.ends_with(".rs") {
        "rust"
    } else if path.ends_with(".ts") || path.ends_with(".tsx") || path.ends_with(".js") {
        "typescript"
    } else if path.ends_with(".py") {
        "python"
    } else if path.ends_with(".go") {
        "go"
    } else if path.ends_with(".php") {
        "php"
    } else {
        "unknown"
    }
}

async fn extract_with_retry(
    adapter: &dyn SourceAdapter,
    entity: &Entity,
    content: &str,
    summary: Option<&str>,
    llm: &dyn LlmProvider,
) -> anyhow::Result<Option<crate::adapter::ExtractedPurpose>> {
    let mut attempts = 0u32;
    loop {
        attempts += 1;
        match adapter.extract_purpose(entity, content, summary, llm).await {
            Ok(result) => return Ok(result),
            Err(e) => {
                if let Some(LlmError::RateLimited { retry_after_secs }) =
                    e.downcast_ref::<LlmError>()
                    && attempts < MAX_RETRIES
                {
                    let backoff = *retry_after_secs;
                    tracing::warn!(
                        "purpose pass rate limited; backing off {backoff}s (attempt {attempts})"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
                    continue;
                }
                if let Some(LlmError::Timeout { .. }) = e.downcast_ref::<LlmError>()
                    && attempts < MAX_RETRIES
                {
                    let backoff = 5u64 * 2u64.pow(attempts - 1);
                    tracing::warn!(
                        "purpose pass timeout; backing off {backoff}s (attempt {attempts})"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
                    continue;
                }
                return Err(e);
            }
        }
    }
}
