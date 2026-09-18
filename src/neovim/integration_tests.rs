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
fn force_quit_exits_with_write_to_commit() {
    let mut config = clean_config();
    config.behavior.write_to_commit = true;
    let handle = spawn_neovim(config, None).expect("failed to spawn neovim");
    recv_until(&handle, |m| matches!(m, FromNeovim::Ready), STARTUP_TIMEOUT)
        .expect("Neovim did not send Ready");
    // Modified acwrite buffer: :q! must still exit (no E37)
    let mut msgs = Vec::new();
    for key in ["a", "b", "<Esc>", ":", "q", "!", "<CR>"] {
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
    assert!(exited, "expected NvimExited after :q!, got {msgs:?}");
}
