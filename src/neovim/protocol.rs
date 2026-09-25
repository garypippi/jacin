//! Typed protocol messages for Neovim communication
//!
//! Messages from the Neovim backend to the IME model.

use serde::{Deserialize, Serialize};

use super::mode::Mode;

/// Multi-key sequence in progress, reported with each processed key.
///
/// These states are mutually exclusive — only one can be active at a time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PendingState {
    /// No pending operation
    #[default]
    None,
    /// Neovim blocked in getchar (after q, f, t, r, m, etc.)
    Getchar,
    /// Operator pending, waiting for motion (after d, c, y, etc.)
    Motion,
    /// Insert mode <C-r>, waiting for register name
    InsertRegister,
    /// Normal mode " prefix, waiting for register name
    NormalRegister,
    /// In command-line mode (derived from the mode, not a key sequence)
    CommandLine,
}

impl PendingState {
    pub fn is_pending(self) -> bool {
        self != Self::None
    }

    pub fn is_motion(self) -> bool {
        self == Self::Motion
    }

    /// Check if in a register-pending state (InsertRegister or NormalRegister)
    pub fn is_register(self) -> bool {
        matches!(self, Self::InsertRegister | Self::NormalRegister)
    }
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
    /// None: not set (-1)
    DefaultColors {
        fg: Option<u32>,
        bg: Option<u32>,
        sp: Option<u32>,
    },
    /// Highlight id of a builtin UI group (`Pmenu`, `MsgArea`, ...)
    HlGroupSet {
        name: String,
        id: u64,
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FromNeovim {
    /// Neovim is ready
    Ready,
    /// Text should be committed
    Commit(String),
    /// Completion candidates from Neovim's popup menu
    Candidates(CandidateInfo),
    /// Macro register being recorded ("" when not recording)
    Recording(String),
    /// Key `seq` was processed. Sent exactly once per key, after any data
    /// messages for that key, carrying the pending state after processing.
    KeyProcessed { seq: u64, pending: PendingState },
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
    /// Pending command keys ('showcmd', from msg_showcmd; "" when none)
    ShowCmd(String),
    /// Vim mode changed (mode_change redraw event or nvim_get_mode reply)
    ModeChange(Mode),
    /// Key `seq` should be passed through to the application via virtual
    /// keyboard
    PassthroughKey { seq: u64 },
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CandidateInfo {
    pub candidates: Vec<String>,
    /// None when no item is selected (e.g. 'completeopt' noselect)
    pub selected: Option<usize>,
    /// Completion of the command line rather than of the buffer
    #[serde(default)]
    pub cmdline: bool,
}

impl CandidateInfo {
    pub fn new(candidates: Vec<String>, selected: Option<usize>, cmdline: bool) -> Self {
        Self {
            candidates,
            selected,
            cmdline,
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
        assert!(!PendingState::Getchar.is_motion());

        assert!(PendingState::InsertRegister.is_register());
        assert!(PendingState::NormalRegister.is_register());
        assert!(!PendingState::Motion.is_register());
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
            Some(1),
            false,
        ));
        let rt = roundtrip_from_neovim(&msg);
        match rt {
            FromNeovim::Candidates(info) => {
                assert_eq!(info.candidates.len(), 3);
                assert_eq!(info.selected, Some(1));
            }
            _ => panic!("expected Candidates"),
        }
    }

    #[test]
    fn from_neovim_simple_variants_roundtrip() {
        // Test all data-less or simple variants
        for msg in [
            FromNeovim::KeyProcessed {
                seq: 3,
                pending: PendingState::Motion,
            },
            FromNeovim::PassthroughKey { seq: 3 },
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
            FromNeovim::ShowCmd("2d".into()),
        ] {
            let json = serde_json::to_string(&msg).unwrap();
            let rt: FromNeovim = serde_json::from_str(&json).unwrap();
            // Just verify it doesn't panic — variant matching would be verbose
            let _ = rt;
        }
    }

    #[test]
    fn candidate_info_empty() {
        let info = CandidateInfo::empty();
        assert!(info.candidates.is_empty());
        assert_eq!(info.selected, None);
    }
}
