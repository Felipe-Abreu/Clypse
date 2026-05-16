use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub max_history_items:    usize,
    pub max_image_size_bytes: u64,
    pub auto_clear_days:      Option<u32>,
    pub enable_images:        bool,
    pub debounce_ms:          u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_history_items:    500,
            max_image_size_bytes: 10 * 1024 * 1024,
            auto_clear_days:      None,
            enable_images:        true,
            debounce_ms:          50,
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = Self::config_path();
        if !path.exists() {
            let cfg = Self::default();
            cfg.save()?;
            return Ok(cfg);
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("reading config {}", path.display()))?;
        toml::from_str(&raw)
            .with_context(|| format!("parsing config {}", path.display()))
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::config_path();
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, toml::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn config_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config"))
            .join("clypse/config.toml")
    }

    pub fn data_dir() -> PathBuf {
        dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/share"))
            .join("clypse")
    }

    pub fn db_path() -> PathBuf {
        Self::data_dir().join("history.db")
    }

#[allow(dead_code)]
    pub fn images_dir() -> PathBuf {
        Self::data_dir().join("images")
    }

    pub fn socket_path() -> PathBuf {
        // $XDG_RUNTIME_DIR is always set in a proper systemd user session
        std::env::var("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir())
            .join("clypse/daemon.sock")
    }
}
