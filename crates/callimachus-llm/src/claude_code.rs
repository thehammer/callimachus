use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::Semaphore;

use crate::{
    error::{LlmError, Result},
    provider::{CompletionRequest, CompletionResponse, LlmProvider, ProviderUsage},
    rate_limit::RateLimiter,
};

const DEFAULT_TIMEOUT_SECS: u64 = 120;
const DEFAULT_CALLS_PER_MINUTE: u32 = 10;

/// Model used by the lean path when neither the request nor the config names one.
const LEAN_DEFAULT_MODEL: &str = "sonnet";

/// Minimal system prompt for lean mode; replaces the CLI's large default.
const SYSTEM: &str = "You are a precise analyst inside an indexing tool. \
Follow the user's output format exactly. Do not use tools.";

#[derive(Debug)]
pub struct ClaudeCodeProvider {
    claude_bin: PathBuf,
    default_model: Option<String>,
    effort: Option<String>,
    extra_args: Vec<String>,
    lean: bool,
    timeout_secs: u64,
    semaphore: Arc<Semaphore>,
    rate_limiter: RateLimiter,
    usage: Arc<Mutex<ProviderUsage>>,
}

impl ClaudeCodeProvider {
    /// Auto-detect `claude` binary. Checks `CLAUDE_BIN` env var, then PATH.
    /// Returns `LlmError::NoProvider` if not found.
    pub fn auto_detect() -> Result<Self> {
        let bin = find_claude_bin().ok_or(LlmError::NoProvider)?;
        Ok(Self::new(bin, None, None, None))
    }

    pub fn new(
        claude_bin: PathBuf,
        model: Option<String>,
        timeout_secs: Option<u64>,
        calls_per_minute: Option<u32>,
    ) -> Self {
        tracing::info!("[pipeline] provider = claude-code (no API key)");
        // TODO(embedder-fingerprint): when a dedicated embedder provider is wired,
        // log its key fingerprint here too via key_fingerprint() (see anthropic.rs).
        let cpm = calls_per_minute.unwrap_or(DEFAULT_CALLS_PER_MINUTE);
        ClaudeCodeProvider {
            claude_bin,
            default_model: model,
            effort: None,
            extra_args: Vec::new(),
            lean: true,
            timeout_secs: timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS),
            semaphore: Arc::new(Semaphore::new(1)),
            rate_limiter: RateLimiter::new(cpm),
            usage: Arc::new(Mutex::new(ProviderUsage::default())),
        }
    }

    /// Lean mode (default) trims the CLI's fixed per-call overhead.
    pub fn with_lean(mut self, lean: bool) -> Self {
        self.lean = lean;
        self
    }

    pub fn with_effort(mut self, effort: Option<String>) -> Self {
        self.effort = effort;
        self
    }

    pub fn with_extra_args(mut self, extra_args: Vec<String>) -> Self {
        self.extra_args = extra_args;
        self
    }

    /// Log the effective configuration once (call after the builder methods).
    pub fn log_config(&self) {
        tracing::info!(
            "[pipeline] claude-code: lean={} model={} effort={}",
            self.lean,
            self.resolve_model(None)
                .as_deref()
                .unwrap_or("(cli default)"),
            self.effort.as_deref().unwrap_or("(cli default)"),
        );
    }

    fn resolve_model(&self, req_model: Option<&str>) -> Option<String> {
        req_model
            .map(str::to_string)
            .or_else(|| self.default_model.clone())
            .or_else(|| self.lean.then(|| LEAN_DEFAULT_MODEL.to_string()))
    }

    /// Build the CLI argv for one request.
    fn build_args(&self, req: &CompletionRequest) -> Vec<String> {
        let mut args: Vec<String> = vec!["--print".into()];
        let model = self.resolve_model(req.model.as_deref());
        if self.lean {
            args.extend(
                [
                    "--output-format",
                    "json",
                    "--no-session-persistence",
                    "--safe-mode",
                    "--strict-mcp-config",
                    "--tools",
                    "",
                    "--system-prompt",
                    SYSTEM,
                ]
                .map(String::from),
            );
        }
        if let Some(m) = model {
            args.extend(["--model".into(), m]);
        }
        if !self.lean {
            args.extend(["--output-format".into(), "json".into()]);
        }
        if let Some(e) = &self.effort {
            args.extend(["--effort".into(), e.clone()]);
        }
        args.extend(self.extra_args.iter().cloned());
        args
    }
}

fn find_claude_bin() -> Option<PathBuf> {
    // 1. Explicit env override.
    if let Ok(val) = std::env::var("CLAUDE_BIN") {
        let p = PathBuf::from(val);
        if p.is_file() {
            return Some(p);
        }
    }

    // 2. Walk PATH.
    let path_var = std::env::var("PATH").ok()?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join("claude");
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    None
}

/// The CLI's `--output-format json` result object.
#[derive(Debug, Default)]
struct CliResult {
    text: String,
    is_error: bool,
    input_tokens: u64,
    output_tokens: u64,
    cost_usd: f64,
}

/// Parse stdout as the CLI's JSON result object. `None` if it isn't one
/// (e.g. an old CLI printing plain text).
fn parse_cli_result(stdout: &str) -> Option<CliResult> {
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    let obj = v.as_object()?;
    if !obj.contains_key("result") && !obj.contains_key("is_error") && !obj.contains_key("usage") {
        return None;
    }
    let tok = |key: &str| {
        obj.get("usage")
            .and_then(|u| u.get(key))
            .and_then(|n| n.as_u64())
            .unwrap_or(0)
    };
    Some(CliResult {
        text: obj
            .get("result")
            .and_then(|r| r.as_str())
            .unwrap_or_default()
            .to_string(),
        is_error: obj
            .get("is_error")
            .and_then(|b| b.as_bool())
            .unwrap_or(false),
        input_tokens: tok("input_tokens")
            + tok("cache_creation_input_tokens")
            + tok("cache_read_input_tokens"),
        output_tokens: tok("output_tokens"),
        cost_usd: obj
            .get("total_cost_usd")
            .and_then(|c| c.as_f64())
            .unwrap_or(0.0),
    })
}

/// Classify a failed CLI call from its stdout and stderr. `None` when
/// `status_ok` (a successful call's text is never inspected).
fn classify_failure(status_ok: bool, stdout: &str, stderr: &str) -> Option<LlmError> {
    if status_ok {
        return None;
    }
    let combined = format!("{stdout}\n{stderr}");
    // ASCII lowering keeps byte offsets identical to `combined`.
    let lower = combined.to_ascii_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| lower.contains(n));

    // Only specific usage-limit phrases count as quota exhaustion. A bare
    // "limit reached" or "resets" can come from unrelated errors (context
    // limits, echoed text) and transient rate limits keep the backoff path.
    if has(&[
        "usage limit",
        "5-hour limit",
        "weekly limit",
        "out of extra usage",
    ]) {
        let resets_at = lower.find("resets").and_then(|i| {
            let rest = combined[i + "resets".len()..].lines().next()?;
            let rest: String = rest.trim().trim_end_matches('.').chars().take(80).collect();
            (!rest.is_empty()).then_some(rest)
        });
        let message = [stdout.trim(), stderr.trim()]
            .into_iter()
            .find(|s| !s.is_empty())
            .unwrap_or("usage limit reached")
            .chars()
            .take(500)
            .collect();
        return Some(LlmError::QuotaExhausted { message, resets_at });
    }
    if has(&["rate limit", "too many requests", "overloaded"]) {
        return Some(LlmError::RateLimited {
            retry_after_secs: 60,
        });
    }
    let head = |s: &str| s.trim().chars().take(500).collect::<String>();
    Some(LlmError::Subprocess(format!(
        "claude failed: stdout: {} | stderr: {}",
        head(stdout),
        head(stderr)
    )))
}

#[async_trait::async_trait]
impl LlmProvider for ClaudeCodeProvider {
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse> {
        // Rate limit first, then acquire the concurrency permit.
        self.rate_limiter.acquire().await;
        let _permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|e| LlmError::Subprocess(format!("semaphore closed: {e}")))?;

        // Fresh empty cwd so no project CLAUDE.md is discovered. Kept alive
        // until the child has exited (dropped at end of this function).
        let workdir = tempfile::tempdir()
            .map_err(|e| LlmError::Subprocess(format!("failed to create temp dir: {e}")))?;

        let mut cmd = Command::new(&self.claude_bin);
        cmd.args(self.build_args(&req))
            .current_dir(workdir.path())
            .kill_on_drop(true)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| LlmError::Subprocess(format!("failed to spawn claude: {e}")))?;

        // Write prompt to stdin and close it.
        // BrokenPipe means the child exited before reading all stdin — not an error
        // on its own; wait_with_output() will surface any real failure below.
        if let Some(mut stdin) = child.stdin.take() {
            match stdin.write_all(req.prompt.as_bytes()).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
                Err(e) => return Err(LlmError::Subprocess(format!("stdin write: {e}"))),
            }
        }

        // Await with timeout. On timeout the child is dropped, and
        // `kill_on_drop` terminates it.
        let output = tokio::time::timeout(
            Duration::from_secs(self.timeout_secs),
            child.wait_with_output(),
        )
        .await
        .map_err(|_| LlmError::Timeout {
            timeout_secs: self.timeout_secs,
        })?
        .map_err(|e| LlmError::Subprocess(e.to_string()))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        let parsed = parse_cli_result(&stdout);
        let is_error = parsed.as_ref().is_some_and(|p| p.is_error);
        if !output.status.success() || is_error {
            // For a JSON error result, classify on its message text rather than
            // the whole JSON blob.
            let out_text = match &parsed {
                Some(p) if !p.text.is_empty() => p.text.as_str(),
                _ => stdout.as_str(),
            };
            return Err(classify_failure(false, out_text, &stderr)
                .expect("classify_failure always classifies a failure"));
        }

        let model_used = self
            .resolve_model(req.model.as_deref())
            .unwrap_or_else(|| "claude-code".to_string());

        let (text, input_tokens, output_tokens, cost) = match parsed {
            Some(p) => (p.text, p.input_tokens, p.output_tokens, p.cost_usd),
            // Old CLI / non-JSON output: plain text, no usage info.
            None => (stdout.trim().to_string(), 0, 0, 0.0),
        };

        {
            let mut u = self.usage.lock().unwrap();
            u.calls += 1;
            u.input_tokens += input_tokens;
            u.output_tokens += output_tokens;
            // Subscription calls are not billed: cost_usd stays 0.
            u.equivalent_cost_usd += cost;
        }

        Ok(CompletionResponse {
            text: text.trim().to_string(),
            input_tokens: input_tokens.min(u32::MAX as u64) as u32,
            output_tokens: output_tokens.min(u32::MAX as u64) as u32,
            model_used,
        })
    }

    fn name(&self) -> &str {
        "claude-code"
    }

    fn supports_parallel(&self) -> bool {
        false
    }

    fn usage(&self) -> ProviderUsage {
        self.usage.lock().unwrap().clone()
    }

    fn reset_usage(&self) {
        *self.usage.lock().unwrap() = ProviderUsage::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn write_mock_claude(content: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("claude");
        std::fs::write(&bin, content).unwrap();
        let mut perms = std::fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin, perms).unwrap();
        (dir, bin)
    }

    fn req(prompt: &str) -> CompletionRequest {
        CompletionRequest {
            prompt: prompt.to_string(),
            model: None,
            max_tokens: None,
            chunk_id: None,
            kind: "test".to_string(),
            pass: "test".to_string(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn successful_completion_with_mock_binary() {
        let (_dir, bin) = write_mock_claude("#!/bin/sh\necho 'hello from claude'");
        let p = ClaudeCodeProvider::new(bin, None, None, None);
        let res = p.complete(req("hello")).await.unwrap();
        assert_eq!(res.text, "hello from claude");
        assert_eq!(p.usage().calls, 1);
        assert_eq!(p.usage().cost_usd, 0.0);
    }

    #[tokio::test]
    async fn timeout_returns_timeout_error() {
        let (_dir, bin) = write_mock_claude("#!/bin/sh\nsleep 10");
        let p = ClaudeCodeProvider::new(bin, None, Some(1), None);
        let err = p.complete(req("hello")).await.unwrap_err();
        assert!(matches!(err, LlmError::Timeout { timeout_secs: 1 }));
    }

    #[tokio::test]
    async fn rate_limit_in_output_returns_error() {
        let (_dir, bin) = write_mock_claude("#!/bin/sh\necho 'Error: rate limit exceeded'\nexit 1");
        let p = ClaudeCodeProvider::new(bin, None, None, None);
        let err = p.complete(req("hello")).await.unwrap_err();
        assert!(matches!(err, LlmError::RateLimited { .. }));
    }

    #[tokio::test]
    async fn binary_not_found_returns_subprocess_error() {
        let p = ClaudeCodeProvider::new(PathBuf::from("/nonexistent/claude"), None, None, None);
        let err = p.complete(req("hello")).await.unwrap_err();
        assert!(matches!(err, LlmError::Subprocess(_)));
    }

    #[test]
    fn auto_detect_missing_binary_returns_no_provider() {
        // Remove CLAUDE_BIN and use a PATH that has no claude binary.
        // SAFETY: single-threaded test, no concurrent env access.
        let original_path = std::env::var("PATH").unwrap_or_default();
        unsafe {
            std::env::remove_var("CLAUDE_BIN");
            std::env::set_var("PATH", "/nonexistent");
        }
        let err = ClaudeCodeProvider::auto_detect().unwrap_err();
        assert!(matches!(err, LlmError::NoProvider));
        unsafe { std::env::set_var("PATH", original_path) };
    }

    fn provider() -> ClaudeCodeProvider {
        ClaudeCodeProvider::new(PathBuf::from("/bin/true"), None, None, None)
    }

    fn has_pair(args: &[String], flag: &str, value: &str) -> bool {
        args.windows(2).any(|w| w[0] == flag && w[1] == value)
    }

    #[test]
    fn build_args_lean_default() {
        let args = provider().build_args(&req("x"));
        for flag in [
            "--print",
            "--safe-mode",
            "--strict-mcp-config",
            "--no-session-persistence",
            "--system-prompt",
        ] {
            assert!(args.iter().any(|a| a == flag), "missing {flag}: {args:?}");
        }
        assert!(has_pair(&args, "--tools", ""));
        assert!(has_pair(&args, "--output-format", "json"));
        assert!(has_pair(&args, "--system-prompt", SYSTEM));
        assert!(has_pair(&args, "--model", "sonnet"));
    }

    #[test]
    fn build_args_config_overrides_and_extra_last() {
        let p =
            ClaudeCodeProvider::new(PathBuf::from("/bin/true"), Some("haiku".into()), None, None)
                .with_effort(Some("low".into()))
                .with_extra_args(vec!["--foo".into(), "bar".into()]);
        let args = p.build_args(&req("x"));
        assert!(has_pair(&args, "--model", "haiku"));
        assert!(has_pair(&args, "--effort", "low"));
        assert_eq!(&args[args.len() - 2..], ["--foo", "bar"]);

        // A request-level model beats the configured default.
        let mut r = req("x");
        r.model = Some("opus".into());
        assert!(has_pair(&p.build_args(&r), "--model", "opus"));
    }

    #[test]
    fn build_args_not_lean_keeps_plain_argv() {
        let args = provider().with_lean(false).build_args(&req("x"));
        assert!(!args.iter().any(|a| a == "--safe-mode"));
        assert!(!args.iter().any(|a| a == "--model"));
        assert_eq!(args, ["--print", "--output-format", "json"]);
    }

    #[tokio::test]
    async fn json_result_parsed_with_usage() {
        let (_dir, bin) = write_mock_claude(
            r#"#!/bin/sh
cat > /dev/null
echo '{"type":"result","is_error":false,"result":"answer","usage":{"input_tokens":10,"cache_creation_input_tokens":200,"cache_read_input_tokens":400,"output_tokens":7},"total_cost_usd":0.25}'
"#,
        );
        let p = ClaudeCodeProvider::new(bin, None, None, None);
        let res = p.complete(req("hi")).await.unwrap();
        assert_eq!(res.text, "answer");
        assert_eq!(res.input_tokens, 610);
        assert_eq!(res.output_tokens, 7);
        assert_eq!(res.model_used, "sonnet");
        p.complete(req("hi")).await.unwrap();
        let u = p.usage();
        assert_eq!(u.calls, 2);
        assert_eq!(u.input_tokens, 1220);
        assert_eq!(u.output_tokens, 14);
        assert!((u.equivalent_cost_usd - 0.5).abs() < 1e-9);
        assert_eq!(u.cost_usd, 0.0);
    }

    #[tokio::test]
    async fn non_json_output_falls_back_to_plain_text() {
        let (_dir, bin) = write_mock_claude("#!/bin/sh\necho 'plain words'");
        let p = ClaudeCodeProvider::new(bin, None, None, None).with_lean(false);
        let res = p.complete(req("hi")).await.unwrap();
        assert_eq!(res.text, "plain words");
        assert_eq!(res.input_tokens, 0);
        assert_eq!(res.model_used, "claude-code");
    }

    #[tokio::test]
    async fn json_error_result_is_classified_as_quota() {
        let (_dir, bin) = write_mock_claude(
            r#"#!/bin/sh
echo '{"is_error":true,"result":"5-hour limit reached ∙ resets 3pm (America/Chicago)"}'
"#,
        );
        let p = ClaudeCodeProvider::new(bin, None, None, None);
        match p.complete(req("hi")).await.unwrap_err() {
            LlmError::QuotaExhausted { resets_at, .. } => {
                assert_eq!(resets_at.as_deref(), Some("3pm (America/Chicago)"));
            }
            other => panic!("expected QuotaExhausted, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn child_runs_in_fresh_cwd_with_lean_argv() {
        let out = tempfile::tempdir().unwrap();
        let cwd_file = out.path().join("cwd");
        let args_file = out.path().join("args");
        let script = format!(
            "#!/bin/sh\npwd > '{}'\nprintf '%s\\n' \"$@\" > '{}'\necho ok\n",
            cwd_file.display(),
            args_file.display()
        );
        let (_dir, bin) = write_mock_claude(&script);
        let p = ClaudeCodeProvider::new(bin, None, None, None);
        p.complete(req("hi")).await.unwrap();
        let cwd = std::fs::read_to_string(&cwd_file).unwrap();
        let test_cwd = std::env::current_dir().unwrap();
        assert_ne!(
            std::fs::canonicalize(cwd.trim()).ok(),
            std::fs::canonicalize(&test_cwd).ok()
        );
        let args = std::fs::read_to_string(&args_file).unwrap();
        assert!(args.contains("--safe-mode"), "args: {args}");
    }

    #[test]
    fn classify_usage_limit_on_stdout_is_quota() {
        let err = classify_failure(
            false,
            "Claude usage limit reached. Your limit resets 7pm (UTC).",
            "warning: bad settings",
        )
        .unwrap();
        match err {
            LlmError::QuotaExhausted { message, resets_at } => {
                assert!(message.contains("usage limit reached"));
                assert_eq!(resets_at.as_deref(), Some("7pm (UTC)"));
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn classify_transient_rate_limit_with_resets_is_not_quota() {
        assert!(matches!(
            classify_failure(false, "rate limit exceeded, resets in 30s", ""),
            Some(LlmError::RateLimited { .. })
        ));
    }

    #[test]
    fn classify_context_limit_is_not_quota() {
        let err = classify_failure(false, "context limit reached", "").unwrap();
        assert!(matches!(err, LlmError::Subprocess(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn json_error_echoing_resets_is_not_quota() {
        let (_dir, bin) = write_mock_claude(
            "#!/bin/sh\necho '{\"is_error\":true,\"result\":\"bad input: the word resets appears\"}'\n",
        );
        let p = ClaudeCodeProvider::new(bin, None, None, None);
        let err = p.complete(req("hi")).await.unwrap_err();
        assert!(matches!(err, LlmError::Subprocess(_)), "got {err:?}");
    }

    #[test]
    fn classify_resets_cut_at_first_newline() {
        let err = classify_failure(
            false,
            "weekly limit reached, resets Mon 9am (UTC)\nsecond line",
            "",
        )
        .unwrap();
        let LlmError::QuotaExhausted { resets_at, .. } = err else {
            panic!("expected quota")
        };
        assert_eq!(resets_at.as_deref(), Some("Mon 9am (UTC)"));
    }

    #[test]
    fn classify_includes_stdout_in_subprocess_error() {
        let err = classify_failure(false, "real cause here", "settings warning").unwrap();
        let LlmError::Subprocess(m) = err else {
            panic!("expected Subprocess")
        };
        assert!(m.contains("real cause here"));
        assert!(m.contains("settings warning"));
    }

    #[test]
    fn classify_rate_limit_and_success() {
        assert!(matches!(
            classify_failure(false, "", "429 Too Many Requests"),
            Some(LlmError::RateLimited {
                retry_after_secs: 60
            })
        ));
        assert!(classify_failure(true, "usage limit resets", "").is_none());
    }

    #[test]
    fn name_and_parallel() {
        let p = ClaudeCodeProvider::new(PathBuf::from("/bin/true"), None, None, None);
        assert_eq!(p.name(), "claude-code");
        assert!(!p.supports_parallel());
    }
}
