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
}

impl PendingState {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::None,
            1 => Self::Getchar,
            2 => Self::Motion,
            3 => Self::TextObject,
            4 => Self::InsertRegister,
            5 => Self::NormalRegister,
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
    /// Completion candidates from nvim-cmp
    Candidates(CandidateInfo),
    /// Visual selection range (None = no visual selection)
    VisualRange(Option<VisualSelection>),
    /// Key was processed (acknowledgment for paths that send no data)
    KeyProcessed,
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
    pub fn new(text: String, cursor_begin: usize, cursor_end: usize, mode: String) -> Self {
        Self {
            text,
            cursor_begin,
            cursor_end,
            mode,
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
    /// Completion candidates (None when cmp not visible)
    pub candidates: Option<Vec<String>>,
    /// Selected candidate index (0-indexed, None when no selection)
    pub selected: Option<i32>,
    /// Visual selection start column (1-indexed byte offset, from Lua)
    #[serde(default)]
    pub visual_begin: Option<usize>,
    /// Visual selection end column (1-indexed byte offset, from Lua, exclusive)
    #[serde(default)]
    pub visual_end: Option<usize>,
}

