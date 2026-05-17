/// Pricing per million tokens, USD. Update as rates change.
pub mod claude_sonnet_4_5 {
    pub const INPUT_PER_M: f64 = 3.00;
    pub const OUTPUT_PER_M: f64 = 15.00;
}

pub fn estimate_cost_usd(model: &str, input_tokens: u64, output_tokens: u64) -> f64 {
    let (input_rate, output_rate) = match model {
        m if m.contains("sonnet") => (
            claude_sonnet_4_5::INPUT_PER_M,
            claude_sonnet_4_5::OUTPUT_PER_M,
        ),
        // Default: use Sonnet rates as a conservative estimate.
        _ => (
            claude_sonnet_4_5::INPUT_PER_M,
            claude_sonnet_4_5::OUTPUT_PER_M,
        ),
    };
    (input_tokens as f64 / 1_000_000.0) * input_rate
        + (output_tokens as f64 / 1_000_000.0) * output_rate
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_zero_for_zero_tokens() {
        assert_eq!(estimate_cost_usd("claude-sonnet-4-5", 0, 0), 0.0);
    }

    #[test]
    fn cost_one_million_input_tokens_sonnet() {
        let cost = estimate_cost_usd("claude-sonnet-4-5", 1_000_000, 0);
        assert!((cost - 3.00).abs() < 1e-9, "got {cost}");
    }

    #[test]
    fn cost_one_million_output_tokens_sonnet() {
        let cost = estimate_cost_usd("claude-sonnet-4-5", 0, 1_000_000);
        assert!((cost - 15.00).abs() < 1e-9, "got {cost}");
    }

    #[test]
    fn cost_mixed_tokens_sonnet() {
        // 500k input @ $3/M = $1.50; 100k output @ $15/M = $1.50 → $3.00
        let cost = estimate_cost_usd("claude-sonnet-4-5", 500_000, 100_000);
        assert!((cost - 3.00).abs() < 1e-9, "got {cost}");
    }

    #[test]
    fn unknown_model_falls_back_to_sonnet_rates() {
        let cost_unknown = estimate_cost_usd("unknown-model", 1_000_000, 0);
        let cost_sonnet = estimate_cost_usd("claude-sonnet-4-5", 1_000_000, 0);
        assert!((cost_unknown - cost_sonnet).abs() < 1e-9);
    }
}
