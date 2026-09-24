//! Layout calculation and constants for the unified popup

use std::time::{Duration, Instant};

use crate::neovim::Mode;
use crate::state::WindowView;

use super::text_render::TextRenderer;

/// RGBA color as (r, g, b, a) tuple — converted to Color at use via `rgba()`.
pub(crate) type Rgba = (u8, u8, u8, u8);

pub(crate) fn rgba(c: Rgba) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba8(c.0, c.1, c.2, c.3)
}

// Colors
pub(crate) const BG_COLOR: Rgba = (40, 44, 52, 240);
pub(crate) const TEXT_COLOR: Rgba = (220, 223, 228, 255);
pub(crate) const BORDER_COLOR: Rgba = (80, 84, 92, 255);
pub(crate) const SELECTED_BG: Rgba = (61, 89, 161, 255);
pub(crate) const CURSOR_BG: Rgba = (97, 175, 239, 255);
pub(crate) const NUMBER_COLOR: Rgba = (152, 195, 121, 255);
pub(crate) const SCROLLBAR_BG: Rgba = (60, 64, 72, 255);
pub(crate) const SCROLLBAR_THUMB: Rgba = (100, 104, 112, 255);

pub(crate) const PADDING: f32 = 8.0;
pub(crate) const MAX_VISIBLE_CANDIDATES: usize = 9;
pub(crate) const SCROLLBAR_WIDTH: f32 = 8.0;
pub(crate) const NUMBER_WIDTH: f32 = 24.0;
pub(crate) const SECTION_SEPARATOR_HEIGHT: f32 = 1.0;
/// Maximum window-grid rows shown in the popup
pub(crate) const MAX_GRID_ROWS: usize = 8;
/// Popup size limits (the SHM pool holds two buffers of the maximum size)
pub(crate) const MAX_POPUP_WIDTH: u32 = 580;
pub(crate) const MAX_POPUP_HEIGHT: u32 = 450;

pub(crate) const ICON_SEPARATOR_WIDTH: f32 = 1.0;
pub(crate) const ICON_SEPARATOR_GAP: f32 = 6.0;
pub(crate) const MODE_GAP: f32 = 4.0;
/// Command line / showcmd text
pub(crate) const HEADER_TEXT_COLOR: Rgba = (166, 173, 186, 255);

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
    pub mode: Mode,
    /// First row of the column right of the mode
    pub header: Option<Header>,
    /// Message row (below the header)
    pub message: Option<String>,
    /// Candidates to show (below the message)
    pub candidates: Vec<String>,
    /// None: no candidate is selected
    pub selected: Option<usize>,
    pub ime_enabled: bool,
    pub recording: String,
    pub rec_blink_on: bool,
    /// Rows of Neovim's window grid (None until Neovim drew a window)
    pub window_view: Option<WindowView>,
}

/// Header row of the column right of the mode
#[derive(Debug, Clone, PartialEq)]
pub enum Header {
    /// Command line text with its cursor byte offset
    Cmdline { text: String, cursor_byte: usize },
    /// Pending command keys ('showcmd')
    ShowCmd(String),
}

impl PopupContent {
    pub fn is_empty(&self) -> bool {
        !self.ime_enabled
            && self.header.is_none()
            && self.message.is_none()
            && self.candidates.is_empty()
    }
}

/// Rows of the column right of the mode, top to bottom: header, message,
/// candidates. The first one shares the mode's row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ColumnRows {
    pub header: Option<usize>,
    pub message: Option<usize>,
    /// Row of the first visible candidate
    pub candidates: usize,
    pub visible_candidates: usize,
    /// Rows taken (at least the mode's)
    pub total: usize,
}

impl ColumnRows {
    pub fn new(content: &PopupContent) -> Self {
        let mut next = 0;
        let mut take = |present: bool, rows: usize| {
            let row = next;
            if present {
                next += rows;
            }
            present.then_some(row)
        };
        let header = take(content.header.is_some(), 1);
        let message = take(content.message.is_some(), 1);
        let visible_candidates = MAX_VISIBLE_CANDIDATES.min(content.candidates.len());
        let candidates = take(true, visible_candidates).unwrap_or(0);
        Self {
            header,
            message,
            candidates,
            visible_candidates,
            total: next.max(1),
        }
    }
}

/// Mode label text and color
pub(crate) fn mode_label(mode: Mode) -> (&'static str, Rgba) {
    match mode {
        Mode::Insert | Mode::Replace => ("INS", MODE_INSERT_COLOR),
        Mode::Normal | Mode::Other => ("NOR", MODE_NORMAL_COLOR),
        Mode::OperatorPending => ("OP", MODE_OP_COLOR),
        Mode::Visual => ("VIS", MODE_VISUAL_COLOR),
        Mode::CmdLine => ("CMD", MODE_CMD_COLOR),
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
    /// Height the content needs (`height` may be more, when held)
    pub natural_height: u32,
    /// Mirrored for a popup placed above the cursor: the window at the
    /// bottom, the mode row above it, the column growing upwards
    pub above: bool,
    pub has_window: bool,
    pub window_y: f32,
    /// y of the separator between the window and the mode row
    pub separator_y: f32,
    /// Top of the block of the mode row and the column
    pub status_y: f32,
    /// x of the separator between the mode and the column
    pub separator_x: f32,
    /// Left edge of the column's text
    pub column_x: f32,
    pub rows: ColumnRows,
    pub has_scrollbar: bool,
}

impl Layout {
    /// Top of column row `row` (row 0 is the mode's)
    pub fn row_y(&self, row: usize, line_height: f32) -> f32 {
        let slot = if self.above {
            self.rows.total - 1 - row
        } else {
            row
        };
        self.status_y + slot as f32 * line_height
    }
}

/// How long the popup keeps a height it no longer needs, so candidates
/// coming and going while typing don't resize it each time
pub(crate) const HEIGHT_HOLD: Duration = Duration::from_millis(1000);

/// Popup height that grows at once and shrinks only after `HEIGHT_HOLD`
/// without needing the height. Near the bottom of the screen the popup sits
/// above the cursor, so each resize moves its top.
#[derive(Debug, Default)]
pub(crate) struct HeightHold {
    held: u32,
    /// When the popup may shrink to what its content needs
    until: Option<Instant>,
}

impl HeightHold {
    /// Height for content needing `natural`
    pub fn height(&mut self, natural: u32, now: Instant) -> u32 {
        if natural >= self.held {
            self.held = natural;
            self.until = None;
            return natural;
        }
        match self.until {
            None => self.until = Some(now + HEIGHT_HOLD),
            Some(until) if now >= until => {
                self.held = natural;
                self.until = None;
            }
            Some(_) => {}
        }
        self.held
    }

    pub fn is_holding(&self) -> bool {
        self.until.is_some()
    }

    pub fn expired(&self, now: Instant) -> bool {
        self.until.is_some_and(|until| now >= until)
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

/// Calculate layout dimensions and section positions: mirrored if `above`,
/// and at least `min_height` tall (the extra space away from the cursor).
///
/// `mono_renderer` measures the mode/REC icons, the header and the window grid.
pub(crate) fn calculate_layout(
    content: &PopupContent,
    above: bool,
    min_height: u32,
    renderer: &mut TextRenderer,
    mono_renderer: &mut TextRenderer,
) -> Layout {
    // Both are always present while the IME is enabled, so the popup
    // doesn't jump as content comes and goes
    let has_window = content.ime_enabled;
    let has_status = content.ime_enabled;
    let rows = ColumnRows::new(content);

    let line_height = renderer.line_height();
    let mut max_width: f32 = 0.0;

    // Mode icons: mode_label + [gap + circle + gap + @reg]
    let (mode_text, _) = mode_label(content.mode);
    let mut icons_width = mono_renderer.measure_text(mode_text);
    if !content.recording.is_empty() {
        let rec_label = format_recording_label(&content.recording);
        icons_width += MODE_GAP
            + REC_CIRCLE_RADIUS * 2.0
            + REC_CIRCLE_TEXT_GAP
            + mono_renderer.measure_text(&rec_label);
    }
    let separator_x = (PADDING + icons_width + ICON_SEPARATOR_GAP).round();
    let column_x = separator_x + ICON_SEPARATOR_WIDTH + ICON_SEPARATOR_GAP;

    // Window section; one empty row until Neovim has drawn a window
    let mut window_height = 0.0;
    if has_window {
        let window_rows = match content.window_view {
            Some(ref view) => {
                let cell_width = mono_renderer.measure_text(" ");
                let cols = view.total_columns();
                max_width = max_width.max(PADDING * 2.0 + cols as f32 * cell_width);
                view.total_rows().max(1)
            }
            None => 1,
        };
        window_height = window_rows as f32 * line_height;
    }

    // Mode row and the column beside it
    let has_scrollbar = content.candidates.len() > MAX_VISIBLE_CANDIDATES;
    let mut status_height = 0.0;
    if has_status {
        let mut column_width: f32 = 0.0;
        match &content.header {
            Some(Header::Cmdline { text, .. }) | Some(Header::ShowCmd(text)) => {
                column_width = column_width.max(mono_renderer.measure_text(text));
            }
            None => {}
        }
        if let Some(ref message) = content.message {
            column_width = column_width.max(renderer.measure_text(message));
        }
        let scrollbar_space = if has_scrollbar {
            SCROLLBAR_WIDTH + 4.0
        } else {
            0.0
        };
        for candidate in content.candidates.iter().take(MAX_VISIBLE_CANDIDATES) {
            let width = NUMBER_WIDTH + renderer.measure_text(candidate) + scrollbar_space;
            column_width = column_width.max(width);
        }
        max_width = max_width.max(column_x + column_width + PADDING);
        status_height = rows.total as f32 * line_height;
    }
    let separator = if has_window && has_status {
        SECTION_SEPARATOR_HEIGHT
    } else {
        0.0
    };

    let natural = PADDING * 2.0 + window_height + separator + status_height;
    let natural_height = (natural.ceil() as u32).clamp(30, MAX_POPUP_HEIGHT);
    let height = natural_height.max(min_height.min(MAX_POPUP_HEIGHT));
    let extra = (height - natural_height) as f32;

    // Top to bottom: window, separator, status (below the cursor), or
    // extra space, status, separator, window (above it)
    let (window_y, separator_y, status_y) = if above {
        let status_y = PADDING + extra;
        let separator_y = status_y + status_height;
        (separator_y + separator, separator_y, status_y)
    } else {
        let separator_y = PADDING + window_height;
        (PADDING, separator_y, separator_y + separator)
    };

    // Align width to 4 bytes for wl_shm
    let width = ((max_width.ceil() as u32) + 3) & !3;
    let width = width.clamp(100, MAX_POPUP_WIDTH);

    Layout {
        width,
        height,
        natural_height,
        above,
        has_window,
        window_y,
        separator_y,
        status_y,
        separator_x,
        column_x,
        rows,
        has_scrollbar,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    // --- ColumnRows ---

    fn content(header: bool, message: bool, candidates: usize) -> PopupContent {
        PopupContent {
            header: header.then(|| Header::ShowCmd("2d".into())),
            message: message.then(|| "msg".into()),
            candidates: (0..candidates).map(|i| i.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn column_is_one_row_when_empty() {
        let rows = ColumnRows::new(&content(false, false, 0));
        assert_eq!((rows.header, rows.message, rows.total), (None, None, 1));
        assert_eq!(rows.visible_candidates, 0);
    }

    #[test]
    fn candidates_start_on_the_mode_row() {
        let rows = ColumnRows::new(&content(false, false, 3));
        assert_eq!((rows.candidates, rows.visible_candidates), (0, 3));
        assert_eq!(rows.total, 3);
    }

    #[test]
    fn header_and_message_come_before_candidates() {
        let rows = ColumnRows::new(&content(true, true, 2));
        assert_eq!(rows.header, Some(0));
        assert_eq!(rows.message, Some(1));
        assert_eq!(rows.candidates, 2);
        assert_eq!(rows.total, 4);

        let rows = ColumnRows::new(&content(false, true, 2));
        assert_eq!((rows.message, rows.candidates, rows.total), (Some(0), 1, 3));
    }

    #[test]
    fn visible_candidates_are_capped() {
        let rows = ColumnRows::new(&content(true, false, 20));
        assert_eq!(rows.visible_candidates, MAX_VISIBLE_CANDIDATES);
        assert_eq!(rows.total, 1 + MAX_VISIBLE_CANDIDATES);
    }

    // --- HeightHold ---

    #[test]
    fn height_grows_at_once_and_shrinks_after_the_hold() {
        let t0 = Instant::now();
        let mut hold = HeightHold::default();
        assert_eq!(hold.height(57, t0), 57);
        assert_eq!(hold.height(214, t0), 214);
        assert!(!hold.is_holding());

        // Candidates gone: kept
        assert_eq!(hold.height(57, t0), 214);
        assert!(hold.is_holding());
        assert!(!hold.expired(t0 + HEIGHT_HOLD / 2));
        assert_eq!(hold.height(57, t0 + HEIGHT_HOLD / 2), 214);

        // Released after the hold
        assert!(hold.expired(t0 + HEIGHT_HOLD));
        assert_eq!(hold.height(57, t0 + HEIGHT_HOLD), 57);
        assert!(!hold.is_holding());
    }

    #[test]
    fn height_needed_again_restarts_the_hold() {
        let t0 = Instant::now();
        let mut hold = HeightHold::default();
        hold.height(214, t0);
        hold.height(57, t0);
        // Candidates back before the hold ends
        let t1 = t0 + HEIGHT_HOLD / 2;
        assert_eq!(hold.height(214, t1), 214);
        assert!(!hold.is_holding());
        hold.height(57, t1);
        assert_eq!(hold.height(57, t0 + HEIGHT_HOLD), 214);
    }

    // --- Layout::row_y ---

    fn layout(above: bool, rows: ColumnRows) -> Layout {
        Layout {
            width: 100,
            height: 100,
            natural_height: 100,
            above,
            has_window: true,
            window_y: 0.0,
            separator_y: 0.0,
            status_y: 10.0,
            separator_x: 0.0,
            column_x: 0.0,
            rows,
            has_scrollbar: false,
        }
    }

    #[test]
    fn rows_go_down_from_the_mode_row() {
        let rows = ColumnRows::new(&content(true, false, 2));
        let layout = layout(false, rows);
        // Mode/header row, then candidates 1 and 2
        assert_eq!(layout.row_y(0, 20.0), 10.0);
        assert_eq!(layout.row_y(1, 20.0), 30.0);
        assert_eq!(layout.row_y(2, 20.0), 50.0);
    }

    #[test]
    fn rows_go_up_from_the_mode_row_when_mirrored() {
        let rows = ColumnRows::new(&content(true, false, 2));
        let layout = layout(true, rows);
        // The mode row is the bottom one, next to the window
        assert_eq!(layout.row_y(0, 20.0), 50.0);
        assert_eq!(layout.row_y(1, 20.0), 30.0);
        assert_eq!(layout.row_y(2, 20.0), 10.0);
    }

    // --- mode_label ---

    #[test]
    fn mode_labels() {
        assert_eq!(mode_label(Mode::Insert), ("INS", MODE_INSERT_COLOR));
        assert_eq!(mode_label(Mode::Replace).0, "INS");
        assert_eq!(mode_label(Mode::Normal), ("NOR", MODE_NORMAL_COLOR));
        assert_eq!(mode_label(Mode::Visual), ("VIS", MODE_VISUAL_COLOR));
        assert_eq!(mode_label(Mode::OperatorPending), ("OP", MODE_OP_COLOR));
        assert_eq!(mode_label(Mode::CmdLine), ("CMD", MODE_CMD_COLOR));
    }
}
