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

    // Enter insert mode — handler queries snapshot, sends Preedit with mode "i"
    handle.send_key("i");
    let msg = recv_until(
        &handle,
        |m| matches!(m, FromNeovim::Preedit(info) if info.mode == "i"),
        MSG_TIMEOUT,
    );
    assert!(msg.is_some(), "expected Preedit with mode 'i' after entering insert mode");

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

    // Enter insert mode, type some text
    handle.send_key("i");
    recv_until(
        &handle,
        |m| matches!(m, FromNeovim::Preedit(info) if info.mode == "i"),
        MSG_TIMEOUT,
    )
    .expect("failed to enter insert mode");

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
        |m| matches!(m, FromNeovim::Preedit(info) if info.mode == "n"),
        MSG_TIMEOUT,
    );
    match msg {
        Some(FromNeovim::Preedit(info)) => {
            assert_eq!(info.mode, "n");
            assert_eq!(info.text, "hi");
            // Normal mode should have block cursor (cursor_begin < cursor_end)
            assert!(
                info.cursor_end > info.cursor_begin,
                "normal mode should have block cursor, got {}..{}",
                info.cursor_begin,
                info.cursor_end
            );
        }
        _ => panic!("expected Preedit in normal mode after Escape"),
    }

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
