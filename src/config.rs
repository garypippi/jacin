use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub keybinds: Keybinds,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Keybinds {
    pub toggle: String,
    pub commit: String,
}

impl Default for Keybinds {
    fn default() -> Self {
        Self {
            toggle: "<A-`>".to_string(),
            commit: "<C-CR>".to_string(),
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let Some(path) = Self::config_path() else {
            return Self::default();
        };

        let contents = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    log::warn!("[CONFIG] Failed to read {}: {}", path.display(), e);
                }
                return Self::default();
            }
        };

        match toml::from_str(&contents) {
            Ok(config) => {
                log::info!("[CONFIG] Loaded from {}", path.display());
                config
            }
            Err(e) => {
                log::warn!("[CONFIG] Parse error in {}: {} (using defaults)", path.display(), e);
                Self::default()
            }
        }
    }

    fn config_path() -> Option<PathBuf> {
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME")
            && !xdg.is_empty()
        {
            return Some(PathBuf::from(xdg).join("jacin/config.toml"));
        }
        if let Ok(home) = std::env::var("HOME") {
            return Some(PathBuf::from(home).join(".config/jacin/config.toml"));
        }
        None
    }
}
