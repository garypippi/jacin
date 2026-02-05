//! UI components for the IME
//!
//! Contains the unified popup window and text rendering functionality.

mod text_render;
mod unified_window;

pub use text_render::TextRenderer;
pub use unified_window::{PopupContent, UnifiedPopup};
