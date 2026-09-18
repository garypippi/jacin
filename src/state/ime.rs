//! IME state machine
//!
//! Explicit state machine for IME mode transitions.

/// Main IME mode state machine
#[derive(Debug, Clone, PartialEq, Default)]
pub enum ImeMode {
    /// IME is disabled, keyboard not grabbed, passthrough mode
    #[default]
    Disabled,
    /// IME is being enabled, waiting for keymap
    Enabling,
    /// IME is fully enabled and processing input
    /// (Vim mode is observed from Neovim, not tracked here)
    Enabled,
}

/// IME lifecycle (the application gets no preedit: text is shown in the
/// popup and only committed)
pub struct ImeState {
    pub mode: ImeMode,
}

impl ImeState {
    pub fn new() -> Self {
        Self {
            mode: ImeMode::Disabled,
        }
    }

    /// Check if IME is enabled (or enabling)
    pub fn is_enabled(&self) -> bool {
        matches!(self.mode, ImeMode::Enabled | ImeMode::Enabling)
    }

    /// Check if IME is fully enabled (not transitioning)
    pub fn is_fully_enabled(&self) -> bool {
        matches!(self.mode, ImeMode::Enabled)
    }

    pub fn start_enabling(&mut self) {
        self.mode = ImeMode::Enabling;
    }

    /// Complete enabling (keymap received). Returns true if transitioned from Enabling.
    pub fn complete_enabling(&mut self) -> bool {
        if self.mode == ImeMode::Enabling {
            self.mode = ImeMode::Enabled;
            true
        } else {
            false
        }
    }

    /// Disable immediately (for toggle off)
    pub fn disable(&mut self) {
        self.mode = ImeMode::Disabled;
    }
}

impl Default for ImeState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_disabled() {
        let state = ImeState::new();
        assert!(!state.is_enabled());
        assert!(!state.is_fully_enabled());
        assert_eq!(state.mode, ImeMode::Disabled);
    }

    #[test]
    fn enabling_lifecycle() {
        let mut state = ImeState::new();
        state.start_enabling();
        assert!(state.is_enabled()); // Enabling counts as "enabled"
        assert!(!state.is_fully_enabled()); // But not fully

        let transitioned = state.complete_enabling();
        assert!(transitioned);
        assert!(state.is_enabled());
        assert!(state.is_fully_enabled());
    }

    #[test]
    fn complete_enabling_only_from_enabling() {
        let mut state = ImeState::new();
        // complete_enabling from Disabled should not transition
        let transitioned = state.complete_enabling();
        assert!(!transitioned);
        assert!(!state.is_enabled());
    }

    #[test]
    fn complete_enabling_from_enabled_is_noop() {
        let mut state = ImeState::new();
        state.start_enabling();
        assert!(state.complete_enabling());

        let transitioned = state.complete_enabling();
        assert!(!transitioned);
        assert_eq!(state.mode, ImeMode::Enabled);
    }

    #[test]
    fn disable_from_enabled() {
        let mut state = ImeState::new();
        state.start_enabling();
        state.complete_enabling();
        state.disable();
        assert!(!state.is_enabled());
        assert_eq!(state.mode, ImeMode::Disabled);
    }
}
