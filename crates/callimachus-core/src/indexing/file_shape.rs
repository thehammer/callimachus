//! File-shape hashing for the Layer-2 cache.
//!
//! The *shape* of a file is the set of entity IDs it declares. The Layer-2
//! cache keys purpose/contract artifacts off this shape rather than off raw
//! content, because the empirical context-leak finding (toy experiments,
//! 2026-05-29) showed that an entity's LLM-derived purpose depends on the
//! whole surrounding file, not just the entity's own bytes. Keying off the
//! entity-id-list means:
//!
//! * editing an entity's body (without adding/removing/renaming any entity)
//!   keeps the same shape hash → the cached purpose is reused, eliminating the
//!   run-to-run nondeterminism the finding documented; and
//! * adding, removing, or renaming an entity changes the shape hash → every
//!   artifact derived against that file is re-derived.

use std::collections::HashMap;

use sha2::{Digest, Sha256};

use crate::types::Entity;

/// Compute the file-shape hash and its canonical entity-id-list JSON.
///
/// Returns `(sha256_hex(sorted(unique(entity_ids))), json_array_string)`.
/// IDs are de-duplicated and sorted so the result is independent of insertion
/// order.
pub fn file_shape_hash(entity_ids: &[String]) -> (String, String) {
    let mut ids: Vec<&str> = entity_ids.iter().map(String::as_str).collect();
    ids.sort_unstable();
    ids.dedup();

    let mut hasher = Sha256::new();
    for id in &ids {
        hasher.update(id.as_bytes());
        hasher.update(b"\n");
    }
    let hash = hex::encode(hasher.finalize());
    let json = serde_json::to_string(&ids).unwrap_or_else(|_| "[]".to_string());
    (hash, json)
}

/// Build a `file_location_uri -> file_shape_hash` map from a set of entities.
///
/// Entities are grouped by their `first_location` URI (for code corpora, the
/// file chunk's location URI). This is the authoritative shape used by the
/// Layer-2 passes for their cache keys — computed from live entity state so it
/// reflects entities created by any pass, not only the structural ones.
pub fn file_shapes_by_uri(entities: &[Entity]) -> HashMap<String, String> {
    let mut ids_by_uri: HashMap<String, Vec<String>> = HashMap::new();
    for entity in entities {
        if let Some(loc) = entity.first_location.as_ref() {
            ids_by_uri
                .entry(loc.uri.clone())
                .or_default()
                .push(entity.id.clone());
        }
    }
    ids_by_uri
        .into_iter()
        .map(|(uri, ids)| (uri, file_shape_hash(&ids).0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_independent() {
        let a = file_shape_hash(&["b".into(), "a".into(), "c".into()]);
        let b = file_shape_hash(&["c".into(), "b".into(), "a".into()]);
        assert_eq!(a.0, b.0);
        assert_eq!(a.1, r#"["a","b","c"]"#);
    }

    #[test]
    fn dedups() {
        let a = file_shape_hash(&["a".into(), "a".into(), "b".into()]);
        let b = file_shape_hash(&["a".into(), "b".into()]);
        assert_eq!(a.0, b.0);
    }

    #[test]
    fn changes_on_add_remove_rename() {
        let base = file_shape_hash(&["a".into(), "b".into()]).0;
        assert_ne!(base, file_shape_hash(&["a".into(), "b".into(), "c".into()]).0); // add
        assert_ne!(base, file_shape_hash(&["a".into()]).0); // remove
        assert_ne!(base, file_shape_hash(&["a".into(), "b2".into()]).0); // rename
    }

    #[test]
    fn empty_is_stable() {
        let (h, j) = file_shape_hash(&[]);
        assert_eq!(j, "[]");
        assert_eq!(h, file_shape_hash(&[]).0);
    }
}
