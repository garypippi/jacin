//! Display state observed from Neovim
//!
//! Everything here mirrors Neovim (mode, command line, completion menu,
//! messages, visual selection). It is only updated from `FromNeovim`
//! messages or cleared on reset — never edited locally.

use std::time::{Duration, Instant};

use super::{BufferMirror, Screen};
use crate::neovim::VisualSelection;

/// How long a transient message stays visible before auto-clearing
pub const TRANSIENT_MESSAGE_DURATION: Duration = Duration::from_millis(2000);

/// Active command line (from ext_cmdline)
#[derive(Debug, Clone, PartialEq)]
pub struct Cmdline {
    /// Display text: prefix (firstc or prompt) + content
    pub text: String,
    /// Cursor byte offset within `text`
    pub cursor_byte: usize,
    /// Byte length of the prefix (firstc or prompt)
    prefix_len: usize,
    /// Nesting level (cmdline_show/pos/hide are ignored for other levels)
    level: u64,
}

/// Display state observed from Neovim
#[derive(Debug, Default)]
pub struct NvimView {
    /// Current vim mode string (i, n, v, no, c, etc.)
    pub vim_mode: String,
    /// Currently recording macro register ("" when not recording)
    pub recording: String,
    /// Visual selection range (None outside visual mode)
    pub visual: Option<VisualSelection>,
    /// Completion candidates
    pub candidates: Vec<String>,
    /// Selected candidate index
    pub selected_candidate: usize,
    /// Active command line (None when not in command-line mode)
    pub cmdline: Option<Cmdline>,
    /// Transient message shown in candidate area (e.g., command output)
    pub transient_message: Option<String>,
    /// When the transient message was set
    transient_message_at: Option<Instant>,
    /// Mirror of Neovim's UI grids (Phase A: shadow only, not rendered)
    pub screen: Screen,
    /// Mirror of the buffer lines (kept across `clear`, like `screen`)
    pub buffer: BufferMirror,
}

impl NvimView {
    /// Clear all observed display state except the vim mode
    /// (mode is re-established on enabling and by mode_change).
    pub fn clear(&mut self) {
        self.recording.clear();
        self.visual = None;
        self.clear_candidates();
        self.cmdline = None;
        self.clear_transient_message();
    }

    pub fn set_vim_mode(&mut self, mode: &str) {
        self.vim_mode = mode.to_string();
    }

    /// Update candidates (clears any transient message — candidates take priority)
    pub fn set_candidates(&mut self, candidates: Vec<String>, selected: usize) {
        self.candidates = candidates;
        self.selected_candidate = selected;
        if !self.candidates.is_empty() {
            self.clear_transient_message();
        }
    }

    pub fn clear_candidates(&mut self) {
        self.candidates.clear();
        self.selected_candidate = 0;
    }

    /// Show the command line: `prefix` is the prompt (input()) or firstc (:, /, ?),
    /// `pos` is the cursor byte offset within `content`.
    pub fn show_cmdline(&mut self, prefix: &str, content: &str, pos: usize, level: u64) {
        let text = format!("{prefix}{content}");
        let cursor_byte = (prefix.len() + pos).min(text.len());
        self.cmdline = Some(Cmdline {
            text,
            cursor_byte,
            prefix_len: prefix.len(),
            level,
        });
        self.set_vim_mode("c");
    }

    /// Update command-line cursor position. Returns true if updated.
    pub fn update_cmdline_cursor(&mut self, pos: usize, level: u64) -> bool {
        match self.cmdline.as_mut() {
            Some(c) if c.level == level => {
                c.cursor_byte = (c.prefix_len + pos).min(c.text.len());
                true
            }
            _ => false,
        }
    }

    /// Hide the command line only if the level matches. Returns true if hidden.
    pub fn hide_cmdline(&mut self, level: u64) -> bool {
        if self.cmdline.as_ref().is_some_and(|c| c.level == level) {
            self.cmdline = None;
            true
        } else {
            false
        }
    }

    pub fn set_transient_message(&mut self, text: String) {
        self.transient_message = Some(text);
        self.transient_message_at = Some(Instant::now());
    }

    pub fn clear_transient_message(&mut self) {
        self.transient_message = None;
        self.transient_message_at = None;
    }

    /// Clear the transient message if it has expired. Returns true if cleared.
    pub fn expire_transient_message(&mut self) -> bool {
        if let Some(at) = self.transient_message_at
            && at.elapsed() >= TRANSIENT_MESSAGE_DURATION
        {
            self.clear_transient_message();
            return true;
        }
        false
    }

    /// Whether a transient message is active (for timer scheduling)
    pub fn has_transient_message(&self) -> bool {
        self.transient_message.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_operations() {
        let mut view = NvimView::default();
        view.set_candidates(vec!["a".into(), "b".into()], 1);
        assert_eq!(view.candidates.len(), 2);
        assert_eq!(view.selected_candidate, 1);

        view.clear_candidates();
        assert!(view.candidates.is_empty());
        assert_eq!(view.selected_candidate, 0);
    }

    #[test]
    fn candidates_clear_transient_message() {
        let mut view = NvimView::default();
        view.set_transient_message("msg".into());
        view.set_candidates(vec!["a".into()], 0);
        assert!(!view.has_transient_message());
    }

    #[test]
    fn transient_message_expires() {
        let mut view = NvimView::default();
        view.set_transient_message("msg".into());
        assert!(!view.expire_transient_message());

        view.transient_message_at =
            Some(Instant::now() - TRANSIENT_MESSAGE_DURATION - Duration::from_millis(1));
        assert!(view.expire_transient_message());
        assert!(!view.has_transient_message());
    }

    #[test]
    fn clear_keeps_vim_mode() {
        let mut view = NvimView::default();
        view.set_vim_mode("n");
        view.recording = "q".into();
        view.set_candidates(vec!["a".into()], 0);
        view.show_cmdline(":", "set", 3, 1);
        view.set_vim_mode("n");

        view.clear();
        assert_eq!(view.vim_mode, "n");
        assert!(view.recording.is_empty());
        assert!(view.candidates.is_empty());
        assert!(view.cmdline.is_none());
    }

    #[test]
    fn show_cmdline_sets_text_cursor_and_mode() {
        let mut view = NvimView::default();
        view.show_cmdline(":", "hello", 2, 1);
        let c = view.cmdline.as_ref().unwrap();
        assert_eq!(c.text, ":hello");
        assert_eq!(c.cursor_byte, 3);
        assert_eq!(view.vim_mode, "c");
    }

    #[test]
    fn show_cmdline_clamps_cursor_to_text_len() {
        let mut view = NvimView::default();
        view.show_cmdline(":", "ab", 100, 1);
        assert_eq!(view.cmdline.unwrap().cursor_byte, 3); // clamped to ":ab".len()
    }

    #[test]
    fn cmdline_cursor_with_multibyte_prompt() {
        let mut view = NvimView::default();
        // Prompt "辞書登録: " is 14 bytes in UTF-8 (4×3 + 1 + 1)
        view.show_cmdline("辞書登録: ", "test", 2, 1);
        assert_eq!(view.cmdline.unwrap().cursor_byte, 16); // 14 + 2
    }

    #[test]
    fn update_cmdline_cursor_with_matching_level() {
        let mut view = NvimView::default();
        view.show_cmdline(":", "hello", 0, 1);
        assert!(view.update_cmdline_cursor(3, 1));
        assert_eq!(view.cmdline.unwrap().cursor_byte, 4); // prefix(1) + 3
    }

    #[test]
    fn update_cmdline_cursor_ignores_level_mismatch() {
        let mut view = NvimView::default();
        view.show_cmdline(":", "hello", 0, 1);
        assert!(!view.update_cmdline_cursor(3, 2));
        assert_eq!(view.cmdline.unwrap().cursor_byte, 1);
    }

    #[test]
    fn update_cmdline_cursor_clamps_to_text_len() {
        let mut view = NvimView::default();
        view.show_cmdline(":", "ab", 0, 1);
        assert!(view.update_cmdline_cursor(100, 1));
        assert_eq!(view.cmdline.unwrap().cursor_byte, 3);
    }

    #[test]
    fn hide_cmdline_respects_level() {
        let mut view = NvimView::default();
        view.show_cmdline(":", "x", 1, 1);
        assert!(!view.hide_cmdline(2));
        assert!(view.cmdline.is_some());
        assert!(view.hide_cmdline(1));
        assert!(view.cmdline.is_none());
    }
}
