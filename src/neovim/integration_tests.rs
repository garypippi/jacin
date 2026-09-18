//! Integration tests for Neovim backend.
//!
//! These tests spawn a real headless Neovim process and verify the
//! communication protocol. They require `nvim` in PATH and are gated
//! behind `#[ignore]` — run with `cargo test -- --ignored`.

use std::time::{Duration, Instant};

use super::{FromNeovim, PendingState, spawn_neovim};
use crate::config::Config;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const MSG_TIMEOUT: Duration = Duration::from_secs(5);

fn clean_config() -> Config {
    Config {
        clean: true,
        ..Config::default()
    }
}

fn clean_config_with_startinsert(startinsert: bool) -> Config {
    let mut config = clean_config();
    config.behavior.startinsert = startinsert;
    config
}

/// Drain messages until one matches the predicate, or timeout.
fn recv_until(
    handle: &super::NeovimHandle,
    predicate: impl Fn(&FromNeovim) -> bool,
    timeout: Duration,
) -> Option<FromNeovim> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return None;
        }
        if let Some(msg) = handle.recv_timeout(remaining.min(Duration::from_millis(100))) {
            if predicate(&msg) {
                return Some(msg);
            }
        }
    }
}

/// Spawn Neovim with --clean and wait for Ready.
fn spawn_and_wait_ready() -> super::NeovimHandle {
    let handle = spawn_neovim(clean_config(), None).expect("failed to spawn neovim");
    let ready = recv_until(&handle, |m| matches!(m, FromNeovim::Ready), STARTUP_TIMEOUT);
    assert!(ready.is_some(), "Neovim did not send Ready within timeout");
    handle
}

/// Shutdown Neovim and wait for NvimExited confirmation.
fn shutdown_and_wait(handle: &super::NeovimHandle) {
    handle.shutdown();
    let exited = recv_until(handle, |m| matches!(m, FromNeovim::NvimExited), MSG_TIMEOUT);
    assert!(exited.is_some(), "expected NvimExited after shutdown");
}

#[test]
#[ignore]
fn spawn_and_receive_ready() {
    let handle = spawn_neovim(clean_config(), None).expect("failed to spawn neovim");
    let msg = recv_until(&handle, |m| matches!(m, FromNeovim::Ready), STARTUP_TIMEOUT);
    assert!(msg.is_some(), "expected Ready message from Neovim");
    shutdown_and_wait(&handle);
}

#[test]
#[ignore]
fn insert_mode_typing_updates_preedit() {
    let handle = spawn_and_wait_ready();

    // Type characters — autocmd pushes snapshot after each key
    for ch in ['h', 'e', 'l', 'l', 'o'] {
        handle.send_key(&ch.to_string());
    }

    // Wait for preedit to contain "hello"
    let msg = recv_until(
        &handle,
        |m| matches!(m, FromNeovim::Preedit(info) if info.text == "hello"),
        MSG_TIMEOUT,
    );
    assert!(msg.is_some(), "expected Preedit with text 'hello'");

    shutdown_and_wait(&handle);
}

#[test]
#[ignore]
fn escape_switches_to_normal_mode() {
    let handle = spawn_and_wait_ready();

    handle.send_key("h");
    handle.send_key("i");
    recv_until(
        &handle,
        |m| matches!(m, FromNeovim::Preedit(info) if info.text == "hi"),
        MSG_TIMEOUT,
    )
    .expect("expected preedit 'hi'");

    // Escape to normal mode
    handle.send_key("<Esc>");
    let msg = recv_until(
        &handle,
        |m| {
            matches!(m, FromNeovim::ModeChange(mode) if mode.starts_with('n'))
                || matches!(m, FromNeovim::Preedit(info) if info.mode.starts_with('n'))
        },
        MSG_TIMEOUT,
    );
    assert!(
        msg.is_some(),
        "expected normal-mode notification after Escape"
    );

    shutdown_and_wait(&handle);
}

#[test]
#[ignore]
fn shutdown_exits_cleanly() {
    let handle = spawn_and_wait_ready();
    handle.shutdown();

    // After shutdown, NvimExited should arrive
    let msg = recv_until(
        &handle,
        |m| matches!(m, FromNeovim::NvimExited),
        MSG_TIMEOUT,
    );
    assert!(msg.is_some(), "expected NvimExited after shutdown");
}

#[test]
#[ignore]
fn startinsert_true_starts_in_insert_mode() {
    let config = clean_config_with_startinsert(true);
    let handle = spawn_neovim(config, None).expect("failed to spawn neovim");
    recv_until(&handle, |m| matches!(m, FromNeovim::Ready), STARTUP_TIMEOUT)
        .expect("Neovim did not send Ready");

    // With startinsert=true, typing 'h' should produce preedit directly (no 'i' needed)
    handle.send_key("h");
    let msg = recv_until(
        &handle,
        |m| matches!(m, FromNeovim::Preedit(info) if info.text == "h" && info.mode == "i"),
        MSG_TIMEOUT,
    );
    assert!(
        msg.is_some(),
        "expected Preedit with text 'h' in insert mode (startinsert=true)"
    );

    shutdown_and_wait(&handle);
}

#[test]
#[ignore]
fn startinsert_false_starts_in_normal_mode() {
    let config = clean_config_with_startinsert(false);
    let handle = spawn_neovim(config, None).expect("failed to spawn neovim");
    recv_until(&handle, |m| matches!(m, FromNeovim::Ready), STARTUP_TIMEOUT)
        .expect("Neovim did not send Ready");

    // With startinsert=false, 'h' is a normal-mode motion — should NOT produce preedit with text 'h'
    handle.send_key("h");
    let msg = recv_until(
        &handle,
        |m| matches!(m, FromNeovim::Preedit(info) if info.text == "h"),
        Duration::from_secs(2),
    );
    assert!(
        msg.is_none(),
        "expected no Preedit with text 'h' in normal mode (startinsert=false)"
    );

    // Now enter insert mode explicitly, then type 'h'
    handle.send_key("i");
    recv_until(
        &handle,
        |m| {
            matches!(m, FromNeovim::ModeChange(mode) if mode == "i")
                || matches!(m, FromNeovim::Preedit(info) if info.mode.starts_with('i'))
        },
        MSG_TIMEOUT,
    )
    .expect("failed to enter insert mode");

    handle.send_key("h");
    let msg = recv_until(
        &handle,
        |m| matches!(m, FromNeovim::Preedit(info) if info.text == "h" && info.mode == "i"),
        MSG_TIMEOUT,
    );
    assert!(
        msg.is_some(),
        "expected Preedit with text 'h' after explicit 'i' (startinsert=false)"
    );

    shutdown_and_wait(&handle);
}

/// Send a key and collect messages up to and including its `KeyProcessed`.
fn send_and_collect(handle: &super::NeovimHandle, key: &str) -> Vec<FromNeovim> {
    handle.send_key(key);
    let deadline = Instant::now() + MSG_TIMEOUT;
    let mut msgs = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let msg = handle
            .recv_timeout(remaining)
            .unwrap_or_else(|| panic!("no KeyProcessed for key {key:?}"));
        let done = matches!(msg, FromNeovim::KeyProcessed { .. });
        msgs.push(msg);
        if done {
            return msgs;
        }
    }
}

fn ack_pending(msgs: &[FromNeovim]) -> PendingState {
    match msgs.last() {
        Some(FromNeovim::KeyProcessed { pending }) => *pending,
        other => panic!("expected KeyProcessed last, got {other:?}"),
    }
}

#[test]
#[ignore]
fn getchar_completion_acknowledges_key_with_pending_state() {
    let handle =
        spawn_neovim(clean_config_with_startinsert(false), None).expect("failed to spawn neovim");
    recv_until(&handle, |m| matches!(m, FromNeovim::Ready), STARTUP_TIMEOUT)
        .expect("Neovim did not send Ready");

    for key in ["i", "a", "b", "c", "<Esc>"] {
        send_and_collect(&handle, key);
    }

    // "r" blocks in getchar → acknowledged with Getchar pending
    let msgs = send_and_collect(&handle, "r");
    assert_eq!(ack_pending(&msgs), PendingState::Getchar);

    // Completing getchar must still be acknowledged, after the snapshot
    let msgs = send_and_collect(&handle, "x");
    assert_eq!(ack_pending(&msgs), PendingState::None);
    assert!(
        msgs.iter()
            .any(|m| matches!(m, FromNeovim::Preedit(info) if info.text == "abx")),
        "expected Preedit 'abx' before KeyProcessed, got {msgs:?}"
    );

    shutdown_and_wait(&handle);
}

#[test]
#[ignore]
fn quit_exits_with_modified_buffer() {
    let handle = spawn_neovim(clean_config(), None).expect("failed to spawn neovim");
    recv_until(&handle, |m| matches!(m, FromNeovim::Ready), STARTUP_TIMEOUT)
        .expect("Neovim did not send Ready");
    // buftype=nofile: plain :q must exit even with a modified buffer (no E37)
    let mut msgs = Vec::new();
    for key in ["a", "b", "<Esc>", ":", "q", "<CR>"] {
        msgs.extend(send_and_collect(&handle, key));
    }
    // NvimExited may arrive before or after the <CR> acknowledgment
    let exited = msgs.iter().any(|m| matches!(m, FromNeovim::NvimExited))
        || recv_until(
            &handle,
            |m| matches!(m, FromNeovim::NvimExited),
            MSG_TIMEOUT,
        )
        .is_some();
    assert!(exited, "expected NvimExited after :q, got {msgs:?}");
}

/// Phase A: the grid mirrored from redraw events must agree with the
/// snapshot preedit (row text and cursor byte offset), including
/// double-width chars and normal-mode cursor.
#[test]
#[ignore]
fn grid_shadow_matches_snapshot() {
    use crate::model::Model;

    let handle = spawn_neovim(clean_config(), None).expect("failed to spawn neovim");
    let mut model = Model::new();
    model.ime.start_enabling();
    model.ime.complete_enabling();
    // The initial grid_resize/redraw arrives before Ready — keep it
    loop {
        let msg = handle
            .recv_timeout(STARTUP_TIMEOUT)
            .expect("Neovim did not send Ready");
        let ready = matches!(msg, FromNeovim::Ready);
        model.reduce(msg, true);
        if ready {
            break;
        }
    }

    let mut check = |keys: &[&str], expect_preedit: &str| {
        for key in keys {
            for msg in send_and_collect(&handle, key) {
                model.reduce(msg, true);
            }
        }
        // Push snapshots and redraws arrive asynchronously after KeyProcessed
        let deadline = Instant::now() + MSG_TIMEOUT;
        while !(model.ime.preedit == expect_preedit && model.shadow_ok == Some(true)) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let msg = handle.recv_timeout(remaining).unwrap_or_else(|| {
                panic!(
                    "grid did not match snapshot for {expect_preedit:?}: preedit={:?} cursor={} grid={:?} grid_cursor={:?}",
                    model.ime.preedit,
                    model.ime.cursor_begin,
                    model
                        .view
                        .screen
                        .cursor_grid()
                        .map(|g| g.row_text(model.view.screen.cursor.row)),
                    model.view.screen.cursor,
                )
            });
            model.reduce(msg, true);
        }
    };

    check(&["a", "b", "c"], "abc");
    check(&["あ", "い", "d"], "abcあいd");
    // Normal mode: block cursor on the last char
    check(&["<Esc>"], "abcあいd");
    check(&["h"], "abcあいd");

    // Multigrid: the cursor is on a window grid (not the global grid 1)
    // whose viewport reports the single buffer line
    let screen = &model.view.screen;
    assert_ne!(screen.cursor.grid, 1);
    let viewport = screen.window(screen.cursor.grid).and_then(|w| w.viewport);
    assert_eq!(viewport.map(|v| v.line_count), Some(1));

    shutdown_and_wait(&handle);
}

/// Grid display (B2): after resizing the UI, a line longer than the width
/// wraps in the window grid and window_view shows it as two rows of one
/// buffer line.
#[test]
#[ignore]
fn resized_ui_wraps_long_line_in_window_view() {
    use crate::model::Model;

    let handle = spawn_neovim(clean_config(), None).expect("failed to spawn neovim");
    handle.resize_ui(20, 9);
    let mut model = Model::new();
    model.ime.start_enabling();
    model.ime.complete_enabling();

    let text = "abcdefghijklmnopqrstuvwxy"; // 25 chars > 20 columns
    for c in text.chars() {
        handle.send_key(&c.to_string());
    }
    let deadline = Instant::now() + MSG_TIMEOUT;
    loop {
        if let Some(view) = model.view.screen.window_view(8)
            && view.rows.len() == 2
        {
            let rows: Vec<String> = view
                .rows
                .iter()
                .map(|r| r.iter().map(|c| c.text.as_str()).collect())
                .collect();
            if rows.concat() == text {
                assert_eq!(rows[0].len(), 20);
                assert_eq!(view.cursor, (1, 5));
                break;
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        let msg = handle
            .recv_timeout(remaining)
            .unwrap_or_else(|| panic!("no wrapped view: {:?}", model.view.screen.window_view(8)));
        model.reduce(msg, true);
    }

    shutdown_and_wait(&handle);
}

/// Neovim is started with `g:jacin = 1` so user config can detect jacin.
#[test]
#[ignore]
fn g_jacin_is_set_for_user_config() {
    let handle =
        spawn_neovim(clean_config_with_startinsert(false), None).expect("failed to spawn neovim");
    recv_until(&handle, |m| matches!(m, FromNeovim::Ready), STARTUP_TIMEOUT)
        .expect("Neovim did not send Ready");

    for key in [":", "echo g:jacin", "<CR>"] {
        handle.send_key(key);
    }
    let msg = recv_until(
        &handle,
        |m| matches!(m, FromNeovim::CmdlineMessage { text, .. } if text == "1"),
        MSG_TIMEOUT,
    );
    assert!(msg.is_some(), "expected `:echo g:jacin` to print 1");

    shutdown_and_wait(&handle);
}
