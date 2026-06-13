use anyhow::{Context, Result};
use callimachus_core::indexing::model_tier::TierConfig;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Secret masking
// ---------------------------------------------------------------------------

/// Mask a secret string for display.
///
/// Shows the first 10 and last 4 characters with `…` between them, giving
/// enough context to identify the key without exposing the full value.
/// If the value is shorter than 15 characters it is fully redacted as
/// `[hidden]` to avoid leaking the whole secret through prefix+suffix.
pub fn mask_secret(val: &str) -> String {
    if val.len() >= 15 {
        let prefix = &val[..10];
        let suffix = &val[val.len() - 4..];
        format!("{prefix}…{suffix}")
    } else {
        "[hidden]".to_string()
    }
}

/// Mask an optional secret string.  `None` stays `None`; `Some(val)` is
/// replaced with `Some(masked_val)`.
fn mask_opt(val: &Option<String>) -> Option<String> {
    val.as_deref().map(mask_secret)
}

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
    /// Return a copy of this config suitable for display: all secret fields are
    /// masked.  Pass `reveal = true` to skip masking (e.g. for `--reveal`).
    pub fn for_display(&self, reveal: bool) -> Self {
        if reveal {
            return self.clone();
        }
        Self {
            storage: self.storage.clone(),
            model_tiers: self.model_tiers.clone(),
            llm: LlmConfig {
                api_key: mask_opt(&self.llm.api_key),
                ..self.llm.clone()
            },
            embedding: self.embedding.as_ref().map(|e| EmbeddingConfig {
                api_key: mask_opt(&e.api_key),
                ..e.clone()
            }),
        }
    }

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
        assert!(
            result.is_err(),
            "enabled embedding without key should error"
        );
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

    // ── Secret masking tests ──────────────────────────────────────────────────

    #[test]
    fn mask_secret_long_key() {
        // 15+ chars → first 10 + … + last 4
        let key = "sk-ant-api03JCtxxxxxxxx2tKg";
        let masked = mask_secret(key);
        assert!(masked.starts_with("sk-ant-api"), "prefix: {masked}");
        assert!(masked.ends_with("2tKg"), "suffix: {masked}");
        assert!(masked.contains('…'), "ellipsis: {masked}");
        assert!(
            !masked.contains("JCtxxxxxxxx"),
            "middle not leaked: {masked}"
        );
    }

    #[test]
    fn mask_secret_short_key() {
        // < 15 chars → fully redacted
        let key = "short-key";
        assert_eq!(mask_secret(key), "[hidden]");
    }

    #[test]
    fn mask_secret_exactly_15_chars() {
        let key = "123456789012345"; // exactly 15
        let masked = mask_secret(key);
        assert_eq!(masked, "1234567890…2345");
    }

    #[test]
    fn for_display_masks_llm_api_key() {
        let mut config = GlobalConfig::default();
        config.llm.api_key = Some("sk-ant-api03JCtxxxxxxxx2tKg".to_string());
        let displayed = config.for_display(false);
        let key = displayed.llm.api_key.as_deref().unwrap();
        assert!(key.contains('…'), "should be masked: {key}");
        assert!(!key.contains("JCtxxxxxxxx"), "middle not leaked: {key}");
    }

    #[test]
    fn for_display_reveal_shows_full_key() {
        let mut config = GlobalConfig::default();
        config.llm.api_key = Some("sk-ant-api03JCtxxxxxxxx2tKg".to_string());
        let displayed = config.for_display(true);
        assert_eq!(
            displayed.llm.api_key.as_deref(),
            Some("sk-ant-api03JCtxxxxxxxx2tKg")
        );
    }

    #[test]
    fn for_display_masks_embedding_api_key() {
        let config = GlobalConfig {
            embedding: Some(EmbeddingConfig {
                enabled: true,
                api_key: Some("pa-voyage-longkeyxxxxxxxx1234".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let displayed = config.for_display(false);
        let key = displayed
            .embedding
            .as_ref()
            .unwrap()
            .api_key
            .as_deref()
            .unwrap();
        assert!(key.contains('…'), "should be masked: {key}");
    }

    #[test]
    fn for_display_unset_key_stays_unset() {
        let config = GlobalConfig::default(); // llm.api_key is None
        let displayed = config.for_display(false);
        assert!(displayed.llm.api_key.is_none());
    }

    #[test]
    fn for_display_does_not_mask_api_key_env() {
        // api_key_env is the *name* of an env var, not a secret — must not be masked
        let config = GlobalConfig {
            embedding: Some(EmbeddingConfig {
                enabled: true,
                api_key_env: Some("MY_VOYAGE_KEY".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let displayed = config.for_display(false);
        assert_eq!(
            displayed.embedding.as_ref().unwrap().api_key_env.as_deref(),
            Some("MY_VOYAGE_KEY"),
            "env var name must not be masked"
        );
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
