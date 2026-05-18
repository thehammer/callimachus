use std::sync::Arc;

use callimachus_llm::{LlmError, LlmProvider};
use uuid::Uuid;

use crate::{
    adapter::SourceAdapter,
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
    llm: Arc<dyn LlmProvider>,
    opts: &IndexOptions,
) -> anyhow::Result<PassStats> {
    let mut stats = PassStats::default();

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
        // Idempotent: skip if already computed.
        if db.purpose_get(&corpus.id, &entity.id)?.is_some() {
            stats.skipped += 1;
            let completed = i as u64 + 1;
            if completed.is_multiple_of(25) {
                tracing::info!("[purpose] {}/{} entities", completed, total);
            }
            continue;
        }

        // Fetch content via first location.
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
            llm.as_ref(),
        )
        .await
        {
            Ok(Some(extracted)) => {
                let now = chrono::Utc::now().to_rfc3339();
                let purpose = EntityPurpose {
                    entity_id: entity.id.clone(),
                    corpus_id: corpus.id.clone(),
                    purpose: extracted.purpose,
                    model: Some(llm.name().to_string()),
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

    Ok(stats)
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
