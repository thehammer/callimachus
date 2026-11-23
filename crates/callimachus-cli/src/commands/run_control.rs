//! Run control for the LLM passes: entity subsets, call/token/time caps and the
//! no-LLM cost estimate. Shared by `calli index` and `calli ingest`.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use callimachus_core::indexing::{
    EntityFilter, IndexOptions, RunCaps,
    pipeline::{IndexResult, PassEstimate},
};
use callimachus_llm::{
    EstimatingProvider, LlmProvider, ProviderConfig,
    estimating::{API_CALL_OVERHEAD_TOKENS, CLAUDE_CODE_CALL_OVERHEAD_TOKENS},
    pricing::estimate_cost_usd,
};

use crate::{config::GlobalConfig, output::Table};

/// Assumed duration of one API call when estimating wall time.
const API_SECS_PER_CALL: u64 = 5;
const DEFAULT_API_CONCURRENCY: usize = 64;
const DEFAULT_CALLS_PER_MINUTE: u32 = 10;

/// The run-control CLI flags, as given.
#[derive(Debug, Clone, Default)]
pub struct RunControl {
    /// `--entities` specs (repeatable; OR-ed together).
    pub entities: Vec<String>,
    pub max_calls: Option<u64>,
    pub max_input_tokens: Option<u64>,
    /// `--stop-after`, e.g. `90m`, `4h`, `1h30m`.
    pub stop_after: Option<String>,
    pub estimate: bool,
    /// `--grounding none|facts|full`; absent = the corpus default.
    pub grounding: Option<String>,
}

impl RunControl {
    /// Copy the flags onto `opts`. Graph clauses are resolved by the passes
    /// once the entities exist.
    pub fn apply(&self, opts: &mut IndexOptions) -> Result<()> {
        opts.entity_filter = EntityFilter::parse(&self.entities).map_err(|e| anyhow!(e))?;
        opts.caps = RunCaps {
            max_calls: self.max_calls,
            max_input_tokens: self.max_input_tokens,
            stop_after: self.stop_after.as_deref().map(parse_duration).transpose()?,
        };
        opts.estimate = self.estimate;
        opts.grounding = self
            .grounding
            .as_deref()
            .map(str::parse)
            .transpose()
            .map_err(|e: String| anyhow!(e))?;
        Ok(())
    }
}

/// Parse `90m`, `4h`, `1h30m`, `45s` (units `h`, `m`, `s`; at least one).
pub fn parse_duration(s: &str) -> Result<Duration> {
    let bad = || anyhow!("invalid duration '{s}' (examples: 90m, 4h, 1h30m)");
    let mut total = 0u64;
    let mut digits = String::new();
    let mut parts = 0;
    for c in s.trim().chars() {
        if c.is_ascii_digit() {
            digits.push(c);
            continue;
        }
        let n: u64 = digits.parse().map_err(|_| bad())?;
        digits.clear();
        total += n * match c {
            'h' => 3600,
            'm' => 60,
            's' => 1,
            _ => return Err(bad()),
        };
        parts += 1;
    }
    if !digits.is_empty() || parts == 0 {
        return Err(bad());
    }
    Ok(Duration::from_secs(total))
}

/// How an estimate run turns tokens into time and money.
pub struct EstimateProfile {
    overhead: u64,
    calls_per_minute: Option<u32>,
    concurrency: usize,
    /// Set for the API provider only: the model to price.
    usd_model: Option<String>,
}

impl EstimateProfile {
    pub fn for_provider(
        cfg: &ProviderConfig,
        config: &GlobalConfig,
        concurrency: Option<usize>,
    ) -> Self {
        let concurrency = concurrency.unwrap_or(DEFAULT_API_CONCURRENCY);
        match cfg {
            ProviderConfig::ClaudeCode {
                calls_per_minute, ..
            } => Self {
                overhead: CLAUDE_CODE_CALL_OVERHEAD_TOKENS,
                calls_per_minute: Some(calls_per_minute.unwrap_or(DEFAULT_CALLS_PER_MINUTE)),
                concurrency,
                usd_model: None,
            },
            ProviderConfig::AnthropicApi { model, .. } => Self {
                overhead: API_CALL_OVERHEAD_TOKENS,
                calls_per_minute: None,
                concurrency,
                usd_model: Some(
                    model
                        .clone()
                        .or_else(|| config.llm.model.clone())
                        .unwrap_or_else(|| "claude-sonnet".to_string()),
                ),
            },
            ProviderConfig::DryRun => Self {
                overhead: API_CALL_OVERHEAD_TOKENS,
                calls_per_minute: None,
                concurrency,
                usd_model: None,
            },
        }
    }

    /// Wrap the real provider so prompts are rendered but never sent.
    pub fn provider(&self, real: Arc<dyn LlmProvider>) -> Arc<dyn LlmProvider> {
        Arc::new(EstimatingProvider::new(real, self.overhead))
    }

    fn wall_secs(&self, calls: u64) -> u64 {
        match self.calls_per_minute {
            Some(cpm) => (calls * 60).div_ceil(u64::from(cpm.max(1))),
            None => calls.div_ceil(self.concurrency.max(1) as u64) * API_SECS_PER_CALL,
        }
    }
}

/// Print the per-pass estimate table.
pub fn print_estimate(result: &IndexResult, profile: &EstimateProfile, summaries_present: bool) {
    let est = &result.estimates;
    if est.is_empty() {
        println!("No LLM passes selected: nothing to estimate.");
        return;
    }
    let with_usd = profile.usd_model.is_some();
    let mut headers = vec![
        "PASS",
        "ENTITIES",
        "CALLS",
        "EST_INPUT_TOK",
        "EST_OUTPUT_TOK",
        "EST_TIME",
    ];
    if with_usd {
        headers.push("EST_USD");
    }
    let mut table = Table::new(headers);
    let mut total = PassEstimate::default();
    for e in est {
        table.add_row(estimate_row(e, profile));
        total.entities += e.entities;
        total.calls += e.calls;
        total.input_tokens += e.input_tokens;
        total.output_tokens += e.output_tokens;
    }
    total.pass = "total".to_string();
    table.add_row(estimate_row(&total, profile));
    table.print();

    println!("Estimates only: nothing was sent and no artifacts were written.");
    if !result.estimate_skipped.is_empty() {
        println!(
            "Skipped non-LLM passes: {}",
            result.estimate_skipped.join(", ")
        );
    }
    let needs_summaries = est
        .iter()
        .any(|e| e.pass == "purpose" || e.pass == "contract");
    if needs_summaries && !summaries_present {
        println!("(summaries not yet present: purpose estimate is a lower bound)");
    }
}

fn estimate_row(e: &PassEstimate, profile: &EstimateProfile) -> Vec<String> {
    let mut row = vec![
        e.pass.clone(),
        e.entities.to_string(),
        e.calls.to_string(),
        e.input_tokens.to_string(),
        e.output_tokens.to_string(),
        format_secs(profile.wall_secs(e.calls)),
    ];
    if let Some(model) = &profile.usd_model {
        row.push(format!(
            "${:.2}",
            estimate_cost_usd(model, e.input_tokens, e.output_tokens)
        ));
    }
    row
}

fn format_secs(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m{:02}s", secs / 60, secs % 60),
        _ => format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60),
    }
}

/// `--estimate` cannot be combined with walking history: each commit would
/// need its own estimate.
pub fn reject_estimate_with_history(control: &RunControl, with_history: bool) -> Result<()> {
    if control.estimate && with_history {
        bail!("--estimate cannot be combined with --with-history");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("90m").unwrap(), Duration::from_secs(5400));
        assert_eq!(parse_duration("4h").unwrap(), Duration::from_secs(14400));
        assert_eq!(parse_duration("1h30m").unwrap(), Duration::from_secs(5400));
        assert_eq!(parse_duration("45s").unwrap(), Duration::from_secs(45));
        for bad in ["", "90", "m", "1x", "h30"] {
            assert!(parse_duration(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn wall_time_uses_rate_limit_or_concurrency() {
        let cc = EstimateProfile {
            overhead: 700,
            calls_per_minute: Some(10),
            concurrency: 64,
            usd_model: None,
        };
        assert_eq!(cc.wall_secs(30), 180);
        let api = EstimateProfile {
            overhead: 0,
            calls_per_minute: None,
            concurrency: 10,
            usd_model: Some("sonnet".into()),
        };
        assert_eq!(api.wall_secs(25), 15);
    }
}
