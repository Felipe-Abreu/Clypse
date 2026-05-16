use anyhow::Result;
use libadwaita as adw;
use libadwaita::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::warn;

// ─── Config (espelho do daemon) ───────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuiConfig {
    #[serde(default = "default_max_history")]
    pub max_history_items:    usize,
    #[serde(default = "default_max_image_size")]
    pub max_image_size_bytes: u64,
    #[serde(default)]
    pub auto_clear_days:      Option<u32>,
    #[serde(default = "default_true")]
    pub enable_images:        bool,
    #[serde(default = "default_debounce")]
    pub debounce_ms:          u64,
}

fn default_max_history()    -> usize { 500 }
fn default_max_image_size() -> u64   { 10 * 1024 * 1024 }
fn default_true()           -> bool  { true }
fn default_debounce()       -> u64   { 50 }

impl Default for GuiConfig {
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

impl GuiConfig {
    pub fn load() -> Result<Self> {
        let path = Self::path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(&path)?;
        Ok(toml::from_str(&raw)?)
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, toml::to_string_pretty(self)?)?;
        Ok(())
    }

    fn path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config")
            })
            .join("clypse/config.toml")
    }
}

// ─── Diálogo de preferências ──────────────────────────────────────────────────

pub fn show(parent: Option<&gtk4::Window>) {
    let config = GuiConfig::load().unwrap_or_default();

    let dialog = adw::PreferencesDialog::new();
    dialog.set_title("Preferences");

    // ── Grupo: Histórico ──────────────────────────────────────────────────────
    let history_group = adw::PreferencesGroup::new();
    history_group.set_title("History");

    let max_items_row = adw::SpinRow::with_range(10.0, 10_000.0, 10.0);
    max_items_row.set_title("Maximum items");
    max_items_row.set_subtitle("Oldest non-favorited entries are removed when this limit is reached");
    max_items_row.set_value(config.max_history_items as f64);
    history_group.add(&max_items_row);

    let auto_clear_row = adw::SpinRow::with_range(0.0, 365.0, 1.0);
    auto_clear_row.set_title("Auto-clear after (days)");
    auto_clear_row.set_subtitle("0 = keep history indefinitely");
    auto_clear_row.set_value(config.auto_clear_days.unwrap_or(0) as f64);
    history_group.add(&auto_clear_row);

    // ── Grupo: Imagens ────────────────────────────────────────────────────────
    let images_group = adw::PreferencesGroup::new();
    images_group.set_title("Images");

    let enable_images_row = adw::SwitchRow::new();
    enable_images_row.set_title("Capture image clips");
    enable_images_row.set_active(config.enable_images);
    images_group.add(&enable_images_row);

    let max_size_row = adw::SpinRow::with_range(1.0, 100.0, 1.0);
    max_size_row.set_title("Maximum image size (MB)");
    max_size_row.set_subtitle("Images larger than this are not captured");
    max_size_row.set_value(config.max_image_size_bytes as f64 / (1024.0 * 1024.0));
    images_group.add(&max_size_row);

    // ── Grupo: Avançado ───────────────────────────────────────────────────────
    let advanced_group = adw::PreferencesGroup::new();
    advanced_group.set_title("Advanced");
    advanced_group.set_description(Some("Changes to advanced settings take effect after restarting the daemon."));

    let debounce_row = adw::SpinRow::with_range(0.0, 500.0, 10.0);
    debounce_row.set_title("Debounce delay (ms)");
    debounce_row.set_subtitle("Minimum delay between consecutive clipboard captures");
    debounce_row.set_value(config.debounce_ms as f64);
    advanced_group.add(&debounce_row);

    let page = adw::PreferencesPage::new();
    page.add(&history_group);
    page.add(&images_group);
    page.add(&advanced_group);
    dialog.add(&page);

    // Salva configuração ao fechar (GNOME HIG: sem botão Apply)
    {
        let r_max_items  = max_items_row.clone();
        let r_auto_clear = auto_clear_row.clone();
        let r_enable_img = enable_images_row.clone();
        let r_max_size   = max_size_row.clone();
        let r_debounce   = debounce_row.clone();

        dialog.connect_closed(move |_| {
            let cfg = GuiConfig {
                max_history_items:    r_max_items.value() as usize,
                max_image_size_bytes: (r_max_size.value() * 1024.0 * 1024.0) as u64,
                auto_clear_days: {
                    let v = r_auto_clear.value() as u32;
                    if v == 0 { None } else { Some(v) }
                },
                enable_images: r_enable_img.is_active(),
                debounce_ms:   r_debounce.value() as u64,
            };
            if let Err(e) = cfg.save() {
                warn!("Failed to save preferences: {}", e);
            }
        });
    }

    dialog.present(parent);
}
