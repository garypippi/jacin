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
    /// Individual keypress entries
    entries: Vec<KeypressEntry>,
    /// Timestamp of the last entry addition (None when empty)
    last_added_at: Option<Instant>,
    /// Pending mode type
    pub pending_type: PendingState,
    /// Current vim mode string (i, n, v, no, etc.)
    pub vim_mode: String,
    /// Currently recording macro register ("" when not recording)
    pub recording: String,
    /// Command-line cursor byte offset within display_text (None when not in cmdline)
    cmdline_cursor_byte: Option<usize>,
    /// Byte length of command-line prefix (firstc or prompt)
    cmdline_prefix_len: usize,
    /// Active command-line level for guard (None when not in cmdline)
    cmdline_level: Option<u64>,
}

impl KeypressState {
    /// Create a new keypress state
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            last_added_at: None,
            pending_type: PendingState::None,
            vim_mode: String::new(),
            recording: String::new(),
            cmdline_cursor_byte: None,
            cmdline_prefix_len: 0,
            cmdline_level: None,
        }
    }

    /// Push a key to the entries
    pub fn push_key(&mut self, key: &str) {
        self.entries.push(KeypressEntry {
            text: key.to_string(),
        });
        self.last_added_at = Some(Instant::now());
        // Trim oldest entries if over limit
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
        self.cmdline_cursor_byte = None;
        self.cmdline_prefix_len = 0;
        self.cmdline_level = None;
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

    /// Clear all entries if no new entries have been added within KEYPRESS_DISPLAY_DURATION.
    /// Skips clearing in command-line mode (display is managed by CmdlineShow).
    /// Returns true if entries were cleared.
    pub fn cleanup_inactive(&mut self) -> bool {
        if self.vim_mode.starts_with('c') {
            return false;
        }
        if let Some(last) = self.last_added_at {
            if last.elapsed() >= KEYPRESS_DISPLAY_DURATION && !self.entries.is_empty() {
                self.entries.clear();
                self.last_added_at = None;
                return true;
            }
        }
        false
    }

    /// Check if we should show the keypress display
    pub fn should_show(&self) -> bool {
        !self.entries.is_empty()
    }

    /// Get entries for rendering
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

    /// Set command-line text with cursor position
    pub fn set_cmdline_text(
        &mut self,
        text: String,
        cursor_byte: usize,
        prefix_len: usize,
        level: u64,
    ) {
        let clamped = cursor_byte.min(text.len());
        self.entries.clear();
        self.entries.push(KeypressEntry { text });
        self.last_added_at = Some(Instant::now());
        self.cmdline_cursor_byte = Some(clamped);
        self.cmdline_prefix_len = prefix_len;
        self.cmdline_level = Some(level);
    }

    /// Update command-line cursor position. Returns true if updated.
    pub fn update_cmdline_cursor(&mut self, pos: usize, level: u64) -> bool {
        if self.cmdline_level != Some(level) {
            return false;
        }
        let display_len = self
            .entries
            .first()
            .map(|e| e.text.len())
            .unwrap_or(0);
        self.cmdline_cursor_byte = Some((self.cmdline_prefix_len + pos).min(display_len));
        true
    }

    /// Clear cmdline state only if the level matches. Returns true if cleared.
    pub fn clear_cmdline_if_level(&mut self, level: u64) -> bool {
        if self.cmdline_level != Some(level) {
            return false;
        }
        self.clear();
        true
    }

    /// Get command-line cursor byte offset (within display_text)
    pub fn cmdline_cursor_byte(&self) -> Option<usize> {
        self.cmdline_cursor_byte
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

    #[test]
    fn set_cmdline_text_stores_cursor_and_level() {
        let mut state = KeypressState::new();
        state.set_cmdline_text(":hello".to_string(), 3, 1, 1);
        assert_eq!(state.display_text(), ":hello");
        assert_eq!(state.cmdline_cursor_byte(), Some(3));
        assert_eq!(state.cmdline_level, Some(1));
        assert_eq!(state.cmdline_prefix_len, 1);
    }

    #[test]
    fn set_cmdline_text_clamps_cursor_to_text_len() {
        let mut state = KeypressState::new();
        state.set_cmdline_text(":ab".to_string(), 100, 1, 1);
        assert_eq!(state.cmdline_cursor_byte(), Some(3)); // clamped to ":ab".len()
    }

    #[test]
    fn update_cmdline_cursor_with_matching_level() {
        let mut state = KeypressState::new();
        // ":hello" — prefix ":" is 1 byte
        state.set_cmdline_text(":hello".to_string(), 1, 1, 1);
        assert_eq!(state.cmdline_cursor_byte(), Some(1));

        // Move cursor to pos=3 within content → prefix_len(1) + 3 = 4
        let updated = state.update_cmdline_cursor(3, 1);
        assert!(updated);
        assert_eq!(state.cmdline_cursor_byte(), Some(4));
    }

    #[test]
    fn update_cmdline_cursor_ignores_level_mismatch() {
        let mut state = KeypressState::new();
        state.set_cmdline_text(":hello".to_string(), 1, 1, 1);

        let updated = state.update_cmdline_cursor(3, 2); // wrong level
        assert!(!updated);
        assert_eq!(state.cmdline_cursor_byte(), Some(1)); // unchanged
    }

    #[test]
    fn update_cmdline_cursor_clamps_to_display_len() {
        let mut state = KeypressState::new();
        state.set_cmdline_text(":ab".to_string(), 1, 1, 1);

        let updated = state.update_cmdline_cursor(100, 1);
        assert!(updated);
        assert_eq!(state.cmdline_cursor_byte(), Some(3)); // clamped to ":ab".len()
    }

    #[test]
    fn clear_resets_cmdline_fields() {
        let mut state = KeypressState::new();
        state.set_cmdline_text(":hello".to_string(), 3, 1, 1);

        state.clear();
        assert_eq!(state.cmdline_cursor_byte(), None);
        assert_eq!(state.cmdline_prefix_len, 0);
        assert_eq!(state.cmdline_level, None);
    }

    #[test]
    fn cmdline_cursor_with_multibyte_prefix() {
        let mut state = KeypressState::new();
        // Prompt "辞書登録: " is 14 bytes in UTF-8 (4×3 + 1 + 1)
        let prompt = "辞書登録: ";
        assert_eq!(prompt.len(), 14);
        let content = "test";
        let display = format!("{}{}", prompt, content);
        let prefix_len = prompt.len();
        let pos = 2; // cursor at byte 2 within content
        state.set_cmdline_text(display, prefix_len + pos, prefix_len, 1);
        assert_eq!(state.cmdline_cursor_byte(), Some(16)); // 14 + 2
    }

}
