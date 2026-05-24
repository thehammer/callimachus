use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SummaryTargetKind {
    Corpus,
    Chunk,
    Entity,
    Range,
}

impl std::fmt::Display for SummaryTargetKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SummaryTargetKind::Corpus => write!(f, "corpus"),
            SummaryTargetKind::Chunk => write!(f, "chunk"),
            SummaryTargetKind::Entity => write!(f, "entity"),
            SummaryTargetKind::Range => write!(f, "range"),
        }
    }
}

impl std::str::FromStr for SummaryTargetKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "corpus" => Ok(SummaryTargetKind::Corpus),
            "chunk" => Ok(SummaryTargetKind::Chunk),
            "entity" => Ok(SummaryTargetKind::Entity),
            "range" => Ok(SummaryTargetKind::Range),
            other => Err(format!("unknown summary target kind: {other}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Summary {
    pub id: String,
    pub corpus_id: String,
    pub target_kind: SummaryTargetKind,
    /// The ID of the target (chunk URI, entity ID, or corpus ID).
    pub target_id: String,
    /// Adapter-defined depth label: "corpus", "chapter", "scene", "function", etc.
    pub depth: String,
    pub text: String,
    /// The LLM model that generated this summary.
    pub model: String,
    /// Coarse quality tier: "opus" > "sonnet" > "haiku" > "unknown".
    pub model_tier: String,
    pub generated_at: String,
    /// The manifest `current_version` at which this summary was last written.
    /// `None` for rows that pre-date migration 012.
    #[serde(default)]
    pub derived_at_version: Option<String>,
}

impl Default for Summary {
    fn default() -> Self {
        Self {
            id: String::new(),
            corpus_id: String::new(),
            target_kind: SummaryTargetKind::Chunk,
            target_id: String::new(),
            depth: String::new(),
            text: String::new(),
            model: String::new(),
            model_tier: String::new(),
            generated_at: String::new(),
            derived_at_version: None,
        }
    }
}
