//! Pure IME model updated from Neovim messages
//!
//! `Model::reduce` applies a `FromNeovim` message to the IME-side state and
//! returns the side effects to perform. It does no I/O, so message sequences
//! can be replayed in tests; `State::apply_effects` executes the effects.

use crate::neovim::FromNeovim;
use crate::state::{BufferMirror, ImeState, KeypressState, NvimView, Screen};

/// Side effect requested by `Model::reduce`
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Re-render the popup
    Render,
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
    /// Neovim exited: release grab, drop backend
    NvimExited,
}

/// IME-side state driven by Neovim messages
#[derive(Default)]
pub struct Model {
    pub ime: ImeState,
    pub keypress: KeypressState,
    pub view: NvimView,
}

impl Model {
    pub fn new() -> Self {
        Self::default()
    }

    /// Clear all display state (keeps the IME mode).
    pub fn reset(&mut self) {
        self.view.clear();
        self.keypress.clear();
    }

    /// Apply a Neovim message.
    pub fn reduce(&mut self, msg: FromNeovim) -> Vec<Effect> {
        let enabled = self.ime.is_fully_enabled();
        match msg {
            FromNeovim::Ready | FromNeovim::KeyProcessed { .. } => vec![],
            FromNeovim::Commit(text) => {
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
            FromNeovim::Recording(reg) => {
                if !enabled {
                    return vec![];
                }
                self.view.recording = reg;
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
            FromNeovim::NvimExited => {
                self.reset();
                self.ime.disable();
                // The mirrored screen belonged to the dead process; a respawned
                // Neovim starts from scratch (otherwise it flashes on re-enable)
                self.view.screen = Screen::default();
                self.view.buffer = BufferMirror::default();
                vec![Effect::NvimExited]
            }
            FromNeovim::BufLines { first, last, lines } => {
                // Always applied: the mirror must follow Neovim even while
                // disabled (e.g. the <Esc>ggdG sent on toggle-off)
                self.view.buffer.apply(first, last, lines);
                vec![]
            }
            FromNeovim::GridFlush(events) => {
                for event in events {
                    self.view.screen.apply(event);
                }
                vec![Effect::Render]
            }
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
            for effect in self.model.reduce(msg) {
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
        vim_mode: String,
        recording: String,
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
            model.view.vim_mode, expect.vim_mode,
            "vim_mode mismatch in {path}"
        );
        assert_eq!(
            model.view.recording, expect.recording,
            "recording mismatch in {path}"
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
    fn replay_macro_recording() {
        run_fixture("tests/fixtures/macro_recording.json");
    }

    #[test]
    fn commit_clears_buffer_and_cancels_toggle() {
        let mut model = enabled_model();
        let effects = model.reduce(FromNeovim::Commit("確定".into()));
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

    #[test]
    fn nvim_exit_discards_mirrored_screen() {
        let mut model = enabled_model();
        model.reduce(grid_flush("old", 3));
        assert!(model.view.screen.window_view(8).is_some());

        model.reduce(FromNeovim::NvimExited);
        assert!(model.view.screen.window_view(8).is_none());
    }

    #[test]
    fn grid_flush_renders() {
        let mut model = enabled_model();
        assert_eq!(model.reduce(grid_flush("a", 1)), vec![Effect::Render]);
    }

    #[test]
    fn recording_ignored_while_enabling() {
        let mut model = Model::new();
        model.ime.start_enabling();
        let effects = model.reduce(FromNeovim::Recording("q".into()));
        assert!(effects.is_empty());
        assert!(model.view.recording.is_empty());
    }
}
