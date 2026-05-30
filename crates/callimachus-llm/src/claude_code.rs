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

#[derive(Debug)]
pub struct ClaudeCodeProvider {
    claude_bin: PathBuf,
    default_model: Option<String>,
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
        let cpm = calls_per_minute.unwrap_or(DEFAULT_CALLS_PER_MINUTE);
        ClaudeCodeProvider {
            claude_bin,
            default_model: model,
            timeout_secs: timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS),
            semaphore: Arc::new(Semaphore::new(1)),
            rate_limiter: RateLimiter::new(cpm),
            usage: Arc::new(Mutex::new(ProviderUsage::default())),
        }
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

        let mut cmd = Command::new(&self.claude_bin);
        cmd.arg("--print");
        if let Some(ref model) = req.model.or_else(|| self.default_model.clone()) {
            cmd.args(["--model", model]);
        }
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| LlmError::Subprocess(format!("failed to spawn claude: {e}")))?;

        // Write prompt to stdin and close it.
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(req.prompt.as_bytes())
                .await
                .map_err(|e| LlmError::Subprocess(format!("stdin write: {e}")))?;
        }

        // Await with timeout.
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

        // Detect rate-limit signals in output.
        let combined = format!("{stdout}{stderr}").to_lowercase();
        if combined.contains("rate limit") || combined.contains("too many requests") {
            return Err(LlmError::RateLimited {
                retry_after_secs: 60,
            });
        }

        if !output.status.success() {
            return Err(LlmError::Subprocess(format!(
                "claude exited with {}: {stderr}",
                output.status
            )));
        }

        let mut u = self.usage.lock().unwrap();
        u.calls += 1;
        // Tokens not reported by subprocess; cost always 0 for subscription.

        Ok(CompletionResponse {
            text: stdout.trim().to_string(),
            input_tokens: 0,
            output_tokens: 0,
            model_used: "claude-code".to_string(),
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

    #[test]
    fn name_and_parallel() {
        let p = ClaudeCodeProvider::new(PathBuf::from("/bin/true"), None, None, None);
        assert_eq!(p.name(), "claude-code");
        assert!(!p.supports_parallel());
    }
}
