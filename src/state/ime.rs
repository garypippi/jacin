//! IME state machine
//!
//! Explicit state machine for IME mode transitions, replacing scattered boolean flags.

/// Main IME mode state machine
#[derive(Debug, Clone, PartialEq, Default)]
#[allow(dead_code)]
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
    /// IME is being disabled
    Disabling,
}

/// Vim editing mode within the IME
#[derive(Debug, Clone, PartialEq, Default)]
#[allow(dead_code)]
pub enum VimMode {
    /// Insert mode - characters inserted at cursor
    #[default]
    Insert,
    /// Normal mode - commands and motions
    Normal,
    /// Visual mode - selection active
    Visual,
    /// Operator pending - waiting for motion (e.g., after 'd')
    OperatorPending {
        /// The operator character (d, c, y, etc.)
        operator: char,
        /// What kind of motion we're waiting for
        awaiting: MotionAwaiting,
    },
}

/// What the operator-pending mode is waiting for
#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub enum MotionAwaiting {
    /// Waiting for any motion (w, e, b, etc.) or text object prefix (i, a)
    Motion,
    /// Waiting for text object character (w, p, ", etc.) after i/a prefix
    TextObjectChar,
}

/// IME state including mode, preedit, and candidates
#[allow(dead_code)]
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
}

#[allow(dead_code)]
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
        }
    }

    /// Check if IME is enabled (or enabling)
    pub fn is_enabled(&self) -> bool {
        matches!(
            self.mode,
            ImeMode::Enabled { .. } | ImeMode::Enabling
        )
    }

    /// Check if IME is fully enabled (not transitioning)
    pub fn is_fully_enabled(&self) -> bool {
        matches!(self.mode, ImeMode::Enabled { .. })
    }

    /// Get current vim mode (if enabled)
    pub fn vim_mode(&self) -> Option<&VimMode> {
        match &self.mode {
            ImeMode::Enabled { vim_mode, .. } => Some(vim_mode),
            _ => None,
        }
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

    /// Start disabling the IME
    pub fn start_disabling(&mut self) {
        self.mode = ImeMode::Disabling;
        self.clear_preedit();
    }

    /// Complete disabling
    pub fn complete_disabling(&mut self) {
        self.mode = ImeMode::Disabled;
    }

    /// Disable immediately (for toggle off)
    pub fn disable(&mut self) {
        self.mode = ImeMode::Disabled;
        self.clear_preedit();
    }

    /// Set vim mode (only when enabled)
    pub fn set_vim_mode(&mut self, vim_mode: VimMode) {
        if let ImeMode::Enabled {
            vim_mode: ref mut current,
            ..
        } = self.mode
        {
            *current = vim_mode;
        }
    }

    /// Update vim mode from Neovim mode string
    pub fn update_vim_mode_from_string(&mut self, mode_str: &str) {
        let vim_mode = match mode_str {
            "i" => VimMode::Insert,
            "n" => VimMode::Normal,
            m if m.starts_with("no") => {
                // Operator-pending mode
                VimMode::OperatorPending {
                    operator: '?', // We don't know the operator from mode string alone
                    awaiting: MotionAwaiting::Motion,
                }
            }
            m if m.starts_with('v') || m.starts_with('V') || m == "\x16" => VimMode::Visual,
            _ => return, // Unknown mode, don't change
        };
        self.set_vim_mode(vim_mode);
    }

    /// Enter operator-pending mode
    pub fn enter_operator_pending(&mut self, operator: char) {
        self.set_vim_mode(VimMode::OperatorPending {
            operator,
            awaiting: MotionAwaiting::Motion,
        });
    }

    /// Advance operator-pending to text object char (after i/a)
    pub fn advance_to_text_object_char(&mut self) {
        if let ImeMode::Enabled {
            vim_mode:
                VimMode::OperatorPending {
                    operator,
                    ref mut awaiting,
                },
            ..
        } = self.mode
        {
            let _ = operator; // Silence unused warning
            *awaiting = MotionAwaiting::TextObjectChar;
        }
    }

    /// Exit operator-pending mode back to normal
    pub fn exit_operator_pending(&mut self) {
        if matches!(
            self.mode,
            ImeMode::Enabled {
                vim_mode: VimMode::OperatorPending { .. },
                ..
            }
        ) {
            self.set_vim_mode(VimMode::Normal);
        }
    }

    /// Check if in operator-pending mode
    pub fn is_operator_pending(&self) -> bool {
        matches!(
            self.mode,
            ImeMode::Enabled {
                vim_mode: VimMode::OperatorPending { .. },
                ..
            }
        )
    }

    /// Get operator-pending awaiting state
    pub fn operator_pending_awaiting(&self) -> Option<&MotionAwaiting> {
        match &self.mode {
            ImeMode::Enabled {
                vim_mode: VimMode::OperatorPending { awaiting, .. },
                ..
            } => Some(awaiting),
            _ => None,
        }
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

    /// Update candidates
    pub fn set_candidates(&mut self, candidates: Vec<String>, selected: usize) {
        self.candidates = candidates;
        self.selected_candidate = selected;
    }

    /// Clear candidates
    pub fn clear_candidates(&mut self) {
        self.candidates.clear();
        self.selected_candidate = 0;
    }

    /// Check if has candidates
    pub fn has_candidates(&self) -> bool {
        !self.candidates.is_empty()
    }
}

impl Default for ImeState {
    fn default() -> Self {
        Self::new()
    }
}
