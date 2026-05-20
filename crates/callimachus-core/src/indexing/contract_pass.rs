use std::sync::Arc;

use callimachus_llm::{LlmError, LlmProvider, model_tier};
use futures::StreamExt;
use uuid::Uuid;

use crate::{
    adapter::SourceAdapter,
    indexing::model_tier::{ModelTier, ModelTierRouter, TierConfig},
    storage::{StorageBackend, run_log::PassStats},
    types::{Corpus, Edge, Entity, EntityContract},
};

use super::{change_manifest::file_path_from_uri, pipeline::IndexOptions};

const MAX_RETRIES: u32 = 8;

/// Kinds of entities for which we extract contracts.
const CONTRACT_KINDS: &[&str] = &["function", "method", "class", "interface", "module"];

// ─── Per-entity outcome ───────────────────────────────────────────────────────

enum ContractOutcome {
    /// Store a default contract and count as processed.
    DefaultContract { entity_id: String },
    /// Contract extracted from LLM.
    Extracted {
        contract: Box<EntityContract>,
        verified_by_edges: Vec<EdgeSpec>,
        discards_result_edges: Vec<EdgeSpec>,
    },
    /// LLM call failed; store a default contract and count as failed.
    Failed {
        entity_id: String,
        corpus_id: String,
        message: String,
    },
    Skip,
}

/// Lightweight edge spec; resolved to real edges in the serial sink.
struct EdgeSpec {
    from_entity_id: String,
    target_name: String,
    kind: String,
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
    let tier_config = opts.tier_config.clone();
    let tier_counts = [0u64; 3];

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
        .filter(|e| CONTRACT_KINDS.contains(&e.kind.as_str()))
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

    let outcomes: Vec<ContractOutcome> = futures::stream::iter(candidates.iter())
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
            ContractOutcome::Skip => {
                stats.skipped += 1;
            }
            ContractOutcome::DefaultContract { entity_id } => {
                // Entity already got a default contract in the async phase (no location).
                // The write happened there; just tally.
                stats.processed += 1;
                let _ = entity_id;
            }
            ContractOutcome::Extracted {
                contract,
                verified_by_edges,
                discards_result_edges,
            } => {
                db.contract_upsert(&contract)?;

                for spec in &verified_by_edges {
                    if let Ok(targets) =
                        db.entity_find_by_name(&contract.corpus_id, &spec.target_name)
                    {
                        for target in &targets {
                            let edge = Edge {
                                id: Uuid::new_v4().to_string(),
                                corpus_id: contract.corpus_id.clone(),
                                from_entity_id: spec.from_entity_id.clone(),
                                to_entity_id: target.id.clone(),
                                kind: spec.kind.clone(),
                                location: crate::types::Location::new(&contract.corpus_id, ""),
                                confidence: 0.5,
                            };
                            db.edge_upsert(&edge)?;
                        }
                    }
                }

                for spec in &discards_result_edges {
                    if let Ok(targets) =
                        db.entity_find_by_name(&contract.corpus_id, &spec.target_name)
                    {
                        for target in &targets {
                            if matches!(
                                db.contract_get(&contract.corpus_id, &target.id),
                                Ok(Some(ref tc)) if tc.is_fallible
                            ) {
                                let edge = Edge {
                                    id: Uuid::new_v4().to_string(),
                                    corpus_id: contract.corpus_id.clone(),
                                    from_entity_id: spec.from_entity_id.clone(),
                                    to_entity_id: target.id.clone(),
                                    kind: spec.kind.clone(),
                                    location: crate::types::Location::new(&contract.corpus_id, ""),
                                    confidence: 0.5,
                                };
                                db.edge_upsert(&edge)?;
                            }
                        }
                    }
                }

                stats.processed += 1;
            }
            ContractOutcome::Failed {
                entity_id,
                corpus_id,
                message,
            } => {
                tracing::warn!("contract pass failed for entity {entity_id}: {message}");
                // Store a default contract so we don't retry on subsequent runs.
                store_default_contract_direct(&db, &corpus_id, &entity_id)?;
                stats.failed += 1;
            }
        }

        let completed = i as u64 + 1;
        if completed.is_multiple_of(25) {
            tracing::info!("[contract] {}/{} entities", completed, total);
        }
    }

    // ── Populate concurrency stats into PassStats ─────────────────────────────
    if let Some(limiter) = llm_haiku.concurrency_limiter() {
        let cs = limiter.stats();
        stats.requests_made = Some(cs.requests_made);
        stats.avg_concurrency = Some(cs.avg_concurrency);
        stats.peak_concurrency = Some(cs.peak_concurrency);
        tracing::info!(
            "[contract] avg_concurrency={:.1} peak={} requests={}",
            cs.avg_concurrency,
            cs.peak_concurrency,
            cs.requests_made,
        );
        limiter.reset();
    }

    tracing::info!(
        "[contract] tier distribution: haiku={} sonnet={} opus={}",
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

async fn process_entity(ctx: &PassContext, entity: &Entity) -> ContractOutcome {
    let db = &ctx.db;
    let corpus_id = ctx.corpus_id.as_str();
    let adapter = &ctx.adapter;
    let llm_haiku = &ctx.llm_haiku;
    let llm_sonnet = &ctx.llm_sonnet;
    let llm_opus = &ctx.llm_opus;
    let tier_config = &ctx.tier_config;
    let full = ctx.full;
    let language = detect_language(entity);

    let content = match entity.first_location.as_ref() {
        Some(loc) => match db.chunk_get_by_uri(&loc.uri) {
            Ok(Some(chunk)) => chunk.content,
            _ => {
                // No content available — store a default contract immediately.
                // We return a sentinel so the serial sink knows to tally it.
                return ContractOutcome::DefaultContract {
                    entity_id: entity.id.clone(),
                };
            }
        },
        None => {
            return ContractOutcome::DefaultContract {
                entity_id: entity.id.clone(),
            };
        }
    };

    let in_deg = db.entity_in_degree(corpus_id, &entity.id).unwrap_or(0);
    let out_deg = db.entity_out_degree(corpus_id, &entity.id).unwrap_or(0);
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
        "[contract] entity={} tier={} kind={} in_deg={} out_deg={} fallible={} panics={}",
        entity.id, tier, entity.kind, in_deg, out_deg,
        routing.is_fallible, routing.panic_call_count,
    );

    // Idempotency: skip if this exact model already produced an artifact.
    let model_name = llm.name();
    if !full && db.contract_get_for_model(corpus_id, &entity.id, model_name).is_ok_and(|r| r.is_some()) {
        return ContractOutcome::Skip;
    }

    let summary_opt = db
        .summary_get(
            corpus_id,
            &crate::types::SummaryTargetKind::Entity,
            &entity.id,
        )
        .ok()
        .flatten()
        .map(|s| s.text);

    let purpose_opt = db
        .purpose_get(corpus_id, &entity.id)
        .ok()
        .flatten()
        .map(|p| p.purpose);

    let signals_json = serde_json::json!({
        "language": language,
        "entity_name": entity.canonical_name,
    });

    match extract_with_retry(
        adapter.as_ref(),
        entity,
        &content,
        summary_opt.as_deref(),
        purpose_opt.as_deref(),
        &signals_json,
        llm,
    )
    .await
    {
        Ok(Some(extracted)) => {
            let now = chrono::Utc::now().to_rfc3339();
            let model = llm.name().to_string();
            let tier_str = model_tier(&model).to_string();
            let contract = EntityContract {
                entity_id: entity.id.clone(),
                corpus_id: corpus_id.to_string(),
                is_public: extracted.is_public,
                is_must_use: extracted.is_must_use,
                is_deprecated: extracted.is_deprecated,
                is_fallible: extracted.is_fallible,
                is_nullable: extracted.is_nullable,
                is_mutating: extracted.is_mutating,
                is_diverging: extracted.is_diverging,
                has_panic_risk: extracted.has_panic_risk,
                has_unsafe: extracted.has_unsafe,
                is_incomplete: extracted.is_incomplete,
                panic_call_count: extracted.panic_call_count as i64,
                debt_markers: extracted.debt_markers,
                assumptions: extracted.assumptions,
                risks: extracted.risks,
                intent_gap: extracted.intent_gap,
                caller_notes: extracted.caller_notes,
                model,
                model_tier: tier_str,
                generated_at: now,
            };

            let verified_by_edges = extracted
                .verified_by_names
                .iter()
                .map(|name| EdgeSpec {
                    from_entity_id: entity.id.clone(),
                    target_name: name.clone(),
                    kind: "verified_by".to_string(),
                })
                .collect();

            let discards_result_edges = extracted
                .discards_result_callees
                .iter()
                .map(|name| EdgeSpec {
                    from_entity_id: entity.id.clone(),
                    target_name: name.clone(),
                    kind: "discards_result".to_string(),
                })
                .collect();

            ContractOutcome::Extracted {
                contract: Box::new(contract),
                verified_by_edges,
                discards_result_edges,
            }
        }
        Ok(None) => {
            // Adapter chose not to extract — store a default contract.
            ContractOutcome::DefaultContract {
                entity_id: entity.id.clone(),
            }
        }
        Err(e) => ContractOutcome::Failed {
            entity_id: entity.id.clone(),
            corpus_id: corpus_id.to_string(),
            message: e.to_string(),
        },
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn store_default_contract_direct(
    db: &Arc<dyn StorageBackend>,
    corpus_id: &str,
    entity_id: &str,
) -> anyhow::Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let contract = EntityContract {
        entity_id: entity_id.to_string(),
        corpus_id: corpus_id.to_string(),
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
