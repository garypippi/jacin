//! Typed protocol messages for Neovim communication
//!
//! Defines all messages that can be sent to/from the Neovim backend.

use serde::{Deserialize, Serialize};

/// Messages sent from IME to Neovim
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToNeovim {
    /// Send a key to Neovim (raw key string like "a", "A", "<BS>", "<CR>")
    Key(String),
    /// Shutdown Neovim
    Shutdown,
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

    /// Check if has candidates
    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }
}

/// Parse candidates from nvim-cmp JSON output
#[derive(Debug, Deserialize)]
pub struct CmpCandidatesJson {
    pub words: Vec<String>,
    pub selected: i32,
    #[allow(dead_code)]
    pub total: usize,
}

impl CmpCandidatesJson {
    /// Convert to CandidateInfo
    pub fn into_candidate_info(self) -> CandidateInfo {
        let selected = if self.selected >= 0 {
            self.selected as usize
        } else {
            0
        };
        CandidateInfo::new(self.words, selected)
    }
}

/// Parse completion items from complete_info() JSON output
#[derive(Debug, Deserialize)]
pub struct CompleteInfoJson {
    pub items: Vec<CompleteItem>,
    pub selected: i32,
}

#[derive(Debug, Deserialize)]
pub struct CompleteItem {
    pub word: String,
}

impl CompleteInfoJson {
    /// Convert to CandidateInfo
    pub fn into_candidate_info(self) -> CandidateInfo {
        let candidates: Vec<String> = self.items.into_iter().map(|i| i.word).collect();
        let selected = if self.selected >= 0 {
            self.selected as usize
        } else {
            0
        };
        CandidateInfo::new(candidates, selected)
    }
}
