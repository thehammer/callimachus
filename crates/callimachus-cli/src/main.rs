mod commands;
mod config;
mod output;

use anyhow::{Context, Result, bail};
use callimachus_core::storage::{SqliteBackend, StorageBackend};
use clap::{Parser, Subcommand};
use commands::collection::CollectionSubcommand;
use commands::corpus::CorpusCommand;
use commands::correct::{CorrectSubcommand, EntityLinkArgs};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Parser)]
#[command(
    name = "calli",
    version,
    about = "Callimachus — a queryable index over arbitrary corpora, exposed as LLM tools",
    long_about = None,
)]
struct Cli {
    /// Path to the Pinakes index file (e.g. index.pinakes).
    /// Overridden by CALLIMACHUS_PINAKES environment variable.
    #[arg(long, global = true, env = "CALLIMACHUS_PINAKES")]
    pinakes: Option<PathBuf>,

    /// Path to the index file [deprecated: use --pinakes].
    /// Overridden by CALLIMACHUS_DB environment variable.
    #[arg(long, global = true, env = "CALLIMACHUS_DB", hide = false)]
    db: Option<PathBuf>,

    /// Log level (error, warn, info, debug, trace).
    #[arg(long, global = true, default_value = "warn", env = "CALLIMACHUS_LOG")]
    log: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Manage corpora (add, list, status, remove).
    #[command(subcommand)]
    Corpus(CorpusCommand),

    /// Manage collections (add, list, status, add-member, remove-member, remove).
    Collection {
        #[command(subcommand)]
        sub: CollectionCommand,
    },

    /// Index a corpus (or specific passes).
    Index {
        corpus_id: String,
        #[arg(long)]
        pass: Option<String>,
        #[arg(long)]
        from_chunk: Option<String>,
        #[arg(long)]
        dry_run: bool,
        /// Force a full reindex: bypass all "skip if already processed" guards.
        #[arg(long)]
        full: bool,
        /// Disable git-aware file walking; index every file under the source path.
        #[arg(long)]
        no_git_filter: bool,
    },

    /// Incremental reindex since a commit or date.
    Reindex {
        corpus_id: String,
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        provider: Option<String>,
    },

    /// Watch a corpus source and reindex on changes.
    Watch {
        corpus_id: String,
        #[arg(long, default_value = "500")]
        debounce: u64,
        #[arg(long)]
        provider: Option<String>,
    },

    /// Inspect indexed data (entities, chunks, runs, corrections).
    Inspect {
        #[command(subcommand)]
        sub: InspectCommand,
    },

    /// Apply a manual correction to the index.
    Correct {
        corpus_id: String,
        #[command(subcommand)]
        sub: CorrectCommand,
    },

    /// Export index data.
    Export {
        corpus_id: String,
        #[arg(long, default_value = "jsonl")]
        format: String,
        #[arg(long)]
        output: Option<PathBuf>,
    },

    /// Start the MCP stdio server.
    Mcp,

    /// Start the HTTP API server.
    Serve {
        #[arg(long, default_value = "7700")]
        port: u16,
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
    },

    /// Show or modify global configuration.
    Config {
        #[command(subcommand)]
        sub: ConfigCommand,
    },

    /// Show pipeline version status for all corpora.
    Status,

    /// Upgrade corpora to the current pipeline version.
    Upgrade {
        /// Corpus to upgrade (omit to upgrade all corpora).
        corpus_id: Option<String>,
        /// Print what would be done without making changes.
        #[arg(long)]
        dry_run: bool,
    },

    /// Cross-corpus entity link utilities.
    Link {
        #[command(subcommand)]
        sub: LinkSubcommand,
    },

    /// Inspect a Pinakes index file.
    Pinakes {
        #[command(subcommand)]
        sub: PinakesSubcommand,
    },
}

#[derive(Debug, Subcommand)]
enum PinakesSubcommand {
    /// Print information about a Pinakes index file: path, size, schema version, and counts.
    Info {
        /// Path to the Pinakes file. Defaults to the active index path.
        path: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
enum InspectCommand {
    /// Browse entities in a corpus (corrections applied).
    Entities {
        corpus_id: String,
        #[arg(long)]
        filter: Option<String>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        min_confidence: Option<f32>,
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Show a specific chunk by location URI.
    Chunk { location: String },
    /// Show indexing run history.
    Runs {
        corpus_id: String,
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Show applied corrections for a corpus.
    Corrections { corpus_id: String },
    /// Show cross-corpus entity links for a collection.
    CollectionLinks {
        collection_id: String,
        #[arg(long)]
        kind: Option<String>,
    },
    /// Assemble a diegesis (narrative exposition) for an entity via BFS over call edges.
    Diegesis {
        corpus_id: String,
        entity: String,
        #[arg(long)]
        max_depth: Option<u8>,
    },
}

#[derive(Debug, Subcommand)]
enum CorrectCommand {
    /// Merge two entities; entity_a is the canonical by default.
    Merge {
        entity_a: String,
        entity_b: String,
        #[arg(long)]
        keep: Option<String>,
    },
    /// Unmerge an entity that was merged in the stored entities table.
    Unmerge {
        entity_id: String,
        #[arg(long, default_value = "scene")]
        split_by: String,
    },
    /// Rename the canonical name of an entity.
    Rename { entity_id: String, new_name: String },
    /// Add or remove aliases on an entity.
    Alias {
        entity_id: String,
        #[arg(long, action = clap::ArgAction::Append)]
        add: Vec<String>,
        #[arg(long, action = clap::ArgAction::Append)]
        remove: Vec<String>,
    },
    /// Replace the generated summary for a chunk, entity, or corpus.
    EditSummary { target: String, text: String },
    /// Record a typed cross-corpus entity link (collection-scoped).
    EntityLink {
        #[arg(long)]
        collection: String,
        #[arg(long, name = "corpus-a")]
        corpus_a: String,
        #[arg(long, name = "entity-a")]
        entity_a: String,
        #[arg(long, name = "corpus-b")]
        corpus_b: String,
        #[arg(long, name = "entity-b")]
        entity_b: String,
        /// Relationship kind: same_as | implements | exemplifies | references | contrasts
        #[arg(long)]
        kind: String,
        #[arg(long)]
        note: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum CollectionCommand {
    /// Create a new collection.
    Add {
        name: String,
        #[arg(long, default_value = "series")]
        kind: Option<String>,
    },
    /// List all collections.
    List,
    /// Show the resolved corpus tree and stats for a collection.
    Status { collection_id: String },
    /// Add a member to a collection.
    AddMember {
        collection_id: String,
        member: String,
        /// Treat `member` as a nested collection (default: corpus).
        #[arg(long)]
        collection: bool,
    },
    /// Remove a member from a collection.
    RemoveMember {
        collection_id: String,
        member: String,
        /// Treat `member` as a nested collection (default: corpus).
        #[arg(long)]
        collection: bool,
    },
    /// Delete a collection.
    Remove { collection_id: String },
}

#[derive(Debug, Subcommand)]
enum LinkSubcommand {
    /// Find candidate entity links between two corpora.
    Candidates { corpus_a: String, corpus_b: String },
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    /// Print the active config.
    Show,
    /// Print the config file path.
    Path,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Init tracing.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&cli.log));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    let global_config = config::GlobalConfig::load().unwrap_or_default();
    let db_path = config::resolve_pinakes_path(cli.pinakes, cli.db, &global_config);

    match cli.command {
        Command::Corpus(sub) => {
            let db = open_db(&db_path)?;
            commands::corpus::run(sub, db.as_ref(), &global_config)
        }

        Command::Collection { sub } => {
            let db = open_db(&db_path)?;
            commands::collection::run(translate_collection_sub(sub), db.as_ref())
        }

        Command::Index {
            corpus_id,
            pass,
            from_chunk,
            dry_run,
            full,
            no_git_filter,
        } => {
            let db = open_db(&db_path)?;
            commands::index::run(
                &corpus_id,
                pass,
                from_chunk,
                dry_run,
                full,
                no_git_filter,
                None,
                db,
                &global_config,
            )
            .await
        }

        Command::Inspect { sub } => {
            let db = open_db(&db_path)?;
            run_inspect(sub, db)
        }

        Command::Correct { corpus_id, sub } => {
            let db = open_db(&db_path)?;
            // EntityLink is collection-scoped and handled separately.
            if let CorrectCommand::EntityLink {
                collection,
                corpus_a,
                entity_a,
                corpus_b,
                entity_b,
                kind,
                note,
            } = sub
            {
                commands::correct::run_entity_link(
                    EntityLinkArgs {
                        collection_id: collection,
                        corpus_a,
                        entity_a,
                        corpus_b,
                        entity_b,
                        kind,
                        note,
                    },
                    db.as_ref(),
                )
            } else {
                let correct_sub = translate_correct_sub(sub);
                commands::correct::run(&corpus_id, correct_sub, db.as_ref())
            }
        }

        Command::Export {
            corpus_id,
            format,
            output,
        } => {
            let db = open_db(&db_path)?;
            match format.as_str() {
                "jsonl" => match output {
                    Some(path) => {
                        let mut file = std::fs::File::create(&path)
                            .with_context(|| format!("creating output file {}", path.display()))?;
                        commands::export::run_to_writer(&corpus_id, db.as_ref(), &mut file)
                    }
                    None => {
                        let stdout = std::io::stdout();
                        let mut out = stdout.lock();
                        commands::export::run_to_writer(&corpus_id, db.as_ref(), &mut out)
                    }
                },
                "scip" => match output {
                    Some(path) => {
                        let mut file = std::fs::File::create(&path)
                            .with_context(|| format!("creating output file {}", path.display()))?;
                        commands::export::run_scip_to_writer(&corpus_id, db.as_ref(), &mut file)
                    }
                    None => {
                        let stdout = std::io::stdout();
                        let mut out = stdout.lock();
                        commands::export::run_scip_to_writer(&corpus_id, db.as_ref(), &mut out)
                    }
                },
                other => {
                    bail!(
                        "unsupported export format {:?}; supported: jsonl, scip",
                        other
                    );
                }
            }
        }

        Command::Reindex {
            corpus_id,
            since,
            dry_run,
            provider,
        } => {
            let db = open_db(&db_path)?;
            commands::reindex::run(&corpus_id, since, dry_run, provider, db, &global_config).await
        }

        Command::Watch {
            corpus_id,
            debounce,
            provider,
        } => commands::watch::run(&corpus_id, debounce, provider, &db_path, &global_config).await,
        Command::Mcp => commands::mcp::run(&db_path).await,
        Command::Serve { host, port } => {
            commands::serve::run(&host, port, &db_path, &global_config).await
        }
        Command::Config { sub } => run_config(&sub, &global_config),

        Command::Status => {
            let db = open_db(&db_path)?;
            commands::status::run(db.as_ref())
        }

        Command::Upgrade { corpus_id, dry_run } => {
            let db = open_db(&db_path)?;
            commands::upgrade::run(corpus_id.as_deref(), dry_run, db.as_ref())
        }

        Command::Link { sub } => {
            let db = open_db(&db_path)?;
            let link_sub = match sub {
                LinkSubcommand::Candidates { corpus_a, corpus_b } => {
                    commands::link::LinkSubcommand::Candidates { corpus_a, corpus_b }
                }
            };
            commands::link::run(link_sub, db.as_ref())
        }

        Command::Pinakes { sub } => match sub {
            PinakesSubcommand::Info { path } => {
                let resolved = commands::pinakes::resolve_info_path(path, &db_path);
                commands::pinakes::run_info(&resolved)
            }
        },
    }
}

// ---------------------------------------------------------------------------
// inspect dispatch
// ---------------------------------------------------------------------------

fn run_inspect(sub: InspectCommand, db: Arc<dyn StorageBackend>) -> Result<()> {
    match sub {
        InspectCommand::Entities {
            corpus_id,
            filter,
            kind,
            min_confidence,
            limit,
        } => {
            let entities = commands::inspect::run_entities(
                &corpus_id,
                filter.as_deref(),
                kind.as_deref(),
                min_confidence,
                limit,
                db.as_ref(),
            )?;

            let mut table =
                output::Table::new(vec!["NAME", "KIND", "APPEARANCES", "CONFIDENCE", "ALIASES"]);
            for e in &entities {
                table.add_row(vec![
                    e.canonical_name.clone(),
                    e.kind.clone(),
                    e.appearance_count.to_string(),
                    format!("{:.2}", e.confidence),
                    e.aliases.join(", "),
                ]);
            }
            table.print();
        }

        InspectCommand::Chunk { location } => {
            match commands::inspect::run_chunk(&location, db.as_ref())? {
                None => {
                    eprintln!("error: chunk not found: {location}");
                    std::process::exit(1);
                }
                Some(chunk) => {
                    output::print_kv("id", &chunk.id);
                    output::print_kv("corpus_id", &chunk.corpus_id);
                    output::print_kv("location_uri", &chunk.location.uri);
                    output::print_kv("kind", &chunk.kind);
                    if let Some(pp) = &chunk.parent_path {
                        output::print_kv("parent_path", pp);
                    }
                    output::print_kv("byte_length", &chunk.byte_length.to_string());
                    output::print_kv("content", &chunk.content);
                }
            }
        }

        InspectCommand::Runs { corpus_id, limit } => {
            let cap = limit.unwrap_or(20);
            let runs = db.as_ref().run_latest(&corpus_id, cap)?;
            let mut table = output::Table::new(vec![
                "PASS", "STATUS", "STARTED", "CHUNKS", "ENTITIES", "COST",
            ]);
            for run in runs.iter().take(cap) {
                let chunks = format!(
                    "{}/{}",
                    run.stats.processed,
                    run.stats.processed + run.stats.skipped + run.stats.failed
                );
                let cost = run
                    .stats
                    .cost_usd
                    .map(|c| format!("${:.2}", c))
                    .unwrap_or_else(|| "-".to_string());
                table.add_row(vec![
                    run.pass.clone(),
                    run.status.clone(),
                    run.started_at.clone(),
                    chunks,
                    "-".to_string(),
                    cost,
                ]);
            }
            table.print();
        }

        InspectCommand::Corrections { corpus_id } => {
            let corrections = commands::inspect::run_corrections(&corpus_id, db.as_ref())?;
            if corrections.is_empty() {
                println!("(no corrections)");
                return Ok(());
            }
            for c in &corrections {
                let description = describe_correction(c);
                println!(
                    "{:>25}  {:>10}  {}",
                    c.applied_at,
                    c.kind.kind_name(),
                    description
                );
            }
        }

        InspectCommand::CollectionLinks {
            collection_id,
            kind,
        } => {
            commands::inspect::run_collection_links(&collection_id, kind.as_deref(), db.as_ref())?;
        }

        InspectCommand::Diegesis {
            corpus_id,
            entity,
            max_depth,
        } => {
            commands::inspect::run_diegesis(&corpus_id, &entity, max_depth, db)?;
        }
    }
    Ok(())
}

fn describe_correction(c: &callimachus_core::corrections::types::Correction) -> String {
    use callimachus_core::corrections::types::CorrectionKind;
    match &c.kind {
        CorrectionKind::Merge {
            entity_a_id,
            entity_b_id,
            canonical_id,
        } => {
            let kept = if canonical_id == entity_a_id {
                entity_b_id
            } else {
                entity_a_id
            };
            format!("Merged {:?} into {:?}", kept, canonical_id)
        }
        CorrectionKind::Unmerge {
            entity_id,
            split_by,
        } => {
            format!("Unmerged {:?} (split by {:?})", entity_id, split_by)
        }
        CorrectionKind::Rename {
            entity_id,
            new_name,
        } => {
            format!("Renamed entity {:?} to {:?}", entity_id, new_name)
        }
        CorrectionKind::Alias {
            entity_id,
            add,
            remove,
        } => {
            format!(
                "Alias update on {:?}: +{} -{}",
                entity_id,
                add.join(","),
                remove.join(",")
            )
        }
        CorrectionKind::EditSummary {
            target_kind,
            target_id,
            ..
        } => {
            format!("Replaced summary for {} {:?}", target_kind, target_id)
        }
        CorrectionKind::EntityLink {
            corpus_a_id,
            entity_a_id,
            corpus_b_id,
            entity_b_id,
            kind,
            ..
        } => {
            format!(
                "EntityLink ({}) {}/{} → {}/{}",
                kind.as_str(),
                corpus_a_id,
                entity_a_id,
                corpus_b_id,
                entity_b_id
            )
        }
    }
}

// ---------------------------------------------------------------------------
// Translate clap CorrectCommand → commands::correct::CorrectSubcommand
// ---------------------------------------------------------------------------

fn translate_correct_sub(sub: CorrectCommand) -> CorrectSubcommand {
    match sub {
        CorrectCommand::Merge {
            entity_a,
            entity_b,
            keep,
        } => {
            // If --keep is provided, use it as the canonical id; otherwise default to entity_a.
            let canonical = keep.unwrap_or_else(|| entity_a.clone());
            // We express this by ordering (entity_a_id=canonical, entity_b_id=other).
            if canonical == entity_a {
                CorrectSubcommand::Merge {
                    entity_a_id: entity_a,
                    entity_b_id: entity_b,
                }
            } else {
                // --keep specifies entity_b as canonical; swap them so entity_a_id = canonical.
                CorrectSubcommand::Merge {
                    entity_a_id: entity_b,
                    entity_b_id: entity_a,
                }
            }
        }
        CorrectCommand::Unmerge {
            entity_id,
            split_by,
        } => CorrectSubcommand::Unmerge {
            entity_id,
            split_by,
        },
        CorrectCommand::Rename {
            entity_id,
            new_name,
        } => CorrectSubcommand::Rename {
            entity_id,
            new_name,
        },
        CorrectCommand::Alias {
            entity_id,
            add,
            remove,
        } => CorrectSubcommand::Alias {
            entity_id,
            add,
            remove,
        },
        CorrectCommand::EditSummary { target, text } => {
            CorrectSubcommand::EditSummary { target, text }
        }
        // EntityLink is handled before translate_correct_sub is called; this branch is unreachable.
        CorrectCommand::EntityLink { .. } => unreachable!("entity-link handled in main"),
    }
}

fn translate_collection_sub(sub: CollectionCommand) -> CollectionSubcommand {
    match sub {
        CollectionCommand::Add { name, kind } => CollectionSubcommand::Add { name, kind },
        CollectionCommand::List => CollectionSubcommand::List,
        CollectionCommand::Status { collection_id } => {
            CollectionSubcommand::Status { collection_id }
        }
        CollectionCommand::AddMember {
            collection_id,
            member,
            collection,
        } => CollectionSubcommand::AddMember {
            collection_id,
            member,
            as_collection: collection,
        },
        CollectionCommand::RemoveMember {
            collection_id,
            member,
            collection,
        } => CollectionSubcommand::RemoveMember {
            collection_id,
            member,
            as_collection: collection,
        },
        CollectionCommand::Remove { collection_id } => {
            CollectionSubcommand::Remove { collection_id }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn open_db(path: &std::path::Path) -> Result<Arc<dyn StorageBackend>> {
    tracing::debug!("opening database at {}", path.display());
    let backend = SqliteBackend::open(path)
        .with_context(|| format!("opening database at {}", path.display()))?;
    Ok(Arc::new(backend))
}

fn run_config(sub: &ConfigCommand, config: &config::GlobalConfig) -> Result<()> {
    match sub {
        ConfigCommand::Show => {
            println!("{}", toml::to_string_pretty(config)?);
        }
        ConfigCommand::Path => {
            println!("{}", config::config_file_path().display());
        }
    }
    Ok(())
}
