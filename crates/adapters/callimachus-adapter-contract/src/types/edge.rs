use crate::types::location::Location;
use crate::types::provenance::Provenance;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub id: String,
    pub corpus_id: String,
    pub from_entity_id: String,
    pub to_entity_id: String,
    /// Adapter-defined kind: "calls", "extends", "imports", "meets",
    /// "located_in", "mentions", "allied_with", etc.
    pub kind: String,
    pub location: Location,
    pub confidence: f32,
    /// The honest-provenance tag for this edge. `None` for rows that
    /// pre-date migration 013.
    #[serde(default)]
    pub provenance: Option<Provenance>,
    /// Number of distinct source sites within the file that produce this
    /// logical edge. Defaults to 1.
    ///
    /// Edge ids are now deterministic over `(corpus_id, from, kind, to,
    /// origin_scope)`, so N call sites to the same function collapse into
    /// one row with `occurrence_count = N`. On incremental reindex the
    /// cascade deletes stale edges and the extractor re-emits the
    /// fully-aggregated count, which is then stored via overwrite upsert —
    /// so counts are idempotent across reindex runs.
    #[serde(default = "default_occurrence_count")]
    pub occurrence_count: u32,
    /// Whether this edge was derived from production code or test-only code.
    ///
    /// Allowed values: `"production"` | `"test"`.
    ///
    /// For Rust, set structurally: `"test"` when the source node falls inside
    /// a `#[cfg(test)] mod` or a `#[test]`/`#[tokio::test]` function body.
    /// All other languages default to `"production"` (TODO: per-language
    /// test-scope detection).
    ///
    /// Rows predating migration 016 default to `"production"` and re-derive
    /// correct scope on next reindex.
    #[serde(default = "default_origin_scope")]
    pub origin_scope: String,
}

fn default_occurrence_count() -> u32 {
    1
}

fn default_origin_scope() -> String {
    "production".to_string()
}

impl Edge {
    pub fn new(
        id: String,
        corpus_id: String,
        from_entity_id: String,
        to_entity_id: String,
        kind: String,
        location: Location,
    ) -> Self {
        Self {
            id,
            corpus_id,
            from_entity_id,
            to_entity_id,
            kind,
            location,
            confidence: 0.5,
            provenance: None,
            occurrence_count: 1,
            origin_scope: "production".to_string(),
        }
    }
}

impl Default for Edge {
    fn default() -> Self {
        Self::new(
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            Location::default(),
        )
    }
}
