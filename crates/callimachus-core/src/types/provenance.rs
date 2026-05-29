//! Honest-provenance types.
//!
//! [`Provenance`] is the single abstraction passes, the walker, and the storage
//! layer speak when describing *when* an artifact was derived. The
//! `derived_at_kind` / `derived_at_sha` SQL columns (migration 013) are merely
//! its storage encoding.
//!
//! The tagged union has exactly two arms (see the PRD / implementation plan):
//!
//! * [`Provenance::Concrete`] — the artifact's substrate was *proven* to be
//!   touched at this SHA (the diff against the neighbour commit changed it).
//! * [`Provenance::RangePredating`] — the artifact is known to predate this SHA
//!   but its exact most-recent-modification commit is not (yet) known. This is
//!   a one-sided upper bound; refinement only ever narrows it (Q2 in the plan).

use serde::{Deserialize, Serialize};

use crate::error::{CalError, Result};

/// SQL `derived_at_kind` value for [`Provenance::Concrete`].
pub const KIND_CONCRETE: &str = "concrete";
/// SQL `derived_at_kind` value for [`Provenance::RangePredating`].
pub const KIND_RANGE_PREDATING: &str = "range_predating";

/// A tagged-union version stamp. See the module docs.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Provenance {
    /// Substrate proven touched at this SHA.
    Concrete(String),
    /// Known to predate this SHA; exact derivation commit unknown.
    RangePredating(String),
}

impl Provenance {
    /// Construct a [`Provenance::Concrete`].
    pub fn concrete(sha: impl Into<String>) -> Self {
        Provenance::Concrete(sha.into())
    }

    /// Construct a [`Provenance::RangePredating`].
    pub fn range_predating(sha: impl Into<String>) -> Self {
        Provenance::RangePredating(sha.into())
    }

    /// `true` for a [`Provenance::Concrete`] tag.
    pub fn is_concrete(&self) -> bool {
        matches!(self, Provenance::Concrete(_))
    }

    /// `true` for a [`Provenance::RangePredating`] tag.
    pub fn is_range_predating(&self) -> bool {
        matches!(self, Provenance::RangePredating(_))
    }

    /// The SHA carried by either arm.
    pub fn sha(&self) -> &str {
        match self {
            Provenance::Concrete(s) | Provenance::RangePredating(s) => s,
        }
    }

    /// The SQL `derived_at_kind` discriminant for this tag.
    pub fn kind_str(&self) -> &'static str {
        match self {
            Provenance::Concrete(_) => KIND_CONCRETE,
            Provenance::RangePredating(_) => KIND_RANGE_PREDATING,
        }
    }

    /// Encode to the `(derived_at_kind, derived_at_sha)` SQL column pair.
    pub fn to_columns(&self) -> (&'static str, &str) {
        (self.kind_str(), self.sha())
    }

    /// Decode from the `(derived_at_kind, derived_at_sha)` SQL column pair.
    ///
    /// Errors on an unrecognised `kind`.
    pub fn from_columns(kind: &str, sha: &str) -> Result<Self> {
        match kind {
            KIND_CONCRETE => Ok(Provenance::Concrete(sha.to_string())),
            KIND_RANGE_PREDATING => Ok(Provenance::RangePredating(sha.to_string())),
            other => Err(CalError::Other(format!(
                "invalid derived_at_kind: {other:?} (expected {KIND_CONCRETE:?} or {KIND_RANGE_PREDATING:?})"
            ))),
        }
    }

    /// Refine this tag given a *concrete* observation at `observed_sha`.
    ///
    /// Monotonic — provenance never widens:
    /// * [`Provenance::Concrete`] is already maximally specific; returns self.
    /// * [`Provenance::RangePredating`] collapses to `Concrete(observed_sha)`:
    ///   we now have a proven substrate-touch point.
    ///
    /// The caller is responsible for only invoking this when `observed_sha` is
    /// an ancestor-or-equal of the current tag's SHA (the walker's contract);
    /// this method does not — and cannot — verify git ancestry itself.
    pub fn refine(self, observed_sha: &str) -> Provenance {
        match self {
            Provenance::Concrete(_) => self,
            Provenance::RangePredating(_) => Provenance::Concrete(observed_sha.to_string()),
        }
    }

    /// Whether this tag asserts the artifact is present at `target_sha`, given
    /// an ancestry oracle.
    ///
    /// `is_ancestor_or_equal(a, b)` must return `true` iff commit `a` is an
    /// ancestor of (or equal to) commit `b`. The presence predicate (PRD §3):
    /// * `Concrete(x)` is valid at `target` iff `x` is an ancestor-or-equal of
    ///   `target` (the artifact was derived at or before the query point).
    /// * `RangePredating(x)` is valid at `target` iff `target` is an
    ///   ancestor-or-equal of `x` (the query point predates the known upper
    ///   bound, so the artifact — which predates `x` — may be present).
    ///
    /// The full death-aware presence query (combining this with tombstones)
    /// lands in a later PR; this is the per-tag building block.
    pub fn is_valid_at<F>(&self, target_sha: &str, is_ancestor_or_equal: F) -> bool
    where
        F: Fn(&str, &str) -> bool,
    {
        match self {
            Provenance::Concrete(x) => is_ancestor_or_equal(x, target_sha),
            Provenance::RangePredating(x) => is_ancestor_or_equal(target_sha, x),
        }
    }
}

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
