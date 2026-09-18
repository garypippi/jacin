//! Integration tests for Neovim backend.
//!
//! These tests spawn a real headless Neovim process and verify the
//! communication protocol. They require `nvim` in PATH and are gated
//! behind `#[ignore]` — run with `cargo test -- --ignored`.

use std::time::{Duration, Instant};

use super::{FromNeovim, PendingState, spawn_neovim};
use crate::config::Config;
use crate::model::Model;

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

/// A Neovim backend whose every message is applied to a `Model`, so the
/// buffer mirror, mode, recording and screen follow Neovim like in jacin.
struct Probe {
    handle: super::NeovimHandle,
    model: Model,
}

impl Probe {
    /// Spawn Neovim with --clean and wait for Ready.
    fn spawn(startinsert: bool) -> Self {
        Self::spawn_with(startinsert, None)
    }

    /// Like `spawn`, resizing the UI (cols x rows) before Ready.
    fn spawn_with(startinsert: bool, ui_size: Option<(usize, usize)>) -> Self {
        let handle = spawn_neovim(clean_config_with_startinsert(startinsert), None)
            .expect("failed to spawn neovim");
        if let Some((cols, rows)) = ui_size {
            handle.resize_ui(cols, rows);
        }
        let mut model = Model::new();
        model.ime.start_enabling();
        model.ime.complete_enabling();
        let mut probe = Self { handle, model };
        probe
            .recv_until(|m| matches!(m, FromNeovim::Ready), STARTUP_TIMEOUT)
            .expect("Neovim did not send Ready");
        probe
    }

    /// Receive (and apply) messages until one matches, or timeout.
    fn recv_until(
        &mut self,
        predicate: impl Fn(&FromNeovim) -> bool,
        timeout: Duration,
    ) -> Option<FromNeovim> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            let Some(msg) = self
                .handle
                .recv_timeout(remaining.min(Duration::from_millis(100)))
            else {
                continue;
            };
            let matched = predicate(&msg);
            self.model.reduce(msg.clone());
            if matched {
                return Some(msg);
            }
        }
    }

    /// Receive messages until the model satisfies `cond`, or panic.
    fn wait(&mut self, what: &str, cond: impl Fn(&Model) -> bool) {
        let deadline = Instant::now() + MSG_TIMEOUT;
        while !cond(&self.model) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let msg = self.handle.recv_timeout(remaining).unwrap_or_else(|| {
                panic!(
                    "timed out waiting for {what}: buffer={:?} mode={:?}",
                    self.model.view.buffer.text(),
                    self.model.view.vim_mode
                )
            });
            self.model.reduce(msg);
        }
    }

    fn wait_buffer(&mut self, text: &str) {
        self.wait(&format!("buffer {text:?}"), |m| {
            m.view.buffer.text() == text
        });
    }

    /// Send a key and collect messages up to and including its `KeyProcessed`.
    fn key(&mut self, key: &str) -> Vec<FromNeovim> {
        self.handle.send_key(key);
        let mut msgs = Vec::new();
        loop {
            let msg = self
                .recv_until(|_| true, MSG_TIMEOUT)
                .unwrap_or_else(|| panic!("no KeyProcessed for key {key:?}"));
            let done = matches!(msg, FromNeovim::KeyProcessed { .. });
            msgs.push(msg);
            if done {
                return msgs;
            }
        }
    }

    fn keys(&mut self, keys: &[&str]) -> Vec<FromNeovim> {
        keys.iter().flat_map(|k| self.key(k)).collect()
    }

    /// Whether a message matching `predicate` is among `msgs` (already
    /// received) or arrives within the timeout.
    fn saw(&mut self, msgs: &[FromNeovim], predicate: impl Fn(&FromNeovim) -> bool) -> bool {
        msgs.iter().any(&predicate) || self.recv_until(predicate, MSG_TIMEOUT).is_some()
    }

    /// Shutdown Neovim and wait for NvimExited confirmation.
    fn shutdown(mut self) {
        self.handle.shutdown();
        let exited = self.recv_until(|m| matches!(m, FromNeovim::NvimExited), MSG_TIMEOUT);
        assert!(exited.is_some(), "expected NvimExited after shutdown");
    }
}

fn ack_pending(msgs: &[FromNeovim]) -> PendingState {
    match msgs.last() {
        Some(FromNeovim::KeyProcessed { pending }) => *pending,
        other => panic!("expected KeyProcessed last, got {other:?}"),
    }
}

fn has_passthrough(msgs: &[FromNeovim]) -> bool {
    msgs.iter().any(|m| matches!(m, FromNeovim::PassthroughKey))
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
        probe.handle.send_key(ch);
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
    probe.wait("normal mode", |m| m.view.vim_mode == "n");

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

/// cmdline_hide (level 1) ends the command-line pending state.
#[test]
#[ignore]
fn cmdline_hide_clears_pending() {
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
    let mut msgs = Vec::new();
    for key in ["a", "b", "<Esc>", ":", "q", "<CR>"] {
        msgs.extend(probe.key(key));
    }
    // NvimExited may arrive before or after the <CR> acknowledgment
    assert!(
        probe.saw(&msgs, |m| matches!(m, FromNeovim::NvimExited)),
        "expected NvimExited after :q, got {msgs:?}"
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
        probe.handle.send_key(&c.to_string());
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

/// The buffer mirror follows the current buffer after it is replaced
/// (:enew wipes the old one → detach → re-attach).
#[test]
#[ignore]
fn buffer_mirror_follows_enew() {
    let mut probe = Probe::spawn(true);

    probe.keys(&["a", "<CR>", "b"]);
    probe.wait_buffer("a\nb");

    probe.keys(&["<Esc>", ":", "enew", "<CR>", "i", "x"]);
    probe.wait_buffer("x");

    // Toggle-off clears the buffer with <Esc>ggdG
    probe.keys(&["<Esc>", "g", "g", "d", "G"]);
    probe.wait_buffer("");

    probe.shutdown();
}
