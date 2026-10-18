use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub keybinds: Keybinds,
    pub behavior: Behavior,
    pub font: FontConfig,
    pub appearance: Appearance,
    pub focus: Focus,
    #[serde(skip)]
    pub clean: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FontConfig {
    /// Proportional font family name (for candidates and messages).
    /// Default: fontconfig auto-detection.
    pub family: Option<String>,
    /// Monospace font family name (for the Neovim window, command line
    /// and mode display).
    /// Default: fontconfig "monospace" match.
    pub mono_family: Option<String>,
    /// Font size in pixels. Default: 16.0.
    pub size: Option<f32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Appearance {
    /// Opacity of the popup background, 0.0 (transparent) to 1.0.
    /// Default: 1.0.
    pub opacity: f32,
}

impl Default for Appearance {
    fn default() -> Self {
        Self { opacity: 1.0 }
    }
}

/// Size of the whole Neovim screen in focus mode (`jacin focus`), in
/// Neovim's `columns` and `lines`. No default: focus mode is available only
/// with both set, as the size that fits depends on the monitor and font.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Focus {
    pub columns: Option<usize>,
    pub lines: Option<usize>,
}

impl Focus {
    pub fn size(&self) -> Option<(usize, usize)> {
        self.columns.zip(self.lines)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Behavior {
    /// If true, IME starts in insert mode.
    /// If false, IME starts in normal mode.
    /// Default: true.
    pub startinsert: bool,
    /// If true, the REC indicator dot blinks while recording a macro.
    /// Default: true.
    pub recording_blink: bool,
    /// Register that keeps the text thrown away without a commit (focus
    /// moved away), e.g. "j". A letter; uppercase appends.
    /// Default: none (the text is lost).
    pub stash_register: Option<char>,
}

impl Default for Behavior {
    fn default() -> Self {
        Self {
            startinsert: true,
            recording_blink: true,
            stash_register: None,
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

impl Keybinds {
    /// Write the keys the way jacin sends them (`<c-enter>` → `<C-CR>`), so
    /// they match keys as strings. A key jacin can't send falls back to the
    /// default.
    fn normalize(&mut self) {
        match crate::keysym::normalize_key(&self.commit) {
            Some(key) => self.commit = key,
            None => {
                let default = Self::default().commit;
                log::warn!(
                    "[CONFIG] keybinds.commit {:?} is not a key jacin can send; using {default}",
                    self.commit
                );
                self.commit = default;
            }
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
                let mut config: Self = config;
                config.keybinds.normalize();
                if config.focus.size().is_none()
                    && (config.focus.columns.is_some() || config.focus.lines.is_some())
                {
                    log::warn!("[CONFIG] [focus] needs both columns and lines; focus mode is off");
                }
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
        assert!(config.behavior.startinsert);
        assert!(config.behavior.recording_blink);
        assert!(!config.clean);
        assert!(config.font.family.is_none());
        assert!(config.font.mono_family.is_none());
        assert!(config.font.size.is_none());
        assert_eq!(config.appearance.opacity, 1.0);
        assert!(config.focus.size().is_none());
        assert!(config.behavior.stash_register.is_none());
    }

    #[test]
    fn stash_register() {
        let config: Config = toml::from_str(
            r#"
            [behavior]
            stash_register = "j"
            "#,
        )
        .unwrap();
        assert_eq!(config.behavior.stash_register, Some('j'));
    }

    #[test]
    fn focus_size() {
        let config: Config = toml::from_str(
            r#"
            [focus]
            columns = 100
            lines = 30
            "#,
        )
        .unwrap();
        assert_eq!(config.focus.size(), Some((100, 30)));
    }

    #[test]
    fn focus_size_needs_both() {
        let config: Config = toml::from_str(
            r#"
            [focus]
            columns = 100
            "#,
        )
        .unwrap();
        assert_eq!(config.focus.size(), None);
    }

    #[test]
    fn appearance_opacity() {
        let config: Config = toml::from_str(
            r#"
            [appearance]
            opacity = 0.9
            "#,
        )
        .unwrap();
        assert_eq!(config.appearance.opacity, 0.9);
    }

    #[test]
    fn empty_toml_uses_defaults() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.keybinds.commit, "<C-CR>");
        assert!(config.behavior.startinsert);
        assert!(config.behavior.recording_blink);
        assert!(config.font.family.is_none());
    }

    #[test]
    fn keybinds_are_normalized() {
        let mut keybinds = Keybinds {
            commit: "<c-enter>".into(),
        };
        keybinds.normalize();
        assert_eq!(keybinds.commit, "<C-CR>");

        let mut keybinds = Keybinds {
            commit: "<C-Nope>".into(),
        };
        keybinds.normalize();
        assert_eq!(keybinds.commit, "<C-CR>");
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
        assert!(config.behavior.startinsert);
    }

    #[test]
    fn partial_toml_behavior_only() {
        let config: Config = toml::from_str(
            r#"
            [behavior]
            startinsert = true
            "#,
        )
        .unwrap();
        assert!(config.behavior.startinsert);
        assert!(config.behavior.recording_blink); // default true even when not specified
        assert_eq!(config.keybinds.commit, "<C-CR>");
    }

    #[test]
    fn startinsert_disabled() {
        let config: Config = toml::from_str(
            r#"
            [behavior]
            startinsert = false
            "#,
        )
        .unwrap();
        assert!(!config.behavior.startinsert);
    }

    #[test]
    fn unknown_keys_are_ignored() {
        // Unknown keys and sections are ignored, so configs written for
        // other versions still load
        let config: Config = toml::from_str(
            r#"
            [behavior]
            write_to_commit = true
            display = "grid"
            startinsert = false

            [completion]
            adapter = "nvim-cmp"
            "#,
        )
        .unwrap();
        assert!(!config.behavior.startinsert);
    }

    #[test]
    fn recording_blink_disabled() {
        let config: Config = toml::from_str(
            r#"
            [behavior]
            recording_blink = false
            "#,
        )
        .unwrap();
        assert!(!config.behavior.recording_blink);
    }

    #[test]
    fn full_toml() {
        let config: Config = toml::from_str(
            r#"
            [keybinds]
            commit = "<C-;>"

            [behavior]
            startinsert = true

            [font]
            family = "Noto Sans CJK JP"
            mono_family = "JetBrains Mono"
            size = 18.0
            "#,
        )
        .unwrap();
        assert_eq!(config.keybinds.commit, "<C-;>");
        assert!(config.behavior.startinsert);
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
