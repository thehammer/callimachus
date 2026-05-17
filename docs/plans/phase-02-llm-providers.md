# Phase 2 — LLM provider system

## Context

Phase 1 delivered the workspace scaffold, domain types, SQLite storage, and `calli corpus` CLI
commands. The `callimachus-llm` crate exists but is a single placeholder function.

This phase fully implements `callimachus-llm`: the `LlmProvider` trait and its three
implementations (Anthropic API, Claude Code subscription, dry-run). This is the foundation Phase 3
(indexing) depends on. No indexing logic ships in this phase — only the provider system and its
tests.

Reference: `docs/plans/callimachus-standalone.md §2`.

## Target

- **Repo:** `callimachus`
- **Branch:** `main` (trunk-based)
- **Base:** Phase 1 commit

## Files to change

### `crates/callimachus-llm/Cargo.toml`

Add dependencies:
- `reqwest = { version = "0.12", features = ["json"] }` — HTTP client for Anthropic API
- `tokio = { workspace = true }` — already in workspace deps
- `async-trait = "0.1"` — for async trait methods
- `serde.workspace = true`, `serde_json.workspace = true`

### `crates/callimachus-llm/src/lib.rs`

Replace the placeholder. Re-export:
```
pub use provider::{LlmProvider, CompletionRequest, CompletionResponse, ProviderUsage};
pub use anthropic::AnthropicApiProvider;
pub use claude_code::ClaudeCodeProvider;
pub use dry_run::DryRunProvider;
pub use resolve::{resolve_provider, ProviderConfig};
pub use error::LlmError;
```

### `crates/callimachus-llm/src/error.rs`

```rust
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("rate limited; retry after {retry_after_secs}s")]
    RateLimited { retry_after_secs: u64 },
    #[error("request failed ({status}): {message}")]
    ApiError { status: u16, message: String },
    #[error("subprocess error: {0}")]
    Subprocess(String),
    #[error("timeout after {timeout_secs}s")]
    Timeout { timeout_secs: u64 },
    #[error("no provider configured (set ANTHROPIC_API_KEY or install claude CLI)")]
    NoProvider,
    #[error("{0}")]
    Other(String),
}
pub type Result<T> = std::result::Result<T, LlmError>;
```

### `crates/callimachus-llm/src/provider.rs`

```rust
#[async_trait::async_trait]
pub trait LlmProvider: Send + Sync {
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse>;
    fn name(&self) -> &str;
    /// Whether this provider supports concurrent calls safely.
    fn supports_parallel(&self) -> bool;
    fn usage(&self) -> ProviderUsage;
    fn reset_usage(&self);
}

pub struct CompletionRequest {
    pub prompt: String,
    pub model: Option<String>,
    pub max_tokens: Option<u32>,
    pub chunk_id: Option<String>,  // for logging/attribution only
}

pub struct CompletionResponse {
    pub text: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub model_used: String,
}

#[derive(Debug, Clone, Default)]
pub struct ProviderUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,  // 0.0 for subscription mode
    pub calls: u64,
}
```

### `crates/callimachus-llm/src/pricing.rs`

Token pricing constants for Anthropic models. Named constants only — no logic. Keep in one place so they're easy to update when Anthropic changes rates.

```rust
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
        // Default: use Sonnet rates as a conservative estimate
        _ => (claude_sonnet_4_5::INPUT_PER_M, claude_sonnet_4_5::OUTPUT_PER_M),
    };
    (input_tokens as f64 / 1_000_000.0) * input_rate
        + (output_tokens as f64 / 1_000_000.0) * output_rate
}
```

### `crates/callimachus-llm/src/rate_limit.rs`

A simple token-bucket rate limiter using `tokio::sync::Semaphore` and `tokio::time`.

```rust
pub struct RateLimiter {
    calls_per_minute: u32,
    // internal: semaphore + refill task
}

impl RateLimiter {
    pub fn new(calls_per_minute: u32) -> Self { ... }
    pub async fn acquire(&self) { ... }  // waits if rate exceeded
}
```

### `crates/callimachus-llm/src/anthropic.rs`

`AnthropicApiProvider` using `reqwest`. Reads `ANTHROPIC_API_KEY` from env at construction — returns `LlmError::NoProvider` if missing.

Key details:
- Base URL: `https://api.anthropic.com/v1/messages`
- Default model: `claude-sonnet-4-5` (overridable in constructor or per-request)
- Default `max_tokens`: 2000
- Headers: `x-api-key`, `anthropic-version: 2023-06-01`, `content-type: application/json`
- Retry: up to 3 attempts. 429 → `RateLimited` + exponential backoff (1s, 4s, 16s). 500/529 → retry with 2s backoff. Other 4xx → `ApiError`, no retry.
- Track `input_tokens` and `output_tokens` from the response `usage` object. Use `pricing.rs` to compute `cost_usd`. Both stored in an `Arc<Mutex<ProviderUsage>>` so `usage()` is thread-safe.
- `supports_parallel() → true`
- `name() → "anthropic-api"`

### `crates/callimachus-llm/src/claude_code.rs`

`ClaudeCodeProvider` using `tokio::process::Command`.

Key details:
- Auto-detects `claude` binary: first checks `CLAUDE_BIN` env var, then walks `PATH`. Returns `LlmError::NoProvider` if not found.
- Invocation: `echo "<prompt>" | claude --print [--model <model>]`
  - Actually: spawn with `stdin = Stdio::piped()`, write prompt bytes, await stdout.
- Timeout: configurable (default 120s). Returns `LlmError::Timeout` on expiry.
- Concurrency: enforced at 1 via `Arc<Semaphore>` with permits=1. Callers queue; none are dropped.
- `supports_parallel() → false`
- Usage tracking: `cost_usd` always 0.0. `input_tokens` and `output_tokens` are 0 (not reported by subprocess). `calls` is tracked.
- Rate limiting: configurable `calls_per_minute` cap (default 10) via the shared `RateLimiter`.
- `name() → "claude-code"`

Parse the subprocess output to detect rate-limit messages:
```rust
if stdout.contains("rate limit") || stdout.contains("too many requests") {
    return Err(LlmError::RateLimited { retry_after_secs: 60 });
}
```

### `crates/callimachus-llm/src/dry_run.rs`

`DryRunProvider`: returns canned responses immediately.

- `complete()`: if prompt contains `"entities"` or `"extract"` → return `{"entities":[],"edges":[],"summary_text":"[dry-run]"}`. Otherwise return `"[dry-run]"`.
- `supports_parallel() → true`
- `name() → "dry-run"`
- Zero cost, zero tokens.

### `crates/callimachus-llm/src/resolve.rs`

Provider auto-detection and config-driven construction.

```rust
pub enum ProviderConfig {
    AnthropicApi {
        api_key: Option<String>,  // None = read from ANTHROPIC_API_KEY
        model: Option<String>,
        max_parallel_calls: Option<u32>,
    },
    ClaudeCode {
        claude_bin: Option<PathBuf>,
        model: Option<String>,
        timeout_secs: Option<u64>,
        calls_per_minute: Option<u32>,
    },
    DryRun,
}

/// Auto-detect the best available provider.
/// Priority: ANTHROPIC_API_KEY set → AnthropicApi; claude in PATH → ClaudeCode; error.
pub fn auto_detect() -> Result<ProviderConfig> { ... }

/// Build a boxed provider from a config.
pub fn build(config: ProviderConfig) -> Result<Box<dyn LlmProvider>> { ... }
```

## Tests

All tests in `crates/callimachus-llm/src/` under `#[cfg(test)]`.

- `anthropic.rs`: Mock `reqwest` responses using `wiremock` or `mockito`. Test: successful completion, 429 retry, 500 retry, 4xx no-retry, missing API key error.
- `claude_code.rs`: Test with a mock binary (a shell script that echoes a fixed response). Place it under `tests/fixtures/mock-claude.sh`. Test: successful completion, timeout, rate-limit detection, binary-not-found error.
- `dry_run.rs`: Test canned responses for extraction-shaped vs plain prompts.
- `resolve.rs`: Test auto-detection with env var set, with mock `claude` on PATH, with neither.
- `pricing.rs`: Test cost estimates for known token counts.
- `rate_limit.rs`: Test that `calls_per_minute=2` blocks the third call until a token refills.

Add `wiremock = "0.6"` as a `[dev-dependencies]` entry in the crate's Cargo.toml.

## Acceptance criteria

- `cargo test -p callimachus-llm` passes
- `cargo clippy -p callimachus-llm -- -D warnings` passes
- `AnthropicApiProvider` completes a real prompt when `ANTHROPIC_API_KEY` is set (manual smoke test)
- `ClaudeCodeProvider` completes a real prompt when `claude` is on PATH (manual smoke test)
