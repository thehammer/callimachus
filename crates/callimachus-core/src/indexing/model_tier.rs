//! Rule-based model tier router for Callimachus indexing passes.
//!
//! Routes each entity to Haiku, Sonnet, or Opus based on static signals
//! (unsafe blocks, fallibility, panic count, debt markers, graph degree, …)
//! so trivial getters go to the cheap tier and security-critical, high-fan-in
//! entities go to the quality tier.
//!
//! The routing table is purely functional — no I/O, fully unit-testable.
//! All policy lives in [`TierConfig`]; the default is `enabled = false`
//! (single-model mode, backward-compatible).

use serde::{Deserialize, Serialize};

// ── Tier enum ────────────────────────────────────────────────────────────────

/// Coarse quality/cost tier for LLM routing.
///
/// The discriminant values are used as array indices (`tier as usize`):
/// Haiku=0, Sonnet=1, Opus=2.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
#[repr(usize)]
pub enum ModelTier {
    Haiku = 0,
    Sonnet = 1,
    Opus = 2,
}

impl std::fmt::Display for ModelTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelTier::Haiku => write!(f, "haiku"),
            ModelTier::Sonnet => write!(f, "sonnet"),
            ModelTier::Opus => write!(f, "opus"),
        }
    }
}

// ── Config ───────────────────────────────────────────────────────────────────

/// Thresholds that promote to Opus.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OpusThresholds {
    /// Minimum inbound-edge count (in-degree) for a fallible entity to escalate to Opus.
    pub min_in_degree_fallible: u32,
    /// Minimum panic/unwrap call count for a public entity to escalate to Opus.
    pub min_panic_count_public: u32,
    /// Minimum out-degree for a module-kind entity to escalate to Opus.
    pub min_module_out_degree: u32,
}

impl Default for OpusThresholds {
    fn default() -> Self {
        Self {
            min_in_degree_fallible: 20,
            min_panic_count_public: 3,
            min_module_out_degree: 15,
        }
    }
}

/// Thresholds that promote to Sonnet.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SonnetThresholds {
    /// Minimum inbound-edge count to escalate to Sonnet.
    pub min_in_degree: u32,
    /// Minimum body line count to escalate to Sonnet.
    pub min_line_count: u32,
}

impl Default for SonnetThresholds {
    fn default() -> Self {
        Self {
            min_in_degree: 10,
            min_line_count: 150,
        }
    }
}

/// Full tier-routing configuration.
///
/// When `enabled = false` (the default), `ModelTierRouter::route` always
/// returns `default` — effectively single-model mode, fully backward-compatible.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TierConfig {
    /// Gate: when false, every entity uses the `default` tier.
    pub enabled: bool,
    /// Tier used for all entities when `enabled = false`, and as the
    /// catch-all when no rule fires.
    pub default: ModelTier,
    /// API model string for the Haiku tier, e.g. `"claude-haiku-4-5"`.
    pub haiku_model: String,
    /// API model string for the Sonnet tier, e.g. `"claude-sonnet-4-5"`.
    pub sonnet_model: String,
    /// API model string for the Opus tier, e.g. `"claude-opus-4-7"`.
    pub opus_model: String,
    /// Thresholds for Opus promotion.
    pub opus_rules: OpusThresholds,
    /// Thresholds for Sonnet promotion.
    pub sonnet_rules: SonnetThresholds,
}

impl Default for TierConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            default: ModelTier::Sonnet,
            haiku_model: "claude-haiku-4-5".to_string(),
            sonnet_model: "claude-sonnet-4-5".to_string(),
            opus_model: "claude-opus-4-7".to_string(),
            opus_rules: OpusThresholds::default(),
            sonnet_rules: SonnetThresholds::default(),
        }
    }
}

// Custom Serialize/Deserialize for ModelTier (maps to/from string).
impl Serialize for ModelTier {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(match self {
            ModelTier::Haiku => "haiku",
            ModelTier::Sonnet => "sonnet",
            ModelTier::Opus => "opus",
        })
    }
}

impl<'de> Deserialize<'de> for ModelTier {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        match s.to_lowercase().as_str() {
            "haiku" => Ok(ModelTier::Haiku),
            "sonnet" => Ok(ModelTier::Sonnet),
            "opus" => Ok(ModelTier::Opus),
            other => Err(serde::de::Error::unknown_variant(
                other,
                &["haiku", "sonnet", "opus"],
            )),
        }
    }
}

// ── Routing inputs ───────────────────────────────────────────────────────────

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

// ── Router ───────────────────────────────────────────────────────────────────

/// Stateless rule-based router.  Construct once per pass run; call `route`
/// for each entity.
pub struct ModelTierRouter<'a> {
    cfg: &'a TierConfig,
}

impl<'a> ModelTierRouter<'a> {
    pub fn new(cfg: &'a TierConfig) -> Self {
        Self { cfg }
    }

    /// Route `inputs` to a tier.  Rules fire in priority order; first match wins.
    ///
    /// ```text
    /// if !enabled                                                    → default
    /// if has_unsafe                                                  → Opus
    /// if in_degree > min_in_degree_fallible && is_fallible           → Opus
    /// if is_public && panic_call_count > min_panic_count_public      → Opus
    /// if kind == "module" && out_degree > min_module_out_degree      → Opus
    /// if is_public && is_fallible                                    → Sonnet
    /// if is_mutating && is_fallible                                  → Sonnet
    /// if panic_call_count > 0                                        → Sonnet
    /// if has_debt_markers                                            → Sonnet
    /// if kind == "class" || kind == "module"                        → Sonnet
    /// if in_degree > min_in_degree                                   → Sonnet
    /// if body_lines > min_line_count                                 → Sonnet
    ///                                                                → Haiku
    /// ```
    pub fn route(&self, inputs: &RoutingInputs) -> ModelTier {
        if !self.cfg.enabled {
            return self.cfg.default;
        }

        let op = &self.cfg.opus_rules;
        let sn = &self.cfg.sonnet_rules;

        // ── Opus rules ───────────────────────────────────────────────────────
        if inputs.has_unsafe {
            return ModelTier::Opus;
        }
        if inputs.in_degree > op.min_in_degree_fallible && inputs.is_fallible {
            return ModelTier::Opus;
        }
        if inputs.is_public && inputs.panic_call_count > op.min_panic_count_public {
            return ModelTier::Opus;
        }
        if inputs.kind == "module" && inputs.out_degree > op.min_module_out_degree {
            return ModelTier::Opus;
        }

        // ── Sonnet rules ─────────────────────────────────────────────────────
        if inputs.is_public && inputs.is_fallible {
            return ModelTier::Sonnet;
        }
        if inputs.is_mutating && inputs.is_fallible {
            return ModelTier::Sonnet;
        }
        if inputs.panic_call_count > 0 {
            return ModelTier::Sonnet;
        }
        if inputs.has_debt_markers {
            return ModelTier::Sonnet;
        }
        if inputs.kind == "class" || inputs.kind == "module" {
            return ModelTier::Sonnet;
        }
        if inputs.in_degree > sn.min_in_degree {
            return ModelTier::Sonnet;
        }
        if inputs.body_lines > sn.min_line_count {
            return ModelTier::Sonnet;
        }

        ModelTier::Haiku
    }

    /// Return the model name string for a given tier.
    pub fn model_name(&self, tier: ModelTier) -> &str {
        match tier {
            ModelTier::Haiku => &self.cfg.haiku_model,
            ModelTier::Sonnet => &self.cfg.sonnet_model,
            ModelTier::Opus => &self.cfg.opus_model,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Returns a TierConfig with routing enabled and default thresholds.
    fn enabled_cfg() -> TierConfig {
        TierConfig {
            enabled: true,
            ..TierConfig::default()
        }
    }

    fn router(cfg: &TierConfig) -> ModelTierRouter<'_> {
        ModelTierRouter::new(cfg)
    }

    // ── disabled ─────────────────────────────────────────────────────────────

    #[test]
    fn disabled_config_returns_default() {
        // Even strong signals like has_unsafe must be ignored when enabled=false.
        let cfg = TierConfig {
            enabled: false,
            default: ModelTier::Sonnet,
            ..TierConfig::default()
        };
        let inputs = RoutingInputs {
            has_unsafe: true,
            is_public: true,
            panic_call_count: 99,
            ..Default::default()
        };
        assert_eq!(router(&cfg).route(&inputs), ModelTier::Sonnet);
    }

    // ── Opus rules ────────────────────────────────────────────────────────────

    #[test]
    fn unsafe_routes_to_opus() {
        let cfg = enabled_cfg();
        let inputs = RoutingInputs {
            has_unsafe: true,
            ..Default::default()
        };
        assert_eq!(router(&cfg).route(&inputs), ModelTier::Opus);
    }

    #[test]
    fn in_degree_fallible_routes_to_opus() {
        // in_degree=21 exceeds min_in_degree_fallible=20, and entity is fallible.
        let cfg = enabled_cfg();
        let inputs = RoutingInputs {
            in_degree: 21,
            is_fallible: true,
            ..Default::default()
        };
        assert_eq!(router(&cfg).route(&inputs), ModelTier::Opus);
    }

    #[test]
    fn in_degree_at_threshold_not_opus() {
        // Rule requires strictly greater-than, so in_degree==20 should NOT fire Opus.
        let cfg = enabled_cfg();
        let inputs = RoutingInputs {
            in_degree: 20,
            is_fallible: true,
            ..Default::default()
        };
        assert_ne!(router(&cfg).route(&inputs), ModelTier::Opus);
    }

    #[test]
    fn public_many_panics_routes_to_opus() {
        // panic_call_count=4 exceeds min_panic_count_public=3, entity is public.
        let cfg = enabled_cfg();
        let inputs = RoutingInputs {
            is_public: true,
            panic_call_count: 4,
            ..Default::default()
        };
        assert_eq!(router(&cfg).route(&inputs), ModelTier::Opus);
    }

    #[test]
    fn module_high_out_degree_routes_to_opus() {
        // out_degree=16 exceeds min_module_out_degree=15.
        let cfg = enabled_cfg();
        let inputs = RoutingInputs {
            kind: "module".to_string(),
            out_degree: 16,
            ..Default::default()
        };
        assert_eq!(router(&cfg).route(&inputs), ModelTier::Opus);
    }

    // ── Sonnet rules ──────────────────────────────────────────────────────────

    #[test]
    fn public_fallible_routes_to_sonnet() {
        let cfg = enabled_cfg();
        let inputs = RoutingInputs {
            is_public: true,
            is_fallible: true,
            ..Default::default()
        };
        assert_eq!(router(&cfg).route(&inputs), ModelTier::Sonnet);
    }

    #[test]
    fn mutating_fallible_routes_to_sonnet() {
        let cfg = enabled_cfg();
        let inputs = RoutingInputs {
            is_mutating: true,
            is_fallible: true,
            ..Default::default()
        };
        assert_eq!(router(&cfg).route(&inputs), ModelTier::Sonnet);
    }

    #[test]
    fn panic_call_count_one_routes_to_sonnet() {
        let cfg = enabled_cfg();
        let inputs = RoutingInputs {
            panic_call_count: 1,
            ..Default::default()
        };
        assert_eq!(router(&cfg).route(&inputs), ModelTier::Sonnet);
    }

    #[test]
    fn debt_markers_routes_to_sonnet() {
        let cfg = enabled_cfg();
        let inputs = RoutingInputs {
            has_debt_markers: true,
            ..Default::default()
        };
        assert_eq!(router(&cfg).route(&inputs), ModelTier::Sonnet);
    }

    #[test]
    fn class_kind_routes_to_sonnet() {
        let cfg = enabled_cfg();
        let inputs = RoutingInputs {
            kind: "class".to_string(),
            ..Default::default()
        };
        assert_eq!(router(&cfg).route(&inputs), ModelTier::Sonnet);
    }

    #[test]
    fn module_kind_routes_to_sonnet() {
        // out_degree=5 is below the Opus threshold (15), so only the Sonnet
        // kind rule fires.
        let cfg = enabled_cfg();
        let inputs = RoutingInputs {
            kind: "module".to_string(),
            out_degree: 5,
            ..Default::default()
        };
        assert_eq!(router(&cfg).route(&inputs), ModelTier::Sonnet);
    }

    #[test]
    fn high_in_degree_routes_to_sonnet() {
        // in_degree=11 exceeds min_in_degree=10 (Sonnet threshold).
        let cfg = enabled_cfg();
        let inputs = RoutingInputs {
            in_degree: 11,
            ..Default::default()
        };
        assert_eq!(router(&cfg).route(&inputs), ModelTier::Sonnet);
    }

    #[test]
    fn high_body_lines_routes_to_sonnet() {
        // body_lines=200 exceeds min_line_count=150.
        let cfg = enabled_cfg();
        let inputs = RoutingInputs {
            body_lines: 200,
            ..Default::default()
        };
        assert_eq!(router(&cfg).route(&inputs), ModelTier::Sonnet);
    }

    // ── Haiku (default) ───────────────────────────────────────────────────────

    #[test]
    fn simple_getter_routes_to_haiku() {
        // No signals set — should fall through all rules to Haiku.
        let cfg = enabled_cfg();
        let inputs = RoutingInputs::default();
        assert_eq!(router(&cfg).route(&inputs), ModelTier::Haiku);
    }

    // ── model_name ────────────────────────────────────────────────────────────

    #[test]
    fn model_name_returns_correct_string() {
        let cfg = enabled_cfg();
        let r = router(&cfg);
        assert_eq!(r.model_name(ModelTier::Haiku), "claude-haiku-4-5");
        assert_eq!(r.model_name(ModelTier::Sonnet), "claude-sonnet-4-5");
        assert_eq!(r.model_name(ModelTier::Opus), "claude-opus-4-7");
    }
}
