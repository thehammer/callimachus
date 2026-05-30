//! `calli history` — history-table management commands.
//!
//! Currently exposes two sub-commands:
//!
//! * `calli history backfill <corpus_id> --from <sha>` — walk the
//!   first-parent git ancestry backward from HEAD's parent down to `<sha>` and
//!   populate the `*_history` tables for each older snapshot without touching
//!   the head tables.
//! * `calli history backfill <corpus_id> --back N` — convenience form that
//!   resolves `--back N` to `HEAD~N` along the first-parent chain and then
//!   performs the same backward backfill.  `--from` and `--back` are mutually
//!   exclusive; exactly one is required.
//! * `calli history prune <corpus_id> --keep N [--dry-run]` — delete all
//!   history rows whose `superseded_at_version` is older than the N-th
//!   most-recent supersession SHA.  **This operation is destructive and
//!   irreversible.**  Use `--dry-run` to preview what would be deleted.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use callimachus_core::{
    indexing::{
        IndexOptions, IndexPipeline,
        history_walk::{WalkOptions, resolve_back_n_sha, walk_history_backward},
        validate_pass_prerequisites,
    },
    storage::{PruneStats, StorageBackend},
    types::{Pass, parse_passes_list},
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
    /// the commit identified by `--from <sha>` or `--back N`, running the
    /// indexing pipeline against each commit's tree and writing all artifacts
    /// to `*_history` tables.  The head tables and `last_indexed_version` are
    /// never modified.
    ///
    /// Requires the corpus to have been previously ingested (`calli ingest …`).
    ///
    /// Exactly one of `--from` or `--back` is required; they are mutually
    /// exclusive.
    Backfill {
        /// Corpus ID to backfill.
        corpus_id: String,

        /// Starting (oldest) commit SHA (full or short).
        /// Must be on HEAD's first-parent ancestry.
        /// Mutually exclusive with --back.
        #[arg(long, required_unless_present = "back", conflicts_with = "back")]
        from: Option<String>,

        /// Number of first-parent steps to walk backward from HEAD.
        /// `--back 1` starts from HEAD's parent; `--back N` from HEAD~N.
        /// If N exceeds the available history, the walk is clamped to the root
        /// commit.  Mutually exclusive with --from.
        #[arg(long, conflicts_with = "from")]
        back: Option<u32>,

        /// LLM provider override (anthropic, claude-code, dry-run).
        #[arg(long)]
        provider: Option<String>,

        /// Fixed concurrency for LLM-heavy passes.
        #[arg(long)]
        concurrency: Option<usize>,

        /// Comma-separated list of passes to run per iteration.  Use `default`
        /// to expand to the standard seven-pass backfill list
        /// (chunk,structure,semantic,aliases,summarize,purpose,contract).
        /// Combine like `--passes "default,theme"` to layer extra passes on top.
        /// Order is ignored; duplicates are removed.
        /// When omitted, the default seven-pass list runs.
        #[arg(long)]
        passes: Option<String>,

        /// Pin Layer-2 LLM calls to deterministic sampling (temperature 0 +
        /// derived seed). Combined with the Layer-2 cache, an unchanged corpus
        /// backfills with zero LLM work.
        #[arg(long)]
        stable_sampling: bool,
    },

    /// Prune history rows older than the N most-recent supersession SHAs.
    ///
    /// Deletes all rows in every `*_history` table whose
    /// `superseded_at_version` is older than the N-th most-recent supersession
    /// SHA (ordered by `MAX(superseded_at)` across all eight history tables).
    /// All eight tables are pruned atomically inside a single transaction; a
    /// forced failure rolls back every DELETE.
    ///
    /// **This operation is destructive and irreversible.**  Pruned history rows
    /// cannot be recovered.  Use `--dry-run` to preview what would be deleted
    /// before committing to an actual prune.
    Prune {
        /// Corpus ID to prune.
        corpus_id: String,

        /// Number of most-recent supersession SHAs to retain.
        /// Must be ≥ 1.  `--keep 0` is rejected at parse time.
        #[arg(long, required = true, value_parser = clap::value_parser!(u32).range(1..))]
        keep: u32,

        /// Preview what would be deleted without modifying the database.
        #[arg(long, default_value_t = false)]
        dry_run: bool,
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
            back,
            provider,
            concurrency,
            passes,
            stable_sampling,
        } => {
            let corpus = db.corpus_require(&corpus_id)?;

            // Resolve the starting SHA from whichever flag was supplied.
            let from_sha = match (from, back) {
                (Some(sha), None) => sha,
                (None, Some(n)) => {
                    let repo = git2::Repository::open(Path::new(&corpus.source))
                        .with_context(|| format!("opening git repo at {}", corpus.source))?;
                    resolve_back_n_sha(&repo, n)?.to_string()
                }
                _ => unreachable!("clap enforces exactly one of --from / --back"),
            };

            // Resolve the pass list: user-supplied --passes overrides the default.
            // Default for backfill: Chunk, Structure, Semantic, Aliases, Summarize,
            // Purpose, Contract (no Pass::History — the walker builds its own manifest).
            let pass_list: Vec<Pass> = match passes {
                Some(ref s) => parse_passes_list(s).map_err(|e| anyhow::anyhow!("{e}"))?,
                None => vec![
                    Pass::Chunk,
                    Pass::Structure,
                    Pass::Semantic,
                    Pass::Aliases,
                    Pass::Summarize,
                    Pass::Purpose,
                    Pass::Contract,
                ],
            };

            // Validate prerequisites against current head state.
            validate_pass_prerequisites(db.as_ref(), &corpus.id, &pass_list)?;

            let provider_config = resolve_provider(provider, config)?;
            let llm = build_provider(provider_config)
                .map_err(|e| anyhow::anyhow!("failed to build LLM provider: {e}"))?;
            let adapter = build_adapter(&corpus)?;

            let opts = IndexOptions {
                passes: pass_list,
                concurrency,
                stable_sampling,
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
                from_sha: Some(from_sha),
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

        HistoryCommand::Prune {
            corpus_id,
            keep,
            dry_run,
        } => {
            // Validate that the corpus exists before running any SQL.
            db.corpus_require(&corpus_id)?;

            let stats: PruneStats = db.prune_history(&corpus_id, keep as usize, dry_run)?;

            let heading_verb = if dry_run { "Would prune" } else { "Pruned" };
            let row_verb = if dry_run {
                "Rows that would be removed from"
            } else {
                "Rows removed from"
            };

            println!("{heading_verb} history for corpus '{corpus_id}':");
            println!(
                "  Supersession SHAs kept:    {}",
                stats.supersession_shas_kept
            );
            println!(
                "  Supersession SHAs pruned:  {}",
                stats.supersession_shas_pruned
            );
            println!(
                "  {row_verb} entities_history:          {}",
                stats.rows_pruned_entities_history
            );
            println!(
                "  {row_verb} edges_history:             {}",
                stats.rows_pruned_edges_history
            );
            println!(
                "  {row_verb} entity_purposes_history:   {}",
                stats.rows_pruned_entity_purposes_history
            );
            println!(
                "  {row_verb} entity_contracts_history:  {}",
                stats.rows_pruned_entity_contracts_history
            );
            println!(
                "  {row_verb} entity_blocks_history:     {}",
                stats.rows_pruned_entity_blocks_history
            );
            println!(
                "  {row_verb} summaries_history:         {}",
                stats.rows_pruned_summaries_history
            );
            println!(
                "  {row_verb} chunks_history:            {}",
                stats.rows_pruned_chunks_history
            );
            println!(
                "  {row_verb} themes_history:            {}",
                stats.rows_pruned_themes_history
            );

            Ok(())
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use clap::Parser;

    /// Minimal re-declaration of the CLI structure for parse-time tests, so
    /// this module does not depend on `main.rs` internals.
    #[derive(Debug, clap::Parser)]
    #[command(name = "calli")]
    struct Cli {
        #[command(subcommand)]
        command: Command,
    }

    #[derive(Debug, clap::Subcommand)]
    enum Command {
        History {
            #[command(subcommand)]
            sub: HistoryCmd,
        },
    }

    #[derive(Debug, clap::Subcommand)]
    enum HistoryCmd {
        Backfill {
            corpus_id: String,
            #[arg(long, required_unless_present = "back", conflicts_with = "back")]
            from: Option<String>,
            #[arg(long, conflicts_with = "from")]
            back: Option<u32>,
            #[arg(long)]
            passes: Option<String>,
        },
        Prune {
            corpus_id: String,
            #[arg(long, required = true, value_parser = clap::value_parser!(u32).range(1..))]
            keep: u32,
            #[arg(long, default_value_t = false)]
            dry_run: bool,
        },
    }

    #[test]
    fn backfill_requires_from_or_back() {
        // No --from and no --back → parse error.
        let result = Cli::try_parse_from(["calli", "history", "backfill", "my-corpus"]);
        assert!(
            result.is_err(),
            "expected parse error when neither --from nor --back is supplied"
        );
    }

    #[test]
    fn backfill_rejects_from_and_back_together() {
        // Both flags supplied → conflict error.
        let result = Cli::try_parse_from([
            "calli",
            "history",
            "backfill",
            "my-corpus",
            "--from",
            "abc1234",
            "--back",
            "5",
        ]);
        assert!(
            result.is_err(),
            "expected parse error when both --from and --back are supplied"
        );
    }

    #[test]
    fn backfill_accepts_back_alone() {
        // Only --back → valid.
        let result =
            Cli::try_parse_from(["calli", "history", "backfill", "my-corpus", "--back", "10"]);
        assert!(
            result.is_ok(),
            "expected parse success with --back 10 alone"
        );
        match result.unwrap().command {
            Command::History {
                sub: HistoryCmd::Backfill { back, from, .. },
            } => {
                assert_eq!(back, Some(10));
                assert_eq!(from, None);
            }
            other => panic!("unexpected command variant: {other:?}"),
        }
    }

    #[test]
    fn backfill_accepts_from_alone() {
        // Only --from → valid (existing behaviour preserved).
        let result = Cli::try_parse_from([
            "calli",
            "history",
            "backfill",
            "my-corpus",
            "--from",
            "deadbeef",
        ]);
        assert!(result.is_ok(), "expected parse success with --from alone");
        match result.unwrap().command {
            Command::History {
                sub: HistoryCmd::Backfill { from, back, .. },
            } => {
                assert_eq!(from.as_deref(), Some("deadbeef"));
                assert_eq!(back, None);
            }
            other => panic!("unexpected command variant: {other:?}"),
        }
    }

    // ── Prune parse tests ─────────────────────────────────────────────────────

    #[test]
    fn prune_requires_keep() {
        // No --keep → parse error.
        let result = Cli::try_parse_from(["calli", "history", "prune", "my-corpus"]);
        assert!(
            result.is_err(),
            "expected parse error when --keep is not supplied"
        );
    }

    #[test]
    fn prune_rejects_keep_zero() {
        // --keep 0 → rejected by value_parser range guard.
        let result = Cli::try_parse_from(["calli", "history", "prune", "my-corpus", "--keep", "0"]);
        assert!(
            result.is_err(),
            "expected parse error when --keep 0 is supplied"
        );
    }

    #[test]
    fn prune_accepts_keep_alone() {
        // --keep 10 without --dry-run → valid; dry_run defaults to false.
        let result =
            Cli::try_parse_from(["calli", "history", "prune", "my-corpus", "--keep", "10"]);
        assert!(
            result.is_ok(),
            "expected parse success with --keep 10 alone"
        );
        match result.unwrap().command {
            Command::History {
                sub: HistoryCmd::Prune { keep, dry_run, .. },
            } => {
                assert_eq!(keep, 10);
                assert!(!dry_run);
            }
            other => panic!("unexpected command variant: {other:?}"),
        }
    }

    #[test]
    fn prune_accepts_dry_run() {
        // --keep 10 --dry-run → valid; dry_run is true.
        let result = Cli::try_parse_from([
            "calli",
            "history",
            "prune",
            "my-corpus",
            "--keep",
            "10",
            "--dry-run",
        ]);
        assert!(
            result.is_ok(),
            "expected parse success with --keep 10 --dry-run"
        );
        match result.unwrap().command {
            Command::History {
                sub: HistoryCmd::Prune { keep, dry_run, .. },
            } => {
                assert_eq!(keep, 10);
                assert!(dry_run);
            }
            other => panic!("unexpected command variant: {other:?}"),
        }
    }

    // ── passes flag tests ─────────────────────────────────────────────────────

    #[test]
    fn backfill_accepts_passes_default_theme() {
        let result = Cli::try_parse_from([
            "calli",
            "history",
            "backfill",
            "my-corpus",
            "--back",
            "5",
            "--passes",
            "default,theme",
        ]);
        assert!(
            result.is_ok(),
            "expected parse success with --passes \"default,theme\", got: {result:?}"
        );
        match result.unwrap().command {
            Command::History {
                sub: HistoryCmd::Backfill { passes, .. },
            } => {
                assert_eq!(passes.as_deref(), Some("default,theme"));
            }
            other => panic!("unexpected command variant: {other:?}"),
        }
    }

    #[test]
    fn backfill_accepts_passes_theme_only() {
        let result = Cli::try_parse_from([
            "calli",
            "history",
            "backfill",
            "my-corpus",
            "--back",
            "3",
            "--passes",
            "theme",
        ]);
        assert!(
            result.is_ok(),
            "expected parse success with --passes \"theme\", got: {result:?}"
        );
        match result.unwrap().command {
            Command::History {
                sub: HistoryCmd::Backfill { passes, .. },
            } => {
                assert_eq!(passes.as_deref(), Some("theme"));
            }
            other => panic!("unexpected command variant: {other:?}"),
        }
    }

    #[test]
    fn backfill_passes_omitted_is_none() {
        // Without --passes the field should be None (default behaviour preserved).
        let result =
            Cli::try_parse_from(["calli", "history", "backfill", "my-corpus", "--back", "1"]);
        assert!(result.is_ok(), "expected parse success, got: {result:?}");
        match result.unwrap().command {
            Command::History {
                sub: HistoryCmd::Backfill { passes, .. },
            } => {
                assert!(passes.is_none(), "passes should be None when not supplied");
            }
            other => panic!("unexpected command variant: {other:?}"),
        }
    }
}
