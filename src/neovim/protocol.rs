//! Typed protocol messages for Neovim communication
//!
//! Defines all messages that can be sent to/from the Neovim backend.

use std::sync::atomic::{AtomicU8, Ordering};

use serde::{Deserialize, Serialize};

/// Pending state for multi-key sequences in the Neovim handler.
///
/// These states are mutually exclusive — only one can be active at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PendingState {
    /// No pending operation
    None = 0,
    /// Neovim blocked in getchar (after q, f, t, r, m, etc.)
    Getchar = 1,
    /// Operator pending, waiting for motion (after d, c, y, etc.)
    Motion = 2,
    /// Operator pending after i/a, waiting for text object char
    TextObject = 3,
    /// Insert mode <C-r>, waiting for register name
    InsertRegister = 4,
    /// Normal mode " prefix, waiting for register name
    NormalRegister = 5,
    /// In command-line mode (after typing :)
    CommandLine = 6,
}

impl PendingState {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Getchar,
            2 => Self::Motion,
            3 => Self::TextObject,
            4 => Self::InsertRegister,
            5 => Self::NormalRegister,
            6 => Self::CommandLine,
            _ => Self::None,
        }
    }

    /// Check if any pending state is active
    pub fn is_pending(self) -> bool {
        self != Self::None
    }

    /// Check if in a motion-pending state (Motion or TextObject)
    pub fn is_motion(self) -> bool {
        matches!(self, Self::Motion | Self::TextObject)
    }

    /// Check if in a register-pending state (InsertRegister or NormalRegister)
    pub fn is_register(self) -> bool {
        matches!(self, Self::InsertRegister | Self::NormalRegister)
    }
}

/// Atomic wrapper around `PendingState` for cross-thread sharing.
pub struct AtomicPendingState(AtomicU8);

impl AtomicPendingState {
    /// Create with `PendingState::None`.
    pub const fn new() -> Self {
        Self(AtomicU8::new(PendingState::None as u8))
    }

    /// Load the current pending state.
    pub fn load(&self) -> PendingState {
        PendingState::from_u8(self.0.load(Ordering::SeqCst))
    }

    /// Store a new pending state.
    pub fn store(&self, state: PendingState) {
        self.0.store(state as u8, Ordering::SeqCst);
    }

    /// Clear to `PendingState::None`.
    pub fn clear(&self) {
        self.store(PendingState::None);
    }
}

/// Messages sent from IME to Neovim
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToNeovim {
    /// Send a key to Neovim (raw key string like "a", "A", "<BS>", "<CR>")
    Key(String),
    /// Shutdown Neovim
    Shutdown,
}

/// Visual selection range from Neovim
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum VisualSelection {
    /// Character-wise visual selection with 0-indexed byte offsets (exclusive end)
    Charwise { begin: usize, end: usize },
}

/// Messages sent from Neovim to IME
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FromNeovim {
    /// Neovim is ready
    Ready,
    /// Preedit text changed
    Preedit(PreeditInfo),
    /// Text should be committed
    Commit(String),
    /// Delete surrounding text (before_length, after_length)
    DeleteSurrounding { before: u32, after: u32 },
    /// Completion candidates from Neovim's popup menu
    Candidates(CandidateInfo),
    /// Visual selection range (None = no visual selection)
    VisualRange(Option<VisualSelection>),
    /// Key was processed (acknowledgment for paths that send no data)
    KeyProcessed,
    /// Command-line text update (display in keypress area)
    CmdlineUpdate(String),
    /// Command-line left (executed or cancelled)
    CmdlineCancelled,
    /// Text auto-committed due to line addition (context break)
    AutoCommit(String),
    /// Command output message (e.g., from :s/foo/bar/g)
    CmdlineMessage(String),
    /// Key should be passed through to the application via virtual keyboard
    PassthroughKey,
    /// Neovim process exited (e.g., :q)
    NvimExited,
}

/// Preedit information
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PreeditInfo {
    /// The preedit text
    pub text: String,
    /// Cursor begin position (byte offset)
    /// cursor_begin == cursor_end for line cursor (insert mode)
    pub cursor_begin: usize,
    /// Cursor end position (byte offset)
    /// cursor_begin < cursor_end for block cursor (normal mode)
    pub cursor_end: usize,
    /// Current vim mode (i, n, v, no, etc.)
    pub mode: String,
    /// Currently recording macro register ("" when not recording)
    pub recording: String,
}

/// Candidate information
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CandidateInfo {
    /// List of candidate words
    pub candidates: Vec<String>,
    /// Currently selected index
    pub selected: usize,
}

impl PreeditInfo {
    /// Create new preedit info
    pub fn new(
        text: String,
        cursor_begin: usize,
        cursor_end: usize,
        mode: String,
        recording: String,
    ) -> Self {
        Self {
            text,
            cursor_begin,
            cursor_end,
            mode,
            recording,
        }
    }

    /// Create empty preedit
    pub fn empty() -> Self {
        Self::default()
    }
}

impl CandidateInfo {
    /// Create new candidate info
    pub fn new(candidates: Vec<String>, selected: usize) -> Self {
        Self {
            candidates,
            selected,
        }
    }

    /// Create empty candidate info
    pub fn empty() -> Self {
        Self::default()
    }
}

/// State snapshot from collect_snapshot() Lua function.
/// Consolidates all state queries into a single RPC call.
#[derive(Debug, Clone, Deserialize)]
pub struct Snapshot {
    /// Current line text (preedit)
    pub preedit: String,
    /// Cursor byte position (1-indexed, from col('.'))
    pub cursor_byte: usize,
    /// Vim mode string ("i", "n", "no", "v", "c", etc.)
    pub mode: String,
    /// Whether Neovim is blocked in getchar
    pub blocking: bool,
    /// Character width under cursor (normal/visual mode only, 0 otherwise)
    #[serde(default)]
    pub char_width: usize,
    /// Visual selection start column (1-indexed byte offset, from Lua)
    #[serde(default)]
    pub visual_begin: Option<usize>,
    /// Visual selection end column (1-indexed byte offset, from Lua, exclusive)
    #[serde(default)]
    pub visual_end: Option<usize>,
    /// Currently recording macro register ("" when not recording)
    #[serde(default)]
    pub recording: String,
}

impl Snapshot {
    /// Convert snapshot to PreeditInfo (shared by push and pull paths).
    /// Translates 1-indexed Lua cursor to 0-indexed byte offsets.
    pub fn to_preedit_info(&self) -> PreeditInfo {
        let cursor_begin = self.cursor_byte.saturating_sub(1);
        let cursor_end = if self.char_width > 0 {
            cursor_begin + self.char_width
        } else {
            cursor_begin
        };
        PreeditInfo::new(
            self.preedit.clone(),
            cursor_begin,
            cursor_end,
            self.mode.clone(),
            self.recording.clone(),
        )
    }

    /// Convert visual fields to VisualSelection (1-indexed Lua → 0-indexed byte offsets).
    pub fn to_visual_selection(&self) -> Option<VisualSelection> {
        match (self.visual_begin, self.visual_end) {
            (Some(begin), Some(end)) => Some(VisualSelection::Charwise {
                begin: begin.saturating_sub(1),
                end: end.saturating_sub(1),
            }),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_state_classification() {
        assert!(!PendingState::None.is_pending());
        assert!(PendingState::Getchar.is_pending());
        assert!(PendingState::Motion.is_pending());

        assert!(PendingState::Motion.is_motion());
        assert!(PendingState::TextObject.is_motion());
        assert!(!PendingState::Getchar.is_motion());

        assert!(PendingState::InsertRegister.is_register());
        assert!(PendingState::NormalRegister.is_register());
        assert!(!PendingState::Motion.is_register());
    }

    #[test]
    fn pending_state_roundtrip() {
        for v in 0..=6u8 {
            let state = PendingState::from_u8(v);
            assert_eq!(state as u8, v);
        }
        // Out-of-range maps to None
        assert_eq!(PendingState::from_u8(255), PendingState::None);
    }

    #[test]
    fn atomic_pending_state() {
        let atomic = AtomicPendingState::new();
        assert_eq!(atomic.load(), PendingState::None);

        atomic.store(PendingState::Motion);
        assert_eq!(atomic.load(), PendingState::Motion);

        atomic.clear();
        assert_eq!(atomic.load(), PendingState::None);
    }

    fn make_snapshot(cursor_byte: usize, char_width: usize, mode: &str) -> Snapshot {
        Snapshot {
            preedit: "hello".into(),
            cursor_byte,
            mode: mode.into(),
            blocking: false,
            char_width,
            visual_begin: None,
            visual_end: None,
            recording: String::new(),
        }
    }

    #[test]
    fn snapshot_to_preedit_insert_mode() {
        // Insert mode: cursor_byte=3, char_width=0 → line cursor at byte 2
        let snap = make_snapshot(3, 0, "i");
        let info = snap.to_preedit_info();
        assert_eq!(info.cursor_begin, 2);
        assert_eq!(info.cursor_end, 2); // Line cursor: begin == end
        assert_eq!(info.text, "hello");
        assert_eq!(info.mode, "i");
    }

    #[test]
    fn snapshot_to_preedit_normal_mode() {
        // Normal mode: cursor_byte=3, char_width=3 (e.g., multibyte char) → block cursor
        let snap = make_snapshot(3, 3, "n");
        let info = snap.to_preedit_info();
        assert_eq!(info.cursor_begin, 2);
        assert_eq!(info.cursor_end, 5); // Block cursor: begin + char_width
    }

    #[test]
    fn snapshot_to_preedit_cursor_at_start() {
        // cursor_byte=1 (first byte) → 0-indexed = 0
        let snap = make_snapshot(1, 1, "n");
        let info = snap.to_preedit_info();
        assert_eq!(info.cursor_begin, 0);
        assert_eq!(info.cursor_end, 1);
    }

    #[test]
    fn snapshot_to_visual_selection() {
        let mut snap = make_snapshot(3, 1, "v");
        snap.visual_begin = Some(2);
        snap.visual_end = Some(5);
        let sel = snap.to_visual_selection().unwrap();
        match sel {
            VisualSelection::Charwise { begin, end } => {
                assert_eq!(begin, 1); // 2 - 1 = 1
                assert_eq!(end, 4); // 5 - 1 = 4
            }
        }
    }

    #[test]
    fn snapshot_no_visual() {
        let snap = make_snapshot(1, 0, "n");
        assert!(snap.to_visual_selection().is_none());
    }
}
