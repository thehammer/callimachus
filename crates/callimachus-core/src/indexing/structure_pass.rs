use std::sync::Arc;

use crate::{
    adapter::SourceAdapter,
    storage::{StorageBackend, run_log::PassStats},
    types::Corpus,
    types::provenance::Provenance,
};

use super::{file_shape, pipeline::IndexOptions};

pub async fn run(
    db: Arc<dyn StorageBackend>,
    corpus: &Corpus,
    adapter: Arc<dyn SourceAdapter>,
    opts: &IndexOptions,
) -> anyhow::Result<PassStats> {
    let mut stats = PassStats::default();

    let mut chunks = db.chunk_list(&corpus.id)?;

    // Skip chunks whose source file is unchanged according to the manifest.
    if let Some(m) = opts.change_manifest.as_ref() {
        chunks.retain(|c| m.is_dirty_for_chunk(c));
    }

    let total = chunks.len() as u64;

    // Two-phase approach: collect all structural data first, then write entities
    // before edges. Edges often reference entities defined in other chunks
    // (e.g. `calls` edges cross file boundaries), so inserting them interleaved
    // with chunk processing causes FK failures when the target entity hasn't
    // been written yet. INSERT OR IGNORE suppresses UNIQUE conflicts but NOT
    // foreign-key violations in SQLite.
    let mut all_parent_paths: Vec<(String, String)> = Vec::new();
    let mut all_entities = Vec::new();
    let mut all_edges = Vec::new();

    for chunk in &chunks {
        if chunk.parent_path.is_some() {
            stats.skipped += 1;
            continue;
        }

        let structure = match adapter.extract_structure(chunk).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("structure extraction failed for {}: {e}", chunk.id);
                stats.failed += 1;
                continue;
            }
        };

        if let Some(ref parent) = structure.parent_path {
            all_parent_paths.push((chunk.id.clone(), parent.clone()));
        }
        all_entities.extend(structure.structural_entities);
        all_edges.extend(structure.structural_edges);
        stats.processed += 1;

        if stats.processed % 25 == 0 {
            tracing::info!("[structure] {}/{} chunks extracted", stats.processed, total);
        }
    }

    if opts.dry_run {
        return Ok(stats);
    }

    // Stamp provenance from the change manifest before writing.
    let version = opts
        .change_manifest
        .as_ref()
        .map(|m| m.current_version.clone());
    for entity in &mut all_entities {
        entity.provenance = version.as_deref().map(Provenance::concrete);
    }
    for edge in &mut all_edges {
        edge.provenance = version.as_deref().map(Provenance::concrete);
    }

    // Phase 1: write parent paths and all entities.
    for (chunk_id, parent) in &all_parent_paths {
        db.chunk_set_parent_path(chunk_id, parent)?;
    }
    for entity in &all_entities {
        db.entity_upsert(entity)?;
    }

    // Phase 2: write edges — all referenced entity IDs now exist.
    for edge in &all_edges {
        db.edge_upsert(edge)?;
    }

    // Phase 3: stamp each file chunk with its file-shape hash — the Layer-2
    // cache's invalidation boundary (see `indexing::file_shape`). Grouped by the
    // entity's `first_location` URI, which for code corpora is the file chunk's
    // location URI.
    let mut ids_by_uri: std::collections::HashMap<&str, Vec<String>> =
        std::collections::HashMap::new();
    for entity in &all_entities {
        if let Some(loc) = entity.first_location.as_ref() {
            ids_by_uri
                .entry(loc.uri.as_str())
                .or_default()
                .push(entity.id.clone());
        }
    }
    for chunk in &chunks {
        if let Some(ids) = ids_by_uri.get(chunk.location.uri.as_str()) {
            let (hash, json) = file_shape::file_shape_hash(ids);
            db.chunk_set_file_shape(&chunk.id, &hash, &json)?;
        }
    }

    tracing::info!(
        "[structure] wrote {} entities, {} edges",
        all_entities.len(),
        all_edges.len()
    );

    Ok(stats)
}
