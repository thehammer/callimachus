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
}
