//! Pure IME model updated from Neovim messages
//!
//! `Model::reduce` applies a `FromNeovim` message to the IME-side state and
//! returns the side effects to perform. It does no I/O, so message sequences
//! can be replayed in tests; `State::apply_effects` executes the effects.

use crate::neovim::{FromNeovim, Mode};
use crate::state::{BufferMirror, ImeState, NvimView, Screen, SentKey, SentKeys};

/// Side effect requested by `Model::reduce`
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Re-render the popup
    Render,
    /// Commit text to the application
    CommitString(String),
    /// Forward a key to the application
    PassthroughKey(SentKey),
    /// Neovim exited: release grab, drop backend
    NvimExited,
}

/// IME-side state driven by Neovim messages
#[derive(Default)]
pub struct Model {
    pub ime: ImeState,
    pub view: NvimView,
    /// Keys sent to Neovim, awaiting `KeyProcessed`
    pub sent_keys: SentKeys,
}

impl Model {
    pub fn new() -> Self {
        Self::default()
    }

    /// Clear all display state (keeps the IME mode).
    pub fn reset(&mut self) {
        self.view.clear();
    }

    /// Apply a Neovim message.
    pub fn reduce(&mut self, msg: FromNeovim) -> Vec<Effect> {
        let enabled = self.ime.is_fully_enabled();
        match msg {
            FromNeovim::Ready => vec![],
            FromNeovim::KeyProcessed { seq, pending } => {
                self.sent_keys.acknowledge(seq);
                let shown = self.view.visible_showcmd().to_owned();
                self.view.pending = pending;
                if enabled && self.view.visible_showcmd() != shown {
                    vec![Effect::Render]
                } else {
                    vec![]
                }
            }
            FromNeovim::Commit(text) => {
                self.view.clear_candidates();
                vec![Effect::CommitString(text), Effect::Render]
            }
            FromNeovim::Candidates(info) => {
                if !enabled {
                    return vec![];
                }
                if info.candidates.is_empty() {
                    self.view.clear_candidates();
                } else {
                    self.view
                        .set_candidates(info.candidates, info.selected, info.cmdline);
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
            FromNeovim::PassthroughKey { seq } => match self.sent_keys.get(seq) {
                Some(key) => vec![Effect::PassthroughKey(key.clone())],
                None => {
                    log::warn!("[IME] PassthroughKey for unknown key #{seq}");
                    vec![]
                }
            },
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
            FromNeovim::ShowCmd(text) => {
                let shown = self.view.visible_showcmd().to_owned();
                self.view.showcmd = text;
                if enabled && self.view.visible_showcmd() != shown {
                    vec![Effect::Render]
                } else {
                    vec![]
                }
            }
            FromNeovim::ModeChange(mode) => {
                // Always applied: key processing reads it, also for keys
                // input while disabled (e.g. the buffer clear on toggle-off)
                self.view.mode = mode;
                if enabled {
                    vec![Effect::Render]
                } else {
                    vec![]
                }
            }
            FromNeovim::NvimExited => {
                self.reset();
                self.ime.disable();
                // The mirrored screen belonged to the dead process; a respawned
                // Neovim starts from scratch (otherwise it flashes on re-enable)
                self.view.screen = Screen::default();
                self.view.buffer = BufferMirror::default();
                self.view.mode = Mode::default();
                self.sent_keys.clear();
                vec![Effect::NvimExited]
            }
            FromNeovim::BufLines { first, last, lines } => {
                // Always applied: the mirror must follow Neovim even while
                // disabled (e.g. the buffer clear sent on toggle-off)
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
        mode: Mode,
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
        assert_eq!(model.view.mode, expect.mode, "mode mismatch in {path}");
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
    fn commit_cancels_toggle() {
        let mut model = enabled_model();
        let effects = model.reduce(FromNeovim::Commit("確定".into()));
        assert_eq!(
            effects,
            vec![Effect::CommitString("確定".into()), Effect::Render,]
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

    fn sent_key(keycode: u32, ctrl: bool) -> SentKey {
        SentKey {
            keycode,
            mods: crate::state::Modifiers {
                depressed: if ctrl { 0x4 } else { 0 },
                ..Default::default()
            },
            vim_key: if ctrl { "<C-x>".into() } else { "x".into() },
            ctrl,
            alt: false,
        }
    }

    /// A passthrough arriving after the next key was sent forwards its own
    /// key, with the modifiers held when it was pressed.
    #[test]
    fn late_passthrough_forwards_its_own_key() {
        let mut model = enabled_model();
        let commit_key = sent_key(28, true);
        let first = model.sent_keys.push(commit_key.clone());
        model.sent_keys.push(sent_key(30, false));

        assert_eq!(
            model.reduce(FromNeovim::PassthroughKey { seq: first }),
            vec![Effect::PassthroughKey(commit_key)]
        );
    }

    #[test]
    fn acknowledged_key_is_not_passed_through() {
        let mut model = enabled_model();
        let seq = model.sent_keys.push(sent_key(14, false));
        model.reduce(FromNeovim::KeyProcessed {
            seq,
            pending: crate::neovim::protocol::PendingState::None,
        });

        assert!(model.reduce(FromNeovim::PassthroughKey { seq }).is_empty());
    }

    #[test]
    fn nvim_exit_forgets_sent_keys() {
        let mut model = enabled_model();
        let seq = model.sent_keys.push(sent_key(14, false));
        model.reduce(FromNeovim::NvimExited);

        assert!(model.sent_keys.get(seq).is_none());
    }

    #[test]
    fn showcmd_is_shown_while_neovim_waits_for_keys() {
        use crate::neovim::protocol::PendingState;
        let mut model = enabled_model();
        let processed = |pending| FromNeovim::KeyProcessed { seq: 0, pending };

        // A complete command (j): shown and cleared, never waited on
        assert!(model.reduce(FromNeovim::ShowCmd("j".into())).is_empty());
        assert!(model.reduce(FromNeovim::ShowCmd(String::new())).is_empty());
        assert!(model.reduce(processed(PendingState::None)).is_empty());
        assert_eq!(model.view.visible_showcmd(), "");

        // An operator waits for its motion
        assert!(model.reduce(FromNeovim::ShowCmd("2d".into())).is_empty());
        assert_eq!(
            model.reduce(processed(PendingState::Motion)),
            vec![Effect::Render]
        );
        assert_eq!(model.view.visible_showcmd(), "2d");
        // Unchanged: nothing to render
        assert!(model.reduce(FromNeovim::ShowCmd("2d".into())).is_empty());
        assert_eq!(
            model.reduce(FromNeovim::ShowCmd("2di".into())),
            vec![Effect::Render]
        );
    }

    #[test]
    fn mode_is_followed_while_disabled() {
        let mut model = Model::new();
        assert!(
            model
                .reduce(FromNeovim::ModeChange(Mode::Insert))
                .is_empty()
        );
        assert_eq!(model.view.mode, Mode::Insert);
    }

    #[test]
    fn last_mode_wins() {
        let mut model = enabled_model();
        model.reduce(FromNeovim::ModeChange(Mode::Insert));
        model.reduce(FromNeovim::ModeChange(Mode::Normal));
        assert_eq!(model.view.mode, Mode::Normal);
    }
}
