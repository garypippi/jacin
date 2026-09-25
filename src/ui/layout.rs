//! Layout calculation and constants for the unified popup

use crate::neovim::Mode;
use crate::state::{FloatView, StyledCell, WindowView};

use super::text_render::TextRenderer;
use super::theme::Theme;

/// RGBA color as (r, g, b, a) tuple — converted to Color at use via `rgba()`.
pub(crate) type Rgba = (u8, u8, u8, u8);

pub(crate) fn rgba(c: Rgba) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba8(c.0, c.1, c.2, c.3)
}

pub(crate) const PADDING: f32 = 8.0;
pub(crate) const MAX_VISIBLE_CANDIDATES: usize = 9;
pub(crate) const SCROLLBAR_WIDTH: f32 = 8.0;
pub(crate) const SECTION_SEPARATOR_HEIGHT: f32 = 1.0;
/// Maximum window-grid rows shown in the popup
pub(crate) const MAX_GRID_ROWS: usize = 8;
/// Popup size limits (the SHM pool holds two buffers of the maximum size);
/// focus mode has its own, from the screen size
pub(crate) const MAX_POPUP_WIDTH: u32 = 580;
pub(crate) const MAX_POPUP_HEIGHT: u32 = 450;
/// Panel rows focus mode leaves room for below the screen: recording,
/// header and message (the candidates are drawn on the screen)
pub(crate) const FOCUS_PANEL_ROWS: usize = 3;

/// Mode strip along the left edge of the text
pub(crate) const MODE_STRIP_X: f32 = 3.0;
pub(crate) const MODE_STRIP_WIDTH: f32 = 3.0;
/// Left edge of the text, right of the mode strip
pub(crate) const TEXT_X: f32 = MODE_STRIP_X + MODE_STRIP_WIDTH + 6.0;
// Mode strip colors: insert blends into the border, the other modes stand
// out (fixed: no builtin highlight group means "Normal mode")
pub(crate) const MODE_NORMAL_COLOR: Rgba = (97, 175, 239, 255); // Blue
pub(crate) const MODE_VISUAL_COLOR: Rgba = (198, 120, 221, 255); // Purple
pub(crate) const MODE_OP_COLOR: Rgba = (229, 192, 123, 255); // Yellow
pub(crate) const MODE_CMD_COLOR: Rgba = (224, 108, 117, 255); // Red

/// Content to display in the unified popup
#[derive(Default, Clone)]
pub struct PopupContent {
    pub mode: Mode,
    /// First row of the panel below the text
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
    pub theme: Theme,
    /// The window view is the whole screen (focus mode)
    pub focus: bool,
}

/// Header row of the panel
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

/// Rows of the panel shown only when it has something, top to bottom:
/// recording, header, message, candidates
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PanelRows {
    pub recording: Option<usize>,
    pub header: Option<usize>,
    pub message: Option<usize>,
    /// Row of the first visible candidate
    pub candidates: usize,
    pub visible_candidates: usize,
    /// Rows taken (0: no panel)
    pub total: usize,
}

impl PanelRows {
    pub fn new(content: &PopupContent) -> Self {
        let mut next = 0;
        let mut take = |present: bool, rows: usize| {
            let row = next;
            if present {
                next += rows;
            }
            present.then_some(row)
        };
        let recording = take(!content.recording.is_empty(), 1);
        let header = take(content.header.is_some(), 1);
        let message = take(content.message.is_some(), 1);
        let visible_candidates = MAX_VISIBLE_CANDIDATES.min(content.candidates.len());
        let candidates = take(true, visible_candidates).unwrap_or(0);
        Self {
            recording,
            header,
            message,
            candidates,
            visible_candidates,
            total: next,
        }
    }
}

/// Color of the mode strip
pub(crate) fn mode_color(mode: Mode, border: Rgba) -> Rgba {
    match mode {
        Mode::Insert | Mode::Replace => border,
        Mode::Normal | Mode::Other => MODE_NORMAL_COLOR,
        Mode::OperatorPending => MODE_OP_COLOR,
        Mode::Visual => MODE_VISUAL_COLOR,
        Mode::CmdLine => MODE_CMD_COLOR,
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
    /// Mirrored for a popup placed above the cursor: the text at the
    /// bottom, the panel above it growing upwards
    pub above: bool,
    /// The mode strip and the text (while the IME is enabled)
    pub has_text: bool,
    /// Top of the text and of the mode strip
    pub text_y: f32,
    pub text_height: f32,
    /// y of the separator between the text and the panel (if both shown)
    pub separator_y: Option<f32>,
    /// Top of the panel
    pub panel_y: f32,
    pub rows: PanelRows,
    pub has_scrollbar: bool,
}

impl Layout {
    /// Top of panel row `row` (row 0 is next to the text)
    pub fn row_y(&self, row: usize, line_height: f32) -> f32 {
        let slot = if self.above {
            self.rows.total - 1 - row
        } else {
            row
        };
        self.panel_y + slot as f32 * line_height
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

/// Calculate layout dimensions and section positions (mirrored if `above`).
///
/// `mono_renderer` measures the recording label, the header and the window grid.
/// The popup is at most `max_size` (width, height).
pub(crate) fn calculate_layout(
    content: &PopupContent,
    above: bool,
    max_size: (u32, u32),
    renderer: &mut TextRenderer,
    mono_renderer: &mut TextRenderer,
) -> Layout {
    let has_text = content.ime_enabled;
    let rows = PanelRows::new(content);
    let has_panel = rows.total > 0;

    let line_height = renderer.line_height();
    let mut max_width: f32 = 0.0;

    // Text; one empty row until Neovim has drawn a window
    let mut text_height = 0.0;
    if has_text {
        let text_rows = match content.window_view {
            Some(ref view) => {
                let cell_width = mono_renderer.measure_text(" ");
                let cols = view.total_columns();
                max_width = max_width.max(TEXT_X + cols as f32 * cell_width + PADDING);
                view.total_rows().max(1)
            }
            None => {
                max_width = max_width.max(TEXT_X + PADDING);
                1
            }
        };
        text_height = text_rows as f32 * line_height;
    }

    // Panel: recording, header, message, candidates
    let has_scrollbar = content.candidates.len() > MAX_VISIBLE_CANDIDATES;
    let mut panel_height = 0.0;
    if has_panel {
        let mut panel_width: f32 = 0.0;
        if !content.recording.is_empty() {
            let label = format_recording_label(&content.recording);
            panel_width =
                REC_CIRCLE_RADIUS * 2.0 + REC_CIRCLE_TEXT_GAP + mono_renderer.measure_text(&label);
        }
        match &content.header {
            Some(Header::Cmdline { text, .. }) | Some(Header::ShowCmd(text)) => {
                panel_width = panel_width.max(mono_renderer.measure_text(text));
            }
            None => {}
        }
        if let Some(ref message) = content.message {
            panel_width = panel_width.max(renderer.measure_text(message));
        }
        let scrollbar_space = if has_scrollbar {
            SCROLLBAR_WIDTH + 4.0
        } else {
            0.0
        };
        for candidate in content.candidates.iter().take(MAX_VISIBLE_CANDIDATES) {
            let width = renderer.measure_text(candidate) + scrollbar_space;
            panel_width = panel_width.max(width);
        }
        max_width = max_width.max(PADDING * 2.0 + panel_width);
        panel_height = rows.total as f32 * line_height;
    }
    let separator = if has_text && has_panel {
        SECTION_SEPARATOR_HEIGHT
    } else {
        0.0
    };

    let natural = PADDING * 2.0 + text_height + separator + panel_height;
    let height = (natural.ceil() as u32).clamp(30, max_size.1);

    // Top to bottom: text, separator, panel (below the cursor), or
    // panel, separator, text (above it)
    let (text_y, separator_y, panel_y) = if above {
        let separator_y = PADDING + panel_height;
        (separator_y + separator, separator_y, PADDING)
    } else {
        let separator_y = PADDING + text_height;
        (PADDING, separator_y, separator_y + separator)
    };

    // Align width to 4 bytes for wl_shm
    let width = ((max_width.ceil() as u32) + 3) & !3;
    let width = width.clamp(100, max_size.0);

    Layout {
        width,
        height,
        above,
        has_text,
        text_y,
        text_height,
        separator_y: (separator > 0.0).then_some(separator_y),
        panel_y,
        rows,
        has_scrollbar,
    }
}

/// zindex of the popup menu (Neovim draws its own at 100)
const PUM_ZINDEX: u64 = 100;

/// The completion menu as a float next to the completed word at screen
/// `at` (row, col), on a screen of `size` (rows, cols): below the word,
/// above it when it doesn't fit below. Like Neovim's own menu, with a
/// scrollbar column when not every item fits.
pub(crate) fn popupmenu_float(
    items: &[String],
    selected: Option<usize>,
    at: (usize, usize),
    size: (usize, usize),
    theme: &Theme,
) -> Option<FloatView> {
    let (rows, cols) = size;
    let visible = MAX_VISIBLE_CANDIDATES.min(items.len());
    if visible == 0 || cols == 0 {
        return None;
    }
    let first = selected
        .map_or(0, |s| (s + 1).saturating_sub(visible))
        .min(items.len() - visible);
    let has_scrollbar = items.len() > visible;

    // A space on either side of the widest item (all items: no jitter while scrolling)
    let text_width = items.iter().map(|i| display_width(i)).max().unwrap_or(0);
    let width = (text_width + 2 + usize::from(has_scrollbar)).min(cols);
    let text_cols = width - usize::from(has_scrollbar);
    let col = at.1.saturating_sub(1).min(cols - width);
    let below = at.0 + 1;
    let row = if below + visible > rows && at.0 >= visible {
        at.0 - visible
    } else {
        below
    };

    let (thumb_top, thumb_len) = if has_scrollbar {
        let len = (visible * visible / items.len()).max(1);
        let top = first * (visible - len) / (items.len() - visible);
        (top, len)
    } else {
        (0, 0)
    };
    let rows = items[first..first + visible]
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let pair = if selected == Some(first + i) {
                theme.pmenu_sel
            } else {
                theme.pmenu
            };
            let cell = |text: &str, bg: Rgba| StyledCell {
                text: text.into(),
                fg: Some(rgb(pair.fg)),
                bg: Some(rgb(bg)),
                reverse: false,
                underline: false,
            };
            let mut cells = vec![cell(" ", pair.bg)];
            for c in item.chars() {
                let wide = is_wide(c);
                if cells.len() + 1 + usize::from(wide) > text_cols {
                    break;
                }
                cells.push(cell(&c.to_string(), pair.bg));
                if wide {
                    cells.push(cell("", pair.bg));
                }
            }
            cells.resize_with(text_cols, || cell(" ", pair.bg));
            if has_scrollbar {
                let on_thumb = (thumb_top..thumb_top + thumb_len).contains(&i);
                cells.push(cell(
                    " ",
                    if on_thumb {
                        theme.thumb
                    } else {
                        theme.scrollbar
                    },
                ));
            }
            cells
        })
        .collect();
    Some(FloatView {
        row,
        col,
        rows,
        zindex: PUM_ZINDEX,
    })
}

fn rgb(c: Rgba) -> u32 {
    (c.0 as u32) << 16 | (c.1 as u32) << 8 | c.2 as u32
}

/// Screen cells of `text`
fn display_width(text: &str) -> usize {
    text.chars().map(|c| 1 + usize::from(is_wide(c))).sum()
}

/// Double-width in a terminal grid (East Asian Wide/Fullwidth, roughly)
fn is_wide(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x115F
        | 0x2E80..=0x303E
        | 0x3041..=0x33FF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xA000..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE4F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1F64F
        | 0x1F900..=0x1F9FF
        | 0x20000..=0x3FFFD)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pum(items: &[&str], selected: Option<usize>, at: (usize, usize)) -> FloatView {
        let items: Vec<String> = items.iter().map(|s| s.to_string()).collect();
        popupmenu_float(&items, selected, at, (20, 12), &Theme::default()).unwrap()
    }

    fn pum_texts(float: &FloatView) -> Vec<String> {
        float
            .rows
            .iter()
            .map(|r| r.iter().map(|c| c.text.as_str()).collect())
            .collect()
    }

    #[test]
    fn popupmenu_goes_below_the_word_with_wide_chars_in_two_cells() {
        let float = pum(&["漢字", "ab"], Some(1), (3, 5));
        assert_eq!((float.row, float.col), (4, 4));
        assert_eq!(pum_texts(&float), [" 漢字 ", " ab   "]);
        assert_eq!(float.rows[0].len(), 6);
        let sel = Theme::default().pmenu_sel.bg;
        assert_eq!(float.rows[1][0].bg, Some(rgb(sel)));
        assert_ne!(float.rows[0][0].bg, Some(rgb(sel)));
    }

    #[test]
    fn popupmenu_goes_above_when_it_does_not_fit_below() {
        let float = pum(&["a", "b", "c"], None, (18, 0));
        assert_eq!((float.row, float.col), (15, 0));
    }

    #[test]
    fn popupmenu_stays_on_the_screen_horizontally() {
        let float = pum(&["abcdef"], None, (0, 10));
        assert_eq!(float.col, 12 - 8);
    }

    #[test]
    fn popupmenu_scrolls_to_the_selection_with_a_scrollbar() {
        let items: Vec<String> = (0..20).map(|i| i.to_string()).collect();
        let items: Vec<&str> = items.iter().map(String::as_str).collect();
        let float = pum(&items, Some(12), (0, 1));
        assert_eq!(float.rows.len(), MAX_VISIBLE_CANDIDATES);
        // The last cell is the scrollbar
        assert_eq!(pum_texts(&float)[0], " 4   ");
        assert_eq!(pum_texts(&float)[MAX_VISIBLE_CANDIDATES - 1], " 12  ");
        let last = |r: usize| float.rows[r].last().unwrap().bg;
        let theme = Theme::default();
        assert!(float.rows.iter().all(|r| r.len() == 5));
        assert!((0..MAX_VISIBLE_CANDIDATES).any(|r| last(r) == Some(rgb(theme.thumb))));
        assert!((0..MAX_VISIBLE_CANDIDATES).any(|r| last(r) == Some(rgb(theme.scrollbar))));
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

    // --- PanelRows ---

    fn content(header: bool, message: bool, candidates: usize) -> PopupContent {
        PopupContent {
            header: header.then(|| Header::ShowCmd("2d".into())),
            message: message.then(|| "msg".into()),
            candidates: (0..candidates).map(|i| i.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn no_panel_when_empty() {
        let rows = PanelRows::new(&content(false, false, 0));
        assert_eq!((rows.header, rows.message, rows.total), (None, None, 0));
        assert_eq!(rows.visible_candidates, 0);
    }

    #[test]
    fn candidates_alone_start_the_panel() {
        let rows = PanelRows::new(&content(false, false, 3));
        assert_eq!((rows.candidates, rows.visible_candidates), (0, 3));
        assert_eq!(rows.total, 3);
    }

    #[test]
    fn header_and_message_come_before_candidates() {
        let rows = PanelRows::new(&content(true, true, 2));
        assert_eq!(rows.header, Some(0));
        assert_eq!(rows.message, Some(1));
        assert_eq!(rows.candidates, 2);
        assert_eq!(rows.total, 4);

        let rows = PanelRows::new(&content(false, true, 2));
        assert_eq!((rows.message, rows.candidates, rows.total), (Some(0), 1, 3));
    }

    #[test]
    fn recording_is_the_first_row() {
        let rows = PanelRows::new(&PopupContent {
            recording: "q".into(),
            ..content(true, false, 2)
        });
        assert_eq!((rows.recording, rows.header), (Some(0), Some(1)));
        assert_eq!((rows.candidates, rows.total), (2, 4));
        assert_eq!(PanelRows::new(&content(true, false, 0)).recording, None);
    }

    #[test]
    fn visible_candidates_are_capped() {
        let rows = PanelRows::new(&content(true, false, 20));
        assert_eq!(rows.visible_candidates, MAX_VISIBLE_CANDIDATES);
        assert_eq!(rows.total, 1 + MAX_VISIBLE_CANDIDATES);
    }

    // --- Layout::row_y ---

    fn layout(above: bool, rows: PanelRows) -> Layout {
        Layout {
            width: 100,
            height: 100,
            above,
            has_text: true,
            text_y: 0.0,
            text_height: 0.0,
            separator_y: None,
            panel_y: 10.0,
            rows,
            has_scrollbar: false,
        }
    }

    #[test]
    fn rows_go_down_from_the_text() {
        let rows = PanelRows::new(&content(true, false, 2));
        let layout = layout(false, rows);
        // Header, then candidates 1 and 2
        assert_eq!(layout.row_y(0, 20.0), 10.0);
        assert_eq!(layout.row_y(1, 20.0), 30.0);
        assert_eq!(layout.row_y(2, 20.0), 50.0);
    }

    #[test]
    fn rows_go_up_from_the_text_when_mirrored() {
        let rows = PanelRows::new(&content(true, false, 2));
        let layout = layout(true, rows);
        // The header is the bottom row, next to the text
        assert_eq!(layout.row_y(0, 20.0), 50.0);
        assert_eq!(layout.row_y(1, 20.0), 30.0);
        assert_eq!(layout.row_y(2, 20.0), 10.0);
    }

    // --- mode_color ---

    #[test]
    fn insert_blends_in_and_other_modes_stand_out() {
        let border = Theme::default().border;
        assert_eq!(mode_color(Mode::Insert, border), border);
        assert_eq!(mode_color(Mode::Replace, border), border);
        for mode in [
            Mode::Normal,
            Mode::OperatorPending,
            Mode::Visual,
            Mode::CmdLine,
        ] {
            assert_ne!(mode_color(mode, border), border);
        }
    }
}
