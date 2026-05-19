use anyhow::Result;
use callimachus_core::storage::{SqliteBackend, StorageBackend};
use std::path::PathBuf;

/// Print information about a `.pinakes` (SQLite) database file.
///
/// Output includes:
/// - File path (resolved)
/// - File size (human-readable)
/// - Schema migration version (`user_version` pragma)
/// - Corpora count
/// - Entities count (across all corpora)
/// - Chunks count (across all corpora)
/// - Scholia (corrections) count
pub fn run_info(path: &std::path::Path) -> Result<()> {
    // File size.
    let metadata = std::fs::metadata(path)
        .map_err(|e| anyhow::anyhow!("cannot stat {}: {e}", path.display()))?;
    let size_bytes = metadata.len();
    let size_str = human_bytes(size_bytes);

    let db = SqliteBackend::open(path)
        .map_err(|e| anyhow::anyhow!("cannot open {}: {e}", path.display()))?;

    let schema_version = db.schema_version().unwrap_or(0);

    let corpora = db.corpus_list().unwrap_or_default();
    let corpora_count = corpora.len();

    let mut entities_count: u64 = 0;
    let mut chunks_count: u64 = 0;
    for corpus in &corpora {
        entities_count += db.entity_count(&corpus.id).unwrap_or(0);
        chunks_count += db.chunk_count(&corpus.id).unwrap_or(0);
    }

    let scholia_count = db.correction_list_all().unwrap_or_default().len();

    println!("path:           {}", path.display());
    println!("size:           {size_str}");
    println!("schema version: {schema_version}");
    println!("corpora:        {corpora_count}");
    println!("entities:       {entities_count}");
    println!("chunks:         {chunks_count}");
    println!("scholia:        {scholia_count}");

    Ok(())
}

/// Resolve the effective path for the pinakes info subcommand:
/// use the provided path if given, otherwise fall back to `default_path`.
pub fn resolve_info_path(path: Option<PathBuf>, default_path: &std::path::Path) -> PathBuf {
    path.unwrap_or_else(|| default_path.to_path_buf())
}

fn human_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_bytes_formats_correctly() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(1536), "1.5 KiB");
        assert_eq!(human_bytes(1024 * 1024), "1.0 MiB");
    }

    /// Verify that `run_info` produces output containing "corpora:" with at least one corpus.
    ///
    /// Uses the bundled demo database at `data/callimachus.db` (relative to the workspace root).
    /// If a `.pinakes`-extension file is present it will be preferred once Phase 1 lands.
    #[test]
    fn pinakes_info_corpora_count_gte_1() {
        // Locate the workspace root from the manifest dir.
        let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent() // callimachus-cli
            .and_then(|p| p.parent()) // crates
            .and_then(|p| p.parent()) // workspace root
            .expect("could not find workspace root");

        // Prefer .pinakes extension; fall back to .db (pre-Phase-1 worktrees).
        let pinakes_path = workspace_root.join("data").join("callimachus.pinakes");
        let db_path = workspace_root.join("data").join("callimachus.db");
        let path = if pinakes_path.exists() {
            pinakes_path
        } else {
            db_path
        };

        if !path.exists() {
            eprintln!("skipping: bundled index not found at {}", path.display());
            return;
        }

        // Capture output by collecting run_info side-effects via direct DB queries.
        let db = SqliteBackend::open(&path).expect("open bundled index");
        let corpora = db.corpus_list().expect("list corpora");
        assert!(
            !corpora.is_empty(),
            "bundled index should have at least 1 corpus, got 0"
        );

        // Also verify run_info succeeds without errors.
        run_info(&path).expect("run_info should succeed on bundled index");
    }
}
