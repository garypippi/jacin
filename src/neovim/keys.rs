//! Key processing as a state machine without I/O
//!
//! Keys are queued and processed one at a time. Processing a key returns
//! `Cmd`s to run: inputs to Neovim, requests whose replies are fed back with
//! `on_reply`, and events for the model. At most one request is outstanding.
//!
//! Non-fast requests (`nvim_buf_get_lines`, `reg_recording()`) don't answer
//! while Neovim is blocked in getchar(), so they are only sent after
//! `nvim_get_mode` (fast) has shown Neovim is not blocked.

use std::collections::VecDeque;

use super::mode::Mode;
use super::protocol::{FromNeovim, PendingState};

/// Keys that clear the buffer after a commit and return to insert mode
const COMMIT_RESET_KEYS: &str = "<Esc>ggdGi";

/// A queued unit of work
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    /// A user key, acknowledged with `KeyProcessed { seq }`
    Key { seq: u64, key: String },
    /// Keys input on jacin's behalf (not acknowledged)
    Input(String),
}

/// State owned by the model that key processing reads
#[derive(Debug, Clone, Copy)]
pub struct Ctx {
    pub mode: Mode,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Cmd {
    /// `nvim_input` (reply ignored)
    Input(String),
    /// Request whose reply is passed to `on_reply`
    Request(Req),
    /// Mode reported by `nvim_get_mode`, for the model
    SetMode(Mode),
    /// Event for the model
    Emit(FromNeovim),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Req {
    /// `nvim_get_mode`
    GetMode,
    /// `nvim_buf_get_lines(0, 0, -1, false)`
    BufLines,
    /// `reg_recording()`
    RegRecording,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    Mode { mode: Mode, blocking: bool },
    Lines(Vec<String>),
    Recording(String),
    Err(String),
}

/// Keys whose effect depends on whether the buffer is empty
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineKey {
    Commit,
    Backspace,
    Enter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegisterKey {
    /// <C-r> in insert mode
    Insert,
    /// " in normal or visual mode
    Normal,
}

/// What the item in progress waits for, and what to do with the reply
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wait {
    /// Mode after the key that answered getchar
    GetcharDone,
    /// Recording register after a key that may start/stop a recording
    Recording,
    /// Mode (blocked or not) before reading the buffer
    BeforeLines(LineKey),
    Lines(LineKey),
    /// Mode deciding whether the key starts a register name
    RegisterCheck(RegisterKey),
    /// Mode after a key in operator-pending mode
    MotionDone,
    /// Mode after an insert-mode key that may wait for an argument
    InsertArgument,
    /// Mode after any other key
    AfterKey,
    /// Mode after an `Item::Input`
    InputDone,
}

#[derive(Debug)]
struct Current {
    /// (seq, key) of a user key; None for `Item::Input`
    key: Option<(u64, String)>,
    wait: Wait,
}

pub struct KeyProcessor {
    queue: VecDeque<Item>,
    current: Option<Current>,
    /// Multi-key sequence in progress. Never CommandLine: that is derived
    /// from the mode.
    pending: PendingState,
    commit_key: String,
}

impl KeyProcessor {
    pub fn new(commit_key: String) -> Self {
        Self {
            queue: VecDeque::new(),
            current: None,
            pending: PendingState::None,
            commit_key,
        }
    }

    /// Queue an item and start it if nothing is in progress
    pub fn push(&mut self, item: Item, ctx: &Ctx) -> Vec<Cmd> {
        self.queue.push_back(item);
        let mut out = Vec::new();
        self.start_next(ctx.mode, &mut out);
        out
    }

    /// Continue the item in progress with the reply to its request, then
    /// start queued items.
    pub fn on_reply(&mut self, reply: Reply, ctx: &Ctx) -> Vec<Cmd> {
        let mut out = Vec::new();
        let Some(current) = self.current.take() else {
            log::warn!("[KEYS] Unexpected reply {reply:?}");
            return out;
        };
        // A mode reply is newer than the model's mode
        let mode = match reply {
            Reply::Mode { mode, .. } => {
                out.push(Cmd::SetMode(mode));
                mode
            }
            _ => ctx.mode,
        };
        self.resume(current, reply, mode, &mut out);
        self.start_next(mode, &mut out);
        out
    }

    /// Drop queued items that haven't started. Returns the seqs of dropped
    /// keys (the one in progress completes normally).
    pub fn discard_queued(&mut self) -> Vec<u64> {
        self.queue
            .drain(..)
            .filter_map(|item| match item {
                Item::Key { seq, .. } => Some(seq),
                Item::Input(_) => None,
            })
            .collect()
    }

    /// An item is in progress or queued
    pub fn is_busy(&self) -> bool {
        self.current.is_some() || !self.queue.is_empty()
    }

    pub fn queued_len(&self) -> usize {
        self.queue.len()
    }

    fn start_next(&mut self, mode: Mode, out: &mut Vec<Cmd>) {
        while self.current.is_none() {
            match self.queue.pop_front() {
                Some(Item::Key { seq, key }) => self.start_key(seq, key, mode, out),
                Some(Item::Input(keys)) => {
                    log::debug!("[KEYS] Input {keys:?}");
                    out.push(Cmd::Input(keys));
                    self.request(None, Wait::InputDone, Req::GetMode, out);
                }
                None => break,
            }
        }
    }

    fn start_key(&mut self, seq: u64, key: String, mode: Mode, out: &mut Vec<Cmd>) {
        log::debug!(
            "[KEYS] #{seq} {key:?} (mode {mode:?}, pending {:?})",
            self.pending
        );
        // Any command line (:, /, input() from a plugin, ...) edits the keys
        if mode == Mode::CmdLine {
            out.push(Cmd::Input(key));
            self.finish(seq, mode, out);
            return;
        }
        if self.pending == PendingState::Getchar {
            self.answer_getchar(seq, key, out);
            return;
        }
        let in_sequence = self.pending.is_motion() || self.pending.is_register();
        if !in_sequence && let Some(line_key) = self.line_key(&key) {
            self.request(
                Some((seq, key)),
                Wait::BeforeLines(line_key),
                Req::GetMode,
                out,
            );
            return;
        }
        if !self.pending.is_pending() {
            let register_key = match key.as_str() {
                "<C-r>" => Some(RegisterKey::Insert),
                "\"" => Some(RegisterKey::Normal),
                _ => None,
            };
            if let Some(register_key) = register_key {
                let wait = Wait::RegisterCheck(register_key);
                self.request(Some((seq, key)), wait, Req::GetMode, out);
                return;
            }
        }
        self.input_key(seq, key, mode, out);
    }

    fn line_key(&self, key: &str) -> Option<LineKey> {
        if key == self.commit_key {
            Some(LineKey::Commit)
        } else if key == "<BS>" {
            Some(LineKey::Backspace)
        } else if matches!(key, "<CR>" | "<C-CR>" | "<A-CR>") {
            Some(LineKey::Enter)
        } else {
            None
        }
    }

    /// Input a key Neovim reads with getchar() (q, f, t, r, m, ...)
    fn answer_getchar(&mut self, seq: u64, key: String, out: &mut Vec<Cmd>) {
        out.push(Cmd::Input(key.clone()));
        self.pending = PendingState::None;
        self.request(Some((seq, key)), Wait::GetcharDone, Req::GetMode, out);
    }

    /// Input a key, continuing a register or motion sequence if one is
    /// pending
    fn input_key(&mut self, seq: u64, key: String, mode: Mode, out: &mut Vec<Cmd>) {
        out.push(Cmd::Input(key.clone()));
        match self.pending {
            PendingState::InsertRegister => {
                // <C-r><C-r> inserts a register literally: still waiting
                if key == "<C-r>" {
                    self.finish(seq, mode, out);
                    return;
                }
                self.pending = PendingState::None;
            }
            PendingState::NormalRegister => {
                // Register selected, the operator comes next
                self.pending = PendingState::None;
                self.finish(seq, mode, out);
                return;
            }
            PendingState::Motion | PendingState::TextObject => {
                self.request(Some((seq, key)), Wait::MotionDone, Req::GetMode, out);
                return;
            }
            _ => {}
        }
        self.after_key(seq, key, mode, out);
    }

    /// Follow a key that was input. `mode` is the mode before the key.
    fn after_key(&mut self, seq: u64, key: String, mode: Mode, out: &mut Vec<Cmd>) {
        // Insert mode is fire-and-forget: the display follows from redraw
        // and buffer events
        if mode == Mode::Insert && !matches!(key.as_str(), "<Esc>" | "<C-c>") {
            if matches!(key.as_str(), "<C-k>" | "<C-v>" | "<C-q>") {
                self.request(Some((seq, key)), Wait::InsertArgument, Req::GetMode, out);
            } else {
                self.finish(seq, mode, out);
            }
            return;
        }
        self.request(Some((seq, key)), Wait::AfterKey, Req::GetMode, out);
    }

    fn resume(&mut self, current: Current, reply: Reply, mode: Mode, out: &mut Vec<Cmd>) {
        let Current { key, wait } = current;
        let Some((seq, key)) = key else {
            match reply {
                Reply::Mode { mode, blocking } => self.sync_pending(mode, blocking),
                reply => log::warn!("[KEYS] Input: unexpected reply {reply:?}"),
            }
            return;
        };

        let blocking = match reply {
            Reply::Mode { blocking, .. } => blocking,
            Reply::Lines(lines) => {
                if let Wait::Lines(line_key) = wait {
                    self.edit_lines(seq, line_key, &lines, out);
                } else {
                    log::warn!("[KEYS] #{seq}: unexpected lines while in {wait:?}");
                }
                self.finish(seq, mode, out);
                return;
            }
            Reply::Recording(reg) => {
                out.push(Cmd::Emit(FromNeovim::Recording(reg)));
                self.finish(seq, mode, out);
                return;
            }
            Reply::Err(e) => {
                log::warn!("[KEYS] #{seq} {key:?}: request failed in {wait:?}: {e}");
                self.finish(seq, mode, out);
                return;
            }
        };

        match wait {
            Wait::GetcharDone => {
                if blocking {
                    self.pending = PendingState::Getchar;
                } else {
                    if mode == Mode::OperatorPending {
                        self.pending = PendingState::Motion;
                    }
                    // q{reg} starts a recording
                    self.request(Some((seq, key)), Wait::Recording, Req::RegRecording, out);
                    return;
                }
            }
            Wait::BeforeLines(line_key) => {
                if blocking {
                    self.answer_getchar(seq, key, out);
                } else {
                    let wait = Wait::Lines(line_key);
                    self.request(Some((seq, key)), wait, Req::BufLines, out);
                }
                return;
            }
            Wait::RegisterCheck(register_key) => {
                if blocking {
                    self.answer_getchar(seq, key, out);
                    return;
                }
                let starts_register = match register_key {
                    RegisterKey::Insert => mode == Mode::Insert,
                    RegisterKey::Normal => matches!(mode, Mode::Normal | Mode::Visual),
                };
                if !starts_register {
                    self.input_key(seq, key, mode, out);
                    return;
                }
                out.push(Cmd::Input(key));
                self.pending = match register_key {
                    RegisterKey::Insert => PendingState::InsertRegister,
                    RegisterKey::Normal => PendingState::NormalRegister,
                };
            }
            Wait::MotionDone => {
                // Otherwise still pending: blocked for an argument (df) or
                // waiting for more (di)
                if !blocking && mode != Mode::OperatorPending {
                    self.pending = PendingState::None;
                }
            }
            Wait::InsertArgument => {
                if blocking {
                    self.pending = PendingState::Getchar;
                }
            }
            Wait::AfterKey => {
                if blocking {
                    self.pending = PendingState::Getchar;
                } else if mode == Mode::OperatorPending {
                    self.pending = PendingState::Motion;
                } else if key == "q" {
                    // q while recording stops it
                    self.request(Some((seq, key)), Wait::Recording, Req::RegRecording, out);
                    return;
                }
            }
            Wait::Lines(_) | Wait::Recording | Wait::InputDone => {
                log::warn!("[KEYS] #{seq}: unexpected mode reply while in {wait:?}");
            }
        }
        self.finish(seq, mode, out);
    }

    /// Commit, Backspace and Enter act on Neovim's buffer unless it is
    /// empty, in which case the app gets the key
    fn edit_lines(&mut self, seq: u64, line_key: LineKey, lines: &[String], out: &mut Vec<Cmd>) {
        // The whole buffer, not the current line: an empty 2nd line must not
        // pass the key to the app
        if lines.len() <= 1 && lines.iter().all(String::is_empty) {
            out.push(Cmd::Emit(FromNeovim::PassthroughKey { seq }));
            return;
        }
        match line_key {
            LineKey::Commit => {
                out.push(Cmd::Emit(FromNeovim::Commit(lines.join("\n"))));
                // Here rather than by the model, so keys typed after the
                // commit key (already queued) are not cleared
                out.push(Cmd::Input(COMMIT_RESET_KEYS.into()));
            }
            LineKey::Backspace => out.push(Cmd::Input("<BS>".into())),
            LineKey::Enter => out.push(Cmd::Input("<CR>".into())),
        }
    }

    /// Pending state after jacin's own input
    fn sync_pending(&mut self, mode: Mode, blocking: bool) {
        self.pending = if blocking {
            PendingState::Getchar
        } else if mode == Mode::OperatorPending {
            PendingState::Motion
        } else {
            PendingState::None
        };
    }

    fn request(&mut self, key: Option<(u64, String)>, wait: Wait, req: Req, out: &mut Vec<Cmd>) {
        out.push(Cmd::Request(req));
        self.current = Some(Current { key, wait });
    }

    fn finish(&mut self, seq: u64, mode: Mode, out: &mut Vec<Cmd>) {
        let pending = if mode == Mode::CmdLine {
            PendingState::CommandLine
        } else {
            self.pending
        };
        out.push(Cmd::Emit(FromNeovim::KeyProcessed { seq, pending }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMIT: &str = "<C-CR>";

    struct Harness {
        kp: KeyProcessor,
        mode: Mode,
        seq: u64,
    }

    impl Harness {
        fn new(mode: Mode) -> Self {
            Self {
                kp: KeyProcessor::new(COMMIT.into()),
                mode,
                seq: 0,
            }
        }

        fn ctx(&self) -> Ctx {
            Ctx { mode: self.mode }
        }

        /// Apply mode updates like the model would
        fn apply(&mut self, cmds: Vec<Cmd>) -> Vec<Cmd> {
            for cmd in &cmds {
                if let Cmd::SetMode(mode) = cmd {
                    self.mode = *mode;
                }
            }
            cmds
        }

        fn key(&mut self, key: &str) -> Vec<Cmd> {
            self.seq += 1;
            let item = Item::Key {
                seq: self.seq,
                key: key.into(),
            };
            let cmds = self.kp.push(item, &self.ctx());
            self.apply(cmds)
        }

        fn reply(&mut self, reply: Reply) -> Vec<Cmd> {
            let cmds = self.kp.on_reply(reply, &self.ctx());
            self.apply(cmds)
        }

        fn mode(&mut self, mode: Mode, blocking: bool) -> Vec<Cmd> {
            self.reply(Reply::Mode { mode, blocking })
        }
    }

    fn input(keys: &str) -> Cmd {
        Cmd::Input(keys.into())
    }

    fn get_mode() -> Cmd {
        Cmd::Request(Req::GetMode)
    }

    fn done(seq: u64, pending: PendingState) -> Cmd {
        Cmd::Emit(FromNeovim::KeyProcessed { seq, pending })
    }

    fn lines(lines: &[&str]) -> Reply {
        Reply::Lines(lines.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn insert_key_is_fire_and_forget() {
        let mut h = Harness::new(Mode::Insert);
        assert_eq!(h.key("a"), vec![input("a"), done(1, PendingState::None)]);
        assert!(!h.kp.is_busy());
    }

    #[test]
    fn completion_insert_modes_take_the_same_path() {
        // Pull reports "ic" while push stays "insert"; both classify as Insert
        let mut h = Harness::new(Mode::from_get_mode("ic"));
        assert_eq!(h.key("a"), vec![input("a"), done(1, PendingState::None)]);
        let mut h = Harness::new(Mode::from_ui("insert").unwrap());
        assert_eq!(h.key("a"), vec![input("a"), done(1, PendingState::None)]);
    }

    #[test]
    fn escape_in_insert_queries_the_mode() {
        let mut h = Harness::new(Mode::Insert);
        assert_eq!(h.key("<Esc>"), vec![input("<Esc>"), get_mode()]);
        assert_eq!(
            h.mode(Mode::Normal, false),
            vec![Cmd::SetMode(Mode::Normal), done(1, PendingState::None)]
        );
    }

    #[test]
    fn other_mode_is_not_fire_and_forget() {
        let mut h = Harness::new(Mode::Other);
        assert_eq!(h.key("a"), vec![input("a"), get_mode()]);
    }

    #[test]
    fn getchar_key_then_argument() {
        let mut h = Harness::new(Mode::Normal);
        assert_eq!(h.key("r"), vec![input("r"), get_mode()]);
        assert_eq!(
            h.mode(Mode::Replace, true),
            vec![Cmd::SetMode(Mode::Replace), done(1, PendingState::Getchar)]
        );
        assert_eq!(h.key("x"), vec![input("x"), get_mode()]);
        assert_eq!(
            h.mode(Mode::Normal, false),
            vec![Cmd::SetMode(Mode::Normal), Cmd::Request(Req::RegRecording)]
        );
        assert_eq!(
            h.reply(Reply::Recording(String::new())),
            vec![
                Cmd::Emit(FromNeovim::Recording(String::new())),
                done(2, PendingState::None)
            ]
        );
    }

    #[test]
    fn r_after_leaving_insert_by_mapping_queries_the_mode() {
        // `inoremap jj <Esc>`: jj is fire-and-forget, then mode_change pushes
        // normal before r is processed
        let mut h = Harness::new(Mode::Insert);
        h.key("j");
        h.key("j");
        h.mode = Mode::from_ui("normal").unwrap();
        assert_eq!(h.key("r"), vec![input("r"), get_mode()]);
        assert_eq!(
            h.mode(Mode::Replace, true),
            vec![Cmd::SetMode(Mode::Replace), done(3, PendingState::Getchar)]
        );
    }

    #[test]
    fn backspace_while_blocked_does_not_read_the_buffer() {
        // r was sent before the mode push arrived, so it looked like insert
        // mode; Neovim is now blocked in getchar
        let mut h = Harness::new(Mode::Insert);
        assert_eq!(h.key("r"), vec![input("r"), done(1, PendingState::None)]);
        assert_eq!(h.key("<BS>"), vec![get_mode()]);
        assert_eq!(
            h.mode(Mode::Replace, true),
            vec![Cmd::SetMode(Mode::Replace), input("<BS>"), get_mode()]
        );
        assert_eq!(
            h.mode(Mode::Normal, false),
            vec![Cmd::SetMode(Mode::Normal), Cmd::Request(Req::RegRecording)]
        );
    }

    #[test]
    fn operator_then_motion() {
        let mut h = Harness::new(Mode::Normal);
        assert_eq!(h.key("d"), vec![input("d"), get_mode()]);
        assert_eq!(
            h.mode(Mode::OperatorPending, false),
            vec![
                Cmd::SetMode(Mode::OperatorPending),
                done(1, PendingState::Motion)
            ]
        );
        assert_eq!(h.key("w"), vec![input("w"), get_mode()]);
        assert_eq!(
            h.mode(Mode::Normal, false),
            vec![Cmd::SetMode(Mode::Normal), done(2, PendingState::None)]
        );
    }

    #[test]
    fn operator_with_text_object_stays_pending() {
        let mut h = Harness::new(Mode::Normal);
        h.key("d");
        h.mode(Mode::OperatorPending, false);
        h.key("i");
        assert_eq!(
            h.mode(Mode::OperatorPending, false),
            vec![
                Cmd::SetMode(Mode::OperatorPending),
                done(2, PendingState::Motion)
            ]
        );
    }

    #[test]
    fn change_motion_into_insert_is_fire_and_forget_afterwards() {
        let mut h = Harness::new(Mode::Normal);
        h.key("c");
        h.mode(Mode::OperatorPending, false);
        h.key("w");
        assert_eq!(
            h.mode(Mode::Insert, false),
            vec![Cmd::SetMode(Mode::Insert), done(2, PendingState::None)]
        );
    }

    #[test]
    fn insert_register() {
        let mut h = Harness::new(Mode::Insert);
        assert_eq!(h.key("<C-r>"), vec![get_mode()]);
        assert_eq!(
            h.mode(Mode::Insert, true),
            vec![Cmd::SetMode(Mode::Insert), input("<C-r>"), get_mode()],
            "blocking before <C-r> is input means an earlier getchar"
        );
    }

    #[test]
    fn insert_register_name() {
        let mut h = Harness::new(Mode::Insert);
        h.key("<C-r>");
        assert_eq!(
            h.mode(Mode::Insert, false),
            vec![
                Cmd::SetMode(Mode::Insert),
                input("<C-r>"),
                done(1, PendingState::InsertRegister)
            ]
        );
        // <C-r><C-r> keeps waiting for the name
        assert_eq!(
            h.key("<C-r>"),
            vec![input("<C-r>"), done(2, PendingState::InsertRegister)]
        );
        assert_eq!(h.key("a"), vec![input("a"), done(3, PendingState::None)]);
    }

    #[test]
    fn ctrl_r_outside_insert_is_a_normal_key() {
        // <C-r> is redo in normal mode
        let mut h = Harness::new(Mode::Normal);
        h.key("<C-r>");
        assert_eq!(
            h.mode(Mode::Normal, false),
            vec![Cmd::SetMode(Mode::Normal), input("<C-r>"), get_mode()]
        );
    }

    #[test]
    fn normal_register() {
        let mut h = Harness::new(Mode::Normal);
        assert_eq!(h.key("\""), vec![get_mode()]);
        assert_eq!(
            h.mode(Mode::Normal, false),
            vec![
                Cmd::SetMode(Mode::Normal),
                input("\""),
                done(1, PendingState::NormalRegister)
            ]
        );
        assert_eq!(h.key("a"), vec![input("a"), done(2, PendingState::None)]);
        assert_eq!(h.key("y"), vec![input("y"), get_mode()]);
    }

    #[test]
    fn quote_in_insert_is_text() {
        let mut h = Harness::new(Mode::Insert);
        h.key("\"");
        assert_eq!(
            h.mode(Mode::Insert, false),
            vec![
                Cmd::SetMode(Mode::Insert),
                input("\""),
                done(1, PendingState::None)
            ]
        );
    }

    #[test]
    fn line_keys_pass_through_on_empty_buffer() {
        for key in ["<BS>", "<CR>", COMMIT] {
            let mut h = Harness::new(Mode::Insert);
            assert_eq!(h.key(key), vec![get_mode()]);
            assert_eq!(
                h.mode(Mode::Insert, false),
                vec![Cmd::SetMode(Mode::Insert), Cmd::Request(Req::BufLines)]
            );
            assert_eq!(
                h.reply(lines(&[""])),
                vec![
                    Cmd::Emit(FromNeovim::PassthroughKey { seq: 1 }),
                    done(1, PendingState::None)
                ],
                "{key}"
            );
        }
    }

    #[test]
    fn line_keys_edit_non_empty_buffer() {
        for (key, sent) in [("<BS>", "<BS>"), ("<A-CR>", "<CR>")] {
            let mut h = Harness::new(Mode::Insert);
            h.key(key);
            h.mode(Mode::Insert, false);
            // An empty 2nd line is not an empty buffer
            assert_eq!(
                h.reply(lines(&["", ""])),
                vec![input(sent), done(1, PendingState::None)]
            );
        }
    }

    #[test]
    fn commit_joins_lines_and_resets_before_later_keys() {
        let mut h = Harness::new(Mode::Insert);
        h.key(COMMIT);
        // Typed right after the commit key: must run after the reset
        assert!(h.key("x").is_empty());
        h.mode(Mode::Insert, false);
        assert_eq!(
            h.reply(lines(&["foo", "bar"])),
            vec![
                Cmd::Emit(FromNeovim::Commit("foo\nbar".into())),
                input(COMMIT_RESET_KEYS),
                done(1, PendingState::None),
                input("x"),
                done(2, PendingState::None),
            ]
        );
    }

    #[test]
    fn line_keys_continue_a_register_sequence() {
        let mut h = Harness::new(Mode::Insert);
        h.key("<C-r>");
        h.mode(Mode::Insert, false);
        assert_eq!(
            h.key("<BS>"),
            vec![input("<BS>"), done(2, PendingState::None)]
        );
    }

    #[test]
    fn cmdline_keys_are_input_only() {
        // Same for :, / and input() from a plugin
        let mut h = Harness::new(Mode::CmdLine);
        for key in ["a", "<BS>", "<C-r>", "\"", "<CR>"] {
            let cmds = h.key(key);
            assert_eq!(
                cmds,
                vec![input(key), done(h.seq, PendingState::CommandLine)]
            );
        }
    }

    #[test]
    fn colon_enters_cmdline_through_the_mode_query() {
        let mut h = Harness::new(Mode::Normal);
        assert_eq!(h.key(":"), vec![input(":"), get_mode()]);
        assert_eq!(
            h.mode(Mode::CmdLine, false),
            vec![
                Cmd::SetMode(Mode::CmdLine),
                done(1, PendingState::CommandLine)
            ]
        );
        assert_eq!(
            h.key("w"),
            vec![input("w"), done(2, PendingState::CommandLine)]
        );
    }

    #[test]
    fn key_after_cmdline_closes_uses_the_new_mode() {
        let mut h = Harness::new(Mode::CmdLine);
        h.key("<CR>");
        h.mode = Mode::from_ui("normal").unwrap();
        assert_eq!(h.key("x"), vec![input("x"), get_mode()]);
    }

    #[test]
    fn q_updates_recording() {
        let mut h = Harness::new(Mode::Normal);
        h.key("q");
        h.mode(Mode::Normal, true);
        h.key("a");
        h.mode(Mode::Normal, false);
        assert_eq!(
            h.reply(Reply::Recording("a".into())),
            vec![
                Cmd::Emit(FromNeovim::Recording("a".into())),
                done(2, PendingState::None)
            ]
        );
        // q again stops it
        h.key("q");
        assert_eq!(
            h.mode(Mode::Normal, false),
            vec![Cmd::SetMode(Mode::Normal), Cmd::Request(Req::RegRecording)]
        );
    }

    #[test]
    fn insert_keys_waiting_for_an_argument() {
        for key in ["<C-k>", "<C-v>", "<C-q>"] {
            let mut h = Harness::new(Mode::Insert);
            assert_eq!(h.key(key), vec![input(key), get_mode()]);
            assert_eq!(
                h.mode(Mode::Insert, true),
                vec![Cmd::SetMode(Mode::Insert), done(1, PendingState::Getchar)]
            );
        }
    }

    #[test]
    fn keys_are_processed_one_at_a_time() {
        let mut h = Harness::new(Mode::Normal);
        assert_eq!(h.key("x"), vec![input("x"), get_mode()]);
        assert!(h.key("y").is_empty());
        assert!(h.key("z").is_empty());
        assert_eq!(h.kp.queued_len(), 2);
        assert_eq!(
            h.mode(Mode::Normal, false),
            vec![
                Cmd::SetMode(Mode::Normal),
                done(1, PendingState::None),
                input("y"),
                get_mode()
            ]
        );
    }

    #[test]
    fn queued_key_starts_with_the_replied_mode() {
        // The model still says Normal, but <Esc>i just replied Insert
        let mut h = Harness::new(Mode::Normal);
        h.kp.push(Item::Input("<Esc>i".into()), &h.ctx());
        h.key("a");
        let cmds = h.kp.on_reply(
            Reply::Mode {
                mode: Mode::Insert,
                blocking: false,
            },
            &Ctx { mode: Mode::Normal },
        );
        assert_eq!(
            cmds,
            vec![
                Cmd::SetMode(Mode::Insert),
                input("a"),
                done(1, PendingState::None)
            ]
        );
    }

    #[test]
    fn input_item_is_not_acknowledged() {
        let mut h = Harness::new(Mode::Insert);
        let cmds = h.kp.push(Item::Input("<Esc><Esc>ggdG".into()), &h.ctx());
        assert_eq!(cmds, vec![input("<Esc><Esc>ggdG"), get_mode()]);
        assert_eq!(
            h.mode(Mode::Normal, false),
            vec![Cmd::SetMode(Mode::Normal)]
        );
        assert!(!h.kp.is_busy());
    }

    #[test]
    fn input_item_syncs_pending() {
        let mut h = Harness::new(Mode::Normal);
        h.key("r");
        h.mode(Mode::Replace, true);
        h.kp.push(Item::Input("<Esc>".into()), &h.ctx());
        h.mode(Mode::Normal, false);
        assert_eq!(h.key("x"), vec![input("x"), get_mode()]);
    }

    #[test]
    fn discard_drops_only_unstarted_items() {
        let mut h = Harness::new(Mode::Normal);
        h.key("x");
        h.key("y");
        h.kp.push(Item::Input("<Esc>i".into()), &h.ctx());
        h.key("z");
        assert_eq!(h.kp.discard_queued(), vec![2, 3]);
        assert_eq!(
            h.mode(Mode::Normal, false),
            vec![Cmd::SetMode(Mode::Normal), done(1, PendingState::None)]
        );
        assert!(!h.kp.is_busy());
    }

    #[test]
    fn request_error_still_acknowledges() {
        let mut h = Harness::new(Mode::Insert);
        h.key("<BS>");
        h.mode(Mode::Insert, false);
        assert_eq!(
            h.reply(Reply::Err("boom".into())),
            vec![done(1, PendingState::None)]
        );
        assert!(!h.kp.is_busy());
    }

    #[test]
    fn unexpected_reply_is_ignored() {
        let mut h = Harness::new(Mode::Insert);
        assert!(h.reply(lines(&["x"])).is_empty());
    }
}
