use serde::{Deserialize, Serialize};

/// Indexing passes, run in order.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pass {
    /// Stream chunks from the adapter and content-address them.
    Chunk,
    /// Parser-driven structural extraction (no LLM).
    Structure,
    /// LLM-driven entity/edge/event extraction.
    Semantic,
    /// LLM-driven alias / entity-deduplication pass (runs after Semantic).
    Aliases,
    /// LLM-driven summarization (bottom-up: scene → chapter → corpus).
    Summarize,
    /// Embedding generation (optional, off by default).
    Embed,
    /// LLM-driven purpose extraction (why an entity exists).
    Purpose,
    /// Static + LLM-driven contract analysis (signals, risks, assumptions).
    Contract,
    /// Corpus-level architectural theme detection (opt-in).
    Theme,
}

impl std::fmt::Display for Pass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Pass::Chunk => write!(f, "chunk"),
            Pass::Structure => write!(f, "structure"),
            Pass::Semantic => write!(f, "semantic"),
            Pass::Aliases => write!(f, "aliases"),
            Pass::Summarize => write!(f, "summarize"),
            Pass::Embed => write!(f, "embed"),
            Pass::Purpose => write!(f, "purpose"),
            Pass::Contract => write!(f, "contract"),
            Pass::Theme => write!(f, "theme"),
        }
    }
}

impl std::str::FromStr for Pass {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "chunk" => Ok(Pass::Chunk),
            "structure" => Ok(Pass::Structure),
            "semantic" => Ok(Pass::Semantic),
            "aliases" => Ok(Pass::Aliases),
            "summarize" => Ok(Pass::Summarize),
            "embed" => Ok(Pass::Embed),
            "purpose" => Ok(Pass::Purpose),
            "contract" => Ok(Pass::Contract),
            "theme" => Ok(Pass::Theme),
            other => Err(format!("unknown pass: {other}")),
        }
    }
}

/// Status of an indexing run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl std::fmt::Display for RunStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunStatus::Running => write!(f, "running"),
            RunStatus::Completed => write!(f, "completed"),
            RunStatus::Failed => write!(f, "failed"),
            RunStatus::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl std::str::FromStr for RunStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "running" => Ok(RunStatus::Running),
            "completed" => Ok(RunStatus::Completed),
            "failed" => Ok(RunStatus::Failed),
            "cancelled" => Ok(RunStatus::Cancelled),
            other => Err(format!("unknown run status: {other}")),
        }
    }
}
