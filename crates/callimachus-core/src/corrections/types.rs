use serde::{Deserialize, Serialize};

/// A persisted correction record: an immutable, append-only log entry.
///
/// Exactly one of `corpus_id` or `collection_id` is `Some`:
/// - `corpus_id = Some(...)` for corpus-scoped corrections (Merge, Unmerge, Rename, Alias, EditSummary).
/// - `collection_id = Some(...)` for collection-scoped corrections (EntityLink).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Correction {
    pub id: String,
    pub corpus_id: Option<String>,
    pub collection_id: Option<String>,
    pub kind: CorrectionKind,
    pub applied_at: String, // ISO 8601
}

/// Tagged union of all supported correction operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CorrectionKind {
    Merge {
        entity_a_id: String,
        entity_b_id: String,
        /// The entity id to keep as canonical (must equal entity_a_id or entity_b_id).
        canonical_id: String,
    },
    Unmerge {
        entity_id: String,
        split_by: SplitGranularity,
    },
    Rename {
        entity_id: String,
        new_name: String,
    },
    Alias {
        entity_id: String,
        add: Vec<String>,
        remove: Vec<String>,
    },
    EditSummary {
        target_kind: String, // "chunk" | "entity" | "corpus"
        target_id: String,
        text: String,
    },
    EntityLink {
        /// Corpus containing entity A.
        corpus_a_id: String,
        entity_a_id: String,
        /// Corpus containing entity B (may differ from corpus_a_id).
        corpus_b_id: String,
        entity_b_id: String,
        /// Semantic relationship between A and B.
        kind: EntityLinkKind,
        /// Free-text human note (optional).
        note: Option<String>,
    },
}

impl CorrectionKind {
    pub fn kind_name(&self) -> &'static str {
        match self {
            CorrectionKind::Merge { .. } => "merge",
            CorrectionKind::Unmerge { .. } => "unmerge",
            CorrectionKind::Rename { .. } => "rename",
            CorrectionKind::Alias { .. } => "alias",
            CorrectionKind::EditSummary { .. } => "edit_summary",
            CorrectionKind::EntityLink { .. } => "entity_link",
        }
    }
}

/// Semantic relationship type for cross-corpus entity links.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EntityLinkKind {
    /// Same person/place/thing across corpora (identity equivalence).
    SameAs,
    /// Entity B implements the pattern/concept of entity A.
    Implements,
    /// Entity B is a concrete example of the abstract concept entity A.
    Exemplifies,
    /// Entity B explicitly references entity A.
    References,
    /// Entity B contrasts with entity A.
    Contrasts,
}

impl EntityLinkKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EntityLinkKind::SameAs => "same_as",
            EntityLinkKind::Implements => "implements",
            EntityLinkKind::Exemplifies => "exemplifies",
            EntityLinkKind::References => "references",
            EntityLinkKind::Contrasts => "contrasts",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "same_as" => Some(Self::SameAs),
            "implements" => Some(Self::Implements),
            "exemplifies" => Some(Self::Exemplifies),
            "references" => Some(Self::References),
            "contrasts" => Some(Self::Contrasts),
            _ => None,
        }
    }
}

impl std::fmt::Display for EntityLinkKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Granularity hint for unmerge splits.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitGranularity {
    Scene,
    Chapter,
}
