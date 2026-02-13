use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub keybinds: Keybinds,
    pub completion: Completion,
    pub behavior: Behavior,
    pub font: FontConfig,
    #[serde(skip)]
    pub clean: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FontConfig {
    /// Proportional font family name (for preedit/candidates).
    /// Default: fontconfig auto-detection.
    pub family: Option<String>,
    /// Monospace font family name (for keypress/mode display).
    /// Default: fontconfig "monospace" match.
    pub mono_family: Option<String>,
    /// Font size in pixels. Default: 16.0.
    pub size: Option<f32>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Behavior {
    /// If true, IME starts in insert mode and returns to insert mode after commands.
    /// If false, IME starts in normal mode.
    /// Default: false.
    pub auto_startinsert: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Completion {
    pub adapter: String,
}

impl Default for Completion {
    fn default() -> Self {
        Self {
            adapter: "native".to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Keybinds {
    pub commit: String,
}

impl Default for Keybinds {
    fn default() -> Self {
        Self {
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
                log::warn!(
                    "[CONFIG] Parse error in {}: {} (using defaults)",
                    path.display(),
                    e
                );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_values() {
        let config = Config::default();
        assert_eq!(config.keybinds.commit, "<C-CR>");
        assert_eq!(config.completion.adapter, "native");
        assert!(!config.behavior.auto_startinsert);
        assert!(!config.clean);
        assert!(config.font.family.is_none());
        assert!(config.font.mono_family.is_none());
        assert!(config.font.size.is_none());
    }

    #[test]
    fn empty_toml_uses_defaults() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.keybinds.commit, "<C-CR>");
        assert_eq!(config.completion.adapter, "native");
        assert!(!config.behavior.auto_startinsert);
        assert!(config.font.family.is_none());
    }

    #[test]
    fn partial_toml_keybinds_only() {
        let config: Config = toml::from_str(
            r#"
            [keybinds]
            commit = "<A-;>"
            "#,
        )
        .unwrap();
        assert_eq!(config.keybinds.commit, "<A-;>");
        // Other sections use defaults
        assert_eq!(config.completion.adapter, "native");
        assert!(!config.behavior.auto_startinsert);
    }

    #[test]
    fn partial_toml_completion_only() {
        let config: Config = toml::from_str(
            r#"
            [completion]
            adapter = "cmp"
            "#,
        )
        .unwrap();
        assert_eq!(config.completion.adapter, "cmp");
        assert_eq!(config.keybinds.commit, "<C-CR>");
    }

    #[test]
    fn partial_toml_behavior_only() {
        let config: Config = toml::from_str(
            r#"
            [behavior]
            auto_startinsert = true
            "#,
        )
        .unwrap();
        assert!(config.behavior.auto_startinsert);
        assert_eq!(config.keybinds.commit, "<C-CR>");
    }

    #[test]
    fn full_toml() {
        let config: Config = toml::from_str(
            r#"
            [keybinds]
            commit = "<C-;>"

            [completion]
            adapter = "cmp"

            [behavior]
            auto_startinsert = true

            [font]
            family = "Noto Sans CJK JP"
            mono_family = "JetBrains Mono"
            size = 18.0
            "#,
        )
        .unwrap();
        assert_eq!(config.keybinds.commit, "<C-;>");
        assert_eq!(config.completion.adapter, "cmp");
        assert!(config.behavior.auto_startinsert);
        assert_eq!(config.font.family.as_deref(), Some("Noto Sans CJK JP"));
        assert_eq!(config.font.mono_family.as_deref(), Some("JetBrains Mono"));
        assert_eq!(config.font.size, Some(18.0));
    }

    #[test]
    fn partial_toml_font_only() {
        let config: Config = toml::from_str(
            r#"
            [font]
            family = "Noto Sans"
            "#,
        )
        .unwrap();
        assert_eq!(config.font.family.as_deref(), Some("Noto Sans"));
        assert!(config.font.mono_family.is_none());
        assert!(config.font.size.is_none());
        // Other sections use defaults
        assert_eq!(config.keybinds.commit, "<C-CR>");
    }

    #[test]
    fn unknown_keys_ignored() {
        let config: Config = toml::from_str(
            r#"
            [keybinds]
            commit = "<C-CR>"
            unknown_key = "value"

            [unknown_section]
            foo = "bar"
            "#,
        )
        .unwrap();
        assert_eq!(config.keybinds.commit, "<C-CR>");
    }

    #[test]
    fn clean_field_skipped_by_serde() {
        // clean is #[serde(skip)], so even if present in TOML it stays false
        let config: Config = toml::from_str(
            r#"
            clean = true
            "#,
        )
        .unwrap();
        assert!(!config.clean);
    }

    #[test]
    fn invalid_toml_is_err() {
        let result: Result<Config, _> = toml::from_str("{{invalid}}");
        assert!(result.is_err());
    }

    #[test]
    fn parse_from_str() {
        let config: Config = toml::from_str(
            r#"
            [keybinds]
            commit = "<A-CR>"
            "#,
        )
        .unwrap();
        assert_eq!(config.keybinds.commit, "<A-CR>");
    }
}
