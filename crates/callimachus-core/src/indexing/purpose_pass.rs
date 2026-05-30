use std::sync::Arc;

use callimachus_llm::{LlmError, LlmProvider, model_tier};
use futures::StreamExt;
use uuid::Uuid;

use std::collections::HashMap;

use crate::{
    adapter::SourceAdapter,
    indexing::model_tier::{ModelTier, ModelTierRouter, TierConfig},
    storage::{StorageBackend, run_log::PassStats},
    types::{Corpus, Entity, EntityBlock, EntityPurpose, Layer2CacheKey},
};

use super::{change_manifest::file_path_from_uri, file_shape, layer2_cache, pipeline::IndexOptions};

const MAX_RETRIES: u32 = 8;

/// Kinds of entities for which we extract purpose.
const PURPOSE_KINDS: &[&str] = &["function", "method", "class", "interface", "module"];

// ─── Per-entity outcome ───────────────────────────────────────────────────────

struct ExtractedPurpose {
    tier: ModelTier,
    purpose: EntityPurpose,
    blocks: Vec<BlockData>,
    edges: Vec<crate::types::Edge>,
    block_entities: Vec<Entity>,
}

/// Result of processing one entity in the concurrent phase.
enum PurposeOutcome {
    Skip,
    Failed(String),
    Extracted(Box<ExtractedPurpose>),
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
    let concurrency = opts.concurrency.unwrap_or(64);

    let all_entities = db.entity_list(&corpus.id)?;
    // Authoritative file-shape map for cache keys, computed from live entity
    // state (covers entities created by the semantic pass, not only structural).
    let file_shapes = file_shape::file_shapes_by_uri(&all_entities);
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

    let current_version = opts
        .change_manifest
        .as_ref()
        .map(|m| m.current_version.clone());

    let ctx = Arc::new(PassContext {
        db: Arc::clone(&db),
        corpus_id: corpus.id.clone(),
        adapter: Arc::clone(&adapter),
        llm_haiku: Arc::clone(&llm_haiku),
        llm_sonnet: Arc::clone(&llm_sonnet),
        llm_opus: Arc::clone(&llm_opus),
        tier_config,
        full: opts.full,
        current_version,
        file_shapes,
        stable_sampling: opts.stable_sampling,
    });

    // ── Interleaved concurrent LLM + per-entity DB writes ────────────────────
    //
    // Each entity is persisted to the DB *as soon as* its LLM call returns,
    // not after the entire pass completes. This bounds the work-loss window
    // on interrupt to ~concurrency in-flight entities, instead of the whole
    // pass.
    let mut stream = futures::stream::iter(candidates)
        .map(|entity| {
            let ctx = Arc::clone(&ctx);
            async move { process_entity(&ctx, &entity).await }
        })
        .buffer_unordered(concurrency);

    let mut completed: u64 = 0;
    while let Some(outcome) = stream.next().await {
        completed += 1;
        match outcome {
            PurposeOutcome::Skip => {
                stats.skipped += 1;
            }
            PurposeOutcome::Failed(msg) => {
                tracing::warn!("purpose pass failed for entity {completed}/{total}: {msg}");
                stats.failed += 1;
            }
            PurposeOutcome::Extracted(ext) => {
                db.purpose_upsert(&ext.purpose)?;
                for be in &ext.block_entities {
                    db.entity_upsert(be)?;
                }
                for block in &ext.blocks {
                    db.block_upsert(&block.entity_block)?;
                }
                for edge in &ext.edges {
                    db.edge_upsert(edge)?;
                }
                tier_counts[ext.tier as usize] += 1;
                stats.processed += 1;
            }
        }

        if completed.is_multiple_of(25) {
            tracing::info!("[purpose] {completed}/{total} entities");
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
    current_version: Option<String>,
    /// `file_location_uri -> file_shape_hash`, the Layer-2 cache key boundary.
    file_shapes: HashMap<String, String>,
    stable_sampling: bool,
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

    // Layer-2 cache: keyed by (entity, file-shape, model). On a hit, the LLM
    // call is skipped entirely; on a miss, derive then store. Consulted even
    // under `--full` (which only bypasses the head-idempotency guard above).
    let file_shape_hash = entity
        .first_location
        .as_ref()
        .and_then(|loc| ctx.file_shapes.get(&loc.uri))
        .cloned()
        .unwrap_or_default();
    let cache_key = Layer2CacheKey {
        artifact_kind: "purpose".to_string(),
        entity_id: Some(entity.id.clone()),
        content_hash: String::new(),
        file_shape_hash,
        model: model_name.to_string(),
        stable_sampling: ctx.stable_sampling,
    };
    let derived = match layer2_cache::cache_get::<crate::adapter::ExtractedPurpose>(
        db.as_ref(),
        &cache_key,
    ) {
        Ok(Some(hit)) => Ok(Some(hit)),
        Ok(None) => {
            match extract_with_retry(adapter.as_ref(), entity, &content, summary_opt.as_deref(), llm)
                .await
            {
                Ok(Some(fresh)) => {
                    let sha = ctx.current_version.as_deref().unwrap_or("");
                    if let Err(e) = layer2_cache::cache_put(db.as_ref(), &cache_key, &fresh, sha) {
                        tracing::warn!("purpose cache_put failed for {}: {e}", entity.id);
                    }
                    Ok(Some(fresh))
                }
                other => other,
            }
        }
        Err(e) => Err(e),
    };

    // Call adapter with retry.
    match derived {
        Ok(Some(extracted)) => {
            let now = chrono::Utc::now().to_rfc3339();
            let model = llm.name().to_string();
            let tier_str = model_tier(&model).to_string();
            let version = ctx.current_version.clone();
            let purpose = EntityPurpose {
                entity_id: entity.id.clone(),
                corpus_id: corpus_id.to_string(),
                purpose: extracted.purpose,
                model,
                model_tier: tier_str,
                generated_at: now.clone(),
                derived_at_version: version.clone(),
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
                    derived_at_version: version.clone(),
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
                    derived_at_version: version.clone(),
                };
                edges.push(edge);
            }

            PurposeOutcome::Extracted(Box::new(ExtractedPurpose {
                tier,
                purpose,
                blocks,
                edges,
                block_entities,
            }))
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
