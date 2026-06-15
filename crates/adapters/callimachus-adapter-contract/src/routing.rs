//! Static routing signals an adapter can supply for model-tier selection.
//!
//! The router that consumes these (`ModelTierRouter`) lives in
//! `callimachus-core`; only the input struct is part of the adapter contract,
//! since adapters populate it from their own static analysis.

/// Flat set of signals fed to `ModelTierRouter::route`.
///
/// Populated by each pass from the entity's static analysis results and graph
/// degree counts. Non-code adapters fill in zeros/false; the router degrades
/// gracefully to `cfg.default`.
#[derive(Debug, Default, Clone)]
pub struct RoutingInputs {
    /// Entity contains an `unsafe` block.
    pub has_unsafe: bool,
    /// Return type is `Result<…>`.
    pub is_fallible: bool,
    /// Entity is `pub` at the function/impl level.
    pub is_public: bool,
    /// First parameter is `&mut self`.
    pub is_mutating: bool,
    /// Count of `.unwrap()` / `.expect(…)` calls in the body.
    pub panic_call_count: u32,
    /// True when debt-marker comments (FIXME/HACK/TODO) are present.
    pub has_debt_markers: bool,
    /// Approximate body line count.
    pub body_lines: u32,
    /// Entity kind, e.g. `"function"`, `"class"`, `"module"`.
    pub kind: String,
    /// Number of edges pointing *into* this entity (in-degree).
    pub in_degree: u32,
    /// Number of edges pointing *out of* this entity (out-degree).
    pub out_degree: u32,
}
