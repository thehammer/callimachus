//! Honest-provenance types for the adapter contract.
//!
//! [`Provenance`] is the single abstraction passes, the walker, and the storage
//! layer speak when describing *when* an artifact was derived. The
//! `derived_at_kind` / `derived_at_sha` SQL columns (migration 013) are merely
//! its storage encoding.

use serde::{Deserialize, Serialize};

/// SQL `derived_at_kind` value for [`Provenance::Concrete`].
pub const KIND_CONCRETE: &str = "concrete";
/// SQL `derived_at_kind` value for [`Provenance::RangePredating`].
pub const KIND_RANGE_PREDATING: &str = "range_predating";

/// A tagged-union version stamp.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Provenance {
    /// Substrate proven touched at this SHA.
    Concrete(String),
    /// Known to predate this SHA; exact derivation commit unknown.
    RangePredating(String),
}

impl Provenance {
    pub fn concrete(sha: impl Into<String>) -> Self {
        Provenance::Concrete(sha.into())
    }

    pub fn range_predating(sha: impl Into<String>) -> Self {
        Provenance::RangePredating(sha.into())
    }

    pub fn is_concrete(&self) -> bool {
        matches!(self, Provenance::Concrete(_))
    }

    pub fn is_range_predating(&self) -> bool {
        matches!(self, Provenance::RangePredating(_))
    }

    pub fn sha(&self) -> &str {
        match self {
            Provenance::Concrete(s) | Provenance::RangePredating(s) => s,
        }
    }

    pub fn kind_str(&self) -> &'static str {
        match self {
            Provenance::Concrete(_) => KIND_CONCRETE,
            Provenance::RangePredating(_) => KIND_RANGE_PREDATING,
        }
    }

    pub fn to_columns(&self) -> (&'static str, &str) {
        (self.kind_str(), self.sha())
    }

    pub fn from_columns(kind: &str, sha: &str) -> anyhow::Result<Self> {
        match kind {
            KIND_CONCRETE => Ok(Provenance::Concrete(sha.to_string())),
            KIND_RANGE_PREDATING => Ok(Provenance::RangePredating(sha.to_string())),
            other => anyhow::bail!(
                "invalid derived_at_kind: {:?} (expected {:?} or {:?})",
                other,
                KIND_CONCRETE,
                KIND_RANGE_PREDATING
            ),
        }
    }

    pub fn refine(self, observed_sha: &str) -> Provenance {
        match self {
            Provenance::Concrete(_) => self,
            Provenance::RangePredating(_) => Provenance::Concrete(observed_sha.to_string()),
        }
    }

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
