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

    /// Check if we should show the keypress display
    pub fn should_show(&self) -> bool {
        self.visible && !self.accumulated.is_empty()
    }

    /// Check if in any pending state
    pub fn is_pending(&self) -> bool {
        self.pending_type != PendingState::None
    }

    /// Check if display has timed out
    pub fn is_timed_out(&self) -> bool {
        if let Some(last_shown) = self.last_shown {
            last_shown.elapsed() >= KEYPRESS_DISPLAY_DURATION
        } else {
            false
        }
    }

    /// Check remaining time before timeout (for calloop timer)
    pub fn time_until_timeout(&self) -> Option<Duration> {
        self.last_shown.map(|t| {
            let elapsed = t.elapsed();
            if elapsed >= KEYPRESS_DISPLAY_DURATION {
                Duration::ZERO
            } else {
                KEYPRESS_DISPLAY_DURATION - elapsed
            }
        })
    }
}

impl Default for KeypressState {
    fn default() -> Self {
        Self::new()
    }
}
