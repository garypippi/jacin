//! Neovim backend module
//!
//! Provides communication with an embedded Neovim instance for vim-skkeleton
//! Japanese input support.

mod event_source;
mod handler;
pub mod protocol;

use std::thread;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, bounded};

use crate::config::Config;

// Re-export event source types (for future calloop integration)
#[allow(unused_imports)]
pub use event_source::{NeovimEventSource, NeovimPing};

pub use handler::pending_state;
pub use protocol::{FromNeovim, PendingState, ToNeovim};

/// Channel capacity for Neovim communication
/// This provides backpressure if messages accumulate
const CHANNEL_CAPACITY: usize = 64;

/// Handle to communicate with Neovim backend
pub struct NeovimHandle {
    sender: Sender<ToNeovim>,
    receiver: Receiver<FromNeovim>,
}

impl NeovimHandle {
    /// Send a key to Neovim
    pub fn send_key(&self, key: &str) {
        let _ = self.sender.send(ToNeovim::Key(key.to_string()));
    }

    /// Try to receive a message from Neovim (non-blocking)
    pub fn try_recv(&self) -> Option<FromNeovim> {
        self.receiver.try_recv().ok()
    }

    /// Receive with timeout
    pub fn recv_timeout(&self, timeout: Duration) -> Option<FromNeovim> {
        self.receiver.recv_timeout(timeout).ok()
    }

    /// Shutdown Neovim
    pub fn shutdown(&self) {
        let _ = self.sender.send(ToNeovim::Shutdown);
    }

    /// Get the receiver for use with calloop event source
    #[allow(dead_code)]
    pub fn receiver(&self) -> &Receiver<FromNeovim> {
        &self.receiver
    }
}

/// Spawn Neovim backend in a separate thread
pub fn spawn_neovim(config: Config) -> anyhow::Result<NeovimHandle> {
    // Use bounded channels for backpressure
    let (to_nvim_tx, to_nvim_rx) = bounded::<ToNeovim>(CHANNEL_CAPACITY);
    let (from_nvim_tx, from_nvim_rx) = bounded::<FromNeovim>(CHANNEL_CAPACITY);

    thread::spawn(move || {
        handler::run_blocking(to_nvim_rx, from_nvim_tx, config);
    });

    Ok(NeovimHandle {
        sender: to_nvim_tx,
        receiver: from_nvim_rx,
    })
}

// Re-export for backwards compatibility during transition
// These will be removed in a future cleanup
impl From<FromNeovim> for OldFromNeovim {
    fn from(msg: FromNeovim) -> Self {
        match msg {
            FromNeovim::Ready => OldFromNeovim::Ready,
            FromNeovim::Preedit(info) => {
                OldFromNeovim::Preedit(info.text, info.cursor_begin, info.cursor_end, info.mode)
            }
            FromNeovim::Commit(text) => OldFromNeovim::Commit(text),
            FromNeovim::DeleteSurrounding { before, after } => {
                OldFromNeovim::DeleteSurrounding(before, after)
            }
            FromNeovim::Candidates(info) => {
                OldFromNeovim::Candidates(info.candidates, info.selected)
            }
        }
    }
}

/// Old message format for backwards compatibility
#[derive(Debug, Clone)]
pub enum OldFromNeovim {
    /// Preedit text changed (text, cursor_begin, cursor_end, mode)
    Preedit(String, usize, usize, String),
    /// Text should be committed
    Commit(String),
    /// Delete surrounding text (before_length, after_length)
    DeleteSurrounding(u32, u32),
    /// Completion candidates from nvim-cmp (candidates, selected_index)
    Candidates(Vec<String>, usize),
    /// Neovim is ready
    Ready,
}
