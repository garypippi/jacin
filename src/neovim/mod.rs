//! Neovim backend module
//!
//! Provides communication with an embedded Neovim instance for input processing.
//! Users manage their own Japanese input plugins inside Neovim.

mod handler;
#[cfg(test)]
mod integration_tests;
#[allow(dead_code)] // not wired up yet
mod keys;
#[allow(dead_code)] // not wired up yet
mod mode;
#[allow(dead_code)] // not wired up yet
mod process;
pub mod protocol;
mod redraw_grid;
#[allow(dead_code)] // not wired up yet
mod rpc;

use std::thread;
use std::time::Duration;

use calloop::ping::Ping;
use crossbeam_channel::{Receiver, Sender, bounded};

use crate::config::Config;

pub use protocol::{FromNeovim, GridCell, GridEvent, HlAttr, PendingState, ToNeovim};

/// Channel capacity for Neovim communication
/// This provides backpressure if messages accumulate
const CHANNEL_CAPACITY: usize = 64;

/// Keys that return to normal mode and clear the buffer from any mode
/// (sent with `NeovimHandle::input`).
/// After an insert-mode key that waits for an argument (<C-r>, <C-v>, <C-k>),
/// the first <Esc> only cancels that key, so a second one is needed to leave
/// insert mode before ggdG.
pub const CLEAR_BUFFER_KEYS: &str = "<Esc><Esc>ggdG";

/// Handle to communicate with Neovim backend
pub struct NeovimHandle {
    sender: Sender<ToNeovim>,
    receiver: Receiver<FromNeovim>,
}

impl NeovimHandle {
    /// Send a user key, acknowledged with `KeyProcessed { seq }`
    /// (non-blocking). Returns false if the key was dropped (channel full).
    pub fn send_key(&self, seq: u64, key: &str) -> bool {
        let sent = self
            .sender
            .try_send(ToNeovim::Key {
                seq,
                key: key.to_string(),
            })
            .is_ok();
        if !sent {
            log::warn!("[NVIM] Key {key:?} dropped: Neovim is not keeping up");
        }
        sent
    }

    /// Input keys on jacin's behalf (not acknowledged). Waits briefly when
    /// the channel is full: a lost reset leaves old text in the buffer.
    pub fn input(&self, keys: &str) {
        if self
            .sender
            .send_timeout(
                ToNeovim::Input(keys.to_string()),
                Duration::from_millis(200),
            )
            .is_err()
        {
            log::warn!("[NVIM] Input {keys:?} dropped: Neovim is not keeping up");
        }
    }

    /// Resize the attached UI grid (non-blocking: dropped if channel full)
    pub fn resize_ui(&self, width: usize, height: usize) {
        let _ = self.sender.try_send(ToNeovim::ResizeUi {
            width: width as u64,
            height: height as u64,
        });
    }

    /// Try to receive a message from Neovim (non-blocking)
    pub fn try_recv(&self) -> Option<FromNeovim> {
        self.receiver.try_recv().ok()
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Option<FromNeovim> {
        self.receiver.recv_timeout(timeout).ok()
    }

    /// Shutdown Neovim (non-blocking: best-effort if channel full)
    pub fn shutdown(&self) {
        // Avoid dropping shutdown silently under backpressure, but also avoid
        // blocking indefinitely if receiver thread is stalled.
        let _ = self
            .sender
            .send_timeout(ToNeovim::Shutdown, Duration::from_millis(200));
    }
}

/// Spawn Neovim backend in a separate thread.
///
/// `wake` is pinged after every message sent to the main thread, so the
/// calloop event loop wakes up to drain it (None in tests without a loop).
pub fn spawn_neovim(config: Config, wake: Option<Ping>) -> anyhow::Result<NeovimHandle> {
    // Use bounded channels for backpressure
    let (to_nvim_tx, to_nvim_rx) = bounded::<ToNeovim>(CHANNEL_CAPACITY);
    let (from_nvim_tx, from_nvim_rx) = bounded::<FromNeovim>(CHANNEL_CAPACITY);

    let main_tx = handler::MainTx::new(from_nvim_tx, wake);
    thread::spawn(move || {
        handler::run_blocking(to_nvim_rx, main_tx, config);
    });

    Ok(NeovimHandle {
        sender: to_nvim_tx,
        receiver: from_nvim_rx,
    })
}
