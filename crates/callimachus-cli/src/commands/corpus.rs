use crate::commands::index::{default_registry, unavailable_kind_reason};
use crate::config::GlobalConfig;
use crate::output::{Table, em_dash, print_kv, print_section};
use anyhow::{Context, Result, bail};
use callimachus_adapter_contract::AdapterRegistry;
use callimachus_core::storage::StorageBackend;
use callimachus_core::types::corpus::Corpus;
use clap::Subcommand;
use std::path::PathBuf;

#[derive(Debug, Subcommand)]
pub enum CorpusCommand {
    /// Register a new corpus.
    Add {
        /// Adapter kind: book, capture, code, wiki.
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
    // The registry this binary was built with — used to annotate (not gate)
    // corpora whose adapter isn't compiled into this build.
    let registry = default_registry();
    match cmd {
        CorpusCommand::Add {
            kind,
            name,
            source,
            config,
            id,
        } => add(db, &registry, kind, name, source, config, id),
        CorpusCommand::List => list(db, &registry),
        CorpusCommand::Status { corpus_id } => status(db, &registry, &corpus_id),
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
    registry: &AdapterRegistry,
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

    // Warn-and-record (PRD A6): registration always succeeds — the corpus may
    // be indexed by another build that carries the adapter. But if *this* build
    // can't service the kind, say so loudly rather than letting `calli index`
    // be the first place the user finds out.
    if let Some(reason) = unavailable_kind_reason(&kind, registry) {
        eprintln!(
            "warning: {reason}. The corpus is registered, but this build of calli \
             cannot index it — run a build that includes the '{kind}' adapter. \
             Adapters available in this build: {}.",
            registry.list().join(", ")
        );
    } else {
        println!("Run `calli index {}` to build the index.", id);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

fn list(db: &dyn StorageBackend, registry: &AdapterRegistry) -> Result<()> {
    let corpora = db.corpus_list()?;
    let mut table = Table::new(vec!["ID", "NAME", "KIND", "STATUS", "LAST INDEXED"]);
    for c in &corpora {
        // Annotate the kind dynamically when this build has no adapter for it
        // (PRD A6c). Computed live; nothing is persisted to the DB.
        let kind_cell = if unavailable_kind_reason(&c.kind, registry).is_some() {
            format!("{} (no adapter)", c.kind)
        } else {
            c.kind.clone()
        };
        table.add_row(vec![
            c.id.clone(),
            c.name.clone(),
            kind_cell,
            c.status.to_string(),
            c.last_indexed_at
                .as_deref()
                .map(short_date)
                .unwrap_or_else(|| em_dash().to_string()),
        ]);
    }
    table.print();

    let missing: Vec<&Corpus> = corpora
        .iter()
        .filter(|c| unavailable_kind_reason(&c.kind, registry).is_some())
        .collect();
    if !missing.is_empty() {
        println!();
        for c in missing {
            // Safe to unwrap the reason: filtered to Some above.
            let reason = unavailable_kind_reason(&c.kind, registry).unwrap();
            println!("⚠ {}: {reason} — not indexable by this build.", c.id);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

fn status(db: &dyn StorageBackend, registry: &AdapterRegistry, corpus_id: &str) -> Result<()> {
    let corpus = db.corpus_require(corpus_id)?;

    print_kv("ID", &corpus.id);
    print_kv("Name", &corpus.name);
    print_kv("Kind", &corpus.kind);
    // Adapter availability for this build (PRD A6c) — dynamic, never persisted.
    match unavailable_kind_reason(&corpus.kind, registry) {
        Some(reason) => print_kv("Adapter", &format!("unavailable — {reason}")),
        None => print_kv("Adapter", "available in this build"),
    }
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

    // ── A6: warn-and-record — unbuilt-but-recognized kinds are still registered ──

    /// Calling `corpus add` with a kind that is in KNOWN_KINDS but has no adapter
    /// compiled into this build must return Ok and persist the corpus — it never
    /// rejects the registration. A different build (or a future build of this one)
    /// that carries the adapter can then index it.
    #[test]
    fn corpus_add_with_known_but_unbuilt_kind_succeeds_and_is_persisted() {
        use callimachus_core::storage::{SqliteBackend, StorageBackend};

        let db = SqliteBackend::open_in_memory().unwrap();
        // Use a real directory so the source-exists check passes.
        let source = env!("CARGO_MANIFEST_DIR").to_string();

        let result = super::run(
            CorpusCommand::Add {
                kind: "docs".to_string(),
                name: "Docs Corpus".to_string(),
                source,
                config: None,
                id: Some("docs-corpus".to_string()),
            },
            &db,
            &GlobalConfig::default(),
        );

        assert!(
            result.is_ok(),
            "corpus add must not reject a recognized-but-unbuilt kind; got: {:?}",
            result.err()
        );
        assert!(
            db.corpus_exists("docs-corpus").unwrap(),
            "corpus must be persisted in the DB even when no adapter is compiled in"
        );
    }

    /// Calling `corpus add` with a fully supported kind (one with an adapter in this
    /// build) also returns Ok and persists the corpus.
    #[test]
    fn corpus_add_with_built_kind_succeeds_and_is_persisted() {
        use callimachus_core::storage::{SqliteBackend, StorageBackend};

        let db = SqliteBackend::open_in_memory().unwrap();
        let source = env!("CARGO_MANIFEST_DIR").to_string();

        let result = super::run(
            CorpusCommand::Add {
                kind: "code".to_string(),
                name: "Code Corpus".to_string(),
                source,
                config: None,
                id: Some("code-corpus".to_string()),
            },
            &db,
            &GlobalConfig::default(),
        );

        assert!(
            result.is_ok(),
            "corpus add with built kind 'code' must succeed; got: {:?}",
            result.err()
        );
        assert!(
            db.corpus_exists("code-corpus").unwrap(),
            "corpus must be persisted in the DB for a built kind"
        );
    }
}
