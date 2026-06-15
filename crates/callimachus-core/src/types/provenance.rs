//! Honest-provenance types.
//!
//! [`Provenance`] is the single abstraction passes, the walker, and the storage
//! layer speak when describing *when* an artifact was derived. The
//! `derived_at_kind` / `derived_at_sha` SQL columns (migration 013) are merely
//! its storage encoding.
//!
//! The type is defined in `callimachus-adapter-contract` and re-exported here
//! so existing `crate::types::provenance::…` references keep resolving.

pub use callimachus_adapter_contract::types::provenance::{
    KIND_CONCRETE, KIND_RANGE_PREDATING, Provenance,
};

/// Outcome of a [`crate::storage::StorageBackend::refine_provenance`] call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefineOutcome {
    /// The tag was tightened.
    Refined,
    /// The input was no more specific than the current tag; nothing changed.
    Unchanged,
    /// The input would have *widened* the tag; refused (monotonicity).
    RejectedMonotonic,
}

/// A set of artifact identities to archive head→history in one call.
///
/// Used by [`crate::storage::StorageBackend::archive_to_history`]. In this PR
/// the implementation is a naive fan-out over the existing per-artifact
/// `archive_*` methods; the unified single-writer implementation lands in a
/// later PR.
#[derive(Clone, Debug, Default)]
pub struct ArchiveSet {
    pub entity_ids: Vec<String>,
    pub chunk_ids: Vec<String>,
    pub theme_ids: Vec<String>,
    /// Summary target ids (entity id or chunk id the summary describes).
    pub summary_target_ids: Vec<String>,
}

impl ArchiveSet {
    /// `true` if nothing would be archived.
    pub fn is_empty(&self) -> bool {
        self.entity_ids.is_empty()
            && self.chunk_ids.is_empty()
            && self.theme_ids.is_empty()
            && self.summary_target_ids.is_empty()
    }
}

/// Row counts archived by [`crate::storage::StorageBackend::archive_to_history`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ArchiveStats {
    pub entities_archived: u64,
    pub edges_archived: u64,
    pub purposes_archived: u64,
    pub contracts_archived: u64,
    pub blocks_archived: u64,
    pub summaries_archived: u64,
    pub chunks_archived: u64,
    pub themes_archived: u64,
}

/// Cache key for a Layer-2 (LLM-derived) artifact.
///
/// Identity = `(artifact_kind, entity_id, content_hash, file_shape_hash, model,
/// stable_sampling)`. [`Self::cache_key`] hashes these into the
/// `layer2_cache.cache_key` primary key.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Layer2CacheKey {
    /// 'purpose' | 'contract' | 'summary' | 'embedding' | 'theme'.
    pub artifact_kind: String,
    /// `None` for corpus-level artifacts (e.g. themes).
    pub entity_id: Option<String>,
    pub content_hash: String,
    pub file_shape_hash: String,
    pub model: String,
    pub stable_sampling: bool,
}

impl Layer2CacheKey {
    /// The deterministic `layer2_cache.cache_key` primary-key value.
    pub fn cache_key(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(self.artifact_kind.as_bytes());
        hasher.update(b"|");
        hasher.update(self.entity_id.as_deref().unwrap_or("").as_bytes());
        hasher.update(b"|");
        hasher.update(self.content_hash.as_bytes());
        hasher.update(b"|");
        hasher.update(self.file_shape_hash.as_bytes());
        hasher.update(b"|");
        hasher.update(self.model.as_bytes());
        hasher.update(b"|");
        hasher.update(if self.stable_sampling { b"1" } else { b"0" });
        hex::encode(hasher.finalize())
    }
}

/// A row read back from `layer2_cache`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CachedArtifact {
    pub cache_key: String,
    pub artifact_kind: String,
    pub entity_id: Option<String>,
    pub content_hash: String,
    pub file_shape_hash: String,
    pub model: String,
    pub stable_sampling: bool,
    /// Pass-specific JSON payload.
    pub payload: String,
    pub created_at: String,
    pub first_seen_at_sha: String,
    pub hit_count: i64,
}

/// A row read back from `artifact_tombstones`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tombstone {
    pub corpus_id: String,
    /// 'chunk' | 'entity' | 'edge' | 'embedding'.
    pub artifact_kind: String,
    pub artifact_id: String,
    pub provenance: Provenance,
    pub reason: Option<String>,
    pub created_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_round_trip() {
        for p in [
            Provenance::concrete("abc"),
            Provenance::range_predating("def"),
        ] {
            let (kind, sha) = p.to_columns();
            let back = Provenance::from_columns(kind, sha).unwrap();
            assert_eq!(p, back);
        }
    }

    #[test]
    fn from_columns_rejects_unknown_kind() {
        assert!(Provenance::from_columns("bogus", "abc").is_err());
    }

    #[test]
    fn refine_range_to_concrete() {
        let p = Provenance::range_predating("c20");
        assert_eq!(p.refine("c10"), Provenance::concrete("c10"));
    }

    #[test]
    fn refine_concrete_is_noop() {
        let p = Provenance::concrete("c10");
        assert_eq!(p.clone().refine("c5"), p);
    }

    #[test]
    fn is_valid_at_uses_ancestry_oracle() {
        // Linear history c1 -> c2 -> c3 (c1 oldest). a <= b means a is ancestor.
        let order = |s: &str| match s {
            "c1" => 1,
            "c2" => 2,
            "c3" => 3,
            _ => 0,
        };
        let anc = |a: &str, b: &str| order(a) <= order(b);

        // Concrete(c2): valid at c2 and c3 (derived at/before), not at c1.
        let c = Provenance::concrete("c2");
        assert!(c.is_valid_at("c2", anc));
        assert!(c.is_valid_at("c3", anc));
        assert!(!c.is_valid_at("c1", anc));

        // RangePredating(c2): valid at c1 and c2 (query predates bound), not c3.
        let r = Provenance::range_predating("c2");
        assert!(r.is_valid_at("c1", anc));
        assert!(r.is_valid_at("c2", anc));
        assert!(!r.is_valid_at("c3", anc));
    }
}
