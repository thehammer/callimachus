/// Deterministic entity and sequencing-edge construction from a capture chunk.
///
/// No LLM is used here — all extraction is driven by the chunk's JSON content
/// (which was built deterministically by the chunker).
///
/// Entity model
/// ─────────────
/// Every chunk represents one distinct `(method, normalized_path)` endpoint.
/// The entity ID is `ep:{signature}` (e.g. `ep:GET /connect/.../user/{id}`).
///
/// Edge model
/// ───────────
/// The chunker embeds a `next_signatures` array in each chunk's JSON content.
/// For each entry, a `"precedes"` edge is emitted from the current endpoint
/// to the next, encoding the observed call sequencing.
use callimachus_core::{
    adapter::{ExtractedStructure, LocationRef},
    types::{Chunk, Edge, Entity, Location},
};
use sha2::{Digest, Sha256};

/// Extract one `Entity` and zero-or-more `"precedes"` `Edge`s from a capture chunk.
pub fn extract_structure(chunk: &Chunk) -> anyhow::Result<ExtractedStructure> {
    let content: serde_json::Value = serde_json::from_str(&chunk.content)
        .map_err(|e| anyhow::anyhow!("capture chunk has invalid JSON content: {e}"))?;

    let signature = content["signature"].as_str().unwrap_or("").to_string();
    let call_count = content["call_count"].as_u64().unwrap_or(1) as u32;

    let entity_id = format!("ep:{signature}");

    let mut entity = Entity::new(
        entity_id.clone(),
        chunk.corpus_id.clone(),
        signature.clone(),
        "endpoint".to_string(),
    );
    entity.first_location = Some(chunk.location.clone());
    entity.last_location = Some(chunk.location.clone());
    entity.appearance_count = call_count;
    entity.confidence = 0.95;
    // description filled by extract_with_llm

    // Sequencing edges from `next_signatures`.
    let mut edges: Vec<Edge> = Vec::new();
    if let Some(next_sigs) = content["next_signatures"].as_array() {
        for next_val in next_sigs {
            if let Some(next_sig) = next_val.as_str() {
                let next_entity_id = format!("ep:{next_sig}");
                let edge_id = deterministic_edge_id(
                    &chunk.corpus_id,
                    &entity_id,
                    "precedes",
                    &next_entity_id,
                );
                let mut edge = Edge::new(
                    edge_id,
                    chunk.corpus_id.clone(),
                    entity_id.clone(),
                    next_entity_id,
                    "precedes".to_string(),
                    chunk.location.clone(),
                );
                edge.confidence = 0.7;
                edges.push(edge);
            }
        }
    }

    Ok(ExtractedStructure {
        parent_path: None,
        child_paths: vec![],
        structural_entities: vec![entity],
        structural_edges: edges,
    })
}

/// Stable edge ID derived from corpus + from + kind + to.
fn deterministic_edge_id(corpus_id: &str, from: &str, kind: &str, to: &str) -> String {
    let mut h = Sha256::new();
    h.update(corpus_id.as_bytes());
    h.update(b"\x00");
    h.update(from.as_bytes());
    h.update(b"\x00");
    h.update(kind.as_bytes());
    h.update(b"\x00");
    h.update(to.as_bytes());
    hex::encode(h.finalize())
}

/// Parse a capture location URI back to a [`LocationRef`].
///
/// Scheme: `ep/{METHOD}/{percent-encoded-normalized-path}`
/// Full URI: `calli://{corpus_id}/ep/{METHOD}/...`
pub fn parse_location(uri: &str) -> anyhow::Result<LocationRef> {
    // Try full calli:// URI first.
    if let Ok(loc) = Location::parse(uri) {
        return Ok(LocationRef {
            corpus_id: loc.corpus_id,
            path: loc.path,
        });
    }
    // Fall back: treat as plain path.
    Ok(LocationRef {
        corpus_id: String::new(),
        path: uri.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_chunk(corpus_id: &str, sig: &str, call_count: u32, next_sigs: &[&str]) -> Chunk {
        use crate::normalize::percent_encode;

        let parts: Vec<&str> = sig.splitn(2, ' ').collect();
        let method = parts.first().copied().unwrap_or("GET");
        let path = parts.get(1).copied().unwrap_or("/");

        let next: Vec<serde_json::Value> = next_sigs.iter().map(|s| serde_json::json!(s)).collect();
        let content = serde_json::json!({
            "signature": sig,
            "method": method,
            "path_template": path,
            "observed_paths": [],
            "call_count": call_count,
            "sequence_range": [0, 0],
            "statuses": [200],
            "request_bodies": [],
            "response_samples": [],
            "request_headers_seen": [],
            "content_types": [],
            "next_signatures": next,
        });

        let location_path = format!("ep/{}/{}", method, percent_encode(path));
        Chunk::new(
            corpus_id.to_string(),
            None,
            "endpoint".to_string(),
            Location::new(corpus_id, location_path),
            serde_json::to_string_pretty(&content).unwrap(),
        )
    }

    #[test]
    fn extract_produces_one_entity() {
        let chunk = make_chunk("test", "GET /api/users/{id}", 3, &[]);
        let extracted = extract_structure(&chunk).unwrap();
        assert_eq!(extracted.structural_entities.len(), 1);
        let entity = &extracted.structural_entities[0];
        assert_eq!(entity.id, "ep:GET /api/users/{id}");
        assert_eq!(entity.canonical_name, "GET /api/users/{id}");
        assert_eq!(entity.kind, "endpoint");
        assert_eq!(entity.appearance_count, 3);
        assert!((entity.confidence - 0.95).abs() < 1e-6);
    }

    #[test]
    fn extract_produces_precedes_edges() {
        let chunk = make_chunk(
            "test",
            "GET /api/users",
            1,
            &["POST /api/orders", "GET /api/profile"],
        );
        let extracted = extract_structure(&chunk).unwrap();
        assert_eq!(extracted.structural_edges.len(), 2);
        assert!(
            extracted
                .structural_edges
                .iter()
                .all(|e| e.kind == "precedes")
        );
        let targets: Vec<&str> = extracted
            .structural_edges
            .iter()
            .map(|e| e.to_entity_id.as_str())
            .collect();
        assert!(targets.contains(&"ep:POST /api/orders"));
        assert!(targets.contains(&"ep:GET /api/profile"));
    }

    #[test]
    fn extract_no_edges_when_no_next_sigs() {
        let chunk = make_chunk("test", "GET /api/final", 1, &[]);
        let extracted = extract_structure(&chunk).unwrap();
        assert_eq!(extracted.structural_edges.len(), 0);
    }

    #[test]
    fn edge_ids_are_deterministic() {
        let id1 = deterministic_edge_id("corpus1", "ep:A", "precedes", "ep:B");
        let id2 = deterministic_edge_id("corpus1", "ep:A", "precedes", "ep:B");
        let id3 = deterministic_edge_id("corpus1", "ep:A", "precedes", "ep:C");
        assert_eq!(id1, id2);
        assert_ne!(id1, id3);
    }
}
