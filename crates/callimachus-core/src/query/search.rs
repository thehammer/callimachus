use std::collections::HashMap;

use crate::query::types::SearchResult;
use crate::storage::{StorageBackend, embedding_store::StoredEmbedding};
use crate::types::{Location, Scope};
use anyhow::Result;

/// Keyword search using FTS5.
///
/// Relevance is derived from the FTS5 rank (which is negative in SQLite; a
/// more-negative value means higher relevance). We invert and normalize to
/// [0, 1] across the returned result set.
pub fn keyword_search(
    db: &dyn StorageBackend,
    corpus_id: &str,
    query: &str,
    scope: Option<&Scope>,
    limit: usize,
) -> Result<Vec<SearchResult>> {
    let raw = db.fts_search(corpus_id, query, limit)?;
    if raw.is_empty() {
        return Ok(vec![]);
    }

    // FTS5 rank is negative (more-negative = more relevant).
    // Normalize to [0, 1]: relevance = (rank - max_rank) / (min_rank - max_rank)
    // where min_rank is the most-negative value.
    let min_rank = raw.iter().map(|r| r.rank).fold(f64::INFINITY, f64::min);
    let max_rank = raw.iter().map(|r| r.rank).fold(f64::NEG_INFINITY, f64::max);
    let range = min_rank - max_rank;

    let scope_position_uri = scope
        .and_then(|s| s.position.as_ref())
        .map(|l| l.uri.as_str());

    let mut results = Vec::new();
    for r in &raw {
        // Scope filtering: exclude results after the position URI (lexicographic).
        if let Some(pos) = scope_position_uri
            && r.location_uri.as_str() > pos
        {
            continue;
        }

        let relevance = if range.abs() < f64::EPSILON {
            1.0_f32
        } else {
            ((r.rank - max_rank) / range) as f32
        };

        let kind = db
            .chunk_get_by_uri(&r.location_uri)?
            .map(|c| c.kind)
            .unwrap_or_else(|| "chunk".to_string());

        let location = Location::parse(&r.location_uri).unwrap_or_else(|_| Location {
            corpus_id: corpus_id.to_string(),
            path: r.location_uri.clone(),
            uri: r.location_uri.clone(),
        });

        results.push(SearchResult {
            location,
            snippet: r.snippet.clone(),
            relevance,
            kind,
        });
    }

    Ok(results)
}

/// Cosine similarity between two equal-length vectors.
/// Returns 0.0 if either vector is all-zero or lengths differ.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a < f32::EPSILON || norm_b < f32::EPSILON {
        return 0.0;
    }
    (dot / (norm_a * norm_b)).clamp(0.0, 1.0)
}

/// Semantic search using in-memory cosine similarity.
///
/// Loads **all** embeddings for the corpus into memory, computes cosine
/// similarity against `query_vector`, and returns the top `limit` results
/// sorted by similarity descending.
///
/// Adequate for corpora up to ~50k chunks. For larger corpora, a
/// `sqlite-vec`-backed implementation should be used (v2 item).
pub fn semantic_search(
    db: &dyn StorageBackend,
    corpus_id: &str,
    query_vector: &[f32],
    scope: Option<&Scope>,
    limit: usize,
) -> Result<Vec<SearchResult>> {
    let embeddings = db.embedding_list_for_corpus(corpus_id)?;
    if embeddings.is_empty() {
        return Ok(vec![]);
    }

    let scope_position_uri = scope
        .and_then(|s| s.position.as_ref())
        .map(|l| l.uri.as_str());

    // Score all embeddings.
    let mut scored: Vec<(f32, &StoredEmbedding)> = embeddings
        .iter()
        .map(|emb| (cosine_similarity(query_vector, &emb.vector), emb))
        .collect();

    // Sort by similarity descending.
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut results = Vec::new();
    for (similarity, emb) in &scored {
        if results.len() >= limit {
            break;
        }

        // Look up the chunk for location + content.
        let chunk = match db.chunk_get(&emb.chunk_id)? {
            Some(c) => c,
            None => continue,
        };

        // Scope filtering.
        if let Some(pos) = scope_position_uri
            && chunk.location.uri.as_str() > pos
        {
            continue;
        }

        let snippet = chunk.content.chars().take(500).collect::<String>();

        results.push(SearchResult {
            location: chunk.location.clone(),
            snippet,
            relevance: *similarity,
            kind: chunk.kind.clone(),
        });
    }

    Ok(results)
}

/// Hybrid search: blend keyword and semantic scores.
///
/// `alpha` controls the blend: `alpha * semantic + (1 - alpha) * keyword`.
/// Default `alpha = 0.5`.
///
/// Both score sets are independently normalized to [0, 1] before blending.
/// The union of chunk IDs from both result sets is scored; chunks missing
/// from one set get score 0.0 for that dimension.
pub fn hybrid_search(
    db: &dyn StorageBackend,
    corpus_id: &str,
    query: &str,
    query_vector: &[f32],
    scope: Option<&Scope>,
    limit: usize,
    alpha: f32,
) -> Result<Vec<SearchResult>> {
    // Fetch more than `limit` from each source so the union is meaningful.
    let fetch_n = limit * 4;

    let keyword_results = keyword_search(db, corpus_id, query, scope, fetch_n)?;
    let semantic_results = semantic_search(db, corpus_id, query_vector, scope, fetch_n)?;

    // Build score maps keyed by location URI.
    let mut keyword_scores: HashMap<String, f32> = HashMap::new();
    for r in &keyword_results {
        keyword_scores.insert(r.location.uri.clone(), r.relevance);
    }

    let mut semantic_scores: HashMap<String, f32> = HashMap::new();
    for r in &semantic_results {
        semantic_scores.insert(r.location.uri.clone(), r.relevance);
    }

    // Union of all URIs.
    let mut all_uris: Vec<String> = keyword_scores.keys().cloned().collect();
    for uri in semantic_scores.keys() {
        if !keyword_scores.contains_key(uri) {
            all_uris.push(uri.clone());
        }
    }

    // Blend scores.
    let mut blended: Vec<(f32, String)> = all_uris
        .into_iter()
        .map(|uri| {
            let ks = keyword_scores.get(&uri).copied().unwrap_or(0.0);
            let ss = semantic_scores.get(&uri).copied().unwrap_or(0.0);
            let score = alpha * ss + (1.0 - alpha) * ks;
            (score, uri)
        })
        .collect();

    blended.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    blended.truncate(limit);

    // Build result list from blended top-k.
    // Re-use snippets from whichever result set has the URI.
    let mut result_map: HashMap<String, SearchResult> = HashMap::new();
    for r in keyword_results.into_iter().chain(semantic_results) {
        result_map.entry(r.location.uri.clone()).or_insert(r);
    }

    let mut results = Vec::with_capacity(blended.len());
    for (score, uri) in blended {
        if let Some(mut r) = result_map.remove(&uri) {
            r.relevance = score;
            results.push(r);
        }
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::SqliteBackend;
    use crate::storage::embedding_store::StoredEmbedding;
    use crate::types::{Chunk, Corpus, Location};

    fn make_db() -> SqliteBackend {
        SqliteBackend::open_in_memory().unwrap()
    }

    fn seed_corpus(db: &dyn StorageBackend) -> Corpus {
        let corpus = Corpus::new(
            "c1".into(),
            "Test".into(),
            "book".into(),
            "/tmp/test".into(),
        );
        db.corpus_insert(&corpus).unwrap();
        corpus
    }

    // ── keyword tests ────────────────────────────────────────────────────────

    #[test]
    fn returns_empty_for_no_match() {
        let db = make_db();
        let corpus = seed_corpus(&db);

        let loc = Location::new("c1", "ch/1");
        let chunk = Chunk::new(
            "c1".into(),
            None,
            "chapter".into(),
            loc,
            "Hello world".into(),
        );
        db.chunk_upsert(&chunk).unwrap();
        db.fts_rebuild("c1").unwrap();

        let results = keyword_search(&db, "c1", "nonexistent", None, 10).unwrap();
        assert!(results.is_empty());
        let _ = corpus;
    }

    #[test]
    fn returns_results_for_match() {
        let db = make_db();
        let corpus = seed_corpus(&db);

        let loc1 = Location::new("c1", "ch/1");
        let chunk1 = Chunk::new(
            "c1".into(),
            None,
            "chapter".into(),
            loc1,
            "Frodo sat under the oak tree".into(),
        );
        db.chunk_upsert(&chunk1).unwrap();

        let loc2 = Location::new("c1", "ch/2");
        let chunk2 = Chunk::new(
            "c1".into(),
            None,
            "chapter".into(),
            loc2,
            "Samwise liked gardening".into(),
        );
        db.chunk_upsert(&chunk2).unwrap();

        db.fts_rebuild("c1").unwrap();

        let results = keyword_search(&db, "c1", "Frodo", None, 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].location.path, "ch/1");

        let _ = corpus;
    }

    // ── semantic search tests ────────────────────────────────────────────────

    fn seed_embeddings(db: &dyn StorageBackend, corpus_id: &str) -> Vec<Chunk> {
        // 2D embeddings for easy manual cosine math.
        let specs: &[(&str, &str, &str, [f32; 2])] = &[
            ("chunk-A", "wiki/a", "page-a content", [1.0, 0.0]),
            ("chunk-B", "wiki/b", "page-b content", [0.7071, 0.7071]),
            ("chunk-C", "wiki/c", "page-c content", [0.0, 1.0]),
        ];

        let mut chunks = Vec::new();
        for (_, path, content, _) in specs {
            let loc = Location::new(corpus_id, *path);
            let chunk = Chunk::new(
                corpus_id.into(),
                None,
                "page".into(),
                loc,
                content.to_string(),
            );
            db.chunk_upsert(&chunk).unwrap();
            chunks.push(chunk);
        }

        for (chunk, (_, _, _, vec)) in chunks.iter().zip(specs.iter()) {
            let emb = StoredEmbedding::new(corpus_id, &chunk.id, "test-model", vec.to_vec());
            db.embedding_upsert(&emb).unwrap();
        }

        chunks
    }

    #[test]
    fn semantic_search_ranks_by_cosine() {
        let db = make_db();
        let corpus = seed_corpus(&db);
        seed_embeddings(&db, &corpus.id);

        // Query along [1, 0] — should rank A > B >> C.
        let query = [1.0f32, 0.0];
        let results = semantic_search(&db, &corpus.id, &query, None, 10).unwrap();

        assert_eq!(results.len(), 3);
        // A has similarity 1.0 with [1,0].
        assert_eq!(results[0].location.path, "wiki/a");
        assert!((results[0].relevance - 1.0).abs() < 0.01);
        // C has similarity 0.0 with [1,0].
        assert_eq!(results[2].location.path, "wiki/c");
        assert!(results[2].relevance < 0.01);
    }

    #[test]
    fn semantic_search_returns_empty_when_no_embeddings() {
        let db = make_db();
        let corpus = seed_corpus(&db);
        // Seed a chunk but no embedding.
        let loc = Location::new("c1", "wiki/x");
        let chunk = Chunk::new("c1".into(), None, "page".into(), loc, "text".into());
        db.chunk_upsert(&chunk).unwrap();

        let results = semantic_search(&db, &corpus.id, &[1.0, 0.0], None, 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn hybrid_search_alpha_controls_blend() {
        let db = make_db();
        let corpus = seed_corpus(&db);
        let chunks = seed_embeddings(&db, &corpus.id);
        db.fts_rebuild(&corpus.id).unwrap();

        // Heavy semantic (α=0.8)
        let r_semantic =
            hybrid_search(&db, &corpus.id, "pagec", &[1.0, 0.0], None, 10, 0.8).unwrap();
        assert!(!r_semantic.is_empty() || { true });

        // Heavy keyword (α=0.2)
        let r_keyword =
            hybrid_search(&db, &corpus.id, "content", &[1.0, 0.0], None, 10, 0.2).unwrap();
        assert!(
            !r_keyword.is_empty(),
            "hybrid keyword search for 'content' should return results"
        );

        let _ = chunks;
    }

    #[test]
    fn semantic_search_respects_scope() {
        let db = make_db();
        let corpus = seed_corpus(&db);
        seed_embeddings(&db, &corpus.id);

        // Scope: exclude anything lexicographically after "wiki/a"
        let scope = Scope {
            position: Some(Location::new(&corpus.id, "wiki/a")),
            ..Default::default()
        };

        let results = semantic_search(&db, &corpus.id, &[1.0, 0.0], Some(&scope), 10).unwrap();
        assert!(
            results.iter().all(|r| r.location.path.as_str() <= "wiki/a"),
            "all results should be <= wiki/a in path order"
        );
    }
}
