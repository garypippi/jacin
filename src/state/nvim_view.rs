//! Display state observed from Neovim
//!
//! Everything here mirrors Neovim (mode, command line, completion menu,
//! messages, screen grids, buffer lines). It is only updated from `FromNeovim`
//! messages or cleared on reset — never edited locally.

use std::time::{Duration, Instant};

use super::{BufferMirror, Screen};
use crate::neovim::protocol::PendingState;
use crate::neovim::{Mode, PumAnchor};

/// How long a transient message stays visible before auto-clearing (counted
/// from when the command line closes, if one is shown)
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
    /// Vim mode, from mode_change and nvim_get_mode (whichever came last)
    pub mode: Mode,
    /// Currently recording macro register ("" when not recording)
    pub recording: String,
    /// Completion candidates
    pub candidates: Vec<String>,
    /// Selected candidate index (None: nothing selected)
    pub selected_candidate: Option<usize>,
    /// The candidates complete the command line
    candidates_for_cmdline: bool,
    /// Where Neovim would draw the menu (None for the command line)
    pub candidates_anchor: Option<PumAnchor>,
    /// Active command line (None when not in command-line mode)
    pub cmdline: Option<Cmdline>,
    /// Pending command keys ('showcmd')
    pub showcmd: String,
    /// What Neovim waits for after the last processed key
    pub pending: PendingState,
    /// Message (e.g. command output, a prompt's question)
    pub transient_message: Option<String>,
    /// When the transient message was set
    transient_message_at: Option<Instant>,
    /// Mirror of Neovim's UI grids (rendered in the popup)
    pub screen: Screen,
    /// Mirror of the buffer lines (kept across `clear`, like `screen`)
    pub buffer: BufferMirror,
}

impl NvimView {
    /// Clear all observed display state except the vim mode
    /// (mode is re-established on enabling and by mode_change).
    pub fn clear(&mut self) {
        self.recording.clear();
        self.clear_candidates();
        self.cmdline = None;
        self.showcmd.clear();
        self.pending = PendingState::None;
        self.clear_transient_message();
    }

    pub fn set_candidates(
        &mut self,
        candidates: Vec<String>,
        selected: Option<usize>,
        cmdline: bool,
    ) {
        self.candidates = candidates;
        self.selected_candidate = selected;
        self.candidates_for_cmdline = cmdline;
    }

    pub fn set_candidates_anchor(&mut self, anchor: Option<PumAnchor>) {
        self.candidates_anchor = anchor;
    }

    pub fn clear_candidates(&mut self) {
        self.candidates.clear();
        self.selected_candidate = None;
        self.candidates_for_cmdline = false;
        self.candidates_anchor = None;
    }

    /// Pending command keys to show: only while Neovim waits for more keys
    /// (it also sends a complete command like `j`, cleared right after)
    pub fn visible_showcmd(&self) -> &str {
        if self.pending.is_pending() {
            &self.showcmd
        } else {
            ""
        }
    }

    /// Candidates to show: while the command line is shown, only its own
    /// completion (a buffer's completion menu can't be used from there, e.g.
    /// during a confirm() prompt)
    pub fn visible_candidates(&self) -> &[String] {
        if self.cmdline.is_some() && !self.candidates_for_cmdline {
            &[]
        } else {
            &self.candidates
        }
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
            // The message stays for its full duration from now
            if self.transient_message.is_some() {
                self.transient_message_at = Some(Instant::now());
            }
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
    /// Kept while the command line is shown: it may be the question of a
    /// prompt (confirm(), input()).
    pub fn expire_transient_message(&mut self) -> bool {
        if self.cmdline.is_none()
            && let Some(at) = self.transient_message_at
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
        view.set_candidates(vec!["a".into(), "b".into()], Some(1), false);
        assert_eq!(view.candidates.len(), 2);
        assert_eq!(view.selected_candidate, Some(1));

        view.clear_candidates();
        assert!(view.candidates.is_empty());
        assert_eq!(view.selected_candidate, None);
    }

    #[test]
    fn candidates_keep_the_message() {
        let mut view = NvimView::default();
        view.set_transient_message("msg".into());
        view.set_candidates(vec!["a".into()], None, false);
        assert!(view.has_transient_message());
    }

    #[test]
    fn buffer_candidates_are_hidden_while_the_cmdline_is_shown() {
        let mut view = NvimView::default();
        view.set_candidates(vec!["a".into()], None, false);
        view.show_cmdline("(Y)es, [N]o, : ", "", 0, 1);
        assert!(view.visible_candidates().is_empty());
        view.hide_cmdline(1);
        assert_eq!(view.visible_candidates(), ["a"]);

        view.show_cmdline(":", "e", 1, 1);
        view.set_candidates(vec!["edit".into()], None, true);
        assert_eq!(view.visible_candidates(), ["edit"]);
    }

    #[test]
    fn message_stays_while_the_cmdline_is_shown() {
        let expired = Instant::now() - TRANSIENT_MESSAGE_DURATION - Duration::from_millis(1);
        let mut view = NvimView::default();
        view.set_transient_message("Really?".into());
        view.show_cmdline("(Y)es, [N]o, : ", "", 0, 1);
        view.transient_message_at = Some(expired);
        assert!(!view.expire_transient_message());

        // Counted from when the command line closes
        view.hide_cmdline(1);
        assert!(!view.expire_transient_message());
        view.transient_message_at = Some(expired);
        assert!(view.expire_transient_message());
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
    fn clear_keeps_mode() {
        let mut view = NvimView {
            mode: Mode::Visual,
            ..Default::default()
        };
        view.recording = "q".into();
        view.set_candidates(vec!["a".into()], Some(0), false);
        view.show_cmdline(":", "set", 3, 1);
        view.showcmd = "2d".into();

        view.clear();
        assert_eq!(view.mode, Mode::Visual);
        assert!(view.recording.is_empty());
        assert!(view.candidates.is_empty());
        assert!(view.cmdline.is_none());
        assert!(view.showcmd.is_empty());
    }

    #[test]
    fn show_cmdline_sets_text_and_cursor() {
        let mut view = NvimView::default();
        view.show_cmdline(":", "hello", 2, 1);
        let c = view.cmdline.as_ref().unwrap();
        assert_eq!(c.text, ":hello");
        assert_eq!(c.cursor_byte, 3);
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
