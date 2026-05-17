use crate::types::location::Location;
use serde::{Deserialize, Serialize};

/// A constraint applied to any read or search operation.
/// All fields are optional — an empty Scope means unrestricted.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Scope {
    /// Treat the corpus as ending at this location (spoiler control for books).
    /// Hard exclude: no results from chunks after this position.
    pub position: Option<Location>,
    /// Only include chunks whose path matches one of these globs/regexes.
    pub include: Option<Vec<String>>,
    /// Exclude chunks whose path matches one of these globs/regexes.
    pub exclude: Option<Vec<String>>,
    /// For git-backed corpora: restrict to this branch or commit.
    pub branch: Option<String>,
    /// Adapter-defined tags to filter by.
    pub tags: Option<Vec<String>>,
}

impl Scope {
    pub fn is_empty(&self) -> bool {
        self.position.is_none()
            && self.include.is_none()
            && self.exclude.is_none()
            && self.branch.is_none()
            && self.tags.is_none()
    }
}
