use serde::{Deserialize, Serialize};

/// Per-entity contract: static signals merged with LLM-inferred semantic data.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EntityContract {
    pub entity_id: String,
    pub corpus_id: String,
    // ── Static signals ──────────────────────────────────────────────────────
    pub is_public: bool,
    pub is_must_use: bool,
    pub is_deprecated: bool,
    pub is_fallible: bool,
    pub is_nullable: bool,
    pub is_mutating: bool,
    pub is_diverging: bool,
    pub has_panic_risk: bool,
    pub has_unsafe: bool,
    pub is_incomplete: bool,
    pub panic_call_count: i64,
    pub debt_markers: Vec<String>,
    // ── LLM-inferred semantics ───────────────────────────────────────────────
    pub assumptions: Vec<String>,
    pub risks: Vec<String>,
    pub intent_gap: Option<String>,
    pub caller_notes: Option<String>,
    // ── Provenance ───────────────────────────────────────────────────────────
    /// The LLM model that generated this artifact.
    pub model: String,
    /// Coarse quality tier: "opus" > "sonnet" > "haiku" > "unknown".
    pub model_tier: String,
    pub generated_at: String,
    /// The manifest `current_version` at which this contract was last written.
    /// `None` for rows that pre-date migration 012.
    #[serde(default)]
    pub derived_at_version: Option<String>,
}
