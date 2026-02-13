//! IME state machine
//!
//! Explicit state machine for IME mode transitions, replacing scattered boolean flags.

use std::time::{Duration, Instant};

/// Main IME mode state machine
#[derive(Debug, Clone, PartialEq, Default)]
pub enum ImeMode {
    /// IME is disabled, keyboard not grabbed, passthrough mode
    #[default]
    Disabled,
    /// IME is being enabled, waiting for keymap
    Enabling,
    /// IME is fully enabled and processing input
    Enabled {
        /// Current Vim editing mode
        vim_mode: VimMode,
    },
}

/// Vim editing mode within the IME
#[derive(Debug, Clone, PartialEq, Default)]
pub enum VimMode {
    /// Insert mode - characters inserted at cursor
    #[default]
    Insert,
    /// Normal mode - commands and motions
    Normal,
}

/// How long a transient message stays visible before auto-clearing
pub const TRANSIENT_MESSAGE_DURATION: Duration = Duration::from_millis(2000);

/// IME state including mode, preedit, and candidates
pub struct ImeState {
    /// Current IME mode
    pub mode: ImeMode,
    /// Current preedit text
    pub preedit: String,
    /// Cursor begin position (byte offset)
    pub cursor_begin: usize,
    /// Cursor end position (byte offset)
    pub cursor_end: usize,
    /// Completion candidates
    pub candidates: Vec<String>,
    /// Selected candidate index
    pub selected_candidate: usize,
    /// Transient message shown in candidate area (e.g., command output)
    pub transient_message: Option<String>,
    /// When the transient message was set
    transient_message_at: Option<Instant>,
}

impl ImeState {
    /// Create new IME state
    pub fn new() -> Self {
        Self {
            mode: ImeMode::Disabled,
            preedit: String::new(),
            cursor_begin: 0,
            cursor_end: 0,
            candidates: Vec::new(),
            selected_candidate: 0,
            transient_message: None,
            transient_message_at: None,
        }
    }

    /// Set a transient message to display in the candidate area
    pub fn set_transient_message(&mut self, text: String) {
        self.transient_message = Some(text);
        self.transient_message_at = Some(Instant::now());
    }

    /// Clear the transient message
    pub fn clear_transient_message(&mut self) {
        self.transient_message = None;
        self.transient_message_at = None;
    }

    /// Check if the transient message has expired and clear it if so.
    /// Returns true if the message was cleared.
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

    /// Check if IME is enabled (or enabling)
    pub fn is_enabled(&self) -> bool {
        matches!(self.mode, ImeMode::Enabled { .. } | ImeMode::Enabling)
    }

    /// Check if IME is fully enabled (not transitioning)
    pub fn is_fully_enabled(&self) -> bool {
        matches!(self.mode, ImeMode::Enabled { .. })
    }

    /// Start enabling the IME
    pub fn start_enabling(&mut self) {
        self.mode = ImeMode::Enabling;
    }

    /// Complete enabling (keymap received). Returns true if transitioned from Enabling.
    pub fn complete_enabling(&mut self, initial_mode: VimMode) -> bool {
        if self.mode == ImeMode::Enabling {
            self.mode = ImeMode::Enabled {
                vim_mode: initial_mode,
            };
            true
        } else {
            false
        }
    }

    /// Disable immediately (for toggle off)
    pub fn disable(&mut self) {
        self.mode = ImeMode::Disabled;
        self.clear_preedit();
        self.clear_transient_message();
    }

    /// Update preedit
    pub fn set_preedit(&mut self, text: String, cursor_begin: usize, cursor_end: usize) {
        self.preedit = text;
        self.cursor_begin = cursor_begin;
        self.cursor_end = cursor_end;
    }

    /// Clear preedit
    pub fn clear_preedit(&mut self) {
        self.preedit.clear();
        self.cursor_begin = 0;
        self.cursor_end = 0;
    }

    /// Update candidates (clears any transient message — candidates take priority)
    pub fn set_candidates(&mut self, candidates: Vec<String>, selected: usize) {
        self.candidates = candidates;
        self.selected_candidate = selected;
        if !self.candidates.is_empty() {
            self.clear_transient_message();
        }
    }

    /// Clear candidates
    pub fn clear_candidates(&mut self) {
        self.candidates.clear();
        self.selected_candidate = 0;
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

        let transitioned = state.complete_enabling(VimMode::Insert);
        assert!(transitioned);
        assert!(state.is_enabled());
        assert!(state.is_fully_enabled());
    }

    #[test]
    fn complete_enabling_only_from_enabling() {
        let mut state = ImeState::new();
        // complete_enabling from Disabled should not transition
        let transitioned = state.complete_enabling(VimMode::Insert);
        assert!(!transitioned);
        assert!(!state.is_enabled());
    }

    #[test]
    fn complete_enabling_sets_requested_vim_mode() {
        let mut state = ImeState::new();
        state.start_enabling();

        let transitioned = state.complete_enabling(VimMode::Normal);
        assert!(transitioned);
        assert_eq!(
            state.mode,
            ImeMode::Enabled {
                vim_mode: VimMode::Normal,
            }
        );
    }

    #[test]
    fn complete_enabling_from_enabled_does_not_override_mode() {
        let mut state = ImeState::new();
        state.start_enabling();
        assert!(state.complete_enabling(VimMode::Insert));

        let transitioned = state.complete_enabling(VimMode::Normal);
        assert!(!transitioned);
        assert_eq!(
            state.mode,
            ImeMode::Enabled {
                vim_mode: VimMode::Insert,
            }
        );
    }

    #[test]
    fn disable_clears_preedit() {
        let mut state = ImeState::new();
        state.start_enabling();
        state.complete_enabling(VimMode::Insert);
        state.set_preedit("hello".into(), 0, 5);

        state.disable();
        assert!(!state.is_enabled());
        assert!(state.preedit.is_empty());
        assert_eq!(state.cursor_begin, 0);
        assert_eq!(state.cursor_end, 0);
    }

    #[test]
    fn preedit_operations() {
        let mut state = ImeState::new();
        state.set_preedit("test".into(), 1, 3);
        assert_eq!(state.preedit, "test");
        assert_eq!(state.cursor_begin, 1);
        assert_eq!(state.cursor_end, 3);

        state.clear_preedit();
        assert!(state.preedit.is_empty());
        assert_eq!(state.cursor_begin, 0);
    }

    #[test]
    fn candidate_operations() {
        let mut state = ImeState::new();
        state.set_candidates(vec!["a".into(), "b".into()], 1);
        assert_eq!(state.candidates.len(), 2);
        assert_eq!(state.selected_candidate, 1);

        state.clear_candidates();
        assert!(state.candidates.is_empty());
        assert_eq!(state.selected_candidate, 0);
    }
}
