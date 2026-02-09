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
