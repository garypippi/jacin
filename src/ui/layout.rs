//! Layout calculation and constants for the unified popup
//!
//! Layout logic extracted from unified_window.rs. `calculate_layout` still
//! depends on `TextRenderer` for text measurement; a future step can make it
//! fully pure by accepting measurement results as parameters.

use crate::neovim::VisualSelection;

use super::text_render::TextRenderer;

/// RGBA color as (r, g, b, a) tuple — converted to Color at use via `rgba()`.
pub(crate) type Rgba = (u8, u8, u8, u8);

pub(crate) fn rgba(c: Rgba) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba8(c.0, c.1, c.2, c.3)
}

// Colors (matching existing windows)
pub(crate) const BG_COLOR: Rgba = (40, 44, 52, 240);
pub(crate) const TEXT_COLOR: Rgba = (220, 223, 228, 255);
pub(crate) const BORDER_COLOR: Rgba = (80, 84, 92, 255);
pub(crate) const SELECTED_BG: Rgba = (61, 89, 161, 255);
pub(crate) const CURSOR_BG: Rgba = (97, 175, 239, 255);
pub(crate) const VISUAL_BG: Rgba = (61, 89, 161, 200);
pub(crate) const NUMBER_COLOR: Rgba = (152, 195, 121, 255);
pub(crate) const SCROLLBAR_BG: Rgba = (60, 64, 72, 255);
pub(crate) const SCROLLBAR_THUMB: Rgba = (100, 104, 112, 255);

pub(crate) const PADDING: f32 = 8.0;
pub(crate) const MAX_VISIBLE_CANDIDATES: usize = 9;
pub(crate) const SCROLLBAR_WIDTH: f32 = 8.0;
pub(crate) const NUMBER_WIDTH: f32 = 24.0;
pub(crate) const SECTION_SEPARATOR_HEIGHT: f32 = 1.0;
pub(crate) const MAX_PREEDIT_WIDTH: f32 = 400.0;

pub(crate) const ICON_SEPARATOR_WIDTH: f32 = 1.0;
pub(crate) const ICON_SEPARATOR_GAP: f32 = 6.0;
pub(crate) const MODE_GAP: f32 = 4.0;
pub(crate) const KEYPRESS_ENTRY_GAP: f32 = 4.0;
pub(crate) const KEYPRESS_TEXT_COLOR: Rgba = (166, 173, 186, 255);

// Mode indicator colors
pub(crate) const MODE_INSERT_COLOR: Rgba = (152, 195, 121, 255); // Green
pub(crate) const MODE_NORMAL_COLOR: Rgba = (97, 175, 239, 255); // Blue
pub(crate) const MODE_VISUAL_COLOR: Rgba = (198, 120, 221, 255); // Purple
pub(crate) const MODE_OP_COLOR: Rgba = (229, 192, 123, 255); // Yellow
pub(crate) const MODE_CMD_COLOR: Rgba = (224, 108, 117, 255); // Red
pub(crate) const MODE_RECORDING_COLOR: Rgba = (224, 108, 117, 255); // Red

/// Content to display in the unified popup
#[derive(Default, Clone)]
pub struct PopupContent {
    pub preedit: String,
    pub cursor_begin: usize,
    pub cursor_end: usize,
    pub vim_mode: String,
    pub keypress_entries: Vec<String>,
    pub candidates: Vec<String>,
    pub selected: usize,
    pub transient_message: Option<String>,
    pub visual_selection: Option<VisualSelection>,
    pub ime_enabled: bool,
    pub recording: String,
    pub rec_blink_on: bool,
    pub cmdline_cursor_pos: Option<usize>,
}

impl PopupContent {
    pub fn is_empty(&self) -> bool {
        !self.ime_enabled
            && self.preedit.is_empty()
            && self.keypress_entries.is_empty()
            && self.candidates.is_empty()
            && self.transient_message.is_none()
    }
}

/// Get mode label text and color from vim_mode string
pub(crate) fn mode_label(vim_mode: &str) -> (&'static str, Rgba) {
    if vim_mode.starts_with("no") {
        ("OP", MODE_OP_COLOR)
    } else {
        match vim_mode {
            "n" => ("NOR", MODE_NORMAL_COLOR),
            "v" | "V" | "\x16" => ("VIS", MODE_VISUAL_COLOR),
            "c" => ("CMD", MODE_CMD_COLOR),
            _ => {
                if vim_mode.starts_with('v') || vim_mode.starts_with('V') {
                    ("VIS", MODE_VISUAL_COLOR)
                } else {
                    ("INS", MODE_INSERT_COLOR)
                }
            }
        }
    }
}

/// Radius of the red recording indicator circle
pub(crate) const REC_CIRCLE_RADIUS: f32 = 4.0;
/// Gap between recording circle and @reg text
pub(crate) const REC_CIRCLE_TEXT_GAP: f32 = 3.0;

/// Format the recording label text (the part after the red circle)
pub(crate) fn format_recording_label(reg: &str) -> String {
    format!("@{}", reg)
}

/// Layout information for rendering
pub(crate) struct Layout {
    pub width: u32,
    pub height: u32,
    pub has_preedit: bool,
    pub has_keypress: bool,
    pub has_candidates: bool,
    pub has_transient_message: bool,
    pub preedit_y: f32,
    pub keypress_y: f32,
    pub candidates_y: f32,
    pub visible_count: usize,
    pub has_scrollbar: bool,
    /// Width of mode+REC icons in keypress row (text starts after this)
    pub keypress_icon_width: f32,
}

/// Calculate preedit scroll offset to keep cursor visible with center-biased scrolling.
///
/// Returns a pixel offset to subtract from each character's x position.
pub(crate) fn preedit_scroll_offset(
    total_text_width: f32,
    visible_width: f32,
    cursor_rel: f32,
) -> f32 {
    if total_text_width <= visible_width {
        return 0.0;
    }
    let margin = visible_width * 0.3;
    if cursor_rel < margin {
        0.0
    } else if cursor_rel > total_text_width - margin {
        (total_text_width - visible_width).max(0.0)
    } else {
        (cursor_rel - visible_width / 2.0).clamp(0.0, total_text_width - visible_width)
    }
}

/// Scrollbar thumb geometry for candidate list.
pub(crate) struct ScrollbarThumb {
    pub height: f32,
    pub y: f32,
}

/// Calculate scrollbar thumb position and size.
pub(crate) fn scrollbar_thumb_geometry(
    visible_count: usize,
    total_count: usize,
    scrollbar_height: f32,
    scroll_offset: usize,
    candidates_y: f32,
) -> ScrollbarThumb {
    debug_assert!(total_count > 0 && visible_count <= total_count);
    let thumb_height = ((visible_count as f32 / total_count as f32) * scrollbar_height).max(20.0);
    let scroll_range = total_count - visible_count;
    let y = if scroll_range > 0 {
        candidates_y
            + (scroll_offset as f32 / scroll_range as f32) * (scrollbar_height - thumb_height)
    } else {
        candidates_y
    };
    ScrollbarThumb {
        height: thumb_height,
        y,
    }
}

/// Calculate layout dimensions and section positions.
///
/// `mono_renderer` is used for measuring mode/REC icon text in the keypress row.
pub(crate) fn calculate_layout(
    content: &PopupContent,
    renderer: &mut TextRenderer,
    mono_renderer: &mut TextRenderer,
) -> Layout {
    // Preedit row is always visible when IME is enabled to prevent
    // layout jumps that cause visual confusion with the keypress row
    let has_preedit = content.ime_enabled;
    // Hide keypress text when candidates are shown, but keypress row itself
    // is always visible when IME is enabled (shows mode/REC icons)
    let has_keypress_text = !content.keypress_entries.is_empty() && content.candidates.is_empty();
    // Keypress row is always present when IME is enabled
    let has_keypress = content.ime_enabled;
    let has_candidates = !content.candidates.is_empty();
    let has_transient_message =
        content.candidates.is_empty() && content.transient_message.is_some();

    let line_height = renderer.line_height();
    let mut y = PADDING;
    let mut max_width: f32 = 0.0;

    // Keypress row icon width: mode_label + [gap + circle + gap + @reg] + separator area
    let (mode_text, _) = mode_label(&content.vim_mode);
    let mode_text_width = mono_renderer.measure_text(mode_text);
    let recording_width = if !content.recording.is_empty() {
        let rec_label = format_recording_label(&content.recording);
        MODE_GAP
            + REC_CIRCLE_RADIUS * 2.0
            + REC_CIRCLE_TEXT_GAP
            + mono_renderer.measure_text(&rec_label)
    } else {
        0.0
    };
    let keypress_icon_width = PADDING
        + mode_text_width
        + recording_width
        + ICON_SEPARATOR_GAP
        + ICON_SEPARATOR_WIDTH
        + ICON_SEPARATOR_GAP;

    // Preedit section (no icon area — preedit starts at PADDING)
    let preedit_y = y;
    if has_preedit {
        if !content.preedit.is_empty() {
            let text_width = renderer.measure_text(&content.preedit);
            let preedit_width =
                (PADDING + text_width + PADDING + 4.0).min(MAX_PREEDIT_WIDTH + PADDING * 2.0);
            max_width = max_width.max(preedit_width);
        }
        y += line_height;
        if has_keypress || has_candidates {
            y += SECTION_SEPARATOR_HEIGHT;
        }
    }

    // Keypress section (always present when IME enabled)
    let keypress_y = if has_keypress { y } else { 0.0 };
    if has_keypress {
        let mut keypress_width = keypress_icon_width;
        if has_keypress_text {
            for (i, entry) in content.keypress_entries.iter().enumerate() {
                if i > 0 {
                    keypress_width += KEYPRESS_ENTRY_GAP;
                }
                keypress_width += mono_renderer.measure_text(entry);
            }
        }
        keypress_width += PADDING; // right padding
        max_width = max_width.max(keypress_width);
        y += line_height;
        if has_candidates || has_transient_message {
            y += SECTION_SEPARATOR_HEIGHT;
        }
    }

    // Candidates section (or transient message)
    let candidates_y = if has_candidates || has_transient_message {
        y
    } else {
        0.0
    };
    let visible_count = if has_candidates {
        MAX_VISIBLE_CANDIDATES.min(content.candidates.len())
    } else {
        0
    };
    let has_scrollbar = content.candidates.len() > MAX_VISIBLE_CANDIDATES;

    if has_candidates {
        let scrollbar_space = if has_scrollbar {
            SCROLLBAR_WIDTH + 4.0
        } else {
            0.0
        };

        // Calculate max candidate width
        for candidate in content.candidates.iter().take(MAX_VISIBLE_CANDIDATES) {
            let text_width = renderer.measure_text(candidate);
            max_width = max_width.max(text_width + NUMBER_WIDTH + PADDING * 2.0 + scrollbar_space);
        }

        y += visible_count as f32 * line_height;
    } else if has_transient_message {
        if let Some(ref msg) = content.transient_message {
            let text_width = renderer.measure_text(msg);
            max_width = max_width.max(text_width + PADDING * 2.0);
        }
        y += line_height;
    }

    y += PADDING;

    // Align width to 4 bytes for wl_shm
    let width = ((max_width.ceil() as u32) + 3) & !3;
    let width = width.clamp(100, 580);
    let height = (y.ceil() as u32).clamp(30, 450);

    Layout {
        width,
        height,
        has_preedit,
        has_keypress,
        has_candidates,
        has_transient_message,
        preedit_y,
        keypress_y,
        candidates_y,
        visible_count,
        has_scrollbar,
        keypress_icon_width,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- preedit_scroll_offset ---

    #[test]
    fn scroll_offset_short_text_returns_zero() {
        // Text fits in visible area — no scrolling
        assert_eq!(preedit_scroll_offset(100.0, 200.0, 50.0), 0.0);
    }

    #[test]
    fn scroll_offset_cursor_near_start() {
        // Cursor within 30% margin from left — no scrolling
        assert_eq!(preedit_scroll_offset(500.0, 200.0, 10.0), 0.0);
    }

    #[test]
    fn scroll_offset_cursor_near_end() {
        // Cursor near end — scroll to show end of text
        let offset = preedit_scroll_offset(500.0, 200.0, 480.0);
        assert_eq!(offset, 300.0); // total - visible
    }

    #[test]
    fn scroll_offset_cursor_in_middle() {
        // Cursor in middle — centers cursor in visible area
        let offset = preedit_scroll_offset(500.0, 200.0, 250.0);
        // cursor_rel - visible/2 = 250 - 100 = 150, clamped to [0, 300]
        assert_eq!(offset, 150.0);
    }

    // --- scrollbar_thumb_geometry ---

    #[test]
    fn thumb_no_scroll_range() {
        // All items visible (visible == total)
        let thumb = scrollbar_thumb_geometry(10, 10, 200.0, 0, 50.0);
        assert_eq!(thumb.y, 50.0);
        // thumb_height = (10/10)*200 = 200, but min 20
        assert_eq!(thumb.height, 200.0);
    }

    #[test]
    fn thumb_at_top() {
        let thumb = scrollbar_thumb_geometry(5, 20, 100.0, 0, 50.0);
        assert_eq!(thumb.y, 50.0); // scroll_offset=0, at top
        assert!(thumb.height >= 20.0);
    }

    #[test]
    fn thumb_at_bottom() {
        let thumb = scrollbar_thumb_geometry(5, 20, 100.0, 15, 50.0);
        // scroll_offset=15 = scroll_range=15, so ratio=1.0
        let expected_y = 50.0 + (100.0 - thumb.height);
        assert!((thumb.y - expected_y).abs() < 0.01);
    }

    #[test]
    fn thumb_minimum_height() {
        // With many items, thumb proportion would be tiny — clamped to 20
        let thumb = scrollbar_thumb_geometry(1, 100, 100.0, 0, 0.0);
        assert_eq!(thumb.height, 20.0);
    }

    // --- mode_label ---

    #[test]
    fn mode_label_insert() {
        let (label, color) = mode_label("i");
        assert_eq!(label, "INS");
        assert_eq!(color, MODE_INSERT_COLOR);
    }

    #[test]
    fn mode_label_normal() {
        let (label, color) = mode_label("n");
        assert_eq!(label, "NOR");
        assert_eq!(color, MODE_NORMAL_COLOR);
    }

    #[test]
    fn mode_label_visual() {
        assert_eq!(mode_label("v").0, "VIS");
        assert_eq!(mode_label("V").0, "VIS");
        assert_eq!(mode_label("\x16").0, "VIS");
        // v-prefix
        assert_eq!(mode_label("vs").0, "VIS");
    }

    #[test]
    fn mode_label_operator_pending() {
        assert_eq!(mode_label("no").0, "OP");
        assert_eq!(mode_label("nov").0, "OP");
    }

    #[test]
    fn mode_label_command() {
        let (label, color) = mode_label("c");
        assert_eq!(label, "CMD");
        assert_eq!(color, MODE_CMD_COLOR);
    }
}
