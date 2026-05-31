use crate::config::GlobalConfig;
use crate::output::{Table, em_dash, print_kv, print_section};
use anyhow::{Context, Result, bail};
use callimachus_core::storage::StorageBackend;
use callimachus_core::types::corpus::Corpus;
use clap::Subcommand;
use std::path::PathBuf;

#[derive(Debug, Subcommand)]
pub enum CorpusCommand {
    /// Register a new corpus.
    Add {
        /// Adapter kind: book, code, wiki, docs.
        kind: String,
        /// Human-readable name for this corpus.
        name: String,
        /// Path (or URL) to the source material.
        source: String,
        /// Path to a TOML config file with adapter-specific options.
        #[arg(long)]
        config: Option<PathBuf>,
        /// Override the generated corpus ID (slug).
        #[arg(long)]
        id: Option<String>,
    },

    /// List all registered corpora.
    List,

    /// Show detailed status for a corpus.
    Status {
        /// Corpus ID.
        corpus_id: String,
    },

    /// Remove a corpus and all its indexed data.
    Remove {
        /// Corpus ID.
        corpus_id: String,
        /// Don't delete the source files — only remove the index.
        #[arg(long)]
        keep_source: bool,
    },
}

pub fn run(cmd: CorpusCommand, db: &dyn StorageBackend, _config: &GlobalConfig) -> Result<()> {
    match cmd {
        CorpusCommand::Add {
            kind,
            name,
            source,
            config,
            id,
        } => add(db, kind, name, source, config, id),
        CorpusCommand::List => list(db),
        CorpusCommand::Status { corpus_id } => status(db, &corpus_id),
        CorpusCommand::Remove {
            corpus_id,
            keep_source,
        } => remove(db, &corpus_id, keep_source),
    }
}

// ---------------------------------------------------------------------------
// add
// ---------------------------------------------------------------------------

fn add(
    db: &dyn StorageBackend,
    kind: String,
    name: String,
    source: String,
    config_file: Option<PathBuf>,
    id_override: Option<String>,
) -> Result<()> {
    // Validate source exists (for local paths).
    let source_path = PathBuf::from(&source);
    if !source.starts_with("http://") && !source.starts_with("https://") && !source_path.exists() {
        bail!("source path does not exist: {}", source);
    }

    // Generate or validate the corpus ID.
    let id = match id_override {
        Some(id) => {
            validate_id(&id)?;
            id
        }
        None => slugify(&name),
    };

    if id.is_empty() {
        bail!(
            "could not generate a valid corpus ID from name {:?}. Use --id to set one explicitly.",
            name
        );
    }

    // Check for collision.
    if db.corpus_exists(&id)? {
        bail!(
            "corpus {:?} already exists. Use --id to choose a different ID, or remove it first with `calli corpus remove {}`.",
            id,
            id
        );
    }

    // Load optional config file.
    let config_value = match config_file {
        Some(path) => {
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("reading config file {}", path.display()))?;
            let table: toml::Value =
                toml::from_str(&raw).with_context(|| "parsing corpus config TOML")?;
            // Convert TOML → JSON for storage.
            serde_json::to_value(table)?
        }
        None => serde_json::Value::Object(Default::default()),
    };

    let mut corpus = Corpus::new(id.clone(), name.clone(), kind.clone(), source.clone());
    corpus.config = config_value;

    db.corpus_insert(&corpus)?;

    println!("✓ Registered corpus {:?}", id);
    println!("  name:   {}", name);
    println!("  kind:   {}", kind);
    println!("  source: {}", source);
    println!();
    println!("Run `calli index {}` to build the index.", id);

    Ok(())
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

fn list(db: &dyn StorageBackend) -> Result<()> {
    let corpora = db.corpus_list()?;
    let mut table = Table::new(vec!["ID", "NAME", "KIND", "STATUS", "LAST INDEXED"]);
    for c in &corpora {
        table.add_row(vec![
            c.id.clone(),
            c.name.clone(),
            c.kind.clone(),
            c.status.to_string(),
            c.last_indexed_at
                .as_deref()
                .map(short_date)
                .unwrap_or_else(|| em_dash().to_string()),
        ]);
    }
    table.print();
    Ok(())
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

fn status(db: &dyn StorageBackend, corpus_id: &str) -> Result<()> {
    let corpus = db.corpus_require(corpus_id)?;

    print_kv("ID", &corpus.id);
    print_kv("Name", &corpus.name);
    print_kv("Kind", &corpus.kind);
    print_kv("Status", &corpus.status.to_string());
    print_kv("Source", &corpus.source);
    print_kv("Created", &short_date(&corpus.created_at));
    print_kv(
        "Last indexed",
        corpus
            .last_indexed_at
            .as_deref()
            .map(short_date)
            .unwrap_or_else(|| em_dash().to_string())
            .as_str(),
    );

    print_section("Index");
    let chunks = db.chunk_count(corpus_id)?;
    let entities = db.entity_count(corpus_id)?;
    let edges = db.edge_count(corpus_id)?;
    print_kv("Chunks", &chunks.to_string());
    print_kv("Entities", &entities.to_string());
    print_kv("Edges", &edges.to_string());

    print_section("Recent runs");
    let runs = db.run_latest(corpus_id, 20)?;
    if runs.is_empty() {
        println!(
            "(no runs yet — use `calli index {}` to start indexing)",
            corpus_id
        );
    } else {
        let mut table = Table::new(vec!["PASS", "STATUS", "STARTED", "PROCESSED", "FAILED"]);
        for run in &runs {
            table.add_row(vec![
                run.pass.clone(),
                run.status.clone(),
                short_date(&run.started_at),
                run.stats.processed.to_string(),
                run.stats.failed.to_string(),
            ]);
        }
        table.print();
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// remove
// ---------------------------------------------------------------------------

fn remove(db: &dyn StorageBackend, corpus_id: &str, keep_source: bool) -> Result<()> {
    let corpus = db.corpus_require(corpus_id)?;

    let deleted = db.corpus_delete(corpus_id)?;
    if !deleted {
        bail!("corpus {:?} not found", corpus_id);
    }

    if !keep_source {
        // Only attempt deletion if the source is a local file/directory that exists.
        let source_path = PathBuf::from(&corpus.source);
        if source_path.exists()
            && !corpus.source.starts_with("http://")
            && !corpus.source.starts_with("https://")
        {
            println!(
                "note: source files at {} were NOT deleted (use --keep-source to suppress this note, or remove manually)",
                corpus.source
            );
        }
    }

    println!("✓ Removed corpus {:?} and all indexed data.", corpus_id);
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn slugify(name: &str) -> String {
    let slug: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    // Collapse runs of hyphens and strip leading/trailing.
    slug.split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

fn validate_id(id: &str) -> Result<()> {
    if id.is_empty() {
        bail!("corpus ID cannot be empty");
    }
    if id.len() > 64 {
        bail!("corpus ID too long (max 64 chars)");
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!("corpus ID may only contain letters, digits, hyphens, and underscores");
    }
    Ok(())
}

fn short_date(iso: &str) -> String {
    // "2026-05-16T12:00:00+00:00" → "2026-05-16"
    iso.get(..10).unwrap_or(iso).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Xenos"), "xenos");
        assert_eq!(slugify("The Maisie Project"), "the-maisie-project");
        assert_eq!(slugify("Foo  Bar--Baz"), "foo-bar-baz");
    }

    #[test]
    fn validate_id_ok() {
        assert!(validate_id("xenos").is_ok());
        assert!(validate_id("my-corpus_1").is_ok());
    }

    #[test]
    fn validate_id_rejects_bad() {
        assert!(validate_id("").is_err());
        assert!(validate_id("has spaces").is_err());
        assert!(validate_id("has/slash").is_err());
    }
}
