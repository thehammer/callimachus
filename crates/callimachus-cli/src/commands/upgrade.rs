use anyhow::{Result, bail};
use callimachus_core::{PIPELINE_VERSION, storage::StorageBackend};

/// Run `calli upgrade [corpus_id] [--dry-run]`.
///
/// Iterates all (or one) corpus and applies any pending upgrade passes
/// until the corpus reaches `PIPELINE_VERSION`.
pub fn run(corpus_id: Option<&str>, dry_run: bool, db: &dyn StorageBackend) -> Result<()> {
    let corpora = if let Some(id) = corpus_id {
        let c = db.corpus_require(id).map_err(|e| anyhow::anyhow!("{e}"))?;
        vec![c]
    } else {
        db.corpus_list().map_err(|e| anyhow::anyhow!("{e}"))?
    };

    if corpora.is_empty() {
        println!("No corpora found.");
        return Ok(());
    }

    for corpus in &corpora {
        let from = corpus.pipeline_version;
        let to = PIPELINE_VERSION;

        if from >= to {
            println!(
                "  {} ({}): already at version {} — skipped",
                corpus.id, corpus.name, from
            );
            continue;
        }

        println!(
            "  {} ({}): upgrading {} → {}{}",
            corpus.id,
            corpus.name,
            from,
            to,
            if dry_run { " [dry-run]" } else { "" }
        );

        for step in (from + 1)..=to {
            apply_step(&corpus.id, step, dry_run, db)?;
        }

        if !dry_run {
            db.corpus_set_pipeline_version(&corpus.id, to)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("    {} now at version {}", corpus.id, to);
        }
    }

    Ok(())
}

/// Apply a single upgrade step for a corpus.
fn apply_step(corpus_id: &str, step: u32, dry_run: bool, _db: &dyn StorageBackend) -> Result<()> {
    match step {
        1 => {
            // Phase 1 — pipeline_version column added via migration; no data migration needed.
            if dry_run {
                println!("    [dry-run] step 1: no-op (schema migration only)");
            }
            Ok(())
        }
        2 => {
            // Phase 2 — summarize pass generalization.
            // Full re-run of the summarize pass is handled by the caller (`calli index --pass summarize`).
            // The upgrade step itself is a no-op here; callers are advised to re-run the pass.
            if dry_run {
                println!(
                    "    [dry-run] step 2: summarize pass must be re-run for corpus {corpus_id}"
                );
            } else {
                println!(
                    "    step 2: note — re-run `calli index {corpus_id} --pass summarize` to \
                     generate new-style summaries"
                );
            }
            Ok(())
        }
        3 => {
            // Phase 3 — abstract_kind backfill was applied by migration 008.
            if dry_run {
                println!("    [dry-run] step 3: no-op (abstract_kind backfilled by migration)");
            }
            Ok(())
        }
        other => {
            bail!("unknown upgrade step {other} — rebuild with a newer binary");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use callimachus_core::{storage::SqliteBackend, types::Corpus};

    fn make_db() -> SqliteBackend {
        SqliteBackend::open_in_memory().unwrap()
    }

    #[test]
    fn upgrade_empty_db() {
        let db = make_db();
        run(None, false, &db).unwrap();
    }

    #[test]
    fn upgrade_single_corpus_noop() {
        let db = make_db();
        let mut corpus = Corpus::new("c1".into(), "C1".into(), "code".into(), "/tmp/c1".into());
        corpus.pipeline_version = PIPELINE_VERSION;
        db.corpus_insert(&corpus).unwrap();
        run(Some("c1"), false, &db).unwrap();
        let after = db.corpus_require("c1").unwrap();
        assert_eq!(after.pipeline_version, PIPELINE_VERSION);
    }

    #[test]
    fn upgrade_bumps_version() {
        let db = make_db();
        let corpus = Corpus::new("c2".into(), "C2".into(), "code".into(), "/tmp/c2".into());
        // Starts at version 0.
        db.corpus_insert(&corpus).unwrap();
        run(Some("c2"), false, &db).unwrap();
        let after = db.corpus_require("c2").unwrap();
        assert_eq!(after.pipeline_version, PIPELINE_VERSION);
    }

    #[test]
    fn upgrade_dry_run_does_not_mutate() {
        let db = make_db();
        let corpus = Corpus::new("c3".into(), "C3".into(), "code".into(), "/tmp/c3".into());
        db.corpus_insert(&corpus).unwrap();
        run(Some("c3"), true, &db).unwrap();
        let after = db.corpus_require("c3").unwrap();
        // Dry run: version stays at 0.
        assert_eq!(after.pipeline_version, 0);
    }
}
