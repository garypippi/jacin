//! Unified popup window combining Neovim's window grid, the mode, and a
//! column with the command line, messages and candidates
//!
//! Uses zwp_input_popup_surface_v2 which is automatically positioned near
//! the text cursor by the compositor.

use std::time::Instant;

use memmap2::MmapMut;
use tiny_skia::{Color, Paint, Pixmap, Rect, Transform};
use wayland_client::QueueHandle;
use wayland_client::protocol::{wl_buffer, wl_shm, wl_shm_pool, wl_surface};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_v2, zwp_input_popup_surface_v2,
};

pub use super::layout::PopupContent;
use super::layout::{
    BG_COLOR, BORDER_COLOR, CURSOR_BG, HEADER_TEXT_COLOR, Header, HeightHold, ICON_SEPARATOR_WIDTH,
    Layout, MAX_GRID_ROWS, MAX_POPUP_HEIGHT, MAX_POPUP_WIDTH, MAX_VISIBLE_CANDIDATES, MODE_GAP,
    MODE_RECORDING_COLOR, NUMBER_COLOR, NUMBER_WIDTH, PADDING, REC_CIRCLE_RADIUS,
    REC_CIRCLE_TEXT_GAP, SCROLLBAR_BG, SCROLLBAR_THUMB, SCROLLBAR_WIDTH, SECTION_SEPARATOR_HEIGHT,
    SELECTED_BG, TEXT_COLOR, calculate_layout, format_recording_label, mode_label, rgba,
    scrollbar_thumb_geometry,
};
use super::text_render::{TextRenderer, copy_pixmap_to_shm, create_shm_pool, draw_border};
use crate::State;
use crate::neovim::Mode;
use crate::state::{StyledCell, WindowView};

/// Pool size: two ARGB buffers of the maximum popup size (double buffering)
const POOL_SIZE: usize = (MAX_POPUP_WIDTH * MAX_POPUP_HEIGHT * 4 * 2) as usize;

/// Double buffer state
struct Buffer {
    buffer: wl_buffer::WlBuffer,
    in_use: bool,
    width: u32,
    height: u32,
}

/// Surface pair: wl_surface + popup role (created/destroyed together)
struct PopupSurface {
    surface: wl_surface::WlSurface,
    popup_surface: zwp_input_popup_surface_v2::ZwpInputPopupSurfaceV2,
}

/// Unified popup window
pub struct UnifiedPopup {
    surfaces: Option<PopupSurface>,
    compositor: wayland_client::protocol::wl_compositor::WlCompositor,
    input_method: zwp_input_method_v2::ZwpInputMethodV2,
    pool: wl_shm_pool::WlShmPool,
    pool_data: MmapMut,
    buffers: [Option<Buffer>; 2],
    current_buffer: usize,
    width: u32,
    height: u32,
    pub visible: bool,
    renderer: TextRenderer,
    mono_renderer: TextRenderer,
    scroll_offset: usize,
    /// The compositor placed the popup above the cursor (last known; kept
    /// while hidden, the field usually stays where it is)
    above: bool,
    height_hold: HeightHold,
}

impl UnifiedPopup {
    pub fn new(
        compositor: &wayland_client::protocol::wl_compositor::WlCompositor,
        input_method: &zwp_input_method_v2::ZwpInputMethodV2,
        shm: &wl_shm::WlShm,
        qh: &QueueHandle<State>,
        renderer: TextRenderer,
        mono_renderer: TextRenderer,
    ) -> Option<Self> {
        let surfaces = Self::create_surfaces(compositor, input_method, qh);

        let (pool, pool_data) = create_shm_pool(shm, qh, POOL_SIZE, "ime-unified-popup")?;

        Some(Self {
            surfaces: Some(surfaces),
            compositor: compositor.clone(),
            input_method: input_method.clone(),
            pool,
            pool_data,
            buffers: [None, None],
            current_buffer: 0,
            width: 200,
            height: 100,
            visible: false,
            renderer,
            mono_renderer,
            scroll_offset: 0,
            above: false,
            height_hold: HeightHold::default(),
        })
    }

    /// Create a new wl_surface + popup_surface pair
    fn create_surfaces(
        compositor: &wayland_client::protocol::wl_compositor::WlCompositor,
        input_method: &zwp_input_method_v2::ZwpInputMethodV2,
        qh: &QueueHandle<State>,
    ) -> PopupSurface {
        let surface = compositor.create_surface(qh, ());

        // Set empty input region so compositor ignores mouse events on the popup.
        let empty_region = compositor.create_region(qh, ());
        surface.set_input_region(Some(&empty_region));
        empty_region.destroy();

        let popup_surface = input_method.get_input_popup_surface(&surface, qh, ());

        PopupSurface {
            surface,
            popup_surface,
        }
    }

    /// Neovim UI size (columns, rows): the window section at the maximum
    /// popup size. Rows exceed the visible window grid to leave room below
    /// the cursor for floats (nvim-cmp opens a menu below only when
    /// `lines - cursor_row > min(10, entries)`). Floats are clamped to
    /// `lines` by their owners, so they still fit the popup.
    pub fn grid_ui_size(&mut self) -> (usize, usize) {
        let cell_width = self.mono_renderer.measure_text(" ").max(1.0);
        let cols = ((MAX_POPUP_WIDTH as f32 - PADDING * 2.0) / cell_width).floor() as usize;
        let line_height = self.renderer.line_height().max(1.0);
        // Minus padding, the mode row and its separator
        let grid_height =
            MAX_POPUP_HEIGHT as f32 - PADDING * 2.0 - line_height - SECTION_SEPARATOR_HEIGHT;
        let rows = (grid_height / line_height).floor() as usize;
        (cols.max(20), rows.max(MAX_GRID_ROWS + 1))
    }

    pub fn update(&mut self, content: &PopupContent, qh: &QueueHandle<State>) {
        if content.is_empty() {
            self.hide();
            return;
        }

        // Recreate surface pair if it was destroyed on hide
        if self.surfaces.is_none() {
            self.surfaces = Some(Self::create_surfaces(
                &self.compositor,
                &self.input_method,
                qh,
            ));
        }

        // Adjust scroll offset to keep the selection visible
        let visible_count = MAX_VISIBLE_CANDIDATES.min(content.candidates.len());
        match content.selected {
            _ if content.candidates.is_empty() => self.scroll_offset = 0,
            Some(selected) if selected < self.scroll_offset => self.scroll_offset = selected,
            Some(selected) if selected >= self.scroll_offset + visible_count => {
                self.scroll_offset = selected + 1 - visible_count;
            }
            _ => {}
        }
        // The list may have shrunk
        self.scroll_offset = self
            .scroll_offset
            .min(content.candidates.len() - visible_count);

        let layout = self.layout(content, Instant::now());
        if (layout.width, layout.height) != (self.width, self.height) {
            log::debug!("[POPUP] Size {}x{}", layout.width, layout.height);
        }
        self.width = layout.width;
        self.height = layout.height;

        self.render(content, &layout, qh);
        self.visible = true;
    }

    /// Layout at the height `HeightHold` keeps
    fn layout(&mut self, content: &PopupContent, now: Instant) -> Layout {
        let layout = self.layout_at(content, 0);
        let height = self.height_hold.height(layout.natural_height, now);
        if height > layout.natural_height {
            self.layout_at(content, height)
        } else {
            layout
        }
    }

    fn layout_at(&mut self, content: &PopupContent, min_height: u32) -> Layout {
        calculate_layout(
            content,
            self.above,
            min_height,
            &mut self.renderer,
            &mut self.mono_renderer,
        )
    }

    /// Record where the compositor placed the popup. Returns true if it
    /// changed (the popup must be laid out again).
    pub fn set_above(&mut self, above: bool) -> bool {
        let changed = self.above != above;
        if changed {
            log::debug!("[POPUP] Placed {}", if above { "above" } else { "below" });
            self.above = above;
        }
        changed
    }

    /// The popup is taller than its content (for a while)
    pub fn is_holding_height(&self) -> bool {
        self.visible && self.height_hold.is_holding()
    }

    /// The held height may be released (the popup must be laid out again)
    pub fn hold_expired(&self, now: Instant) -> bool {
        self.visible && self.height_hold.expired(now)
    }

    pub fn hide(&mut self) {
        self.height_hold = HeightHold::default();
        if self.visible {
            // First unmap the surface for immediate visual feedback, then
            // destroy both the popup surface role and wl_surface so the
            // compositor stops tracking them for hit-testing. Without the
            // destroy, the unmapped popup surface can absorb pointer clicks
            // and prevent refocusing text fields. Both are recreated on
            // next update().
            if let Some(s) = self.surfaces.take() {
                s.surface.attach(None, 0, 0);
                s.surface.commit();
                s.popup_surface.destroy();
                s.surface.destroy();
            }
            self.visible = false;
            self.scroll_offset = 0;
        }
    }

    /// Mark a buffer as released (called from Dispatch)
    pub fn buffer_released(&mut self, buffer_idx: usize) {
        if let Some(buf) = self.buffers[buffer_idx].as_mut() {
            buf.in_use = false;
        }
    }

    pub fn destroy(self) {
        for slot in self.buffers.into_iter().flatten() {
            slot.buffer.destroy();
        }
        if let Some(s) = self.surfaces {
            s.popup_surface.destroy();
            s.surface.destroy();
        }
        self.pool.destroy();
    }

    fn render(&mut self, content: &PopupContent, layout: &Layout, qh: &QueueHandle<State>) {
        let _perf_start = std::time::Instant::now();
        let buffer_size = (self.width * self.height * 4) as usize;
        if buffer_size * 2 > POOL_SIZE {
            log::warn!(
                "[POPUP] Buffer too large ({}x{}), skipping render",
                self.width,
                self.height
            );
            return;
        }

        let buffer_idx = self.find_available_buffer();
        let offset = buffer_idx * buffer_size;

        let Some(mut pixmap) = Pixmap::new(self.width, self.height) else {
            log::warn!(
                "[POPUP] Failed to allocate pixmap ({}x{}), skipping render",
                self.width,
                self.height
            );
            return;
        };

        pixmap.fill(rgba(BG_COLOR));

        draw_border(&mut pixmap, self.width, self.height, rgba(BORDER_COLOR));

        if layout.has_window {
            if let Some(ref view) = content.window_view {
                self.render_grid_section(&mut pixmap, view, content.mode, layout);
            }

            // Separator between the window and the mode row (pixel-aligned:
            // fractional positions trip tiny-skia's AA hairline path)
            let sep_y = layout.separator_y.round();
            fill_rect(
                &mut pixmap,
                PADDING,
                sep_y,
                self.width as f32 - PADDING,
                1.0,
                rgba(BORDER_COLOR),
            );
        }

        self.render_status_section(&mut pixmap, content, layout);

        let dest = &mut self.pool_data[offset..offset + buffer_size];
        copy_pixmap_to_shm(&pixmap, dest);

        // Get or create wl_buffer for this slot (reuse if dimensions match)
        let needs_new_buffer = match &self.buffers[buffer_idx] {
            None => true,
            Some(buf) => buf.width != self.width || buf.height != self.height,
        };
        if needs_new_buffer {
            if let Some(old) = self.buffers[buffer_idx].take() {
                old.buffer.destroy();
            }
            let buffer = self.pool.create_buffer(
                offset as i32,
                self.width as i32,
                self.height as i32,
                (self.width * 4) as i32,
                wl_shm::Format::Argb8888,
                qh,
                buffer_idx,
            );
            self.buffers[buffer_idx] = Some(Buffer {
                buffer,
                in_use: true,
                width: self.width,
                height: self.height,
            });
        } else {
            self.buffers[buffer_idx].as_mut().unwrap().in_use = true;
        }

        let Some(ref s) = self.surfaces else {
            return;
        };
        let buffer = &self.buffers[buffer_idx].as_ref().unwrap().buffer;
        s.surface.attach(Some(buffer), 0, 0);
        s.surface
            .damage_buffer(0, 0, self.width as i32, self.height as i32);
        s.surface.commit();

        self.current_buffer = buffer_idx;
        log::trace!(
            "[PERF] render: {:.2}ms ({}x{})",
            _perf_start.elapsed().as_secs_f64() * 1000.0,
            self.width,
            self.height
        );
    }

    /// Render Neovim's window grid: monospace cells with
    /// Neovim highlight colors over the popup theme, the cursor, and floating
    /// windows (e.g. nvim-cmp menus) on top.
    fn render_grid_section(
        &mut self,
        pixmap: &mut Pixmap,
        view: &WindowView,
        mode: Mode,
        layout: &Layout,
    ) {
        let geo = CellGeometry {
            left: PADDING,
            top: layout.window_y,
            cell_width: self.mono_renderer.measure_text(" "),
            line_height: self.renderer.line_height(),
            right_edge: layout.width as f32 - PADDING,
        };
        let block_cursor =
            view.cursor_visible && !matches!(mode, Mode::Insert | Mode::Replace | Mode::CmdLine);
        let (cursor_row, cursor_col) = view.cursor;

        for (row, cells) in view.rows.iter().enumerate() {
            let style_of = |col: usize, cell: &StyledCell| {
                let on_cursor = block_cursor
                    && row == cursor_row
                    && (col == cursor_col || (col == cursor_col + 1 && cell.text.is_empty()));
                if on_cursor {
                    CellStyle::Cursor
                } else {
                    CellStyle::Normal
                }
            };
            self.draw_grid_row(pixmap, &geo, row, 0, cells, style_of);
        }

        // Cursor past the end of the row (or on an empty row)
        let row_len = view.rows.get(cursor_row).map_or(0, Vec::len);
        let x0 = geo.x(cursor_col);
        if view.cursor_visible && x0 < geo.right_edge {
            let (y_top, height) = (geo.y(cursor_row), geo.line_height.round());
            if !block_cursor {
                fill_rect(pixmap, x0, y_top, x0 + 2.0, height, rgba(TEXT_COLOR));
            } else if cursor_col >= row_len {
                let x1 = geo.x(cursor_col + 1).min(geo.right_edge);
                fill_rect(pixmap, x0, y_top, x1, height, rgba(CURSOR_BG));
            }
        }

        // Floating windows on top, lowest zindex first
        for float in &view.floats {
            for (r, cells) in float.rows.iter().enumerate() {
                self.draw_grid_row(pixmap, &geo, float.row + r, float.col, cells, |_, _| {
                    CellStyle::Opaque
                });
            }
        }
    }

    /// Draw one row of grid cells starting at screen column `first_col`.
    /// All backgrounds are painted before any glyph: the right half of a
    /// double-width char is a separate (empty) cell whose background would
    /// otherwise cover the glyph.
    fn draw_grid_row(
        &mut self,
        pixmap: &mut Pixmap,
        geo: &CellGeometry,
        row: usize,
        first_col: usize,
        cells: &[StyledCell],
        style_of: impl Fn(usize, &StyledCell) -> CellStyle,
    ) {
        let visible = (0..cells.len())
            .take_while(|&i| geo.x(first_col + i + 1) <= geo.right_edge)
            .count();
        let colors: Vec<(Color, Option<Color>)> = cells[..visible]
            .iter()
            .enumerate()
            .map(|(i, cell)| cell_colors(cell, style_of(first_col + i, cell)))
            .collect();
        let y_top = geo.y(row);

        for (i, cell) in cells[..visible].iter().enumerate() {
            let (fg, bg) = colors[i];
            let (x0, x1) = (geo.x(first_col + i), geo.x(first_col + i + 1));
            if let Some(bg) = bg {
                fill_rect(pixmap, x0, y_top, x1, geo.line_height.round(), bg);
            }
            if cell.underline {
                let y = (y_top + geo.line_height * 0.85).round();
                fill_rect(pixmap, x0, y, x1, 1.0, fg);
            }
        }

        for (i, cell) in cells[..visible].iter().enumerate() {
            if cell.text.is_empty() || cell.text == " " {
                continue;
            }
            // Center the glyph in its span (two cells for double-width chars)
            let span = if cells.get(i + 1).is_some_and(|c| c.text.is_empty()) {
                2
            } else {
                1
            };
            let (x0, x1) = (geo.x(first_col + i), geo.x(first_col + i + span));
            let advance = self.mono_renderer.measure_text(&cell.text);
            let x = x0 + ((x1 - x0) - advance).max(0.0) / 2.0;
            let y_baseline = y_top + geo.line_height * 0.75;
            self.mono_renderer
                .draw_text(pixmap, &cell.text, x, y_baseline, colors[i].0);
        }
    }

    /// Render the mode/REC icons and the column right of them: header,
    /// message and candidates
    fn render_status_section(
        &mut self,
        pixmap: &mut Pixmap,
        content: &PopupContent,
        layout: &Layout,
    ) {
        let line_height = self.renderer.line_height();
        let mode_y = layout.row_y(0, line_height);
        let y_baseline = mode_y + line_height * 0.75;

        let (mode_text, mode_color) = mode_label(content.mode);
        let mode_x = PADDING;
        self.mono_renderer
            .draw_text(pixmap, mode_text, mode_x, y_baseline, rgba(mode_color));

        if !content.recording.is_empty() {
            let rec_x = mode_x + self.mono_renderer.measure_text(mode_text) + MODE_GAP;

            // Red filled circle (hidden during blink-off phase)
            if content.rec_blink_on {
                draw_filled_circle(
                    pixmap,
                    rec_x + REC_CIRCLE_RADIUS,
                    mode_y + line_height * 0.5,
                    REC_CIRCLE_RADIUS,
                    rgba(MODE_RECORDING_COLOR),
                );
            }

            let rec_label = format_recording_label(&content.recording);
            let text_x = rec_x + REC_CIRCLE_RADIUS * 2.0 + REC_CIRCLE_TEXT_GAP;
            self.mono_renderer.draw_text(
                pixmap,
                &rec_label,
                text_x,
                y_baseline,
                rgba(MODE_RECORDING_COLOR),
            );
        }

        // Separator between the icons and the column, along the column
        let column_height = (layout.rows.total as f32 * line_height).round();
        fill_rect(
            pixmap,
            layout.separator_x,
            layout.status_y.round(),
            layout.separator_x + ICON_SEPARATOR_WIDTH,
            column_height,
            rgba(BORDER_COLOR),
        );

        if let (Some(row), Some(header)) = (layout.rows.header, &content.header) {
            let y = layout.row_y(row, line_height);
            match header {
                Header::Cmdline { text, cursor_byte } => {
                    self.render_cmdline(pixmap, text, *cursor_byte, layout.column_x, y)
                }
                Header::ShowCmd(text) => self.mono_renderer.draw_text(
                    pixmap,
                    text,
                    layout.column_x,
                    y + line_height * 0.75,
                    rgba(HEADER_TEXT_COLOR),
                ),
            }
        }

        if let (Some(row), Some(message)) = (layout.rows.message, &content.message) {
            let y_text = layout.row_y(row, line_height) + line_height * 0.75;
            self.renderer
                .draw_text(pixmap, message, layout.column_x, y_text, rgba(TEXT_COLOR));
        }

        if layout.rows.visible_candidates > 0 {
            self.render_candidates(pixmap, content, layout);
        }
    }

    /// Command line text with a line cursor at `cursor_byte`
    fn render_cmdline(
        &mut self,
        pixmap: &mut Pixmap,
        text: &str,
        cursor_byte: usize,
        x: f32,
        y: f32,
    ) {
        let line_height = self.renderer.line_height();
        let color = rgba(HEADER_TEXT_COLOR);
        let mut cursor_x = None;
        let mut char_x = x;
        for (byte, c) in text.char_indices() {
            if byte == cursor_byte {
                cursor_x = Some(char_x);
            }
            let s = c.to_string();
            self.mono_renderer
                .draw_text(pixmap, &s, char_x, y + line_height * 0.75, color);
            char_x += self.mono_renderer.measure_text(&s);
        }
        let cursor_x = cursor_x.unwrap_or(char_x);
        fill_rect(pixmap, cursor_x, y, cursor_x + 2.0, line_height, color);
    }

    /// Candidates with the selection and a scrollbar (upwards from the mode
    /// row when mirrored)
    fn render_candidates(&mut self, pixmap: &mut Pixmap, content: &PopupContent, layout: &Layout) {
        let line_height = self.renderer.line_height();
        let visible_count = layout.rows.visible_candidates;
        let first_y = layout.row_y(layout.rows.candidates, line_height);
        let last_y = layout.row_y(layout.rows.candidates + visible_count - 1, line_height);
        let candidates_y = first_y.min(last_y);
        let scrollbar_x = self.width as f32 - SCROLLBAR_WIDTH - 2.0;

        for (visible_idx, candidate) in content
            .candidates
            .iter()
            .skip(self.scroll_offset)
            .take(visible_count)
            .enumerate()
        {
            let actual_idx = self.scroll_offset + visible_idx;
            let y_base = layout.row_y(layout.rows.candidates + visible_idx, line_height);
            let y_text = y_base + line_height * 0.75;

            if content.selected == Some(actual_idx) {
                let right = if layout.has_scrollbar {
                    scrollbar_x - 2.0
                } else {
                    self.width as f32
                };
                let left = layout.separator_x + ICON_SEPARATOR_WIDTH;
                fill_rect(pixmap, left, y_base, right, line_height, rgba(SELECTED_BG));
            }

            let number = format!("{}.", actual_idx + 1);
            self.renderer
                .draw_text(pixmap, &number, layout.column_x, y_text, rgba(NUMBER_COLOR));
            self.renderer.draw_text(
                pixmap,
                candidate,
                layout.column_x + NUMBER_WIDTH,
                y_text,
                rgba(TEXT_COLOR),
            );
        }

        if layout.has_scrollbar {
            let scrollbar_height = visible_count as f32 * line_height;
            fill_rect(
                pixmap,
                scrollbar_x,
                candidates_y,
                scrollbar_x + SCROLLBAR_WIDTH,
                scrollbar_height,
                rgba(SCROLLBAR_BG),
            );
            let thumb = scrollbar_thumb_geometry(
                visible_count,
                content.candidates.len(),
                scrollbar_height,
                self.scroll_offset,
                candidates_y,
            );
            // The first candidate is at the bottom when mirrored
            let thumb_y = if layout.above {
                2.0 * candidates_y + scrollbar_height - thumb.y - thumb.height
            } else {
                thumb.y
            };
            fill_rect(
                pixmap,
                scrollbar_x,
                thumb_y,
                scrollbar_x + SCROLLBAR_WIDTH,
                thumb.height,
                rgba(SCROLLBAR_THUMB),
            );
        }
    }

    fn find_available_buffer(&mut self) -> usize {
        let other = 1 - self.current_buffer;
        if self.buffers[other]
            .as_ref()
            .map(|b| !b.in_use)
            .unwrap_or(true)
        {
            return other;
        }
        self.current_buffer
    }
}

/// Draw a filled circle on the pixmap using midpoint algorithm
fn draw_filled_circle(pixmap: &mut Pixmap, cx: f32, cy: f32, radius: f32, color: Color) {
    let r = radius as i32;
    let cx_i = cx as i32;
    let cy_i = cy as i32;
    let pw = pixmap.width() as i32;
    let ph = pixmap.height() as i32;

    let mut paint = Paint::default();
    paint.set_color(color);

    // Scan lines from top to bottom of bounding box
    for dy in -r..=r {
        let py = cy_i + dy;
        if py < 0 || py >= ph {
            continue;
        }
        // Half-width at this scanline
        let half_w = ((radius * radius - (dy as f32) * (dy as f32)).max(0.0)).sqrt();
        let x_start = (cx_i as f32 - half_w).ceil() as i32;
        let x_end = (cx_i as f32 + half_w).floor() as i32;
        let x_start = x_start.max(0);
        let x_end = x_end.min(pw - 1);
        if x_start <= x_end
            && let Some(rect) =
                Rect::from_xywh(x_start as f32, py as f32, (x_end - x_start + 1) as f32, 1.0)
        {
            pixmap.fill_rect(rect, &paint, Transform::identity(), None);
        }
    }
}

/// 0xRRGGBB → opaque color
fn rgb_color(rgb: u32) -> Color {
    Color::from_rgba8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, 255)
}

/// Pixel geometry of the grid area in the popup
struct CellGeometry {
    left: f32,
    top: f32,
    cell_width: f32,
    line_height: f32,
    /// Cells must end at or before this x
    right_edge: f32,
}

impl CellGeometry {
    /// Left edge of `col` (integer edges keep fills crisp and off
    /// tiny-skia's AA hairline path)
    fn x(&self, col: usize) -> f32 {
        (self.left + col as f32 * self.cell_width).round()
    }

    /// Top edge of `row`
    fn y(&self, row: usize) -> f32 {
        (self.top + row as f32 * self.line_height).round()
    }
}

/// Foreground and optional background of a cell over the popup theme
fn cell_colors(cell: &StyledCell, style: CellStyle) -> (Color, Option<Color>) {
    let theme_fg = rgba(TEXT_COLOR);
    let theme_bg = rgba(BG_COLOR);
    let mut fg = cell.fg.map_or(theme_fg, rgb_color);
    let mut bg = cell.bg.map(rgb_color);
    if cell.reverse {
        (fg, bg) = (bg.unwrap_or(theme_bg), Some(fg));
    }
    match style {
        CellStyle::Normal => (fg, bg),
        CellStyle::Cursor => (Color::from_rgba8(40, 44, 52, 255), Some(rgba(CURSOR_BG))),
        // Floats hide what is below them even where they have no highlight
        CellStyle::Opaque => (fg, Some(bg.unwrap_or(theme_bg))),
    }
}

/// How a grid cell is painted
#[derive(Clone, Copy)]
enum CellStyle {
    Normal,
    /// Under the block cursor
    Cursor,
    /// Part of a floating window: always paints its background
    Opaque,
}

/// Fill [x0, x1) x [y, y + h) with a solid color
fn fill_rect(pixmap: &mut Pixmap, x0: f32, y: f32, x1: f32, h: f32, color: Color) {
    if let Some(rect) = Rect::from_xywh(x0, y, x1 - x0, h) {
        let mut paint = Paint::default();
        paint.set_color(color);
        pixmap.fill_rect(rect, &paint, Transform::identity(), None);
    }
}
