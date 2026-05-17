use std::path::PathBuf;

use crate::{
    anthropic::AnthropicApiProvider,
    claude_code::ClaudeCodeProvider,
    dry_run::DryRunProvider,
    error::{LlmError, Result},
    provider::LlmProvider,
};

#[derive(Debug)]
pub enum ProviderConfig {
    AnthropicApi {
        /// `None` → read from `ANTHROPIC_API_KEY`.
        api_key: Option<String>,
        model: Option<String>,
        #[allow(dead_code)]
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
///
/// Priority: `ANTHROPIC_API_KEY` set → `AnthropicApi`; `claude` in PATH → `ClaudeCode`; error.
pub fn auto_detect() -> Result<ProviderConfig> {
    if std::env::var("ANTHROPIC_API_KEY").is_ok() {
        return Ok(ProviderConfig::AnthropicApi {
            api_key: None,
            model: None,
            max_parallel_calls: None,
        });
    }

    // Check for claude binary on PATH or via CLAUDE_BIN.
    if claude_on_path() {
        return Ok(ProviderConfig::ClaudeCode {
            claude_bin: None,
            model: None,
            timeout_secs: None,
            calls_per_minute: None,
        });
    }

    Err(LlmError::NoProvider)
}

fn claude_on_path() -> bool {
    if let Ok(val) = std::env::var("CLAUDE_BIN") {
        let p = PathBuf::from(val);
        if p.is_file() {
            return true;
        }
    }
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            if dir.join("claude").is_file() {
                return true;
            }
        }
    }
    false
}

/// Build a boxed provider from a config.
pub fn build(config: ProviderConfig) -> Result<Box<dyn LlmProvider>> {
    match config {
        ProviderConfig::AnthropicApi { api_key, model, .. } => {
            let key = api_key
                .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok())
                .ok_or(LlmError::NoProvider)?;
            Ok(Box::new(AnthropicApiProvider::new(key, model, None)))
        }
        ProviderConfig::ClaudeCode {
            claude_bin,
            model,
            timeout_secs,
            calls_per_minute,
        } => {
            let bin = claude_bin
                .or_else(|| std::env::var("CLAUDE_BIN").ok().map(PathBuf::from))
                .or_else(find_claude_in_path)
                .ok_or(LlmError::NoProvider)?;
            Ok(Box::new(ClaudeCodeProvider::new(
                bin,
                model,
                timeout_secs,
                calls_per_minute,
            )))
        }
        ProviderConfig::DryRun => Ok(Box::new(DryRunProvider::new())),
    }
}

fn find_claude_in_path() -> Option<PathBuf> {
    let path_var = std::env::var("PATH").ok()?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join("claude");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_detect_with_api_key_returns_anthropic() {
        // SAFETY: single-threaded test, no concurrent env access.
        unsafe { std::env::set_var("ANTHROPIC_API_KEY", "test-key") };
        let config = auto_detect().unwrap();
        assert!(matches!(config, ProviderConfig::AnthropicApi { .. }));
        unsafe { std::env::remove_var("ANTHROPIC_API_KEY") };
    }

    #[test]
    fn auto_detect_without_key_or_claude_returns_no_provider() {
        let original = std::env::var("PATH").unwrap_or_default();
        // SAFETY: single-threaded test, no concurrent env access.
        unsafe {
            std::env::remove_var("ANTHROPIC_API_KEY");
            std::env::remove_var("CLAUDE_BIN");
            std::env::set_var("PATH", "/nonexistent");
        }
        let result = auto_detect();
        unsafe { std::env::set_var("PATH", original) };
        assert!(matches!(result.unwrap_err(), LlmError::NoProvider));
    }

    #[test]
    fn auto_detect_with_mock_claude_on_path() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("claude");
        std::fs::write(&bin, "#!/bin/sh\necho ok").unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin, perms).unwrap();

        // SAFETY: single-threaded test, no concurrent env access.
        unsafe {
            std::env::remove_var("ANTHROPIC_API_KEY");
            std::env::set_var("CLAUDE_BIN", bin.to_str().unwrap());
        }
        let config = auto_detect().unwrap();
        assert!(matches!(config, ProviderConfig::ClaudeCode { .. }));
        unsafe { std::env::remove_var("CLAUDE_BIN") };
    }

    #[test]
    fn build_dry_run() {
        let provider = build(ProviderConfig::DryRun).unwrap();
        assert_eq!(provider.name(), "dry-run");
    }

    #[test]
    fn build_anthropic_without_key_fails() {
        // SAFETY: single-threaded test, no concurrent env access.
        unsafe { std::env::remove_var("ANTHROPIC_API_KEY") };
        let result = build(ProviderConfig::AnthropicApi {
            api_key: None,
            model: None,
            max_parallel_calls: None,
        });
        let err = result
            .err()
            .expect("expected build to fail without API key");
        assert!(matches!(err, LlmError::NoProvider));
    }
}
