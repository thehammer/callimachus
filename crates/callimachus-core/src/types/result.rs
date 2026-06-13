use crate::types::scope::Scope;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostMetadata {
    pub cached: bool,
    pub tokens_used: Option<u32>,
}

impl Default for CostMetadata {
    fn default() -> Self {
        Self {
            cached: true,
            tokens_used: None,
        }
    }
}

/// Per-corpus index freshness stamp, included in every successful tool response.
///
/// `last_indexed_at` is `null` when the corpus has never been indexed.
/// Consumers should use this value — not the envelope-level `indexed_at`
/// (which records query execution time) — to determine how stale a corpus is.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusFreshness {
    pub corpus_id: String,
    pub last_indexed_at: Option<String>,
}

/// Successful tool result envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSuccess<T> {
    pub ok: bool, // always true
    pub data: T,
    pub scope_applied: Scope,
    /// Query execution timestamp. This records **when the query ran**, not when the
    /// corpus was indexed. Use `corpus_freshness[*].last_indexed_at` for index-time
    /// freshness of the source material.
    pub indexed_at: String,
    pub cost_metadata: CostMetadata,
    /// Index freshness for every corpus consulted by this call.
    ///
    /// For corpus-scoped tools this is a single entry; for collection tools it
    /// enumerates every member corpus consulted, including those that contributed
    /// zero results. `last_indexed_at` is `null` when a corpus has never been
    /// successfully indexed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub corpus_freshness: Vec<CorpusFreshness>,
}

impl<T> ToolSuccess<T> {
    pub fn new(data: T) -> Self {
        Self {
            ok: true,
            data,
            scope_applied: Scope::default(),
            indexed_at: chrono::Utc::now().to_rfc3339(),
            cost_metadata: CostMetadata::default(),
            corpus_freshness: Vec::new(),
        }
    }

    pub fn with_scope(mut self, scope: Scope) -> Self {
        self.scope_applied = scope;
        self
    }

    pub fn with_corpus_freshness(mut self, freshness: Vec<CorpusFreshness>) -> Self {
        self.corpus_freshness = freshness;
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolError {
    NotFound {
        #[serde(skip_serializing_if = "Option::is_none")]
        suggestions: Option<Vec<String>>,
    },
    Error {
        code: String,
        message: String,
        retriable: bool,
    },
    Ambiguous {
        candidates: Vec<String>,
    },
    InvalidInput {
        message: String,
    },
}

/// The outer result type returned by all tools.
/// On the wire: `{ "ok": true, "data": {...}, ... }` or `{ "ok": false, "kind": "...", ... }`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolResult<T> {
    Ok(ToolSuccess<T>),
    Err(ToolResultError),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultError {
    pub ok: bool, // always false
    #[serde(flatten)]
    pub error: ToolError,
}

impl<T> ToolResult<T> {
    pub fn ok(data: T) -> Self {
        ToolResult::Ok(ToolSuccess::new(data))
    }

    /// Attach per-corpus freshness stamps to a successful result.
    /// No-ops on error envelopes.
    pub fn with_corpus_freshness(self, freshness: Vec<CorpusFreshness>) -> Self {
        match self {
            ToolResult::Ok(s) => ToolResult::Ok(s.with_corpus_freshness(freshness)),
            err => err,
        }
    }

    pub fn not_found(suggestions: Option<Vec<String>>) -> Self {
        ToolResult::Err(ToolResultError {
            ok: false,
            error: ToolError::NotFound { suggestions },
        })
    }

    pub fn error(code: impl Into<String>, message: impl Into<String>, retriable: bool) -> Self {
        ToolResult::Err(ToolResultError {
            ok: false,
            error: ToolError::Error {
                code: code.into(),
                message: message.into(),
                retriable,
            },
        })
    }

    pub fn invalid_input(message: impl Into<String>) -> Self {
        ToolResult::Err(ToolResultError {
            ok: false,
            error: ToolError::InvalidInput {
                message: message.into(),
            },
        })
    }

    pub fn ambiguous(candidates: Vec<String>) -> Self {
        ToolResult::Err(ToolResultError {
            ok: false,
            error: ToolError::Ambiguous { candidates },
        })
    }
}
