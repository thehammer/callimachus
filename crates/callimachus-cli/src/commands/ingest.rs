//! `calli ingest` — register a corpus and index it in one step.
//!
//! With `--with-history` this command walks the first-parent git history of the
//! source directory from `--from` (or the root commit) forward to HEAD,
//! running the full indexing pipeline at each commit.

use std::sync::Arc;

use anyhow::{Result, bail};
use callimachus_core::{
    indexing::{
        IndexOptions, IndexPipeline,
        history_walk::{WalkOptions, walk_history_forward},
        validate_pass_prerequisites,
    },
    storage::StorageBackend,
    types::{Corpus, Pass, parse_passes_list},
};
use callimachus_llm::{build_embedding_provider, build_provider};

use crate::commands::index::{build_adapter, build_embedding_provider_config, resolve_provider};
use crate::config::GlobalConfig;

/// Register the corpus at `(kind, name, path)` if it does not already exist,
/// then run either a single-snapshot index or a forward history walk.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    kind: String,
    name: String,
    path: String,
    with_history: bool,
    from: Option<String>,
    yes: bool,
    dry_run: bool,
    concurrency: Option<usize>,
    passes: Option<String>,
    stable_sampling: bool,
    provider_override: Option<String>,
    db: Arc<dyn StorageBackend>,
    config: &GlobalConfig,
) -> Result<()> {
    // Validate source exists.
    if !std::path::Path::new(&path).exists() {
        bail!("source path does not exist: {path}");
    }

    // Register the corpus, or find an existing one with the same source path.
    let corpus = register_or_find(db.as_ref(), kind, name, path)?;

    // Build LLM provider and adapter.
    let provider_config = resolve_provider(provider_override, config)?;
    let llm = build_provider(provider_config)
        .map_err(|e| anyhow::anyhow!("failed to build LLM provider: {e}"))?;
    let adapter = build_adapter(&corpus)?;

    // Resolve the pass list: user-supplied --passes overrides the default.
    // Default: History, Chunk, Structure, Semantic, Aliases, Summarize, Purpose, Contract.
    let pass_list: Vec<Pass> = match passes {
        Some(ref s) => parse_passes_list(s).map_err(|e| anyhow::anyhow!("{e}"))?,
        None => vec![
            Pass::History,
            Pass::Chunk,
            Pass::Structure,
            Pass::Semantic,
            Pass::Aliases,
            Pass::Summarize,
            Pass::Purpose,
            Pass::Contract,
        ],
    };

    // Validate prerequisites against current head state before running anything.
    validate_pass_prerequisites(db.as_ref(), &corpus.id, &pass_list)?;

    // Fail-fast: if embed was requested, the embedding config must be usable.
    let embed_requested = pass_list.contains(&Pass::Embed);
    let embedder = if embed_requested {
        let embed_cfg = build_embedding_provider_config(config);
        match build_embedding_provider(embed_cfg) {
            Ok(Some(p)) => Some(p),
            Ok(None) => {
                bail!(
                    "--pass embed/all requested but [embedding] is disabled or absent \
                     in config; set [embedding] enabled = true with a Voyage api_key_env"
                );
            }
            Err(e) => {
                bail!("embeddings requested via --pass but not usable: {e}");
            }
        }
    } else {
        None
    };

    let opts = IndexOptions {
        passes: pass_list,
        dry_run,
        concurrency,
        stable_sampling,
        tier_config: config.model_tiers.clone(),
        ..IndexOptions::default()
    };

    let pipeline = IndexPipeline {
        db,
        adapter,
        llm: Arc::new(llm),
        embedder,
    };

    if with_history {
        let walk_opts = WalkOptions {
            from_sha: from,
            skip_confirm: yes,
        };
        let stats = walk_history_forward(&pipeline, &corpus, opts, walk_opts).await?;
        println!("Walked {} commit(s).", stats.commits_processed);
        println!("  Chunks:   {}", stats.total_chunks);
        println!("  Entities: {}", stats.total_entities);
        println!("  Edges:    {}", stats.total_edges);
        if stats.cost_usd > 0.0 {
            println!("  Cost:     ${:.4}", stats.cost_usd);
        }
    } else {
        let result = pipeline.run(&corpus, opts).await?;
        println!("Done.");
        println!("  Chunks:   {}", result.total_chunks);
        println!("  Entities: {}", result.total_entities);
        println!("  Edges:    {}", result.total_edges);
        if result.cost_usd > 0.0 {
            println!("  Cost:     ${:.4}", result.cost_usd);
        }
    }

    Ok(())
}

/// Return an existing corpus whose `source` matches `path`, or register a
/// new one from `(kind, name, path)`.
fn register_or_find(
    db: &dyn StorageBackend,
    kind: String,
    name: String,
    path: String,
) -> Result<Corpus> {
    // Look for an existing corpus with the same source path.
    let existing = db.corpus_list()?.into_iter().find(|c| c.source == path);

    if let Some(corpus) = existing {
        tracing::info!(
            "[ingest] reusing existing corpus {:?} (source matches)",
            corpus.id
        );
        return Ok(corpus);
    }

    // Generate an ID from the name.
    let id = slugify(&name);
    if id.is_empty() {
        anyhow::bail!(
            "could not generate a valid corpus ID from name {:?}. Use `calli corpus add --id` to set one explicitly.",
            name
        );
    }

    // If a corpus with this ID already exists (different source), use a suffix.
    let final_id = if db.corpus_exists(&id)? {
        find_free_id(db, &id)?
    } else {
        id
    };

    let corpus = Corpus::new(final_id.clone(), name.clone(), kind.clone(), path.clone());
    db.corpus_insert(&corpus)?;
    eprintln!("✓ Registered corpus {final_id:?} ({kind} at {path})");
    Ok(corpus)
}

/// Find a free corpus ID by appending `-2`, `-3`, … until one is unused.
fn find_free_id(db: &dyn StorageBackend, base: &str) -> Result<String> {
    for n in 2..=100u32 {
        let candidate = format!("{base}-{n}");
        if !db.corpus_exists(&candidate)? {
            return Ok(candidate);
        }
    }
    anyhow::bail!("could not find a free corpus ID derived from {base:?}")
}

/// Convert a human-readable name to a URL-safe slug.
fn slugify(name: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = true;
    for ch in name.chars() {
        if ch.is_alphanumeric() {
            slug.push(ch.to_lowercase().next().unwrap());
            last_dash = false;
        } else if !last_dash && !slug.is_empty() {
            slug.push('-');
            last_dash = true;
        }
    }
    if slug.ends_with('-') {
        slug.pop();
    }
    slug
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use clap::Parser;

    // Re-use the top-level Cli struct for parse-time validation tests.
    #[derive(Debug, clap::Parser)]
    #[command(name = "calli")]
    struct Cli {
        #[command(subcommand)]
        command: Command,
    }

    #[derive(Debug, clap::Subcommand)]
    enum Command {
        Ingest {
            kind: String,
            name: String,
            path: String,
            #[arg(long)]
            with_history: bool,
            #[arg(long, requires = "with_history")]
            from: Option<String>,
            #[arg(long)]
            yes: bool,
            #[arg(long)]
            passes: Option<String>,
        },
    }

    #[test]
    fn from_without_with_history_errors_at_parse_time() {
        let result = Cli::try_parse_from([
            "calli",
            "ingest",
            "code",
            "name",
            "/tmp/repo",
            "--from",
            "abc123",
        ]);
        let err = result.unwrap_err().to_string();
        // clap reports the 'requires' relationship.
        assert!(
            err.contains("with-history") || err.contains("with_history"),
            "expected mention of with-history in error, got: {err}"
        );
    }

    #[test]
    fn with_history_alone_parses_ok() {
        let result = Cli::try_parse_from([
            "calli",
            "ingest",
            "code",
            "name",
            "/tmp/repo",
            "--with-history",
            "--yes",
        ]);
        assert!(
            result.is_ok(),
            "--with-history --yes should parse cleanly, got: {result:?}"
        );
    }

    #[test]
    fn passes_default_theme_parses_ok() {
        let result = Cli::try_parse_from([
            "calli",
            "ingest",
            "code",
            "name",
            "/tmp/repo",
            "--passes",
            "default,theme",
        ]);
        assert!(
            result.is_ok(),
            "--passes \"default,theme\" should parse cleanly, got: {result:?}"
        );
        match result.unwrap().command {
            Command::Ingest { passes, .. } => {
                assert_eq!(passes.as_deref(), Some("default,theme"));
            }
        }
    }

    #[test]
    fn passes_theme_only_parses_ok() {
        let result = Cli::try_parse_from([
            "calli",
            "ingest",
            "code",
            "name",
            "/tmp/repo",
            "--passes",
            "theme",
        ]);
        assert!(
            result.is_ok(),
            "--passes \"theme\" should parse cleanly, got: {result:?}"
        );
        match result.unwrap().command {
            Command::Ingest { passes, .. } => {
                assert_eq!(passes.as_deref(), Some("theme"));
            }
        }
    }
}
