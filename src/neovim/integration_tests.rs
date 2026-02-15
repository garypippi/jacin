//! Integration tests for Neovim backend.
//!
//! These tests spawn a real headless Neovim process and verify the
//! communication protocol. They require `nvim` in PATH and are gated
//! behind `#[ignore]` — run with `cargo test -- --ignored`.

use std::time::{Duration, Instant};

use super::{FromNeovim, spawn_neovim};
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
    let handle = spawn_neovim(clean_config()).expect("failed to spawn neovim");
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
    let handle = spawn_neovim(clean_config()).expect("failed to spawn neovim");
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
    let handle = spawn_neovim(config).expect("failed to spawn neovim");
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
    let handle = spawn_neovim(config).expect("failed to spawn neovim");
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
