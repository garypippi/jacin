//! Integration tests for Neovim backend.
//!
//! These tests spawn a real headless Neovim process and verify the
//! communication protocol. They require `nvim` in PATH and are gated
//! behind `#[ignore]` — run with `cargo test -- --ignored`.

use std::collections::VecDeque;
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

use super::protocol::PendingState;
use super::{FromNeovim, Mode, Neovim, ReadStatus};
use crate::config::Config;
use crate::model::Model;
use crate::state::{Modifiers, SentKey};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const MSG_TIMEOUT: Duration = Duration::from_secs(5);

fn clean_config_with_startinsert(startinsert: bool) -> Config {
    let mut config = Config {
        clean: true,
        ..Config::default()
    };
    config.behavior.startinsert = startinsert;
    config
}

/// A Neovim backend whose every message is applied to a `Model` as it is
/// handled, so the buffer mirror, mode, recording and screen follow Neovim
/// like in jacin. Handled messages are also kept for the test to inspect.
struct Probe {
    nvim: Neovim,
    model: Model,
    /// Handled messages not taken by `recv_timeout` yet
    received: VecDeque<FromNeovim>,
    exited: bool,
}

impl Probe {
    /// Spawn Neovim with --clean and wait for Ready.
    fn spawn(startinsert: bool) -> Self {
        Self::spawn_with(startinsert, None)
    }

    /// Like `spawn`, with the UI sized cols x rows.
    fn spawn_with(startinsert: bool, ui_size: Option<(usize, usize)>) -> Self {
        let nvim = Neovim::spawn(&clean_config_with_startinsert(startinsert), ui_size)
            .expect("failed to spawn neovim");
        let mut model = Model::new();
        model.ime.start_enabling();
        model.ime.complete_enabling();
        let mut probe = Self {
            nvim,
            model,
            received: VecDeque::new(),
            exited: false,
        };
        probe
            .recv_until(|m| matches!(m, FromNeovim::Ready), STARTUP_TIMEOUT)
            .expect("Neovim did not send Ready");
        probe
    }

    fn apply(&mut self, events: Vec<FromNeovim>) {
        for event in events {
            self.model.reduce(event.clone());
            self.received.push_back(event);
        }
    }

    /// Wait up to `timeout` for Neovim's pipes, then handle what it sent
    fn pump(&mut self, timeout: Duration) {
        if self.exited {
            std::thread::sleep(timeout);
            return;
        }
        let mut fds = [
            libc::pollfd {
                fd: self.nvim.read_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: self.nvim.write_fd().as_raw_fd(),
                events: if self.nvim.wants_write() {
                    libc::POLLOUT
                } else {
                    0
                },
                revents: 0,
            },
        ];
        // SAFETY: fds is a valid pollfd array for the duration of the call
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout.as_millis() as i32) };
        if ready <= 0 {
            return;
        }
        if fds[1].revents != 0 {
            self.nvim.flush();
        }
        if fds[0].revents == 0 {
            return;
        }
        let status = self.nvim.read();
        loop {
            let mode = self.model.view.mode;
            match self.nvim.next_message(mode).expect("broken stream") {
                Some(events) => self.apply(events),
                None => break,
            }
        }
        if status == ReadStatus::Eof {
            self.exited = true;
            self.apply(vec![FromNeovim::NvimExited]);
        }
    }

    /// The next handled message, waiting up to `timeout` for one
    fn recv_timeout(&mut self, timeout: Duration) -> Option<FromNeovim> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(msg) = self.received.pop_front() {
                return Some(msg);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            self.pump(remaining.min(Duration::from_millis(100)));
        }
    }

    /// Receive messages until one matches, or timeout.
    fn recv_until(
        &mut self,
        predicate: impl Fn(&FromNeovim) -> bool,
        timeout: Duration,
    ) -> Option<FromNeovim> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let msg = self.recv_timeout(remaining)?;
            if predicate(&msg) {
                return Some(msg);
            }
        }
    }

    /// Receive messages until the model satisfies `cond`, or panic.
    fn wait(&mut self, what: &str, cond: impl Fn(&Model) -> bool) {
        let deadline = Instant::now() + MSG_TIMEOUT;
        while !cond(&self.model) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if self.recv_timeout(remaining).is_none() {
                panic!(
                    "timed out waiting for {what}: buffer={:?} mode={:?}",
                    self.model.view.buffer.text(),
                    self.model.view.mode
                );
            }
        }
    }

    fn wait_buffer(&mut self, text: &str) {
        self.wait(&format!("buffer {text:?}"), |m| {
            m.view.buffer.text() == text
        });
    }

    /// Send a key without waiting; returns its sequence number.
    fn send(&mut self, key: &str) -> u64 {
        let seq = self.model.sent_keys.push(SentKey {
            keycode: 0,
            mods: Modifiers::default(),
            vim_key: key.to_string(),
            ctrl: false,
            alt: false,
        });
        let events = self.nvim.push_key(seq, key, self.model.view.mode);
        self.apply(events);
        seq
    }

    /// Send a key and collect messages up to and including its `KeyProcessed`.
    fn key(&mut self, key: &str) -> Vec<FromNeovim> {
        let seq = self.send(key);
        let mut msgs = Vec::new();
        loop {
            let msg = self
                .recv_timeout(MSG_TIMEOUT)
                .unwrap_or_else(|| panic!("no KeyProcessed for key {key:?}"));
            let done = matches!(msg, FromNeovim::KeyProcessed { seq: s, .. } if s == seq);
            msgs.push(msg);
            if done {
                return msgs;
            }
        }
    }

    fn keys(&mut self, keys: &[&str]) -> Vec<FromNeovim> {
        keys.iter().flat_map(|k| self.key(k)).collect()
    }

    /// Reset Neovim like IME off (`insert: false`) or a commit / enabling
    fn reset(&mut self, insert: bool) {
        let events = self.nvim.reset(insert, self.model.view.mode);
        self.apply(events);
    }

    /// Reset, then wait until Neovim reports the mode it leaves
    fn reset_and_wait(&mut self, insert: bool) {
        self.reset(insert);
        let mode = if insert { Mode::Insert } else { Mode::Normal };
        self.wait(&format!("{mode:?} after reset"), |m| {
            m.view.buffer.text().is_empty() && m.view.mode == mode
        });
    }

    /// Whether a message matching `predicate` is among `msgs` (already
    /// received) or arrives within the timeout.
    fn saw(&mut self, msgs: &[FromNeovim], predicate: impl Fn(&FromNeovim) -> bool) -> bool {
        msgs.iter().any(&predicate) || self.recv_until(predicate, MSG_TIMEOUT).is_some()
    }

    fn shutdown(self) {
        self.nvim.shutdown();
    }
}

fn ack_pending(msgs: &[FromNeovim]) -> PendingState {
    match msgs.last() {
        Some(FromNeovim::KeyProcessed { pending, .. }) => *pending,
        other => panic!("expected KeyProcessed last, got {other:?}"),
    }
}

fn has_passthrough(msgs: &[FromNeovim]) -> bool {
    msgs.iter()
        .any(|m| matches!(m, FromNeovim::PassthroughKey { .. }))
}

#[test]
#[ignore]
fn spawn_and_shutdown() {
    Probe::spawn(true).shutdown();
}

/// Typing in insert mode updates the buffer mirror and the window grid.
#[test]
#[ignore]
fn insert_mode_typing_updates_buffer_and_window() {
    let mut probe = Probe::spawn(true);

    for ch in ["h", "e", "l", "l", "o"] {
        probe.send(ch);
    }
    probe.wait_buffer("hello");
    probe.wait("window row 'hello'", |m| {
        m.view.screen.window_view(8).is_some_and(|v| {
            let row: String = v.rows[0].iter().map(|c| c.text.as_str()).collect();
            row == "hello" && v.cursor == (0, 5)
        })
    });

    probe.shutdown();
}

#[test]
#[ignore]
fn escape_switches_to_normal_mode() {
    let mut probe = Probe::spawn(true);

    probe.keys(&["h", "i"]);
    probe.wait_buffer("hi");
    probe.key("<Esc>");
    probe.wait("normal mode", |m| m.view.mode == Mode::Normal);

    probe.shutdown();
}

#[test]
#[ignore]
fn startinsert_true_starts_in_insert_mode() {
    let mut probe = Probe::spawn(true);

    // No 'i' needed
    probe.key("h");
    probe.wait_buffer("h");

    probe.shutdown();
}

#[test]
#[ignore]
fn startinsert_false_starts_in_normal_mode() {
    let mut probe = Probe::spawn(false);

    // 'h' is a normal-mode motion: nothing is inserted
    probe.key("h");
    probe.key("l");
    assert!(probe.model.view.buffer.is_empty());

    probe.keys(&["i", "h"]);
    probe.wait_buffer("h");

    probe.shutdown();
}

#[test]
#[ignore]
fn getchar_completion_acknowledges_key_with_pending_state() {
    let mut probe = Probe::spawn(false);

    probe.keys(&["i", "a", "b", "c", "<Esc>"]);

    // "r" blocks in getchar → acknowledged with Getchar pending
    let msgs = probe.key("r");
    assert_eq!(ack_pending(&msgs), PendingState::Getchar);

    // Completing getchar is acknowledged with no pending state
    let msgs = probe.key("x");
    assert_eq!(ack_pending(&msgs), PendingState::None);
    probe.wait_buffer("abx");

    probe.shutdown();
}

/// Operator-pending (d) waits for the motion, then applies it.
#[test]
#[ignore]
fn operator_pending_then_motion() {
    let mut probe = Probe::spawn(false);

    probe.keys(&["i", "a", "b", "<Esc>", "0"]);
    let msgs = probe.key("d");
    assert_eq!(ack_pending(&msgs), PendingState::Motion);
    let msgs = probe.key("l");
    assert_eq!(ack_pending(&msgs), PendingState::None);
    probe.wait_buffer("b");

    probe.shutdown();
}

/// q{reg} starts recording, q stops it (REC indicator).
#[test]
#[ignore]
fn macro_recording_is_reported() {
    let mut probe = Probe::spawn(false);

    probe.keys(&["q", "a"]);
    probe.wait("recording a", |m| m.view.recording == "a");
    probe.key("q");
    probe.wait("recording stopped", |m| m.view.recording.is_empty());

    probe.shutdown();
}

/// Leaving the command line ends the command-line pending state.
#[test]
#[ignore]
fn leaving_cmdline_clears_pending() {
    let mut probe = Probe::spawn(false);

    let msgs = probe.keys(&[":", "echo 1"]);
    assert_eq!(ack_pending(&msgs), PendingState::CommandLine);
    let msgs = probe.key("<CR>");
    assert!(
        probe.saw(&msgs, |m| matches!(m, FromNeovim::CmdlineHide { level: 1 })),
        "expected cmdline_hide"
    );

    // Back to normal keys: a motion is not forwarded as command-line input
    let msgs = probe.key("l");
    assert_eq!(ack_pending(&msgs), PendingState::None);

    probe.shutdown();
}

#[test]
#[ignore]
fn quit_exits_with_modified_buffer() {
    let mut probe = Probe::spawn(true);
    // buftype=nofile: plain :q must exit even with a modified buffer (no E37)
    probe.keys(&["a", "b", "<Esc>", ":", "q"]);
    // Neovim exits before answering, so <CR> is never acknowledged
    probe.send("<CR>");
    assert!(
        probe
            .recv_until(|m| matches!(m, FromNeovim::NvimExited), MSG_TIMEOUT)
            .is_some(),
        "expected NvimExited after :q"
    );
}

/// After resizing the UI, a line longer than the width wraps in the window
/// grid and window_view shows it as two rows of one buffer line.
#[test]
#[ignore]
fn resized_ui_wraps_long_line_in_window_view() {
    let mut probe = Probe::spawn_with(true, Some((20, 9)));

    let text = "abcdefghijklmnopqrstuvwxy"; // 25 chars > 20 columns
    for c in text.chars() {
        probe.send(&c.to_string());
    }
    probe.wait("wrapped view", |m| {
        m.view.screen.window_view(8).is_some_and(|view| {
            let rows: Vec<String> = view
                .rows
                .iter()
                .map(|r| r.iter().map(|c| c.text.as_str()).collect())
                .collect();
            rows.len() == 2 && rows.concat() == text && view.cursor == (1, 5)
        })
    });

    probe.shutdown();
}

/// A float opened with focus (e.g. a dictionary registration window) is
/// overlaid on the text window, with the cursor drawn in it.
#[test]
#[ignore]
fn focused_float_is_overlaid_with_cursor() {
    let mut probe = Probe::spawn_with(true, Some((20, 9)));

    probe.keys(&["a", "b", "<Esc>", ":"]);
    probe.keys(&[
        "call nvim_open_win(nvim_create_buf(0, 1), 1, {'relative': 'cursor', \
         'row': 1, 'col': 0, 'width': 8, 'height': 1, 'border': 'single'})",
        "<CR>",
        "i",
        "x",
        "y",
    ]);
    probe.wait("float with cursor", |m| {
        m.view.screen.window_view(8).is_some_and(|view| {
            let text: Vec<String> = view
                .rows
                .iter()
                .map(|r| r.iter().map(|c| c.text.as_str()).collect())
                .collect();
            let [float] = view.floats.as_slice() else {
                return false;
            };
            let float_text: String = float.rows[1].iter().map(|c| c.text.as_str()).collect();
            // Border row, then the text row; the cursor after "xy"
            text == ["ab"]
                && float.row == 1
                && float_text.contains("xy")
                && view.cursor_on_float
                && view.cursor == (2, float.col + 3)
        })
    });

    probe.shutdown();
}

/// Neovim is started with `g:jacin = 1` so user config can detect jacin.
#[test]
#[ignore]
fn g_jacin_is_set_for_user_config() {
    let mut probe = Probe::spawn(false);

    let msgs = probe.keys(&[":", "echo g:jacin", "<CR>"]);
    assert!(
        probe.saw(&msgs, |m| {
            matches!(m, FromNeovim::CmdlineMessage { text, .. } if text == "1")
        }),
        "expected `:echo g:jacin` to print 1"
    );

    probe.shutdown();
}

/// Multiline: <CR> is a native newline, <CR>/<BS> on an empty 2nd line stay
/// in Neovim, and the commit key commits all lines joined with "\n"
/// (trailing empty line kept).
#[test]
#[ignore]
fn multiline_enter_and_commit() {
    let mut probe = Probe::spawn(true);

    let msgs = probe.keys(&["a", "<CR>", "<CR>", "<BS>", "b", "<CR>"]);
    assert!(!has_passthrough(&msgs), "unexpected passthrough: {msgs:?}");
    probe.wait_buffer("a\nb\n");

    let msgs = probe.key("<C-CR>");
    assert!(
        msgs.iter()
            .any(|m| matches!(m, FromNeovim::Commit(text) if text == "a\nb\n")),
        "expected Commit 'a\\nb\\n', got {msgs:?}"
    );

    probe.shutdown();
}

/// Passthrough only when the whole buffer is empty.
#[test]
#[ignore]
fn empty_buffer_passes_through_enter_bs_and_commit() {
    let mut probe = Probe::spawn(true);

    for key in ["<CR>", "<BS>", "<C-CR>"] {
        let msgs = probe.key(key);
        assert!(
            has_passthrough(&msgs),
            "expected passthrough for {key}: {msgs:?}"
        );
    }

    probe.shutdown();
}

/// Another buffer in the main window (:enew, :e memo.md) only hides the
/// main buffer: it stays attached, and a reset brings it back.
#[test]
#[ignore]
fn main_buffer_survives_another_buffer_in_its_window() {
    let mut probe = Probe::spawn(true);

    probe.keys(&["a", "<CR>", "b"]);
    probe.wait_buffer("a\nb");

    probe.keys(&["<Esc>", ":", "enew", "<CR>", "i", "x"]);
    probe.keys(&["<Esc>", ":", "echo 'synced'", "<CR>"]);
    probe.wait("the echo", |m| {
        m.view.transient_message.as_deref() == Some("synced")
    });
    assert_eq!(probe.model.view.buffer.text(), "a\nb");

    // Toggle-off clears the main buffer; typing goes to it again
    probe.reset_and_wait(false);
    probe.keys(&["i", "z"]);
    probe.wait_buffer("z");

    probe.shutdown();
}

/// Toggle-off clears the buffer even while an insert-mode key waits for its
/// argument (the keys must not be inserted as text).
#[test]
#[ignore]
fn reset_works_after_pending_insert_keys() {
    let mut probe = Probe::spawn(true);

    for pending_key in ["<C-r>", "<C-v>", "<C-k>"] {
        probe.keys(&["a", "b", "c", pending_key]);
        probe.reset_and_wait(false);
        // Back to insert mode for the next case, as re-enabling does
        probe.reset_and_wait(true);
    }
    probe.key("x");
    probe.wait_buffer("x");

    probe.shutdown();
}

/// A reset gets back to an empty buffer from any mode.
#[test]
#[ignore]
fn reset_works_from_any_mode() {
    let mut probe = Probe::spawn(true);

    let cases: &[&[&str]] = &[
        // command line
        &["a", "<Esc>", ":", "echo 1"],
        // input() from insert mode: leaving it returns to insert mode
        &["a", "<C-o>", ":", "call input('x: ')", "<CR>", "zz"],
        // operator pending with a count
        &["a", "<Esc>", "2", "d"],
        &["a", "<Esc>", "v"],
        // r waiting for its argument
        &["a", "<Esc>", "r"],
    ];
    for keys in cases {
        probe.keys(keys);
        probe.reset_and_wait(true);
        probe.key("x");
        probe.wait_buffer("x");
        probe.reset_and_wait(true);
    }

    probe.shutdown();
}

/// A floating window with the cursor (like a plugin's prompt) is closed,
/// and the main buffer is cleared rather than the float's.
#[test]
#[ignore]
fn reset_closes_a_focused_float() {
    let mut probe = Probe::spawn(true);

    probe.keys(&["a", "b"]);
    probe.wait_buffer("ab");
    open_focused_float(&mut probe);
    probe.keys(&["f", "o", "o"]);

    probe.reset_and_wait(true);
    probe.key("x");
    probe.wait_buffer("x");

    probe.shutdown();
}

/// The commit key commits the main buffer even while a floating window has
/// the cursor, and the reset after it closes the float.
#[test]
#[ignore]
fn commit_from_a_focused_float_commits_the_main_buffer() {
    let mut probe = Probe::spawn(true);

    probe.keys(&["a", "b"]);
    probe.wait_buffer("ab");
    open_focused_float(&mut probe);
    probe.keys(&["f", "o", "o"]);

    let msgs = probe.key("<C-CR>");
    assert!(
        msgs.iter()
            .any(|m| matches!(m, FromNeovim::Commit(text) if text == "ab")),
        "expected Commit 'ab', got {msgs:?}"
    );
    probe.wait("reset after commit", |m| {
        m.view.buffer.text().is_empty() && m.view.mode == Mode::Insert
    });
    probe.key("x");
    probe.wait_buffer("x");

    probe.shutdown();
}

/// A reset brings back one window showing the main buffer: jacin shows
/// neither the tabline nor other windows.
#[test]
#[ignore]
fn reset_closes_other_tabs_and_windows() {
    let mut probe = Probe::spawn(true);

    probe.keys(&["a", "b"]);
    probe.wait_buffer("ab");
    for command in ["tabnew", "new"] {
        probe.keys(&["<Esc>", ":", command, "<CR>", "i", "x", "y"]);
    }

    let msgs = probe.key("<C-CR>");
    assert!(
        msgs.iter()
            .any(|m| matches!(m, FromNeovim::Commit(text) if text == "ab")),
        "expected Commit 'ab', got {msgs:?}"
    );
    probe.wait("reset after commit", |m| {
        m.view.buffer.text().is_empty() && m.view.mode == Mode::Insert
    });

    // Typing goes to the main buffer, in the only window
    probe.key("z");
    probe.wait_buffer("z");
    probe.keys(&["<Esc>", ":", "echo tabpagenr('$') .. winnr('$')", "<CR>"]);
    probe.wait("one tab page and one window", |m| {
        m.view.transient_message.as_deref() == Some("11")
    });

    probe.shutdown();
}

/// Open a floating window with the cursor in it, in insert mode
fn open_focused_float(probe: &mut Probe) {
    probe.keys(&[
        "<C-o>",
        ":",
        "call nvim_open_win(nvim_create_buf(0, 1), 1, #{relative: 'editor', row: 1, col: 1, width: 20, height: 1})",
        "<CR>",
    ]);
    probe.wait("insert mode in the float", |m| m.view.mode == Mode::Insert);
}

/// A reset is not acknowledged, so the next key's wait ends only at its own
/// `KeyProcessed`.
#[test]
#[ignore]
fn reset_is_not_acknowledged() {
    let mut probe = Probe::spawn(true);

    probe.keys(&["a", "b"]);
    probe.reset(true);
    let msgs = probe.key("c");
    let acks = msgs
        .iter()
        .filter(|m| matches!(m, FromNeovim::KeyProcessed { .. }))
        .count();
    assert_eq!(acks, 1, "unexpected acknowledgments: {msgs:?}");
    probe.wait_buffer("c");

    probe.shutdown();
}

/// A passthrough names the key it belongs to.
#[test]
#[ignore]
fn passthrough_carries_its_key_seq() {
    let mut probe = Probe::spawn(true);

    let seq = probe.send("<BS>");
    let msg = probe.recv_until(
        |m| matches!(m, FromNeovim::PassthroughKey { .. }),
        MSG_TIMEOUT,
    );
    assert!(
        matches!(msg, Some(FromNeovim::PassthroughKey { seq: s }) if s == seq),
        "expected PassthroughKey for key #{seq}, got {msg:?}"
    );

    probe.shutdown();
}

/// Leaving insert mode by a mapping is followed: `r` then waits for its
/// argument, and a <BS> right after it neither deadlocks nor edits the
/// buffer (nvim_buf_get_lines would not answer while blocked in getchar).
#[test]
#[ignore]
fn escape_by_mapping_then_getchar_key() {
    let mut probe = Probe::spawn(true);

    probe.keys(&["<Esc>", ":", "inoremap jj <lt>Esc>", "<CR>", "i", "a", "b"]);
    probe.wait_buffer("ab");
    // Sent back to back: r may start before mode_change reports normal mode
    probe.send("j");
    probe.send("j");
    probe.send("r");
    // Cancels r
    probe.key("<BS>");
    probe.wait("normal mode", |m| m.view.mode == Mode::Normal);

    let msgs = probe.key("r");
    assert_eq!(ack_pending(&msgs), PendingState::Getchar);
    probe.key("x");
    probe.wait_buffer("ax");

    probe.shutdown();
}

/// The key right after executing a command line is a normal-mode key, even
/// before cmdline_hide / mode_change arrive.
#[test]
#[ignore]
fn key_right_after_cmdline_is_normal_mode_key() {
    let mut probe = Probe::spawn(false);

    probe.keys(&["i", "a", "<Esc>", ":", "echo 1"]);
    probe.send("<CR>");
    let msgs = probe.key("r");
    assert_eq!(ack_pending(&msgs), PendingState::Getchar);
    probe.key("x");
    probe.wait_buffer("x");

    probe.shutdown();
}

/// Keys typed right after the commit key (queued before the commit is
/// known) are not cleared with the committed text.
#[test]
#[ignore]
fn key_typed_right_after_commit_is_kept() {
    let mut probe = Probe::spawn(true);

    probe.keys(&["a", "b"]);
    probe.wait_buffer("ab");
    probe.send("<C-CR>");
    probe.send("c");
    assert!(
        probe
            .recv_until(
                |m| matches!(m, FromNeovim::Commit(text) if text == "ab"),
                MSG_TIMEOUT
            )
            .is_some(),
        "expected Commit 'ab'"
    );
    probe.wait_buffer("c");

    probe.shutdown();
}

/// A prompt opened while the completion menu is shown (like a plugin's
/// confirm()) shows its question and prompt instead of the menu.
#[test]
#[ignore]
fn prompt_over_completion_menu_is_shown() {
    let mut probe = Probe::spawn(true);

    probe.keys(&[
        "<C-o>",
        ":",
        "set completeopt=menuone,noselect \
         | inoremap <lt>F2> <lt>Cmd>call confirm('Really?', \"&Yes\\n&No\")<lt>CR>\
         | inoremap <lt>F3> <lt>Cmd>call complete(1, ['foo', 'bar'])<lt>CR>",
        "<CR>",
    ]);
    probe.send("<F3>");
    probe.wait("completion menu", |m| m.view.candidates.len() == 2);
    // Nothing selected ('completeopt' noselect)
    assert_eq!(probe.model.view.selected_candidate, None);

    probe.send("<F2>");
    probe.wait("confirm prompt", |m| {
        m.view
            .cmdline
            .as_ref()
            .is_some_and(|c| c.text.contains("(N)o"))
    });
    assert_eq!(
        probe.model.view.transient_message.as_deref(),
        Some("Really?")
    );
    assert!(probe.model.view.visible_candidates().is_empty());

    probe.key("n");
    probe.wait("prompt closed", |m| m.view.cmdline.is_none());

    probe.shutdown();
}
