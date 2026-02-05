//! Calloop event source for Neovim messages
//!
//! Wraps crossbeam receiver with a ping mechanism for integration with calloop.
//!
//! Note: This is infrastructure for event-driven Neovim message handling.
//! Currently the IME uses polling in the event loop callback, which is
//! sufficient since key events trigger Wayland events that wake the loop.

#![allow(dead_code)]

use calloop::{
    EventSource, Poll, PostAction, Readiness, Token, TokenFactory,
    ping::{Ping, PingSource, make_ping},
};
use crossbeam_channel::{Receiver, TryRecvError};
use std::io;

use super::protocol::FromNeovim;

/// Event source that delivers Neovim messages to the calloop event loop
pub struct NeovimEventSource {
    receiver: Receiver<FromNeovim>,
    ping_source: PingSource,
}

/// Handle to wake up the event source when messages arrive
#[derive(Clone)]
pub struct NeovimPing(Ping);

impl NeovimPing {
    /// Signal that a message is available
    pub fn ping(&self) {
        self.0.ping();
    }
}

impl NeovimEventSource {
    /// Create a new event source wrapping a crossbeam receiver
    ///
    /// Returns the event source and a ping handle that should be used
    /// by the sender thread to wake up the event loop.
    pub fn new(receiver: Receiver<FromNeovim>) -> io::Result<(Self, NeovimPing)> {
        let (ping, ping_source) = make_ping()?;

        Ok((
            Self {
                receiver,
                ping_source,
            },
            NeovimPing(ping),
        ))
    }

    /// Try to receive all pending messages (non-blocking)
    pub fn drain_messages(&self) -> Vec<FromNeovim> {
        let mut messages = Vec::new();
        loop {
            match self.receiver.try_recv() {
                Ok(msg) => messages.push(msg),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
        messages
    }
}

impl EventSource for NeovimEventSource {
    type Event = FromNeovim;
    type Metadata = ();
    type Ret = ();
    type Error = io::Error;

    fn process_events<F>(
        &mut self,
        readiness: Readiness,
        token: Token,
        mut callback: F,
    ) -> Result<PostAction, Self::Error>
    where
        F: FnMut(Self::Event, &mut Self::Metadata) -> Self::Ret,
    {
        // Process the ping source to clear the wake-up signal
        // PingError is infallible so we just ignore the result
        let _ = self.ping_source.process_events(readiness, token, |_, _| {});

        // Drain all available messages
        loop {
            match self.receiver.try_recv() {
                Ok(msg) => callback(msg, &mut ()),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    return Ok(PostAction::Remove);
                }
            }
        }

        Ok(PostAction::Continue)
    }

    fn register(
        &mut self,
        poll: &mut Poll,
        token_factory: &mut TokenFactory,
    ) -> calloop::Result<()> {
        self.ping_source.register(poll, token_factory)
    }

    fn reregister(
        &mut self,
        poll: &mut Poll,
        token_factory: &mut TokenFactory,
    ) -> calloop::Result<()> {
        self.ping_source.reregister(poll, token_factory)
    }

    fn unregister(&mut self, poll: &mut Poll) -> calloop::Result<()> {
        self.ping_source.unregister(poll)
    }
}
