//! Keypress display state
//!
//! Tracks accumulated key sequences for visual feedback during Vim-style input.

use std::time::{Duration, Instant};

use crate::neovim::PendingState;

/// Duration to show keypress window before auto-hide
pub const KEYPRESS_DISPLAY_DURATION: Duration = Duration::from_millis(1500);

/// State for keypress display window
#[derive(Debug)]
pub struct KeypressState {
    /// Accumulated key sequence (e.g., "di" while waiting for text object)
    pub accumulated: String,
    /// Whether display is visible
    pub visible: bool,
    /// Pending mode type
    pub pending_type: PendingState,
    /// Current vim mode string (i, n, v, no, etc.)
    pub vim_mode: String,
    /// Time when keypress display was last shown/updated
    pub last_shown: Option<Instant>,
    /// Currently recording macro register ("" when not recording)
    pub recording: String,
}

impl KeypressState {
    /// Create a new keypress state
    pub fn new() -> Self {
        Self {
            accumulated: String::new(),
            visible: false,
            pending_type: PendingState::None,
            vim_mode: String::new(),
            last_shown: None,
            recording: String::new(),
        }
    }

    /// Push a key to the accumulated sequence
    pub fn push_key(&mut self, key: &str) {
        self.accumulated.push_str(key);
        self.visible = true;
        self.last_shown = Some(Instant::now());
    }

    /// Clear accumulated keys and hide display
    pub fn clear(&mut self) {
        self.accumulated.clear();
        self.visible = false;
        self.pending_type = PendingState::None;
        self.last_shown = None;
        // NOTE: recording is NOT cleared here — it's driven by Neovim snapshots,
        // not by keypress display lifecycle. Cleared explicitly on disable/exit.
    }

    /// Set the pending type
    pub fn set_pending(&mut self, pending_type: PendingState) {
        self.pending_type = pending_type;
    }

    /// Update vim mode
    pub fn set_vim_mode(&mut self, mode: &str) {
        self.vim_mode = mode.to_string();
    }

    /// Check if in normal mode
    pub fn is_normal_mode(&self) -> bool {
        self.vim_mode == "n" || self.vim_mode.starts_with("no")
    }

    /// Check if in visual mode
    pub fn is_visual_mode(&self) -> bool {
        self.vim_mode == "v" || self.vim_mode.starts_with('v')
    }

    /// Check if we should show the keypress display
    pub fn should_show(&self) -> bool {
        self.visible && !self.accumulated.is_empty()
    }

    /// Check if display has timed out
    pub fn is_timed_out(&self) -> bool {
        // Command-line mode should not auto-hide
        if self.vim_mode == "c" {
            return false;
        }
        if let Some(last_shown) = self.last_shown {
            last_shown.elapsed() >= KEYPRESS_DISPLAY_DURATION
        } else {
            false
        }
    }
}

impl Default for KeypressState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_state_is_hidden_and_empty() {
        let state = KeypressState::new();
        assert!(state.accumulated.is_empty());
        assert!(!state.visible);
        assert_eq!(state.pending_type, PendingState::None);
        assert!(state.vim_mode.is_empty());
        assert_eq!(state.last_shown, None);
        assert!(!state.should_show());
        assert!(!state.is_timed_out());
    }

    #[test]
    fn push_key_accumulates_and_shows_display() {
        let mut state = KeypressState::new();
        state.push_key("d");
        state.push_key("i");
        state.push_key("w");

        assert_eq!(state.accumulated, "diw");
        assert!(state.visible);
        assert!(state.last_shown.is_some());
        assert!(state.should_show());
    }

    #[test]
    fn clear_resets_display_state_but_keeps_recording() {
        let mut state = KeypressState::new();
        state.push_key("a");
        state.set_pending(PendingState::Motion);
        state.recording = "q".to_string();

        state.clear();

        assert_eq!(state.accumulated, "");
        assert!(!state.visible);
        assert_eq!(state.pending_type, PendingState::None);
        assert_eq!(state.last_shown, None);
        assert_eq!(state.recording, "q");
        assert!(!state.should_show());
    }

    #[test]
    fn mode_classification_normal_mode() {
        let mut state = KeypressState::new();

        state.set_vim_mode("n");
        assert!(state.is_normal_mode());

        state.set_vim_mode("no");
        assert!(state.is_normal_mode());

        state.set_vim_mode("nov");
        assert!(state.is_normal_mode());

        state.set_vim_mode("i");
        assert!(!state.is_normal_mode());
    }

    #[test]
    fn mode_classification_visual_mode() {
        let mut state = KeypressState::new();

        state.set_vim_mode("v");
        assert!(state.is_visual_mode());

        state.set_vim_mode("vs");
        assert!(state.is_visual_mode());

        state.set_vim_mode("V");
        assert!(!state.is_visual_mode());

        state.set_vim_mode("n");
        assert!(!state.is_visual_mode());
    }

    #[test]
    fn should_show_requires_visible_and_non_empty() {
        let mut state = KeypressState::new();
        assert!(!state.should_show());

        state.visible = true;
        assert!(!state.should_show());

        state.accumulated = "x".to_string();
        assert!(state.should_show());

        state.visible = false;
        assert!(!state.should_show());
    }

    #[test]
    fn timeout_depends_on_elapsed_duration() {
        let mut state = KeypressState::new();
        state.set_vim_mode("n");
        state.last_shown = Some(Instant::now() - KEYPRESS_DISPLAY_DURATION - Duration::from_millis(1));
        assert!(state.is_timed_out());

        state.last_shown = Some(Instant::now());
        assert!(!state.is_timed_out());

        state.last_shown = None;
        assert!(!state.is_timed_out());
    }

    #[test]
    fn command_line_mode_never_times_out() {
        let mut state = KeypressState::new();
        state.set_vim_mode("c");
        state.last_shown = Some(Instant::now() - KEYPRESS_DISPLAY_DURATION - Duration::from_secs(1));
        assert!(!state.is_timed_out());
    }
}
