//! State management module
//!
//! Separates IME state into distinct components:
//! - WaylandState: Protocol handles and serial tracking
//! - KeyboardState: XKB context and modifier tracking
//! - ImeState: IME mode state machine and preedit

mod ime;
mod keyboard;
mod wayland;

pub use ime::{ImeMode, ImeState, MotionAwaiting, VimMode};
pub use keyboard::KeyboardState;
pub use wayland::WaylandState;
