//! Git ancestry oracle for tombstone-aware reads.
//!
//! Honest-provenance presence queries (`entity_list_at_sha`, `is_tombstoned_at`)
//! need to decide whether one commit is an ancestor of another. The storage
//! layer has no git handle of its own, so callers pass an [`AncestryReader`]
//! that answers ancestry questions against the corpus's git repository.
//!
//! For corpora *without* an attached git repo (book / wiki adapters) the caller
//! passes `None`; the backend then falls back to **literal SHA equality** — a
//! commit is "present at" exactly the version stamp it was derived at. This is
//! the honest answer when there is no commit graph to reason about.
//!
//! Version stamps in storage are recorded as `git:<oid>` (see the walker). The
//! [`Git2Ancestry`] implementation strips that prefix before parsing the OID, so
//! callers may pass either the bare OID or the `git:`-prefixed version ref.

use std::path::Path;

use git2::{Oid, Repository};

use crate::error::{CalError, Result};

/// Answers "is commit `ancestor` an ancestor-or-equal of commit `descendant`?".
///
/// The contract mirrors [`crate::types::provenance::Provenance::is_valid_at`]'s
/// oracle: `is_ancestor_or_equal(a, b)` is `true` iff `a == b` or `a` is reachable
/// by walking parents from `b`.
///
/// Not bound by `Send + Sync`: an `AncestryReader` is only ever borrowed for the
/// duration of a single storage call, never stored across threads.
pub trait AncestryReader {
    /// `true` iff `ancestor` is an ancestor of, or equal to, `descendant`.
    fn is_ancestor_or_equal(&self, ancestor: &str, descendant: &str) -> bool;
}

/// A [`git2`]-backed [`AncestryReader`] over a corpus's repository.
pub struct Git2Ancestry {
    repo: Repository,
}

impl Git2Ancestry {
    /// Open the repository at `path` for ancestry queries.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let repo = Repository::open(path.as_ref())
            .map_err(|e| CalError::Other(format!("opening git repo for ancestry: {e}")))?;
        Ok(Self { repo })
    }

    /// Parse a version stamp into an [`Oid`], tolerating a leading `git:` prefix.
    fn parse_oid(raw: &str) -> Option<Oid> {
        let stripped = raw.strip_prefix("git:").unwrap_or(raw);
        Oid::from_str(stripped).ok()
    }
}

impl AncestryReader for Git2Ancestry {
    fn is_ancestor_or_equal(&self, ancestor: &str, descendant: &str) -> bool {
        if ancestor == descendant {
            return true;
        }
        let (Some(anc), Some(desc)) = (Self::parse_oid(ancestor), Self::parse_oid(descendant))
        else {
            // Unparseable stamps can't be related by the commit graph; fall back
            // to the equality already checked above (i.e. not related).
            return false;
        };
        if anc == desc {
            return true;
        }
        // graph_descendant_of(commit, ancestor) — true iff `commit` descends from
        // `ancestor` (strictly). Equality is handled above.
        self.repo.graph_descendant_of(desc, anc).unwrap_or(false)
    }
}

/// Resolve an ancestry decision through an optional reader.
///
/// With `Some(reader)` the git commit graph is consulted. With `None` (no
/// attached repo) the honest fallback is **literal equality** — see the module
/// docs.
pub(crate) fn is_ancestor_or_equal(
    ancestry: Option<&dyn AncestryReader>,
    ancestor: &str,
    descendant: &str,
) -> bool {
    match ancestry {
        Some(reader) => reader.is_ancestor_or_equal(ancestor, descendant),
        None => ancestor == descendant,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake reader backed by a linear rank function: `rank(a) <= rank(b)` means
    /// `a` is an ancestor-or-equal of `b`.
    struct LinearAncestry;
    impl AncestryReader for LinearAncestry {
        fn is_ancestor_or_equal(&self, ancestor: &str, descendant: &str) -> bool {
            let rank = |s: &str| match s {
                "c1" => 1,
                "c2" => 2,
                "c3" => 3,
                _ => 0,
            };
            rank(ancestor) <= rank(descendant)
        }
    }

    #[test]
    fn none_falls_back_to_literal_equality() {
        assert!(is_ancestor_or_equal(None, "c1", "c1"));
        assert!(!is_ancestor_or_equal(None, "c1", "c2"));
    }

    #[test]
    fn some_consults_reader() {
        let r = LinearAncestry;
        let r: Option<&dyn AncestryReader> = Some(&r);
        assert!(is_ancestor_or_equal(r, "c1", "c2"));
        assert!(is_ancestor_or_equal(r, "c2", "c2"));
        assert!(!is_ancestor_or_equal(r, "c3", "c2"));
    }
}
