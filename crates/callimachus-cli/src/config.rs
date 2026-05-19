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

/// Resolve the database path, in priority order:
/// 1. CALLIMACHUS_DB environment variable
/// 2. --db CLI flag (passed in)
/// 3. Global config file db_path
/// 4. XDG default
pub fn resolve_db_path(flag: Option<PathBuf>, config: &GlobalConfig) -> PathBuf {
    if let Ok(env) = std::env::var("CALLIMACHUS_DB") {
        return PathBuf::from(env);
    }
    if let Some(p) = flag {
        return p;
    }
    if let Some(p) = &config.storage.db_path {
        return p.clone();
    }
    default_db_path()
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
        .join("index.db")
}
