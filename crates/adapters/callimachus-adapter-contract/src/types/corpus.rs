use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorpusStatus {
    Registered,
    Indexing,
    Ready,
    Error,
}

impl std::fmt::Display for CorpusStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CorpusStatus::Registered => write!(f, "registered"),
            CorpusStatus::Indexing => write!(f, "indexing"),
            CorpusStatus::Ready => write!(f, "ready"),
            CorpusStatus::Error => write!(f, "error"),
        }
    }
}

impl std::str::FromStr for CorpusStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "registered" => Ok(CorpusStatus::Registered),
            "indexing" => Ok(CorpusStatus::Indexing),
            "ready" => Ok(CorpusStatus::Ready),
            "error" => Ok(CorpusStatus::Error),
            other => Err(format!("unknown corpus status: {other}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Corpus {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub source: String,
    /// Adapter-specific configuration, stored as JSON.
    pub config: serde_json::Value,
    pub status: CorpusStatus,
    pub created_at: String,
    pub last_indexed_at: Option<String>,
    /// Pipeline version at which this corpus was last fully indexed.
    /// 0 means indexed before versioning was introduced.
    #[serde(default)]
    pub pipeline_version: u32,
    /// Version reference at which this corpus was last successfully indexed
    /// by Pass::History.  For git-backed code corpora this is `"git:<full-oid>"`;
    /// for others it is `"v1-tree:<hex-digest>"`.  `None` until the first
    /// successful pipeline run that includes Pass::History.
    #[serde(default)]
    pub last_indexed_version: Option<String>,
}

impl Corpus {
    pub fn new(id: String, name: String, kind: String, source: String) -> Self {
        Self {
            id,
            name,
            kind,
            source,
            config: serde_json::Value::Object(Default::default()),
            status: CorpusStatus::Registered,
            created_at: chrono::Utc::now().to_rfc3339(),
            last_indexed_at: None,
            pipeline_version: 0,
            last_indexed_version: None,
        }
    }
}
