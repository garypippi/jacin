//! Keypress display state
//!
//! Tracks accumulated key sequences for visual feedback during Vim-style input.

use std::time::{Duration, Instant};

use crate::neovim::PendingState;

/// Duration of inactivity before all keypress entries are cleared
pub const KEYPRESS_DISPLAY_DURATION: Duration = Duration::from_millis(1500);

/// Maximum number of display entries kept
const MAX_DISPLAY_ENTRIES: usize = 20;

/// A single keypress display entry
#[derive(Debug, Clone)]
pub struct KeypressEntry {
    pub text: String,
}

/// State for keypress display window
#[derive(Debug)]
pub struct KeypressState {
    entries: Vec<KeypressEntry>,
    /// Timestamp of the last entry addition (None when empty)
    last_added_at: Option<Instant>,
    /// Pending state reported by the last processed key
    pub pending_type: PendingState,
}

impl KeypressState {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            last_added_at: None,
            pending_type: PendingState::None,
        }
    }

    pub fn push_key(&mut self, key: &str) {
        self.entries.push(KeypressEntry {
            text: key.to_string(),
        });
        self.last_added_at = Some(Instant::now());
        if self.entries.len() > MAX_DISPLAY_ENTRIES {
            let excess = self.entries.len() - MAX_DISPLAY_ENTRIES;
            self.entries.drain(..excess);
        }
    }

    /// Clear all entries and hide display
    pub fn clear(&mut self) {
        self.entries.clear();
        self.last_added_at = None;
        self.pending_type = PendingState::None;
    }

    pub fn set_pending(&mut self, pending_type: PendingState) {
        self.pending_type = pending_type;
    }

    /// Clear all entries if no new entries have been added within KEYPRESS_DISPLAY_DURATION.
    /// Returns true if entries were cleared.
    pub fn cleanup_inactive(&mut self) -> bool {
        if let Some(last) = self.last_added_at
            && last.elapsed() >= KEYPRESS_DISPLAY_DURATION
            && !self.entries.is_empty()
        {
            self.entries.clear();
            self.last_added_at = None;
            return true;
        }
        false
    }

    /// Check if we should show the keypress display
    pub fn should_show(&self) -> bool {
        !self.entries.is_empty()
    }

    pub fn entries(&self) -> &[KeypressEntry] {
        &self.entries
    }

    /// Build display text from all current entries (for tests)
    #[cfg(test)]
    pub fn display_text(&self) -> String {
        let mut s = String::new();
        for entry in &self.entries {
            s.push_str(&entry.text);
        }
        s
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
    fn clear_resets_display_state() {
        let mut state = KeypressState::new();
        state.push_key("a");
        state.set_pending(PendingState::Motion);

        state.clear();

        assert_eq!(state.display_text(), "");
        assert_eq!(state.pending_type, PendingState::None);
        assert!(!state.should_show());
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
    fn cleanup_inactive_clears_after_timeout() {
        let mut state = KeypressState::new();
        state.push_key("old");
        // Simulate time passing by backdating last_added_at
        state.last_added_at =
            Some(Instant::now() - KEYPRESS_DISPLAY_DURATION - Duration::from_millis(1));

        assert!(state.should_show());
        let changed = state.cleanup_inactive();
        assert!(changed);
        assert!(!state.should_show());
    }

    #[test]
    fn cleanup_inactive_keeps_recent_entries() {
        let mut state = KeypressState::new();
        state.push_key("new");

        let changed = state.cleanup_inactive();
        assert!(!changed);
        assert!(state.should_show());
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
}
