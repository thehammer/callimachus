use anyhow::{Result, bail};
use callimachus_core::corrections::CorrectionsEngine;
use callimachus_core::corrections::fact_import::{self, fact_entity_id};
use callimachus_core::query::service::QueryService;
use callimachus_core::query::types::EntityInput;
use callimachus_core::storage::StorageBackend;
use callimachus_core::types::ToolResult;
use clap::Subcommand;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Subcommand)]
pub enum FactsCommand {
    /// Import fact records (a facts file, or an index file listing several).
    Import {
        /// Corpus to import into.
        corpus_id: String,
        /// Facts files or index files.
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Retract facts in the corpus that are absent from this import.
        #[arg(long)]
        prune: bool,
    },

    /// List the current facts of a corpus.
    List {
        corpus_id: String,
        /// Only facts of this kind.
        #[arg(long)]
        kind: Option<String>,
        /// Only facts of this topic.
        #[arg(long)]
        topic: Option<String>,
        /// Emit JSON.
        #[arg(long)]
        json: bool,
    },

    /// Show one fact with its anchors and linked reference entities.
    Show { corpus_id: String, fact_id: String },
}

pub fn run(cmd: FactsCommand, db: Arc<dyn StorageBackend>) -> Result<()> {
    match cmd {
        FactsCommand::Import {
            corpus_id,
            paths,
            prune,
        } => import(db.as_ref(), &corpus_id, &paths, prune),
        FactsCommand::List {
            corpus_id,
            kind,
            topic,
            json,
        } => list(db.as_ref(), &corpus_id, kind, topic, json),
        FactsCommand::Show { corpus_id, fact_id } => show(db, &corpus_id, &fact_id),
    }
}

fn import(db: &dyn StorageBackend, corpus_id: &str, paths: &[PathBuf], prune: bool) -> Result<()> {
    let r = fact_import::import(db, corpus_id, paths, prune)?;
    println!(
        "added {}  changed {}  unchanged {}  retracted {}  rejected {}  warnings {}",
        r.added,
        r.changed,
        r.unchanged,
        r.retracted,
        r.rejected.len(),
        r.warnings.len()
    );
    println!("anchored {}  materialised {}", r.anchored, r.materialised);
    for line in &r.rejected {
        eprintln!("rejected: {line}");
    }
    for line in &r.warnings {
        eprintln!("warning: {line}");
    }
    Ok(())
}

fn list(
    db: &dyn StorageBackend,
    corpus_id: &str,
    kind: Option<String>,
    topic: Option<String>,
    json: bool,
) -> Result<()> {
    db.corpus_require(corpus_id)?;
    let engine = CorrectionsEngine::load(db, corpus_id)?;
    let mut facts = engine.facts_in_corpus(corpus_id);
    facts.retain(|(f, _)| kind.as_ref().is_none_or(|k| &f.kind == k));
    facts.retain(|(f, _)| topic.as_ref().is_none_or(|t| f.topic.as_ref() == Some(t)));
    facts.sort_by(|a, b| a.0.id.cmp(&b.0.id));
    if json {
        let items: Vec<_> = facts
            .iter()
            .map(|(f, a)| serde_json::json!({ "fact": f, "anchors": a }))
            .collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
        return Ok(());
    }
    for (f, anchors) in facts {
        println!(
            "{:<32} {:<9} {:<9} {}{}",
            f.id,
            f.kind,
            f.confidence,
            f.summary,
            if anchors.is_empty() {
                String::new()
            } else {
                format!("  [{}]", anchors.join(", "))
            }
        );
    }
    Ok(())
}

fn show(db: Arc<dyn StorageBackend>, corpus_id: &str, fact_id: &str) -> Result<()> {
    db.corpus_require(corpus_id)?;
    let engine = CorrectionsEngine::load(db.as_ref(), corpus_id)?;
    let Some((fact, anchors)) = engine
        .facts_in_corpus(corpus_id)
        .into_iter()
        .find(|(f, _)| f.id == fact_id)
        .map(|(f, a)| (f.clone(), a.to_vec()))
    else {
        bail!("no fact '{fact_id}' in corpus '{corpus_id}'");
    };
    println!("{}", serde_json::to_string_pretty(&fact)?);
    let entity_ids = if anchors.is_empty() {
        vec![fact_entity_id(corpus_id, &fact.id)]
    } else {
        anchors.clone()
    };
    println!(
        "anchors: {}",
        if anchors.is_empty() {
            "(none; materialised)".to_string()
        } else {
            anchors.join(", ")
        }
    );
    let qs = QueryService::with_corrections(Arc::clone(&db), engine);
    for id in entity_ids {
        let ToolResult::Ok(s) = qs.entity(EntityInput {
            corpus_id: corpus_id.to_string(),
            name_or_id: id,
            kind: None,
        }) else {
            continue;
        };
        for r in &s.data.reference {
            println!(
                "reference: {}/{}{}  {}",
                r.corpus_id,
                r.name,
                confidence_tag(&r.confidence),
                r.summary.as_deref().unwrap_or("")
            );
        }
        for v in &s.data.variants {
            println!(
                "variant: {}{}  {}",
                v.id,
                confidence_tag(&v.confidence),
                v.summary.as_deref().unwrap_or("")
            );
        }
    }
    Ok(())
}

/// ` (inferred)` for any non-confirmed confidence label, else empty.
fn confidence_tag(confidence: &str) -> String {
    if confidence == "confirmed" {
        String::new()
    } else {
        format!(" ({confidence})")
    }
}
