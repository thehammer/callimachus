//! `calli history` — history-table management commands.
//!
//! Currently exposes a single sub-command:
//!
//! * `calli history backfill <corpus_id> --from <sha>` — walk the
//!   first-parent git ancestry backward from HEAD's parent down to `<sha>` and
//!   populate the `*_history` tables for each older snapshot without touching
//!   the head tables.

use std::sync::Arc;

use anyhow::Result;
use callimachus_core::{
    indexing::{
        IndexOptions, IndexPipeline,
        history_walk::{WalkOptions, walk_history_backward},
    },
    storage::StorageBackend,
    types::Pass,
};
use callimachus_llm::build_provider;
use clap::Subcommand;

use crate::commands::index::{build_adapter, resolve_provider};
use crate::config::GlobalConfig;

/// Sub-commands for `calli history`.
#[derive(Debug, Subcommand)]
pub enum HistoryCommand {
    /// Populate `*_history` tables for commits older than HEAD.
    ///
    /// Walks the first-parent git ancestry backward from HEAD's parent down to
    /// `--from <sha>`, running the indexing pipeline against each commit's
    /// tree and writing all artifacts to `*_history` tables.  The head tables
    /// and `last_indexed_version` are never modified.
    ///
    /// Requires the corpus to have been previously ingested (`calli ingest …`).
    Backfill {
        /// Corpus ID to backfill.
        corpus_id: String,

        /// Starting (oldest) commit SHA (full or short).
        /// Must be on HEAD's first-parent ancestry.
        /// This flag is required — there is no default.
        #[arg(long, required = true)]
        from: String,

        /// LLM provider override (anthropic, claude-code, dry-run).
        #[arg(long)]
        provider: Option<String>,

        /// Fixed concurrency for LLM-heavy passes.
        #[arg(long)]
        concurrency: Option<usize>,
    },
}

/// Dispatch a `HistoryCommand`.
pub async fn run(
    sub: HistoryCommand,
    db: Arc<dyn StorageBackend>,
    config: &GlobalConfig,
) -> Result<()> {
    match sub {
        HistoryCommand::Backfill {
            corpus_id,
            from,
            provider,
            concurrency,
        } => {
            let corpus = db.corpus_require(&corpus_id)?;

            let provider_config = resolve_provider(provider, config)?;
            let llm = build_provider(provider_config)
                .map_err(|e| anyhow::anyhow!("failed to build LLM provider: {e}"))?;
            let adapter = build_adapter(&corpus)?;

            let opts = IndexOptions {
                passes: vec![
                    Pass::Chunk,
                    Pass::Structure,
                    Pass::Semantic,
                    Pass::Aliases,
                    Pass::Summarize,
                    Pass::Purpose,
                    Pass::Contract,
                ],
                concurrency,
                tier_config: config.model_tiers.clone(),
                ..IndexOptions::default()
            };

            let pipeline = IndexPipeline {
                db,
                adapter,
                llm: Arc::new(llm),
                embedder: None,
            };

            let walk_opts = WalkOptions {
                from_sha: Some(from),
                skip_confirm: true, // backfill doesn't prompt; cost is user's responsibility
            };

            let stats = walk_history_backward(&pipeline, &corpus, opts, walk_opts).await?;
            println!("Backfilled {} commit(s).", stats.commits_processed);
            println!("  Chunks:   {}", stats.total_chunks);
            println!("  Entities: {}", stats.total_entities);
            println!("  Edges:    {}", stats.total_edges);
            if stats.cost_usd > 0.0 {
                println!("  Cost:     ${:.4}", stats.cost_usd);
            }
            Ok(())
        }
    }
}
