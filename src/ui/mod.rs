//! UI components for the IME
//!
//! Contains the unified popup window and text rendering functionality.

mod layout;
mod text_render;
mod theme;
mod unified_window;

pub(crate) use layout::MAX_GRID_ROWS;
pub use layout::{Header, PopupContent};
pub use text_render::TextRenderer;
pub use theme::Theme;
pub use unified_window::UnifiedPopup;
