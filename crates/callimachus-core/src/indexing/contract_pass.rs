use std::sync::Arc;

use callimachus_llm::{LlmError, LlmProvider, model_tier};
use uuid::Uuid;

use crate::{
    adapter::SourceAdapter,
    storage::{StorageBackend, run_log::PassStats},
    types::{Corpus, Edge, Entity, EntityContract},
};

use super::pipeline::IndexOptions;

const MAX_RETRIES: u32 = 8;

/// Kinds of entities for which we extract contracts.
const CONTRACT_KINDS: &[&str] = &["function", "method", "class", "interface", "module"];

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
        .filter(|e| CONTRACT_KINDS.contains(&e.kind.as_str()))
        .collect();
    let total = candidates.len() as u64;

    for (i, entity) in candidates.iter().enumerate() {
        // Idempotent: skip only if this exact model has already produced an artifact.
        let model_name = llm.name();
        if !opts.full
            && db
                .contract_get_for_model(&corpus.id, &entity.id, model_name)?
                .is_some()
        {
            stats.skipped += 1;
            let completed = i as u64 + 1;
            if completed.is_multiple_of(25) {
                tracing::info!("[contract] {}/{} entities", completed, total);
            }
            continue;
        }

        // Fetch source content.
        let content = match entity.first_location.as_ref() {
            Some(loc) => match db.chunk_get_by_uri(&loc.uri)? {
                Some(chunk) => chunk.content,
                None => {
                    store_default_contract(&db, corpus, entity)?;
                    stats.processed += 1;
                    let completed = i as u64 + 1;
                    if completed.is_multiple_of(25) {
                        tracing::info!("[contract] {}/{} entities", completed, total);
                    }
                    continue;
                }
            },
            None => {
                store_default_contract(&db, corpus, entity)?;
                stats.processed += 1;
                let completed = i as u64 + 1;
                if completed.is_multiple_of(25) {
                    tracing::info!("[contract] {}/{} entities", completed, total);
                }
                continue;
            }
        };

        // Detect language from location path (used in signals_json).
        let language = detect_language(entity);

        // Fetch related text.
        let summary_opt = db
            .summary_get(
                &corpus.id,
                &crate::types::SummaryTargetKind::Entity,
                &entity.id,
            )
            .ok()
            .flatten()
            .map(|s| s.text);

        let purpose_opt = db
            .purpose_get(&corpus.id, &entity.id)
            .ok()
            .flatten()
            .map(|p| p.purpose);

        // Build signals JSON by calling the code adapter's static analysis.
        // We pass an opaque JSON blob; the adapter fills it internally.
        let signals_json = serde_json::json!({
            "language": language,
            "entity_name": entity.canonical_name,
        });

        // Call adapter with retry.
        match extract_with_retry(
            adapter.as_ref(),
            entity,
            &content,
            summary_opt.as_deref(),
            purpose_opt.as_deref(),
            &signals_json,
            llm.as_ref(),
        )
        .await
        {
            Ok(Some(extracted)) => {
                let now = chrono::Utc::now().to_rfc3339();
                let model = llm.name().to_string();
                let tier = model_tier(&model).to_string();
                let contract = EntityContract {
                    entity_id: entity.id.clone(),
                    corpus_id: corpus.id.clone(),
                    assumptions: extracted.assumptions,
                    risks: extracted.risks,
                    intent_gap: extracted.intent_gap,
                    caller_notes: extracted.caller_notes,
                    model,
                    model_tier: tier,
                    generated_at: now,
                    ..EntityContract::default()
                };
                db.contract_upsert(&contract)?;

                // Insert `verified_by` edges (test entity verifies production entity).
                for verified_name in &extracted.verified_by_names {
                    if let Ok(targets) = db.entity_find_by_name(&corpus.id, verified_name) {
                        for target in &targets {
                            let edge = Edge {
                                id: Uuid::new_v4().to_string(),
                                corpus_id: corpus.id.clone(),
                                from_entity_id: entity.id.clone(),
                                to_entity_id: target.id.clone(),
                                kind: "verified_by".to_string(),
                                location: crate::types::Location::new(&corpus.id, ""),
                                confidence: 0.5,
                            };
                            db.edge_upsert(&edge)?;
                        }
                    }
                }

                // Insert `discards_result` edges.
                for callee_name in &extracted.discards_result_callees {
                    if let Ok(targets) = db.entity_find_by_name(&corpus.id, callee_name) {
                        for target in &targets {
                            // Only if callee is known to be fallible.
                            if matches!(
                                db.contract_get(&corpus.id, &target.id),
                                Ok(Some(ref tc)) if tc.is_fallible
                            ) {
                                let edge = Edge {
                                    id: Uuid::new_v4().to_string(),
                                    corpus_id: corpus.id.clone(),
                                    from_entity_id: entity.id.clone(),
                                    to_entity_id: target.id.clone(),
                                    kind: "discards_result".to_string(),
                                    location: crate::types::Location::new(&corpus.id, ""),
                                    confidence: 0.5,
                                };
                                db.edge_upsert(&edge)?;
                            }
                        }
                    }
                }

                stats.processed += 1;
            }
            Ok(None) => {
                store_default_contract(&db, corpus, entity)?;
                stats.processed += 1;
            }
            Err(e) => {
                tracing::warn!("contract pass failed for entity {}: {e}", entity.id);
                // Store a default contract so we don't retry on subsequent runs.
                store_default_contract(&db, corpus, entity)?;
                stats.failed += 1;
            }
        }

        let completed = i as u64 + 1;
        if completed.is_multiple_of(25) {
            tracing::info!("[contract] {}/{} entities", completed, total);
        }
    }

    Ok(stats)
}

fn store_default_contract(
    db: &Arc<dyn StorageBackend>,
    corpus: &Corpus,
    entity: &Entity,
) -> anyhow::Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let contract = EntityContract {
        entity_id: entity.id.clone(),
        corpus_id: corpus.id.clone(),
        model: "unknown".to_string(),
        model_tier: "unknown".to_string(),
        generated_at: now,
        ..EntityContract::default()
    };
    db.contract_upsert(&contract)?;
    Ok(())
}

fn detect_language(entity: &Entity) -> &'static str {
    let uri = entity
        .first_location
        .as_ref()
        .map(|l| l.uri.as_str())
        .unwrap_or("");

    // Strip symbol anchor if present.
    let path = uri.split('#').next().unwrap_or(uri);

    if path.ends_with(".rs") {
        "rust"
    } else if path.ends_with(".ts") || path.ends_with(".tsx") || path.ends_with(".js") {
        "typescript"
    } else if path.ends_with(".py") {
        "python"
    } else if path.ends_with(".go") {
        "go"
    } else {
        "unknown"
    }
}

async fn extract_with_retry(
    adapter: &dyn SourceAdapter,
    entity: &Entity,
    content: &str,
    summary: Option<&str>,
    purpose: Option<&str>,
    signals: &serde_json::Value,
    llm: &dyn LlmProvider,
) -> anyhow::Result<Option<crate::adapter::ExtractedContract>> {
    let mut attempts = 0u32;
    loop {
        attempts += 1;
        match adapter
            .extract_contract(entity, content, summary, purpose, signals, llm)
            .await
        {
            Ok(result) => return Ok(result),
            Err(e) => {
                if let Some(LlmError::RateLimited { retry_after_secs }) =
                    e.downcast_ref::<LlmError>()
                    && attempts < MAX_RETRIES
                {
                    let backoff = *retry_after_secs;
                    tracing::warn!(
                        "contract pass rate limited; backing off {backoff}s (attempt {attempts})"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
                    continue;
                }
                if let Some(LlmError::Timeout { .. }) = e.downcast_ref::<LlmError>()
                    && attempts < MAX_RETRIES
                {
                    let backoff = 5u64 * 2u64.pow(attempts - 1);
                    tracing::warn!(
                        "contract pass timeout; backing off {backoff}s (attempt {attempts})"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
                    continue;
                }
                return Err(e);
            }
        }
    }
}
