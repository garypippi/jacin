//! Pure IME model updated from Neovim messages
//!
//! `Model::reduce` applies a `FromNeovim` message to the IME-side state and
//! returns the side effects to perform. It does no I/O, so message sequences
//! can be replayed in tests; `State::apply_effects` executes the effects.

use crate::neovim::FromNeovim;
use crate::state::{ImeState, KeypressState, NvimView};

/// Side effect requested by `Model::reduce`
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Send the current preedit to the compositor and re-render the popup
    SyncPreedit,
    /// Re-render the popup
    Render,
    /// The mirrored grid changed (re-render when the popup shows the grid)
    GridUpdated,
    /// Commit text to the application
    CommitString(String),
    /// Delete text around the cursor in the application
    DeleteSurrounding { before: u32, after: u32 },
    /// Forward the key being processed to the application
    PassthroughKey,
    /// Send keys to Neovim
    NvimInput(&'static str),
    /// Drop a pending IME toggle request (Alt in the commit key <A-;>
    /// also triggers SIGUSR1 — don't let it re-enable after commit)
    CancelToggle,
    /// Neovim exited: clear compositor preedit, release grab, drop backend
    NvimExited,
}

/// IME-side state driven by Neovim messages
#[derive(Default)]
pub struct Model {
    pub ime: ImeState,
    pub keypress: KeypressState,
    pub view: NvimView,
    /// Phase A shadow check: whether the grid cursor row agreed with the
    /// snapshot preedit at the last check (None = not checked yet)
    pub shadow_ok: Option<bool>,
}

impl Model {
    pub fn new() -> Self {
        Self::default()
    }

    /// Clear preedit and all display state (keeps the IME mode).
    pub fn reset(&mut self) {
        self.ime.clear_preedit();
        self.view.clear();
        self.keypress.clear();
    }

    /// Apply a Neovim message. `active` is whether a text input is focused.
    pub fn reduce(&mut self, msg: FromNeovim, active: bool) -> Vec<Effect> {
        let enabled = self.ime.is_fully_enabled();
        match msg {
            FromNeovim::Ready | FromNeovim::KeyProcessed { .. } => vec![],
            FromNeovim::Preedit(info) => {
                if !enabled {
                    return vec![];
                }
                self.ime
                    .set_preedit(info.text, info.cursor_begin, info.cursor_end);
                self.view.set_vim_mode(&info.mode);
                self.view.recording = info.recording;
                self.shadow_check();
                vec![Effect::SyncPreedit]
            }
            FromNeovim::Commit(text) => {
                self.ime.clear_preedit();
                self.view.clear_candidates();
                self.keypress.clear();
                vec![
                    Effect::CommitString(text),
                    Effect::CancelToggle,
                    // Clear Neovim buffer and stay in insert mode for next input
                    Effect::NvimInput("<Esc>ggdGi"),
                    Effect::Render,
                ]
            }
            FromNeovim::DeleteSurrounding { before, after } => {
                vec![Effect::DeleteSurrounding { before, after }]
            }
            FromNeovim::Candidates(info) => {
                if !enabled {
                    return vec![];
                }
                if info.candidates.is_empty() {
                    self.view.clear_candidates();
                } else {
                    self.view.set_candidates(info.candidates, info.selected);
                }
                vec![Effect::Render]
            }
            FromNeovim::VisualRange(selection) => {
                if !enabled {
                    return vec![];
                }
                self.view.visual = selection;
                vec![Effect::Render]
            }
            FromNeovim::PassthroughKey => vec![Effect::PassthroughKey],
            FromNeovim::CmdlineShow {
                content,
                pos,
                firstc,
                prompt,
                level,
            } => {
                if !enabled {
                    return vec![];
                }
                // Display prompt + content for @-mode, firstc + content for :/?
                let prefix = if !prompt.is_empty() { &prompt } else { &firstc };
                self.view.show_cmdline(prefix, &content, pos, level);
                self.keypress.clear();
                vec![Effect::Render]
            }
            FromNeovim::CmdlinePos { pos, level } => {
                if enabled && self.view.update_cmdline_cursor(pos, level) {
                    vec![Effect::Render]
                } else {
                    vec![]
                }
            }
            FromNeovim::CmdlineHide { level } => {
                // Only clear if the level matches the active cmdline
                if self.view.hide_cmdline(level) {
                    self.keypress.clear();
                    vec![Effect::Render]
                } else {
                    vec![]
                }
            }
            FromNeovim::CmdlineCancelled { cmdtype, .. } => {
                self.keypress.clear();
                self.view.cmdline = None;
                // ':' commands usually return to normal mode; '@' input() prompts return
                // to insert mode. ModeChanged snapshot will still correct this if needed.
                self.view
                    .set_vim_mode(if cmdtype == "@" { "i" } else { "n" });
                vec![Effect::Render]
            }
            FromNeovim::CmdlineMessage { text, .. } => {
                if !enabled {
                    return vec![];
                }
                if text.is_empty() {
                    self.view.clear_transient_message();
                } else {
                    self.view.set_transient_message(text);
                }
                vec![Effect::Render]
            }
            FromNeovim::ModeChange(mode) => {
                if !enabled {
                    return vec![];
                }
                self.view.set_vim_mode(&mode);
                vec![Effect::Render]
            }
            FromNeovim::AutoCommit(text) => {
                if text.is_empty() {
                    return vec![];
                }
                if !enabled {
                    // Still commit if the text input is focused: the line was
                    // already removed from the Neovim buffer (e.g. IME toggled
                    // off or Neovim exited while the notification was in flight).
                    return if active {
                        vec![Effect::CommitString(text)]
                    } else {
                        vec![]
                    };
                }
                self.ime.clear_preedit();
                self.view.clear_candidates();
                self.view.visual = None;
                self.keypress.clear();
                vec![Effect::CommitString(text), Effect::Render]
            }
            FromNeovim::NvimExited => {
                self.reset();
                self.ime.disable();
                vec![Effect::NvimExited]
            }
            FromNeovim::GridFlush(events) => {
                for event in events {
                    self.view.screen.apply(event);
                }
                self.shadow_check();
                vec![Effect::GridUpdated]
            }
        }
    }

    /// Phase A shadow check: compare the grid (display) with the snapshot
    /// (source of truth). Logs only when the agreement changes, since grid
    /// and snapshot arrive independently and may briefly disagree.
    fn shadow_check(&mut self) {
        if !self.ime.is_fully_enabled() || self.view.vim_mode.starts_with('c') {
            return;
        }
        let screen = &self.view.screen;
        let Some(grid) = screen.cursor_grid() else {
            return;
        };
        let (row, col) = (screen.cursor.row, screen.cursor.col);
        let row_text = grid.row_text(row);
        let grid_cursor = grid.byte_offset(row, col);
        let ok = row_text.trim_end() == self.ime.preedit.trim_end()
            && grid_cursor == self.ime.cursor_begin;
        if self.shadow_ok != Some(ok) {
            if ok {
                log::debug!("[SHADOW] grid matches snapshot");
            } else {
                let line_count = screen
                    .window(screen.cursor.grid)
                    .and_then(|w| w.viewport)
                    .map(|v| v.line_count);
                log::debug!(
                    "[SHADOW] mismatch: grid {} row {} {:?} cursor={} vs snapshot {:?} cursor={} (mode={}, line_count={:?})",
                    screen.cursor.grid,
                    row,
                    row_text.trim_end(),
                    grid_cursor,
                    self.ime.preedit,
                    self.ime.cursor_begin,
                    self.view.vim_mode,
                    line_count
                );
            }
            self.shadow_ok = Some(ok);
        }
    }
}

#[cfg(test)]
mod replay_tests {
    use serde::Deserialize;

    use super::*;

    /// Model started as fully enabled (most replay scenarios assume enabled IME)
    fn enabled_model() -> Model {
        let mut model = Model::new();
        model.ime.start_enabling();
        model.ime.complete_enabling();
        model
    }

    /// Outcome of replaying messages through the real reducer
    struct Replay {
        model: Model,
        committed: Vec<String>,
        exited: bool,
    }

    impl Replay {
        fn new() -> Self {
            Self {
                model: enabled_model(),
                committed: Vec::new(),
                exited: false,
            }
        }

        fn apply(&mut self, msg: FromNeovim) {
            for effect in self.model.reduce(msg, true) {
                match effect {
                    Effect::CommitString(text) => self.committed.push(text),
                    Effect::NvimExited => self.exited = true,
                    _ => {}
                }
            }
        }
    }

    #[derive(Deserialize)]
    struct Fixture {
        #[allow(dead_code)]
        description: String,
        messages: Vec<serde_json::Value>,
        expect: Expected,
    }

    #[derive(Deserialize)]
    struct Expected {
        preedit: String,
        cursor_begin: usize,
        cursor_end: usize,
        vim_mode: String,
        candidates_count: usize,
        committed: Vec<String>,
        exited: bool,
    }

    fn run_fixture(path: &str) {
        let content = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("failed to read fixture {path}: {e}"));
        let fixture: Fixture = serde_json::from_str(&content)
            .unwrap_or_else(|e| panic!("failed to parse fixture {path}: {e}"));

        let mut replay = Replay::new();
        for (i, value) in fixture.messages.iter().enumerate() {
            let msg: FromNeovim = serde_json::from_value(value.clone())
                .unwrap_or_else(|e| panic!("failed to parse message {i} in {path}: {e}"));
            replay.apply(msg);
        }

        let model = &replay.model;
        let expect = &fixture.expect;
        assert_eq!(
            model.ime.preedit, expect.preedit,
            "preedit mismatch in {path}"
        );
        assert_eq!(
            model.ime.cursor_begin, expect.cursor_begin,
            "cursor_begin mismatch in {path}"
        );
        assert_eq!(
            model.ime.cursor_end, expect.cursor_end,
            "cursor_end mismatch in {path}"
        );
        assert_eq!(
            model.view.vim_mode, expect.vim_mode,
            "vim_mode mismatch in {path}"
        );
        assert_eq!(
            model.view.candidates.len(),
            expect.candidates_count,
            "candidates_count mismatch in {path}"
        );
        assert_eq!(
            replay.committed, expect.committed,
            "committed mismatch in {path}"
        );
        assert_eq!(replay.exited, expect.exited, "exited mismatch in {path}");
    }

    #[test]
    fn replay_insert_and_commit() {
        run_fixture("tests/fixtures/insert_and_commit.json");
    }

    #[test]
    fn replay_candidates_and_select() {
        run_fixture("tests/fixtures/candidates_and_select.json");
    }

    #[test]
    fn replay_cmdline_and_cancel() {
        run_fixture("tests/fixtures/cmdline_and_cancel.json");
    }

    #[test]
    fn replay_nvim_exit() {
        run_fixture("tests/fixtures/nvim_exit.json");
    }

    #[test]
    fn auto_commit_after_nvim_exit_still_commits_when_active() {
        let mut model = enabled_model();
        model.reduce(FromNeovim::NvimExited, true);
        let effects = model.reduce(FromNeovim::AutoCommit("hel lo".into()), true);
        assert_eq!(effects, vec![Effect::CommitString("hel lo".into())]);
    }

    #[test]
    fn auto_commit_after_nvim_exit_skipped_when_inactive() {
        let mut model = enabled_model();
        model.reduce(FromNeovim::NvimExited, true);
        let effects = model.reduce(FromNeovim::AutoCommit("x".into()), false);
        assert!(effects.is_empty());
    }

    #[test]
    fn auto_commit_ignores_empty_text() {
        let mut model = enabled_model();
        assert!(
            model
                .reduce(FromNeovim::AutoCommit(String::new()), true)
                .is_empty()
        );
    }

    #[test]
    fn commit_clears_buffer_and_cancels_toggle() {
        let mut model = enabled_model();
        let effects = model.reduce(FromNeovim::Commit("確定".into()), true);
        assert_eq!(
            effects,
            vec![
                Effect::CommitString("確定".into()),
                Effect::CancelToggle,
                Effect::NvimInput("<Esc>ggdGi"),
                Effect::Render,
            ]
        );
    }

    fn grid_flush(row_text: &str, cursor_col: usize) -> FromNeovim {
        use crate::neovim::{GridCell, GridEvent};
        // Window grid 2 holds the buffer text (ext_multigrid)
        FromNeovim::GridFlush(vec![
            GridEvent::Resize {
                grid: 2,
                width: 10,
                height: 2,
            },
            GridEvent::Line {
                grid: 2,
                row: 0,
                col_start: 0,
                cells: vec![GridCell {
                    text: row_text.into(),
                    hl: 0,
                    repeat: 1,
                }],
                wrap: false,
            },
            GridEvent::CursorGoto {
                grid: 2,
                row: 0,
                col: cursor_col,
            },
        ])
    }

    fn preedit(text: &str, cursor: usize) -> FromNeovim {
        FromNeovim::Preedit(crate::neovim::protocol::PreeditInfo::new(
            text.into(),
            cursor,
            cursor,
            "i".into(),
            String::new(),
        ))
    }

    #[test]
    fn shadow_check_detects_match_and_mismatch() {
        let mut model = enabled_model();
        // Each char is one cell here, so cursor col == byte offset for ASCII
        assert_eq!(
            model.reduce(grid_flush("a", 1), true),
            vec![Effect::GridUpdated]
        );
        model.reduce(preedit("a", 1), true);
        assert_eq!(model.shadow_ok, Some(true));

        model.reduce(preedit("ab", 2), true);
        assert_eq!(model.shadow_ok, Some(false));
    }

    #[test]
    fn shadow_check_skipped_in_cmdline_mode() {
        let mut model = enabled_model();
        model.reduce(FromNeovim::ModeChange("c".into()), true);
        model.reduce(grid_flush("x", 0), true);
        assert_eq!(model.shadow_ok, None);
    }

    #[test]
    fn preedit_ignored_while_enabling() {
        let mut model = Model::new();
        model.ime.start_enabling();
        let effects = model.reduce(
            FromNeovim::Preedit(crate::neovim::protocol::PreeditInfo::new(
                "stale".into(),
                0,
                0,
                "i".into(),
                String::new(),
            )),
            true,
        );
        assert!(effects.is_empty());
        assert!(model.ime.preedit.is_empty());
    }
}
