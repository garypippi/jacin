//! State management module
//!
//! Separates IME state into distinct components:
//! - WaylandState: Protocol handles and serial tracking
//! - KeyboardState: XKB context and modifier tracking
//! - ImeState: IME mode state machine
//! - NvimView: Display state observed from Neovim

mod animation;
mod buffer;
mod grid;
mod ime;
mod keyboard;
mod keypress;
mod nvim_view;
mod repeat;
mod screen;
mod wayland;

pub use animation::Animations;
pub use buffer::BufferMirror;
pub use grid::Grid;
pub use ime::ImeState;
pub use keyboard::KeyboardState;
pub use keypress::KeypressState;
pub use nvim_view::NvimView;
pub use repeat::KeyRepeatState;
pub use screen::{Screen, StyledCell, WindowView};
pub use wayland::WaylandState;
