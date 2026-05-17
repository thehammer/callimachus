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

/// Successful tool result envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSuccess<T> {
    pub ok: bool, // always true
    pub data: T,
    pub scope_applied: Scope,
    pub indexed_at: String,
    pub cost_metadata: CostMetadata,
}

impl<T> ToolSuccess<T> {
    pub fn new(data: T) -> Self {
        Self {
            ok: true,
            data,
            scope_applied: Scope::default(),
            indexed_at: chrono::Utc::now().to_rfc3339(),
            cost_metadata: CostMetadata::default(),
        }
    }

    pub fn with_scope(mut self, scope: Scope) -> Self {
        self.scope_applied = scope;
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
