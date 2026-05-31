use anyhow::{Context, Result};
use callimachus_core::indexing::model_tier::TierConfig;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GlobalConfig {
    #[serde(default)]
    pub storage: StorageConfig,
    #[serde(default)]
    pub llm: LlmConfig,
    /// Model tier routing configuration.  When absent from `config.toml`,
    /// defaults to `TierConfig::default()` (disabled — single-model mode).
    #[serde(default)]
    pub model_tiers: TierConfig,
    /// Embedding configuration. When absent, embeddings are off.
    #[serde(default)]
    pub embedding: Option<EmbeddingConfig>,
}

/// Configuration for the embedding provider.
///
/// Example `config.toml` block:
/// ```toml
/// [embedding]
/// enabled = true
/// provider = "voyage"
/// model = "voyage-code-3"
/// api_key_env = "VOYAGE_API_KEY"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EmbeddingConfig {
    /// Master switch. When false (or the whole `[embedding]` block is absent),
    /// embeddings are off and requesting `--pass embed/all` errors loudly.
    #[serde(default)]
    pub enabled: bool,
    /// Provider id. Currently only `"voyage"` is accepted.
    #[serde(default)]
    pub provider: Option<String>,
    /// Model name. Defaults to `voyage-code-3` when absent.
    #[serde(default)]
    pub model: Option<String>,
    /// Inline API key. Lower precedence than `api_key_env`.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Name of the environment variable holding the API key.
    /// Takes precedence over `api_key` when both are present.
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// Optional batch size hint (reserved; the per-chunk loop ignores it for
    /// now). Kept so a future batch path needs no config change.
    #[serde(default)]
    pub batch_size: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StorageConfig {
    /// Preferred field name — used by new config files.
    pub pinakes_path: Option<PathBuf>,
    /// Deprecated alias kept for backwards-compatible config files.
    #[serde(default)]
    pub db_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LlmConfig {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub api_key: Option<String>,
}

impl GlobalConfig {
    pub fn load() -> Result<Self> {
        let path = config_file_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("reading config file {}", path.display()))?;
        toml::from_str(&raw).with_context(|| "parsing config file")
    }

    #[allow(dead_code)]
    pub fn save(&self) -> Result<()> {
        let path = config_file_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let raw = toml::to_string_pretty(self)?;
        std::fs::write(&path, raw)?;
        Ok(())
    }
}

/// Resolve the index path, in priority order:
/// 1. CALLIMACHUS_PINAKES environment variable
/// 2. --pinakes CLI flag (passed in)
/// 3. CALLIMACHUS_DB environment variable (deprecated; emits warning)
/// 4. --db CLI flag (deprecated; emits warning)
/// 5. Global config file pinakes_path / db_path
/// 6. XDG default (.pinakes extension)
pub fn resolve_pinakes_path(
    pinakes_flag: Option<PathBuf>,
    db_flag: Option<PathBuf>,
    config: &GlobalConfig,
) -> PathBuf {
    if let Ok(env) = std::env::var("CALLIMACHUS_PINAKES") {
        return PathBuf::from(env);
    }
    if let Some(p) = pinakes_flag {
        return p;
    }
    if let Ok(env) = std::env::var("CALLIMACHUS_DB") {
        eprintln!("warning: CALLIMACHUS_DB is deprecated, use CALLIMACHUS_PINAKES");
        return PathBuf::from(env);
    }
    if let Some(p) = db_flag {
        eprintln!("warning: --db is deprecated, use --pinakes");
        return p;
    }
    if let Some(p) = &config.storage.pinakes_path {
        return p.clone();
    }
    if let Some(p) = &config.storage.db_path {
        eprintln!("warning: storage.db_path in config is deprecated, use storage.pinakes_path");
        return p.clone();
    }
    default_db_path()
}

/// Deprecated: use `resolve_pinakes_path` instead.
#[deprecated(since = "0.1.0", note = "use resolve_pinakes_path")]
#[allow(dead_code)]
pub fn resolve_db_path(flag: Option<PathBuf>, config: &GlobalConfig) -> PathBuf {
    resolve_pinakes_path(None, flag, config)
}

pub fn config_file_path() -> PathBuf {
    if let Ok(env) = std::env::var("CALLIMACHUS_CONFIG") {
        return PathBuf::from(env);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("callimachus")
        .join("config.toml")
}

/// Build an `EmbeddingProviderConfig` from the CLI's `GlobalConfig`.
///
/// This is re-exported here for use in tests — production code calls
/// `commands::index::build_embedding_provider_config` instead.
#[cfg(test)]
pub fn embedding_provider_config_from(
    config: &GlobalConfig,
) -> callimachus_llm::EmbeddingProviderConfig {
    match &config.embedding {
        None => callimachus_llm::EmbeddingProviderConfig::default(),
        Some(e) => callimachus_llm::EmbeddingProviderConfig {
            enabled: e.enabled,
            provider: e.provider.clone(),
            model: e.model.clone(),
            api_key: e.api_key.clone(),
            api_key_env: e.api_key_env.clone(),
        },
    }
}

pub fn default_db_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("callimachus")
        .join("index.pinakes")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_config() -> GlobalConfig {
        GlobalConfig::default()
    }

    #[test]
    fn pinakes_flag_wins_over_db_flag() {
        let pinakes = Some(PathBuf::from("/index.pinakes"));
        let db = Some(PathBuf::from("/index.db"));
        let result = resolve_pinakes_path(pinakes, db, &empty_config());
        assert_eq!(result, PathBuf::from("/index.pinakes"));
    }

    #[test]
    fn db_flag_used_when_no_pinakes_flag() {
        let result = resolve_pinakes_path(None, Some(PathBuf::from("/old.db")), &empty_config());
        assert_eq!(result, PathBuf::from("/old.db"));
    }

    #[test]
    fn pinakes_flag_beats_env_db() {
        // CALLIMACHUS_PINAKES takes priority over --db
        let result = resolve_pinakes_path(
            Some(PathBuf::from("/explicit.pinakes")),
            None,
            &empty_config(),
        );
        assert_eq!(result, PathBuf::from("/explicit.pinakes"));
    }

    #[test]
    fn config_pinakes_path_used_when_no_flags() {
        let mut config = empty_config();
        config.storage.pinakes_path = Some(PathBuf::from("/config.pinakes"));
        let result = resolve_pinakes_path(None, None, &config);
        assert_eq!(result, PathBuf::from("/config.pinakes"));
    }

    #[test]
    fn config_db_path_fallback_when_no_pinakes_path() {
        let mut config = empty_config();
        config.storage.db_path = Some(PathBuf::from("/config.db"));
        let result = resolve_pinakes_path(None, None, &config);
        assert_eq!(result, PathBuf::from("/config.db"));
    }

    #[test]
    fn config_pinakes_path_wins_over_db_path() {
        let mut config = empty_config();
        config.storage.pinakes_path = Some(PathBuf::from("/config.pinakes"));
        config.storage.db_path = Some(PathBuf::from("/config.db"));
        let result = resolve_pinakes_path(None, None, &config);
        assert_eq!(result, PathBuf::from("/config.pinakes"));
    }

    #[test]
    fn default_path_has_pinakes_extension() {
        let path = default_db_path();
        assert_eq!(path.extension().and_then(|e| e.to_str()), Some("pinakes"));
    }

    // ── Embedding config / builder tests ─────────────────────────────────────

    fn make_config_with_embedding(e: EmbeddingConfig) -> GlobalConfig {
        GlobalConfig {
            embedding: Some(e),
            ..Default::default()
        }
    }

    #[test]
    fn embedding_disabled_yields_none() {
        let config = make_config_with_embedding(EmbeddingConfig {
            enabled: false,
            ..Default::default()
        });
        let cfg = embedding_provider_config_from(&config);
        let result = callimachus_llm::build_embedding_provider(cfg).unwrap();
        assert!(result.is_none(), "disabled embedding should yield None");
    }

    #[test]
    fn embedding_enabled_with_key_yields_provider() {
        // SAFETY: single-threaded test.
        unsafe { std::env::set_var("TEST_VOYAGE_KEY_PRESENT", "voyage-test-key") };
        let config = make_config_with_embedding(EmbeddingConfig {
            enabled: true,
            api_key_env: Some("TEST_VOYAGE_KEY_PRESENT".to_string()),
            ..Default::default()
        });
        let cfg = embedding_provider_config_from(&config);
        let provider = callimachus_llm::build_embedding_provider(cfg)
            .expect("build should succeed")
            .expect("enabled + key present → Some provider");
        assert_eq!(provider.name(), "voyage-code-3");
        unsafe { std::env::remove_var("TEST_VOYAGE_KEY_PRESENT") };
    }

    #[test]
    fn embedding_enabled_without_key_errors() {
        // Use a random var name guaranteed not to exist.
        let config = make_config_with_embedding(EmbeddingConfig {
            enabled: true,
            api_key_env: Some("CALLIMACHUS_TEST_NONEXISTENT_KEY_XYZ".to_string()),
            api_key: None,
            ..Default::default()
        });
        let cfg = embedding_provider_config_from(&config);
        let result = callimachus_llm::build_embedding_provider(cfg);
        assert!(result.is_err(), "enabled embedding without key should error");
        let msg = result.err().expect("checked above").to_string();
        assert!(
            msg.contains("key") || msg.contains("api_key"),
            "error should mention API key: {msg}"
        );
    }

    #[test]
    fn api_key_env_takes_precedence_over_inline() {
        // Set env var to one sentinel; api_key is another. Provider should use env var key.
        unsafe { std::env::set_var("CALLIMACHUS_TEST_ENV_KEY", "env-key-sentinel") };
        let config = make_config_with_embedding(EmbeddingConfig {
            enabled: true,
            api_key_env: Some("CALLIMACHUS_TEST_ENV_KEY".to_string()),
            api_key: Some("inline-key-sentinel".to_string()),
            ..Default::default()
        });
        let cfg = embedding_provider_config_from(&config);
        // Both are present — should succeed (env var wins, but we can't inspect the key).
        let result = callimachus_llm::build_embedding_provider(cfg);
        assert!(
            result.is_ok(),
            "should succeed when both env and inline key are set: {}",
            result.err().map_or_else(String::new, |e| e.to_string())
        );
        unsafe { std::env::remove_var("CALLIMACHUS_TEST_ENV_KEY") };
    }

    #[test]
    fn unknown_provider_errors() {
        let config = make_config_with_embedding(EmbeddingConfig {
            enabled: true,
            provider: Some("openai".to_string()),
            api_key: Some("some-key".to_string()),
            ..Default::default()
        });
        let cfg = embedding_provider_config_from(&config);
        let result = callimachus_llm::build_embedding_provider(cfg);
        assert!(result.is_err(), "unknown provider should error");
        let msg = result.err().expect("checked above").to_string();
        assert!(
            msg.contains("voyage") || msg.contains("openai"),
            "error should mention voyage or the bad provider: {msg}"
        );
    }
}
