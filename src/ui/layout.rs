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
    pub keypress: String,
    pub candidates: Vec<String>,
    pub selected: usize,
    pub visual_selection: Option<VisualSelection>,
    pub ime_enabled: bool,
    pub recording: String,
}

impl PopupContent {
    pub fn is_empty(&self) -> bool {
        !self.ime_enabled
            && self.preedit.is_empty()
            && self.keypress.is_empty()
            && self.candidates.is_empty()
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

/// Layout information for rendering
pub(crate) struct Layout {
    pub width: u32,
    pub height: u32,
    pub icon_area_width: f32,
    pub has_preedit: bool,
    pub has_keypress: bool,
    pub has_candidates: bool,
    pub preedit_y: f32,
    pub keypress_y: f32,
    pub candidates_y: f32,
    pub visible_count: usize,
    pub has_scrollbar: bool,
}

/// Calculate layout dimensions and section positions
pub(crate) fn calculate_layout(content: &PopupContent, renderer: &mut TextRenderer) -> Layout {
    let has_preedit = !content.preedit.is_empty();
    // Hide keypress when candidates are shown
    let has_keypress = !content.keypress.is_empty() && content.candidates.is_empty();
    let has_candidates = !content.candidates.is_empty();

    let line_height = renderer.line_height();
    let mut y = PADDING;
    let mut max_width: f32 = 0.0;

    // Icon area width: PADDING + mode_label + [gap + recording] + gap + separator + gap
    let (mode_text, _) = mode_label(&content.vim_mode);
    let mode_text_width = renderer.measure_text(mode_text);
    let recording_width = if !content.recording.is_empty() {
        let rec_label = format!("REC @{}", content.recording);
        MODE_GAP + renderer.measure_text(&rec_label)
    } else {
        0.0
    };
    let icon_area_width = PADDING
        + mode_text_width
        + recording_width
        + ICON_SEPARATOR_GAP
        + ICON_SEPARATOR_WIDTH
        + ICON_SEPARATOR_GAP;

    // First row is always present (icon + optional preedit)
    let preedit_y = y;
    if has_preedit {
        let text_width = renderer.measure_text(&content.preedit);
        let preedit_width = (icon_area_width + text_width + PADDING + 4.0)
            .min(MAX_PREEDIT_WIDTH + icon_area_width);
        max_width = max_width.max(preedit_width);
    }
    // Minimum width: icon area + right padding
    max_width = max_width.max(icon_area_width + PADDING);
    y += line_height;
    if has_keypress || has_candidates {
        y += SECTION_SEPARATOR_HEIGHT;
    }

    // Keypress section
    let keypress_y = if has_keypress { y } else { 0.0 };
    if has_keypress {
        let text_width = renderer.measure_text(&content.keypress);
        max_width = max_width.max(text_width + PADDING * 2.0);
        y += line_height;
        if has_candidates {
            y += SECTION_SEPARATOR_HEIGHT;
        }
    }

    // Candidates section
    let candidates_y = if has_candidates { y } else { 0.0 };
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
            max_width =
                max_width.max(text_width + NUMBER_WIDTH + PADDING * 2.0 + scrollbar_space);
        }

        y += visible_count as f32 * line_height;
    }

    y += PADDING;

    // Align width to 4 bytes for wl_shm
    let width = ((max_width.ceil() as u32) + 3) & !3;
    let width = width.clamp(100, 580);
    let height = (y.ceil() as u32).clamp(30, 450);

    Layout {
        width,
        height,
        icon_area_width,
        has_preedit,
        has_keypress,
        has_candidates,
        preedit_y,
        keypress_y,
        candidates_y,
        visible_count,
        has_scrollbar,
    }
}
