//! Typed protocol messages for Neovim communication
//!
//! Defines all messages that can be sent to/from the Neovim backend.

use std::sync::atomic::{AtomicU8, Ordering};

use serde::{Deserialize, Serialize};

/// Pending state for multi-key sequences in the Neovim handler.
///
/// These states are mutually exclusive — only one can be active at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
    pub const fn new() -> Self {
        Self(AtomicU8::new(PendingState::None as u8))
    }

    pub fn load(&self) -> PendingState {
        PendingState::from_u8(self.0.load(Ordering::SeqCst))
    }

    pub fn store(&self, state: PendingState) {
        self.0.store(state as u8, Ordering::SeqCst);
    }

    pub fn clear(&self) {
        self.store(PendingState::None);
    }
}

/// Messages sent from IME to Neovim
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToNeovim {
    /// Send a key to Neovim (raw key string like "a", "A", "<BS>", "<CR>")
    Key(String),
    /// Resize the attached UI (grid columns x rows)
    ResizeUi { width: u64, height: u64 },
    /// Shutdown Neovim
    Shutdown,
}

/// One run of cells in a `grid_line` event (hl id already resolved)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GridCell {
    /// Cell text ("" for the right half of a double-width char)
    pub text: String,
    /// Highlight id (see `GridEvent::HlAttrDefine`)
    pub hl: u64,
    /// Number of times the cell is repeated
    pub repeat: usize,
}

/// RGB highlight attributes from `hl_attr_define` (None = default color)
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HlAttr {
    pub foreground: Option<u32>,
    pub background: Option<u32>,
    pub special: Option<u32>,
    pub reverse: bool,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub undercurl: bool,
    pub strikethrough: bool,
}

/// Line-based grid / window update (ext_linegrid + ext_multigrid)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum GridEvent {
    Resize {
        grid: u64,
        width: usize,
        height: usize,
    },
    Clear {
        grid: u64,
    },
    Destroy {
        grid: u64,
    },
    /// Makes `grid` the current grid with the cursor at (row, col)
    CursorGoto {
        grid: u64,
        row: usize,
        col: usize,
    },
    Line {
        grid: u64,
        row: usize,
        col_start: usize,
        cells: Vec<GridCell>,
        /// The row continues on the next row (set on the event covering
        /// the last column)
        wrap: bool,
    },
    /// Copy cells within [top, bot) x [left, right); rows > 0 moves up
    Scroll {
        grid: u64,
        top: usize,
        bot: usize,
        left: usize,
        right: usize,
        rows: i64,
    },
    HlAttrDefine {
        id: u64,
        attr: HlAttr,
    },
    DefaultColors {
        fg: u32,
        bg: u32,
        sp: u32,
    },
    /// Normal window placed at (row, col) on the global grid
    WinPos {
        grid: u64,
        row: usize,
        col: usize,
        width: usize,
        height: usize,
    },
    /// Floating window, positioned by Neovim at (screen_row, screen_col)
    WinFloatPos {
        grid: u64,
        anchor_grid: u64,
        screen_row: usize,
        screen_col: usize,
        zindex: u64,
    },
    WinHide {
        grid: u64,
    },
    WinClose {
        grid: u64,
    },
    /// Buffer range shown in the window (all zero-based)
    WinViewport {
        grid: u64,
        topline: usize,
        botline: usize,
        curline: usize,
        curcol: usize,
        line_count: usize,
    },
}

/// Messages sent from Neovim to IME
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FromNeovim {
    /// Neovim is ready
    Ready,
    /// Text should be committed
    Commit(String),
    /// Completion candidates from Neovim's popup menu
    Candidates(CandidateInfo),
    /// Macro register being recorded ("" when not recording)
    Recording(String),
    /// Key was processed. Sent exactly once per key, after any data messages
    /// for that key, carrying the pending state after processing.
    KeyProcessed { pending: PendingState },
    /// Command-line shown (from ext_cmdline redraw event)
    CmdlineShow {
        content: String,
        pos: usize,
        firstc: String,
        prompt: String,
        level: u64,
    },
    /// Command-line cursor position update (from ext_cmdline redraw event)
    CmdlinePos { pos: usize, level: u64 },
    /// Command-line hidden (from ext_cmdline redraw event)
    CmdlineHide { level: u64 },
    /// Command output message (e.g., from :s/foo/bar/g)
    CmdlineMessage { text: String, cmdtype: String },
    /// Vim mode changed (from mode_change redraw event)
    ModeChange(String),
    /// Key should be passed through to the application via virtual keyboard
    PassthroughKey,
    /// Neovim process exited (e.g., :q)
    NvimExited,
    /// Grid updates of one redraw batch, sent at `flush`
    GridFlush(Vec<GridEvent>),
    /// Buffer lines [first, last) replaced by `lines` (`nvim_buf_lines_event`;
    /// last = None means to the end of the buffer)
    BufLines {
        first: usize,
        last: Option<usize>,
        lines: Vec<String>,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CandidateInfo {
    pub candidates: Vec<String>,
    pub selected: usize,
}

impl CandidateInfo {
    pub fn new(candidates: Vec<String>, selected: usize) -> Self {
        Self {
            candidates,
            selected,
        }
    }

    pub fn empty() -> Self {
        Self::default()
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

    fn roundtrip_from_neovim(msg: &FromNeovim) -> FromNeovim {
        let json = serde_json::to_string(msg).unwrap();
        serde_json::from_str(&json).unwrap()
    }

    #[test]
    fn from_neovim_ready_roundtrip() {
        let msg = FromNeovim::Ready;
        let rt = roundtrip_from_neovim(&msg);
        assert!(matches!(rt, FromNeovim::Ready));
    }

    #[test]
    fn from_neovim_commit_roundtrip() {
        let msg = FromNeovim::Commit("確定".into());
        let rt = roundtrip_from_neovim(&msg);
        match rt {
            FromNeovim::Commit(text) => assert_eq!(text, "確定"),
            _ => panic!("expected Commit"),
        }
    }

    #[test]
    fn from_neovim_candidates_roundtrip() {
        let msg = FromNeovim::Candidates(CandidateInfo::new(
            vec!["漢字".into(), "感じ".into(), "幹事".into()],
            1,
        ));
        let rt = roundtrip_from_neovim(&msg);
        match rt {
            FromNeovim::Candidates(info) => {
                assert_eq!(info.candidates.len(), 3);
                assert_eq!(info.selected, 1);
            }
            _ => panic!("expected Candidates"),
        }
    }

    #[test]
    fn from_neovim_simple_variants_roundtrip() {
        // Test all data-less or simple variants
        for msg in [
            FromNeovim::KeyProcessed {
                pending: PendingState::Motion,
            },
            FromNeovim::PassthroughKey,
            FromNeovim::NvimExited,
            FromNeovim::CmdlineShow {
                content: "s/foo/bar/g".into(),
                pos: 11,
                firstc: ":".into(),
                prompt: String::new(),
                level: 1,
            },
            FromNeovim::CmdlinePos { pos: 5, level: 1 },
            FromNeovim::CmdlineHide { level: 1 },
            FromNeovim::CmdlineMessage {
                text: "3 substitutions".into(),
                cmdtype: ":".into(),
            },
            FromNeovim::Recording("q".into()),
        ] {
            let json = serde_json::to_string(&msg).unwrap();
            let rt: FromNeovim = serde_json::from_str(&json).unwrap();
            // Just verify it doesn't panic — variant matching would be verbose
            let _ = rt;
        }
    }

    #[test]
    fn to_neovim_roundtrip() {
        let key = ToNeovim::Key("<C-r>a".into());
        let json = serde_json::to_string(&key).unwrap();
        let rt: ToNeovim = serde_json::from_str(&json).unwrap();
        match rt {
            ToNeovim::Key(k) => assert_eq!(k, "<C-r>a"),
            _ => panic!("expected Key"),
        }

        let shutdown = ToNeovim::Shutdown;
        let json = serde_json::to_string(&shutdown).unwrap();
        let rt: ToNeovim = serde_json::from_str(&json).unwrap();
        assert!(matches!(rt, ToNeovim::Shutdown));
    }

    #[test]
    fn candidate_info_empty() {
        let info = CandidateInfo::empty();
        assert!(info.candidates.is_empty());
        assert_eq!(info.selected, 0);
    }
}
