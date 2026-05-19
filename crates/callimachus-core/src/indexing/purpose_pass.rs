use std::sync::Arc;

use callimachus_llm::{LlmError, LlmProvider, model_tier};
use uuid::Uuid;

use crate::{
    adapter::SourceAdapter,
    indexing::model_tier::{ModelTier, ModelTierRouter},
    storage::{StorageBackend, run_log::PassStats},
    types::{Corpus, Entity, EntityBlock, EntityPurpose},
};

use super::pipeline::IndexOptions;

const MAX_RETRIES: u32 = 8;

/// Kinds of entities for which we extract purpose.
const PURPOSE_KINDS: &[&str] = &["function", "method", "class", "interface", "module"];

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

    if opts.dry_run {
        return Ok(stats);
    }

    let all_entities = db.entity_list(&corpus.id)?;
    let candidates: Vec<&Entity> = all_entities
        .iter()
        .filter(|e| PURPOSE_KINDS.contains(&e.kind.as_str()))
        .collect();
    let total = candidates.len() as u64;

    for (i, entity) in candidates.iter().enumerate() {
        // Fetch content via first location (needed for routing and extraction).
        let content = match entity.first_location.as_ref() {
            Some(loc) => match db.chunk_get_by_uri(&loc.uri)? {
                Some(chunk) => chunk.content,
                None => {
                    stats.skipped += 1;
                    let completed = i as u64 + 1;
                    if completed.is_multiple_of(25) {
                        tracing::info!("[purpose] {}/{} entities", completed, total);
                    }
                    continue;
                }
            },
            None => {
                stats.skipped += 1;
                let completed = i as u64 + 1;
                if completed.is_multiple_of(25) {
                    tracing::info!("[purpose] {}/{} entities", completed, total);
                }
                continue;
            }
        };

        // Compute routing inputs.
        let in_deg = db.entity_in_degree(&corpus.id, &entity.id).unwrap_or(0);
        let out_deg = db.entity_out_degree(&corpus.id, &entity.id).unwrap_or(0);
        // Detect language from entity location for static analysis.
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

        let tier = router.route(&routing);
        let llm: &dyn LlmProvider = match tier {
            ModelTier::Haiku => llm_haiku.as_ref(),
            ModelTier::Sonnet => llm_sonnet.as_ref(),
            ModelTier::Opus => llm_opus.as_ref(),
        };
        tier_counts[tier as usize] += 1;

        tracing::debug!(
            "[purpose] entity={} tier={} kind={} in_deg={} out_deg={} fallible={} panics={}",
            entity.id,
            tier,
            entity.kind,
            in_deg,
            out_deg,
            routing.is_fallible,
            routing.panic_call_count
        );

        // Idempotent: skip only if this exact model has already produced an artifact.
        // A different model (e.g. Sonnet after Haiku) will add a new row.
        let model_name = llm.name();
        if !opts.full
            && db
                .purpose_get_for_model(&corpus.id, &entity.id, model_name)?
                .is_some()
        {
            stats.skipped += 1;
            let completed = i as u64 + 1;
            if completed.is_multiple_of(25) {
                tracing::info!("[purpose] {}/{} entities", completed, total);
            }
            continue;
        }

        // Fetch existing summary text if available.
        let summary_opt = db
            .summary_get(
                &corpus.id,
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
                let tier = model_tier(&model).to_string();
                let purpose = EntityPurpose {
                    entity_id: entity.id.clone(),
                    corpus_id: corpus.id.clone(),
                    purpose: extracted.purpose,
                    model,
                    model_tier: tier,
                    generated_at: now.clone(),
                };
                db.purpose_upsert(&purpose)?;

                // Store block blurbs for complex functions.
                for (i, block) in extracted.blocks.iter().enumerate() {
                    let block_entity_id = format!("{}:block:{}", entity.id, i);
                    // Insert a lightweight entity row for the block.
                    let block_entity = Entity::new(
                        block_entity_id.clone(),
                        corpus.id.clone(),
                        block.label.clone(),
                        "block".to_string(),
                    );
                    db.entity_upsert(&block_entity)?;

                    // Insert block record.
                    let eb = EntityBlock {
                        id: Uuid::new_v4().to_string(),
                        entity_id: entity.id.clone(),
                        corpus_id: corpus.id.clone(),
                        label: block.label.clone(),
                        description: block.description.clone(),
                        position: i as i64,
                    };
                    db.block_upsert(&eb)?;

                    // Emit `defines` edge: parent → block entity.
                    let edge = crate::types::Edge {
                        id: Uuid::new_v4().to_string(),
                        corpus_id: corpus.id.clone(),
                        from_entity_id: entity.id.clone(),
                        to_entity_id: block_entity_id,
                        kind: "defines".to_string(),
                        location: crate::types::Location::new(&corpus.id, ""),
                        confidence: 0.5,
                    };
                    db.edge_upsert(&edge)?;
                }

                stats.processed += 1;
            }
            Ok(None) => {
                stats.skipped += 1;
            }
            Err(e) => {
                tracing::warn!("purpose pass failed for entity {}: {e}", entity.id);
                stats.failed += 1;
            }
        }

        let completed = i as u64 + 1;
        if completed.is_multiple_of(25) {
            tracing::info!("[purpose] {}/{} entities", completed, total);
        }
    }

    tracing::info!(
        "[purpose] tier distribution: haiku={} sonnet={} opus={}",
        tier_counts[ModelTier::Haiku as usize],
        tier_counts[ModelTier::Sonnet as usize],
        tier_counts[ModelTier::Opus as usize],
    );

    Ok(stats)
}

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
