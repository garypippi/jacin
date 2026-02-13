//! Keypress display state
//!
//! Tracks accumulated key sequences for visual feedback during Vim-style input.

use std::time::{Duration, Instant};

use crate::neovim::PendingState;

/// Duration to show each keypress entry before auto-hide
pub const KEYPRESS_DISPLAY_DURATION: Duration = Duration::from_millis(1500);

/// Maximum number of display entries kept
const MAX_DISPLAY_ENTRIES: usize = 20;

/// A single keypress display entry with its timestamp
#[derive(Debug, Clone)]
struct KeypressEntry {
    text: String,
    added_at: Instant,
}

/// State for keypress display window
#[derive(Debug)]
pub struct KeypressState {
    /// Individual keypress entries with timestamps
    entries: Vec<KeypressEntry>,
    /// Pending mode type
    pub pending_type: PendingState,
    /// Current vim mode string (i, n, v, no, etc.)
    pub vim_mode: String,
    /// Currently recording macro register ("" when not recording)
    pub recording: String,
}

impl KeypressState {
    /// Create a new keypress state
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            pending_type: PendingState::None,
            vim_mode: String::new(),
            recording: String::new(),
        }
    }

    /// Push a key to the entries
    pub fn push_key(&mut self, key: &str) {
        self.entries.push(KeypressEntry {
            text: key.to_string(),
            added_at: Instant::now(),
        });
        // Trim oldest entries if over limit
        if self.entries.len() > MAX_DISPLAY_ENTRIES {
            let excess = self.entries.len() - MAX_DISPLAY_ENTRIES;
            self.entries.drain(..excess);
        }
    }

    /// Clear all entries and hide display
    pub fn clear(&mut self) {
        self.entries.clear();
        self.pending_type = PendingState::None;
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
    #[cfg(test)]
    pub fn is_normal_mode(&self) -> bool {
        self.vim_mode == "n" || self.vim_mode.starts_with("no")
    }

    /// Check if in visual mode
    #[cfg(test)]
    pub fn is_visual_mode(&self) -> bool {
        matches!(self.vim_mode.as_str(), "v" | "V" | "\x16")
            || self.vim_mode.starts_with('v')
            || self.vim_mode.starts_with('V')
    }

    /// Remove entries older than KEYPRESS_DISPLAY_DURATION.
    /// Returns true if any entries were removed.
    pub fn cleanup_expired(&mut self) -> bool {
        let before = self.entries.len();
        self.entries
            .retain(|e| e.added_at.elapsed() < KEYPRESS_DISPLAY_DURATION);
        self.entries.len() != before
    }

    /// Check if we should show the keypress display
    pub fn should_show(&self) -> bool {
        !self.entries.is_empty()
    }

    /// Build display text from all current entries
    pub fn display_text(&self) -> String {
        let mut s = String::new();
        for entry in &self.entries {
            s.push_str(&entry.text);
        }
        s
    }

    /// Set entries directly from text (for CmdlineUpdate/CmdlineMessage)
    pub fn set_display_text(&mut self, text: String) {
        self.entries.clear();
        self.entries.push(KeypressEntry {
            text,
            added_at: Instant::now(),
        });
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
        assert!(state.entries.is_empty());
        assert_eq!(state.pending_type, PendingState::None);
        assert!(state.vim_mode.is_empty());
        assert!(!state.should_show());
    }

    #[test]
    fn push_key_accumulates_and_shows_display() {
        let mut state = KeypressState::new();
        state.push_key("d");
        state.push_key("i");
        state.push_key("w");

        assert_eq!(state.display_text(), "diw");
        assert!(state.should_show());
    }

    #[test]
    fn clear_resets_display_state_but_keeps_recording() {
        let mut state = KeypressState::new();
        state.push_key("a");
        state.set_pending(PendingState::Motion);
        state.recording = "q".to_string();

        state.clear();

        assert_eq!(state.display_text(), "");
        assert_eq!(state.pending_type, PendingState::None);
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
        assert!(state.is_visual_mode());

        state.set_vim_mode("\x16");
        assert!(state.is_visual_mode());

        state.set_vim_mode("n");
        assert!(!state.is_visual_mode());
    }

    #[test]
    fn should_show_requires_non_empty_entries() {
        let mut state = KeypressState::new();
        assert!(!state.should_show());

        state.push_key("x");
        assert!(state.should_show());

        state.clear();
        assert!(!state.should_show());
    }

    #[test]
    fn cleanup_expired_removes_old_entries() {
        let mut state = KeypressState::new();
        // Insert an entry with a timestamp in the past
        state.entries.push(KeypressEntry {
            text: "old".to_string(),
            added_at: Instant::now() - KEYPRESS_DISPLAY_DURATION - Duration::from_millis(1),
        });
        state.push_key("new");

        assert_eq!(state.entries.len(), 2);
        let changed = state.cleanup_expired();
        assert!(changed);
        assert_eq!(state.entries.len(), 1);
        assert_eq!(state.display_text(), "new");
    }

    #[test]
    fn max_entries_trims_oldest() {
        let mut state = KeypressState::new();
        for i in 0..25 {
            state.push_key(&format!("{}", i % 10));
        }
        assert_eq!(state.entries.len(), MAX_DISPLAY_ENTRIES);
        // First entry should be the 6th push (index 5)
        assert_eq!(state.entries[0].text, "5");
    }

    #[test]
    fn set_display_text_replaces_entries() {
        let mut state = KeypressState::new();
        state.push_key("a");
        state.push_key("b");
        state.set_display_text(":wq".to_string());
        assert_eq!(state.display_text(), ":wq");
        assert_eq!(state.entries.len(), 1);
    }
}
