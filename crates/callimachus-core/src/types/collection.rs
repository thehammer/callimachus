use serde::{Deserialize, Serialize};

/// The semantic kind of a collection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CollectionKind {
    /// An ordered set of corpora that form a series (e.g. book trilogy).
    Series,
    /// A catalog grouping multiple series or standalone corpora.
    Catalog,
    /// A broad domain grouping across publishers or organizations.
    Domain,
    /// A workspace grouping of related projects (e.g. code + docs).
    Workspace,
    /// Any user-defined kind.
    Other(String),
}

impl CollectionKind {
    pub fn as_str(&self) -> &str {
        match self {
            CollectionKind::Series => "series",
            CollectionKind::Catalog => "catalog",
            CollectionKind::Domain => "domain",
            CollectionKind::Workspace => "workspace",
            CollectionKind::Other(s) => s.as_str(),
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        match s {
            "series" => Self::Series,
            "catalog" => Self::Catalog,
            "domain" => Self::Domain,
            "workspace" => Self::Workspace,
            other => Self::Other(other.to_string()),
        }
    }
}

impl std::fmt::Display for CollectionKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether a collection member is a corpus or a nested collection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemberType {
    Corpus,
    Collection,
}

impl MemberType {
    pub fn as_str(&self) -> &str {
        match self {
            MemberType::Corpus => "corpus",
            MemberType::Collection => "collection",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "corpus" => Some(Self::Corpus),
            "collection" => Some(Self::Collection),
            _ => None,
        }
    }
}

/// A direct member of a collection (corpus or nested collection).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionMember {
    pub member_id: String,
    pub member_type: MemberType,
}

/// A named, recursive group of corpora and/or other collections.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Collection {
    pub id: String,
    pub name: String,
    pub kind: CollectionKind,
    pub created_at: String,
    pub members: Vec<CollectionMember>,
}

impl Collection {
    pub fn new(id: impl Into<String>, name: impl Into<String>, kind: CollectionKind) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            kind,
            created_at: chrono::Utc::now().to_rfc3339(),
            members: vec![],
        }
    }
}
