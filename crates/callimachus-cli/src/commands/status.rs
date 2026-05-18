use anyhow::Result;
use callimachus_core::{PIPELINE_VERSION, storage::StorageBackend};

/// Run `calli status`.
///
/// Prints a table of all corpora with their pipeline version status.
pub fn run(db: &dyn StorageBackend) -> Result<()> {
    let corpora = db.corpus_list().map_err(|e| anyhow::anyhow!("{e}"))?;

    if corpora.is_empty() {
        println!("No corpora registered.");
        return Ok(());
    }

    // Column widths (dynamic).
    let id_w = corpora.iter().map(|c| c.id.len()).max().unwrap_or(2).max(2);
    let name_w = corpora
        .iter()
        .map(|c| c.name.len())
        .max()
        .unwrap_or(4)
        .max(4);

    println!(
        "{:<id_w$}  {:<name_w$}  {:>16}  {:>7}  {}",
        "ID",
        "NAME",
        "PIPELINE VERSION",
        "CURRENT",
        "STATUS",
        id_w = id_w,
        name_w = name_w,
    );
    println!("{}", "-".repeat(id_w + name_w + 40));

    for c in &corpora {
        let status_str = if c.pipeline_version >= PIPELINE_VERSION {
            "up-to-date"
        } else {
            "needs upgrade"
        };
        println!(
            "{:<id_w$}  {:<name_w$}  {:>16}  {:>7}  {}",
            c.id,
            c.name,
            c.pipeline_version,
            PIPELINE_VERSION,
            status_str,
            id_w = id_w,
            name_w = name_w,
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use callimachus_core::{storage::SqliteBackend, types::Corpus};

    #[test]
    fn status_empty() {
        let db = SqliteBackend::open_in_memory().unwrap();
        run(&db).unwrap();
    }

    #[test]
    fn status_with_corpora() {
        let db = SqliteBackend::open_in_memory().unwrap();
        let corpus = Corpus::new(
            "mybook".into(),
            "My Book".into(),
            "book".into(),
            "/tmp/book.epub".into(),
        );
        db.corpus_insert(&corpus).unwrap();
        // Should print without error.
        run(&db).unwrap();
    }
}
